# Bounded media storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make log and download storage cost independent of stored media, and let a podcast failure leave SD logging running (t-121).

**Architecture:** Card layout v2 adds a 16 MiB LittleFS log region and turns the media region into raw 1 MiB allocation units. The EKV catalogue stores the unit bitmap, a used-unit counter and one unit list per episode, all changed in single transactions. A new `UnitMedia` implements the (now catalogue-aware) `MediaStore<F>` trait. The storage owner tears down only the failed scope (logs or podcasts); bus faults stay global.

**Tech Stack:** Rust `no_std` + `alloc`, EKV 1.0.0 (vendored), `littlefs-rust` (vendored), `allocator-api2`, `heapless`, Embassy on ESP32-S3.

**Spec:** `docs/superpowers/specs/2026-11-11-bounded-media-storage-design.md` (read it first).

## Global Constraints

- No deadline increases: 500 ms per request, 2 s mount, 1.2 s shutdown, 2.5 s client stay as they are. `OWNER_MARGIN` = 20 ms is taken *inside* the 500 ms.
- Boot never formats or migrates (ADR-0015). Only `prepare_image` (host, create-new file) initializes regions.
- Only v2 descriptors (`THEOSD02`) are accepted; v1 is rejected as `LayoutError::Version`. Legacy whole-card LittleFS (no descriptor) is unchanged.
- Log region: 16 MiB = 32,768 sectors. Unit: 1 MiB = 2,048 sectors. Bitmap page: 4,096 bytes = 32,768 units. Record: `state u8`, `len u64 LE`, then `u32 LE` units; max 1,021 units.
- Catalogue keys added: `b` + `u16 BE` page, `m/units` (`u32 LE`), `u` + `u64 BE` episode. Schema 1 → 2, no migration.
- ESP/board code stays in `src/device/`; `crates/*` stay HAL-independent and host-tested.
- Regression tests live in side-by-side `*_tests.rs`, tagged `// Regression (t-121): ...`.
- Host commands in the sandbox: prefix with `RUSTC_WRAPPER= CARGO_BUILD_BUILD_DIR=$PWD/target/build-dir`. Link/flash recipes in a worktree need `RUSTFLAGS="-C link-arg=-Tlinkall.x"`. No board operation without an explicit request.
- Commits reference `(t-121)`, no `Co-Authored-By`, and need `Reviewed-by:` or `Review-waived:` (hook).

## File map

| File | Change |
|---|---|
| `crates/theo-storage/src/card_layout.rs` (+ `_tests.rs`) | v2 descriptor with log region, `media_units()` |
| `crates/theo-storage/src/regions.rs` (+ `_tests.rs`) | three-way `split`, `Discovered::Regions` with log handle, failure-aware owner |
| `crates/theo-storage/src/littlefs.rs` | `SectorIo::device_failed` |
| `crates/theo-storage/src/scope.rs` (+ `_tests.rs`) | new: `Scope`, `failure_scope` |
| `crates/theo-podcasts/src/units.rs` (+ `_tests.rs`) | new: `UnitTable`, record codec, catalogue unit persistence |
| `crates/theo-podcasts/src/unit_media.rs` (+ `_tests.rs`) | new: `UnitMedia` (`MediaStore<F>`) |
| `crates/theo-podcasts/src/media.rs`, `download.rs`, `reclaim.rs`, `recovery.rs`, `runtime.rs`, `worker.rs`, `media_test_support.rs`, their tests | `MediaStore<F>` with `cat` parameter |
| `crates/theo-podcasts/src/littlefs_media.rs` (+ `_tests.rs`) | deleted |
| `crates/theo-podcasts/src/catalogue.rs` | schema 2, `m/units` at create |
| `crates/theo-podcasts/examples/prepare_image/*` | v2 image |
| `src/device/sd_worker.rs` | log/media regions, `UnitMedia`, soft/hard deadlines, per-scope teardown |
| `docs/adr/0020-*.md`, `docs/adr/README.md`, `crates/theo-storage/ARCHITECTURE.md`, ADR-0019 note | docs |

---

### Task 1: Card layout v2 and three-way region split

**Files:**
- Modify: `crates/theo-storage/src/card_layout.rs`
- Modify: `crates/theo-storage/src/regions.rs`
- Test: `crates/theo-storage/src/card_layout_tests.rs`, `crates/theo-storage/src/regions_tests.rs`
- Modify callers: `crates/theo-podcasts/examples/prepare_image/main.rs`, `src/device/sd_worker.rs` (compile only; behaviour in Tasks 5–6)

**Interfaces:**
- Produces: `CardLayout::log() -> Region`, `CardLayout::media_units() -> u32`, `pub const LOG_SECTORS: u32 = 32_768`, `pub const UNIT_SECTORS: u32 = 2_048`; `regions::split(device, layout) -> (RegionIo<D>, RegionIo<D>, RegionIo<D>)` in order database, log, media; `Discovered::Regions { database, log, media, layout }`.

- [ ] **Step 1: Write the failing tests** in `card_layout_tests.rs`:

```rust
// Regression (t-121): logs get their own region so log allocation never scans media.
#[test]
fn v2_layout_places_log_then_unit_aligned_media() {
    let card = 8 * 1024 * 1024 * 2; // 8 GiB in sectors
    let layout = CardLayout::new(card, 4096).unwrap();
    let db_end = layout.database().start() + layout.database().sectors();
    assert_eq!(layout.log().start(), db_end.next_multiple_of(8));
    assert_eq!(layout.log().sectors(), LOG_SECTORS);
    let log_end = layout.log().start() + LOG_SECTORS;
    assert_eq!(layout.media().start(), log_end.next_multiple_of(UNIT_SECTORS));
    assert_eq!(layout.media().sectors() % UNIT_SECTORS, 0);
    assert_eq!(layout.media_units(), layout.media().sectors() / UNIT_SECTORS);
    assert!(layout.media().start() as u64 + layout.media().sectors() as u64 <= card);
}

// Regression (t-121): v1 descriptors are no longer mountable (none exist).
#[test]
fn v1_descriptor_is_rejected_as_version() {
    let card = 8 * 1024 * 1024 * 2;
    let mut v1 = CardLayout::new(card, 4096).unwrap().encode();
    v1[..8].copy_from_slice(b"THEOSD01");
    let crc = crate::page_store::crc32(&v1[..508]);
    v1[508..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(CardLayout::select(&v1, &v1, card), Err(LayoutError::Version));
}

#[test]
fn v2_round_trips_and_too_small_media_is_rejected() {
    let card = 8 * 1024 * 1024 * 2;
    let layout = CardLayout::new(card, 4096).unwrap();
    let bytes = layout.encode();
    assert_eq!(CardLayout::select(&bytes, &bytes, card), Ok(Some(layout)));
    let tight = layout.media().start() as u64 + 65_536 - 1;
    assert_eq!(CardLayout::new(tight, 4096), Err(LayoutError::Geometry));
}
```

And in `regions_tests.rs`, a test that `split` returns three handles whose relative sector 0 translates to `database().start()`, `log().start()` and `media().start()` respectively, and that writing relative sector 0 on each lands on those absolute sectors of the underlying test device (reuse the file's existing in-memory device).

Update existing tests that build the layout or call `split`/`Discovered::Regions` to the new shapes (fix `media()` expectations: media now starts after the log region).

- [ ] **Step 2: Run** `RUSTC_WRAPPER= CARGO_BUILD_BUILD_DIR=$PWD/target/build-dir just test` — expect compile errors / FAIL on the new tests.

- [ ] **Step 3: Implement** in `card_layout.rs`:

```rust
const MAGIC: &[u8; 8] = b"THEOSD02";
const VERSION: u32 = 2;
const V1_MAGIC: &[u8; 8] = b"THEOSD01";
pub const LOG_SECTORS: u32 = 32_768; // 16 MiB: two 128 KiB log files plus LittleFS headroom.
pub const UNIT_SECTORS: u32 = 2_048; // 1 MiB raw media allocation unit.
```

- Add `log: Region` to `CardLayout`. In `new`: `log_start = (DATABASE_START + database_sectors).next_multiple_of(8)`; `media_start = (log_start + LOG_SECTORS).next_multiple_of(UNIT_SECTORS)`; `media_sectors = (card - media_start) / UNIT_SECTORS * UNIT_SECTORS`; keep `MIN_MEDIA_SECTORS` check.
- `encode`: version `2`; fields at 16 card, 24/32 database, 40/48 log, 56/64 media, pages at 72..76; rest zero; CRC at 508.
- `decode`: `if &bytes[..8] == V1_MAGIC { return Err(LayoutError::Version) }` before the magic check (after CRC check is fine, but v1 with valid CRC must report `Version`); version field must equal 2; pages from 72..76.
- `pub const fn log(self) -> Region`, `pub const fn media_units(self) -> u32 { self.media.sectors / UNIT_SECTORS }`.
- Update the module doc to say v2 and list the four ranges.

In `regions.rs`: `split` returns `(database, log, media)` (three `RegionIo` sharing one `Owner`), `Discovered::Regions { database, log, media, layout }` (named fields), `discover` builds it. Update doc comments ("three handles").

Update callers to compile: `prepare_image` takes `(db, log, media)` and, for now, formats LittleFS on `log` with `Geometry::new(layout.log().sectors() as u64)`; `sd_worker::mount_owner` destructures three handles and mounts LittleFS on the log handle with the log geometry, passing `media` nowhere yet (keep `LittlefsMedia` on the same log `fs` temporarily so the build stays green; Task 5 replaces it).

- [ ] **Step 4: Run** `just test` (prefixed) and `RUSTC_WRAPPER= CARGO_BUILD_BUILD_DIR=$PWD/target/build-dir just check` — expect PASS.

- [ ] **Step 5: Commit** `feat(storage): card layout v2 with a dedicated log region (t-121)`.

---

### Task 2: Catalogue unit accounting

**Files:**
- Create: `crates/theo-podcasts/src/units.rs`, `crates/theo-podcasts/src/units_tests.rs`
- Modify: `crates/theo-podcasts/src/catalogue.rs` (schema 2, `m/units`, expose `commit_error`/`read_error`/`write_error` as `pub(crate)` — already), `crates/theo-podcasts/src/lib.rs` (`pub mod units;`), `crates/theo-podcasts/Cargo.toml` (`allocator-api2.workspace = true` and `theo-storage.workspace = true` in `[dependencies]`; drop the dev-only `theo-storage` entry)

**Interfaces:**
- Produces:
  - `pub use theo_storage::card_layout::UNIT_SECTORS; pub const UNIT_BYTES: u64 = 1 << 20; pub const UNITS_PER_PAGE: u32 = 32_768; pub const PAGE_BYTES: usize = 4096; pub const MAX_RECORD_UNITS: usize = 1021; pub const RECORD_HEADER: usize = 9;`
  - `#[repr(u8)] pub enum RecordState { Temp = 0, Published = 1 }`
  - `pub struct UnitTable<A: Allocator>` with `new_in(units: u32, alloc: A) -> Result<Self, AllocError>`, `units() -> u32`, `used() -> u32`, `free() -> u32`, `allocate() -> Option<u32>`, `release(unit: u32) -> bool`, `page_of(&self, unit: u32) -> u16`, `page(p: u16) -> &[u8]`, `pub(crate) page_mut(p: u16) -> &mut [u8]`, `pages() -> u16`.
  - `pub fn encode_record(state: RecordState, len: u64, units: &[u32], out: &mut [u8]) -> usize` and `pub struct RecordView<'a> { pub state: RecordState, pub len: u64, units: &'a [u8] }` with `decode(bytes) -> Option<Self>`, `count() -> usize`, `unit(i) -> u32`.
  - On `Catalogue<F>` (in `units.rs`, `impl<F: Flash> Catalogue<F>`):
    - `pub(crate) async fn load_units<A: Allocator>(&mut self, table: &mut UnitTable<A>) -> Result<(), Error<F::Error>>`
    - `pub(crate) async fn commit_units<A: Allocator>(&mut self, table: &UnitTable<A>, dirty: &[u16], id: EpisodeId, record: Option<&[u8]>) -> Result<(), Error<F::Error>>` (writes dirty `b` pages from the table, `m/units` = `table.used()`, then `u<id>` = record or delete)
    - `pub(crate) async fn unit_record<'a>(&mut self, id: EpisodeId, buf: &'a mut [u8]) -> Result<Option<RecordView<'a>>, Error<F::Error>>`
    - `pub(crate) async fn unit_ids(&mut self, after: Option<EpisodeId>, out: &mut [EpisodeId; FILE_PAGE], buf: &mut [u8]) -> Result<usize, Error<F::Error>>`

- [ ] **Step 1: Write the failing tests** in `units_tests.rs`:

```rust
use super::*;
use crate::{catalogue::*, test_support::Image};
use allocator_api2::alloc::Global;
use futures::executor::block_on;

#[test]
fn table_allocates_next_fit_and_counts() {
    let mut t = UnitTable::new_in(40_000, Global).unwrap();
    assert_eq!(t.pages(), 2);
    assert_eq!(t.allocate(), Some(0));
    assert_eq!(t.allocate(), Some(1));
    assert!(t.release(0));
    assert_eq!(t.allocate(), Some(2)); // next-fit, not first-fit
    assert_eq!(t.used(), 2);
    assert!(!t.release(0)); // double free refused
    assert_eq!(t.page_of(32_768), 1);
}

#[test]
fn table_is_full_after_all_units() {
    let mut t = UnitTable::new_in(3, Global).unwrap();
    for _ in 0..3 { t.allocate().unwrap(); }
    assert_eq!(t.allocate(), None);
    assert_eq!(t.free(), 0);
}

#[test]
fn record_round_trips_and_rejects_oversize() {
    let mut buf = [0; 4096];
    let n = encode_record(RecordState::Temp, 5, &[7, 9], &mut buf);
    let r = RecordView::decode(&buf[..n]).unwrap();
    assert_eq!((r.state, r.len, r.count(), r.unit(1)), (RecordState::Temp, 5, 2, 9));
    assert!(RecordView::decode(&buf[..n - 1]).is_none());
    assert_eq!(RECORD_HEADER + MAX_RECORD_UNITS * 4, 4093);
}

// Regression (t-121): bitmap, counter and records change in one transaction.
#[test]
fn committed_units_reload_with_matching_counter() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut t = UnitTable::new_in(100, Global).unwrap();
        cat.load_units(&mut t).await.unwrap();
        let unit = t.allocate().unwrap();
        let mut rec = [0; 4096];
        let n = encode_record(RecordState::Temp, 0, &[unit], &mut rec);
        cat.commit_units(&t, &[t.page_of(unit)], EpisodeId(3), Some(&rec[..n])).await.unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let mut again = UnitTable::new_in(100, Global).unwrap();
        cat.load_units(&mut again).await.unwrap();
        assert_eq!(again.used(), 1);
        let mut buf = [0; 4096];
        assert_eq!(cat.unit_record(EpisodeId(3), &mut buf).await.unwrap().unwrap().unit(0), unit);
        let mut ids = [EpisodeId(0); crate::media::FILE_PAGE];
        assert_eq!(cat.unit_ids(None, &mut ids, &mut buf).await.unwrap(), 1);
    });
}

// Regression (t-121): a counter that disagrees with the bitmap is corruption, not a format.
#[test]
fn counter_mismatch_is_corrupt() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut t = UnitTable::new_in(100, Global).unwrap();
        t.allocate().unwrap(); // bitmap in RAM says 1 ...
        let page = t.page(0).to_vec();
        let mut tx = cat.db.write_transaction().await;
        tx.write(&[b'b', 0, 0], &page).await.unwrap(); // ... counter stays 0
        tx.commit().await.unwrap();
        let mut fresh = UnitTable::new_in(100, Global).unwrap();
        assert!(matches!(cat.load_units(&mut fresh).await, Err(Error::Corrupt)));
    });
}
```

Also: a bit set beyond `units()` in the last page → `Corrupt`; schema 1 catalogue → `Schema` on `open` (write `m/schema` = 1 directly through `db` in the test, then reopen).

- [ ] **Step 2: Run** the podcasts tests (`just test`, prefixed) — FAIL (module missing).

- [ ] **Step 3: Implement** `units.rs`:

```rust
//! Raw media allocation units, persisted in the catalogue (t-121, ADR-0020).
//!
//! Bitmap pages (`b`), the used counter (`m/units`) and per-episode unit
//! records (`u`) only ever change together in one EKV transaction, so the
//! bitmap always equals the union of records.
use crate::catalogue::{Catalogue, EpisodeId, Error, commit_error, read_error, write_error};
use crate::media::FILE_PAGE;
use allocator_api2::{alloc::{AllocError, Allocator}, vec::Vec};
use ekv::flash::Flash;

pub use theo_storage::card_layout::UNIT_SECTORS; // one definition (Task 1)
pub const UNIT_BYTES: u64 = UNIT_SECTORS as u64 * 512;
pub const UNITS_PER_PAGE: u32 = 32_768;
pub const PAGE_BYTES: usize = 4096;
pub const RECORD_HEADER: usize = 9;
pub const MAX_RECORD_UNITS: usize = (4096 - RECORD_HEADER) / 4;
pub(crate) const UNITS_KEY: &[u8] = b"m/units";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordState { Temp = 0, Published = 1 }

pub struct UnitTable<A: Allocator> { bitmap: Vec<u8, A>, units: u32, used: u32, cursor: u32 }

impl<A: Allocator> UnitTable<A> {
    pub fn new_in(units: u32, alloc: A) -> Result<Self, AllocError> {
        let pages = units.div_ceil(UNITS_PER_PAGE).max(1) as usize;
        let mut bitmap = Vec::new_in(alloc);
        bitmap.try_reserve_exact(pages * PAGE_BYTES).map_err(|_| AllocError)?;
        bitmap.resize(pages * PAGE_BYTES, 0);
        Ok(Self { bitmap, units, used: 0, cursor: 0 })
    }
    pub fn units(&self) -> u32 { self.units }
    pub fn used(&self) -> u32 { self.used }
    pub fn free(&self) -> u32 { self.units - self.used }
    pub fn pages(&self) -> u16 { (self.bitmap.len() / PAGE_BYTES) as u16 }
    pub fn page_of(&self, unit: u32) -> u16 { (unit / UNITS_PER_PAGE) as u16 }
    pub fn page(&self, p: u16) -> &[u8] { &self.bitmap[p as usize * PAGE_BYTES..][..PAGE_BYTES] }
    pub(crate) fn page_mut(&mut self, p: u16) -> &mut [u8] { &mut self.bitmap[p as usize * PAGE_BYTES..][..PAGE_BYTES] }
    fn bit(&self, unit: u32) -> bool { self.bitmap[(unit / 8) as usize] & (1 << (unit % 8)) != 0 }
    fn set(&mut self, unit: u32, on: bool) {
        let byte = &mut self.bitmap[(unit / 8) as usize];
        if on { *byte |= 1 << (unit % 8) } else { *byte &= !(1 << (unit % 8)) }
    }
    /// Next-fit: spreads writes and keeps a sequential download contiguous.
    pub fn allocate(&mut self) -> Option<u32> {
        for step in 0..self.units {
            let unit = (self.cursor + step) % self.units;
            if !self.bit(unit) {
                self.set(unit, true);
                self.used += 1;
                self.cursor = (unit + 1) % self.units;
                return Some(unit);
            }
        }
        None
    }
    pub fn release(&mut self, unit: u32) -> bool {
        if unit >= self.units || !self.bit(unit) { return false; }
        self.set(unit, false);
        self.used -= 1;
        true
    }
    /// Recount after loading; bits past `units` must be clear.
    fn recount(&mut self) -> Option<u32> {
        let mut count = 0u32;
        for (i, byte) in self.bitmap.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    if (i * 8 + bit) as u32 >= self.units { return None; }
                    count += 1;
                }
            }
        }
        Some(count)
    }
}

pub fn encode_record(state: RecordState, len: u64, units: &[u32], out: &mut [u8]) -> usize {
    assert!(units.len() <= MAX_RECORD_UNITS && out.len() >= RECORD_HEADER + units.len() * 4);
    out[0] = state as u8;
    out[1..9].copy_from_slice(&len.to_le_bytes());
    for (i, unit) in units.iter().enumerate() {
        out[RECORD_HEADER + i * 4..][..4].copy_from_slice(&unit.to_le_bytes());
    }
    RECORD_HEADER + units.len() * 4
}

pub struct RecordView<'a> { pub state: RecordState, pub len: u64, units: &'a [u8] }
impl<'a> RecordView<'a> {
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let units = bytes.get(RECORD_HEADER..)?;
        if units.len() % 4 != 0 || units.len() / 4 > MAX_RECORD_UNITS { return None; }
        let state = match bytes[0] { 0 => RecordState::Temp, 1 => RecordState::Published, _ => return None };
        let len = u64::from_le_bytes(bytes[1..9].try_into().ok()?);
        if len > (units.len() / 4) as u64 * UNIT_BYTES { return None; }
        Some(Self { state, len, units })
    }
    pub fn count(&self) -> usize { self.units.len() / 4 }
    pub fn unit(&self, i: usize) -> u32 { u32::from_le_bytes(self.units[i * 4..][..4].try_into().unwrap_or([0; 4])) }
}

fn page_key(p: u16) -> [u8; 3] { let b = p.to_be_bytes(); [b'b', b[0], b[1]] }
fn record_key(id: EpisodeId) -> [u8; 9] {
    let mut k = [0; 9]; k[0] = b'u'; k[1..].copy_from_slice(&id.0.to_be_bytes()); k
}
```

Catalogue methods (same `check`/`begin`/`finish` discipline as `finish_download`; `scratch` checks don't apply since buffers are explicit):

- `load_units`: `begin()`; read `m/units` (exactly 4 bytes else `Corrupt`; `KeyNotFound` → `Corrupt`); for each page `p < table.pages()` read `page_key(p)` into `table.page_mut(p)` (`KeyNotFound` → zero-fill; a length other than 4096 → `Corrupt`); `table.recount()` must equal the counter (else `Corrupt`); set `table.used`, `cursor = 0`; `finish`.
- `commit_units`: `begin()`; `dirty` must be strictly increasing (caller guarantees; `debug_assert!`); one `write_transaction`: for each dirty page `tx.write(&page_key(p), table.page(p))`, then `tx.write(UNITS_KEY, &table.used().to_le_bytes())`, then `record.map_or(tx.delete(&record_key(id)), |r| tx.write(&record_key(id), r))`; `tx.commit()`; `finish`. (`b` < `m/units` < `u`: lexical order holds.)
- `unit_record`: `begin()`; `tx.read(&record_key(id), buf)`; `KeyNotFound` → `Ok(None)`; decode or `Corrupt`; `finish`.
- `unit_ids`: `begin()`; `read_range` over `(Excluded(record_key(after)) | Included(b"u"), Excluded(b"v"))` like `scan_inner`, reading values into `buf` and keys into a `[u8; 9]`; stop after `FILE_PAGE`; id 0 or key length ≠ 9 → `Corrupt`; `finish`.

In `catalogue.rs`: `const SCHEMA: u32 = 2;` and `create` writes `m/next`, `m/schema`, then `UNITS_KEY` = `0u32` (lexical order). Note the lexical-order comment.

- [ ] **Step 4: Run** `just test` — PASS (existing catalogue tests still pass with schema 2).

- [ ] **Step 5: Commit** `feat(podcasts): persist media allocation units in the catalogue (t-121)`.

---

### Task 3: Catalogue-aware `MediaStore<F>`

Mechanical interface change, behaviour unchanged; existing tests are the safety net.

**Files:**
- Modify: `crates/theo-podcasts/src/media.rs`, `download.rs`, `reclaim.rs`, `recovery.rs`, `runtime.rs`, `worker.rs`, `littlefs_media.rs`, `media_test_support.rs`, and every `*_tests.rs` calling these.

**Interfaces:**
- Produces:

```rust
#[allow(async_fn_in_trait)]
pub trait MediaStore<F: Flash> {
    type Error;
    fn active(&self, id: EpisodeId) -> bool;
    async fn space(&mut self, cat: &mut Catalogue<F>) -> Result<Space, Self::Error>;
    async fn reset_temp(&mut self, cat: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Self::Error>;
    async fn append_temp(&mut self, cat: &mut Catalogue<F>, id: EpisodeId, bytes: &[u8]) -> Result<(), Self::Error>;
    async fn sync_temp(&mut self, cat: &mut Catalogue<F>, id: EpisodeId) -> Result<u64, Self::Error>;
    async fn publish_temp(&mut self, cat: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Self::Error>;
    async fn unlink_temp(&mut self, cat: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Self::Error>;
    async fn unlink_audio(&mut self, cat: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Self::Error>;
    async fn audio_len(&mut self, cat: &mut Catalogue<F>, id: EpisodeId) -> Result<Option<u64>, Self::Error>;
    async fn list_ids(&mut self, cat: &mut Catalogue<F>, after: Option<EpisodeId>, out: &mut [EpisodeId; FILE_PAGE]) -> Result<usize, Self::Error>;
}
```

- Generic bounds change from `M: MediaStore` to `M: MediaStore<F>` in `download`, `reclaim::make_space`, `recovery::{recover_one, recover_all, Recovery::step}`, `Runtime<F, M>`, `worker`. Each call site passes the `cat` it already holds, e.g. `media.unlink_temp(cat, job.id)`.
- `HostMedia` and `LittlefsMedia` implement `MediaStore<F>` for all `F: Flash` and ignore `cat`.
- Update the trait doc: "Methods receive the unique catalogue so a store may persist its own accounting in the same database; they never begin a competing writer." Keep the existing poison sentence but change "entire shared mount" to "the podcast scope (ADR-0020)".

- [ ] **Step 1:** Change the trait and `HostMedia`; run `just test` to get the compile-error list (RED is a compile failure here).
- [ ] **Step 2:** Update each caller and test until `just test` and `just check` (prefixed) PASS with the same test count as before.
- [ ] **Step 3: Commit** `refactor(podcasts): give MediaStore the catalogue (t-121)`.

---

### Task 4: `UnitMedia` store

**Files:**
- Create: `crates/theo-podcasts/src/unit_media.rs`, `crates/theo-podcasts/src/unit_media_tests.rs`
- Modify: `crates/theo-podcasts/src/lib.rs` (`pub mod unit_media;`)

**Interfaces:**
- Consumes: Task 2 `UnitTable`, `encode_record`, `RecordView`, `Catalogue::{load_units, commit_units, unit_record, unit_ids}`; Task 3 `MediaStore<F>`; `theo_storage::littlefs::SectorIo`.
- Produces:

```rust
pub enum UnitError<E> { Catalogue(crate::catalogue::Error<E>), Device(littlefs_rust::Error), Full, Invalid, Exists, Disabled, Alloc }
pub struct UnitMedia<D: SectorIo, A: Allocator + Clone> { /* device, table, buf, temp, tail, enabled */ }
impl<D: SectorIo, A: Allocator + Clone> UnitMedia<D, A> {
    pub async fn open_in<F: Flash>(cat: &mut Catalogue<F>, device: D, units: u32, alloc: A) -> Result<Self, UnitError<F::Error>>;
}
impl<F: Flash, D: SectorIo, A: Allocator + Clone> MediaStore<F> for UnitMedia<D, A> { type Error = UnitError<F::Error>; ... }
```

Internal state: `table: UnitTable<A>`, `buf: Vec<u8, A>` (4096, fallible), `temp: Option<Temp>` with `Temp { id, len: u64, units: heapless::Vec<u32, MAX_RECORD_UNITS>, tail: [u8; 512] }` (boxed in `A` via `allocator_api2::boxed::Box::try_new_in` to keep 4 KiB off the stack), `enabled: bool`. Any error sets `enabled = false` and later calls return `Disabled`.

Behaviour (spec §2 "Operations"):
- `space`: `free = table.free() as u64 * UNIT_BYTES + temp.map_or(0, |t| t.units.len() as u64 * UNIT_BYTES - t.len)`; `capacity = table.units() as u64 * UNIT_BYTES`. No I/O.
- `reset_temp(id)`: if `temp` is another id → `unlink_temp(that)`. If a persisted record for `id` is `Published` → `Exists`; if `Temp` → free it (as `unlink_temp`). Then `temp = Some(empty)`.
- `append_temp(id, bytes)`: `1..=AUDIO_CHUNK` bytes else `Invalid`; temp must be `id`. Loop byte-by-sector: when `len == units.len() * UNIT_BYTES`, allocate: `units.len() == MAX_RECORD_UNITS` → `Invalid`; `table.allocate()` or `Full`; push; encode `Temp` record into `buf`; `cat.commit_units(&table, &[page_of(unit)], id, Some(record))`. Copy into `tail` at `len % 512`; when `tail` fills, `device.write_sector(unit * UNIT_SECTORS + (len % UNIT_BYTES) / 512, &tail)`. Advance `len`.
- `sync_temp(id)`: write the partial `tail` (zero-padded beyond `len % 512`) if `len % 512 != 0`, then `device.sync()`; return `len`.
- `publish_temp(id)`: `len == 0` → `Invalid`; `sync_temp`; encode `Published` record with `len`; `commit_units(&table, &[], id, Some(record))`; `temp = None`.
- `unlink_temp(id)` / `unlink_audio(id)`: drop RAM temp if it is `id` (temp case); read record into `buf`; absent or other state → `Ok(())`; release every unit (`release` false → `Disabled` + `Catalogue(Corrupt)`); collect dirty pages into a sorted `heapless::Vec<u16, 64>` (dedup); `commit_units(&table, &dirty, id, None)`.
- `audio_len(id)`: `Published` record → `Some(len)`; otherwise `None`.
- `list_ids`: `cat.unit_ids(after, out, &mut buf)`.

- [ ] **Step 1: Write the failing tests** in `unit_media_tests.rs`. Fixture: `Image` catalogue (`test_support`) plus

```rust
struct Disk { sectors: std::collections::BTreeMap<u32, [u8; 512]>, keep: bool, reads: usize, writes: usize, limit: u32 }
impl SectorIo for Disk { /* bounds-check against limit, count, store only when keep */ fn check(&mut self) -> Result<(), Error> { Ok(()) } }
```

Tests (each tagged `// Regression (t-121): ...` where it guards the bound):

```rust
// Regression (t-121): space and append cost must not grow with stored media.
#[test]
fn space_and_append_cost_is_independent_of_stored_media() {
    let mut costs = Vec::new();
    for stored_mib in [1u64, 4, 64] {
        block_on(async {
            let image = Image::with_pages(256);
            let mut cat = image.create().await;
            let mut media = UnitMedia::open_in(&mut cat, Disk::counting(200 * 2048), 200, Global).await.unwrap();
            fill(&mut cat, &mut media, EpisodeId(1), stored_mib << 20).await; // reset/append/publish
            let (db_writes, reads, writes) = (image.writes.get(), media.device().reads, media.device().writes);
            media.space(&mut cat).await.unwrap();
            media.reset_temp(&mut cat, EpisodeId(2)).await.unwrap();
            media.append_temp(&mut cat, EpisodeId(2), &[7; 4096]).await.unwrap(); // first unit
            media.append_temp(&mut cat, EpisodeId(2), &[7; 4096]).await.unwrap(); // same unit
            costs.push((image.writes.get() - db_writes, media.device().reads - reads, media.device().writes - writes));
        });
    }
    assert!(costs.windows(2).all(|w| w[0] == w[1]), "{costs:?}");
}
```

Plus:
- `one_catalogue_commit_per_unit`: appending 3 MiB in 4096-byte chunks commits exactly 3 unit allocations (count `commit_units` via `Image.writes` deltas measured around single appends: a boundary append writes, a mid-unit append does not).
- `bytes_round_trip_across_unit_and_sector_boundaries`: append chunks of 1000, 4096, 3 bytes etc. crossing 512 and 1 MiB boundaries; publish; read back raw sectors from the `Disk` following the record; equal to the input.
- `invariant_holds_after_mixed_history`: publish 3 episodes, unlink the middle one, reload a fresh `UnitTable` via `open_in` on the reopened catalogue; `used` equals the sum of record counts and `free + used == units`.
- `cut_after_each_step_leaks_no_unit`: for `cut` in every catalogue write index of a reset/append(2 units)/publish/unlink sequence, set `image.cut` (existing `Image` fault hook), run until failure, reopen, `UnitMedia::open_in`, run `recovery::recover_all`, assert `free == units` or (only for cuts after publish of an `Available` episode) the published record's units equal `used`.
- `full_card_reports_full_and_download_waits`: 2-unit store; `DownloadSession` of 3 MiB expected bytes gets `Start::WaitingForSpace` (via `make_space` + `reserve`, use a capacity where `reserve` permits) — if `reserve`'s 16 MiB minimum makes this impossible on a small store, assert `append_temp` returns `UnitError::Full` directly instead.
- `oversize_episode_is_refused`: with `MAX_RECORD_UNITS + 1` units available, appending past `MAX_RECORD_UNITS` MiB returns `Invalid` and leaves `used == MAX_RECORD_UNITS` freed after `unlink_temp`.
- `publish_refuses_existing_audio` and `reset_of_published_id_is_exists`.

- [ ] **Step 2: Run** `just test` — FAIL (module missing).
- [ ] **Step 3: Implement** `unit_media.rs` per the behaviour list above, with a module doc citing ADR-0020 and the crash rule ("a unit is owned by a record before any data sector is written to it").
- [ ] **Step 4: Run** `just test` — PASS. Run the bound test alone with `--nocapture` once and note the three cost tuples in the commit message.
- [ ] **Step 5: Commit** `feat(podcasts): raw allocation-unit media store (t-121)`.

---

### Task 5: Wire v2 into the image tool and the storage owner

**Files:**
- Modify: `crates/theo-podcasts/examples/prepare_image/main.rs`, `prepare_image_tests.rs`
- Modify: `src/device/sd_worker.rs` (`mount_owner`, `PodcastOwner`/`MediaFs` type aliases)
- Delete: `crates/theo-podcasts/src/littlefs_media.rs`, `littlefs_media_tests.rs`; remove from `lib.rs`

**Interfaces:**
- Consumes: Task 1 `split` → `(db, log, media)`, `CardLayout::{log, media_units}`; Task 4 `UnitMedia::open_in`.
- Produces: `type PodcastOwner = Runtime<PageStore<AsyncRegion<CardIo>>, UnitMedia<RegionIo<CardIo>, &'static ExternalMemory>>` (exact generic path follows the existing alias).

- [ ] **Step 1: Write the failing test** in `prepare_image_tests.rs`:

```rust
// Regression (t-121): a fresh image mounts logs on the log region and opens an empty unit store.
#[test]
fn prepared_image_has_v2_log_region_and_empty_units() {
    // create_image(path, sectors) as existing tests do, then:
    // - CardLayout::select on sectors 0/8 → Some(layout) with log().sectors() == LOG_SECTORS
    // - mount_for_writes on the log region succeeds
    // - Catalogue::open on the database region + UnitMedia::open_in(.., layout.media_units(), Global)
    //   → space().free == media_units * UNIT_BYTES
}
```

(Write it fully using the file's existing helpers for the host image device.)

- [ ] **Step 2: Run** `RUSTUP_TOOLCHAIN=stable cargo test -p theo-podcasts --example prepare_image --target $(rustc +stable -vV | sed -n 's/^host: //p')` (prefixed) — FAIL.
- [ ] **Step 3: Implement** `create_image`: format and mount LittleFS on `log` (geometry from `layout.log()`), create the catalogue (schema 2 writes `m/units = 0`), sync, then publish descriptors as today. In `sd_worker::mount_owner`: LittleFS on the log handle with `Geometry::new(layout.log().sectors() as u64)`; after `Catalogue::open`, `UnitMedia::open_in(&mut catalogue, media, layout.media_units(), &esp_alloc::ExternalMemory)`; `Runtime::new(catalogue, unit_media)`. Delete `LittlefsMedia`.
- [ ] **Step 4: Run** the example tests, `just test`, `just check` (all prefixed) — PASS.
- [ ] **Step 5: Commit** `feat(storage): prepare and mount layout v2 with unit media (t-121)`.

---

### Task 6: Per-scope failure isolation

**Files:**
- Create: `crates/theo-storage/src/scope.rs`, `crates/theo-storage/src/scope_tests.rs`
- Modify: `crates/theo-storage/src/littlefs.rs` (`SectorIo::device_failed`), `crates/theo-storage/src/regions.rs`, `crates/theo-storage/src/regions_tests.rs`, `crates/theo-storage/src/lib.rs`
- Modify: `src/device/sd_worker.rs` (`CardIo`, `owner`, `mount_owner`, frontend `request`/`append`)

**Interfaces:**
- Produces:

```rust
// scope.rs
//! Which storage scope a failed request disables (t-121, ADR-0020).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope { Bus, Logs, Podcasts }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind { Mount, Log, Podcast }
/// A poisoned bus wins; otherwise a failure stays in its request's scope.
pub fn failure_scope(bus_poisoned: bool, kind: RequestKind) -> Scope {
    match (bus_poisoned, kind) {
        (true, _) | (false, RequestKind::Mount) => Scope::Bus,
        (false, RequestKind::Log) => Scope::Logs,
        (false, RequestKind::Podcast) => Scope::Podcasts,
    }
}
pub const OWNER_MARGIN_MS: u64 = 20;
```

```rust
// littlefs.rs, in SectorIo
/// True when the device itself is unusable (not merely past a deadline).
/// Wrappers that share a device use it to decide whether to drop it.
fn device_failed(&self) -> bool { true }
```

- `RegionIo::access` takes the shared owner only when `device.device_failed()`; otherwise it returns the error and keeps the device. `RegionIo` forwards `device_failed`.

- [ ] **Step 1: Write the failing tests:**
  - `scope_tests.rs`: the four `failure_scope` cases.
  - `regions_tests.rs`:

```rust
// Regression (t-121): a deadline error in one region must not take the shared device.
#[test]
fn soft_failure_keeps_shared_device_for_other_regions() {
    // device whose check()/read return Err(Error::Io) while `expired` is set,
    // and device_failed() returns `poisoned` (false here)
    // fail a read on the media handle, clear `expired`, read on the log handle → Ok
}
#[test]
fn hard_failure_still_disables_every_region() {
    // same device with device_failed() == true → subsequent log read is Err(Io)
}
```

- [ ] **Step 2: Run** `just test` — FAIL.
- [ ] **Step 3: Implement** the crate changes. Then in `sd_worker.rs`:
  - `CardIo` gets `soft: Rc<Cell<Instant>>` (request deadline − `OWNER_MARGIN_MS`, saturating at now) and keeps `deadline` as the hard one. `ensure_live`: `if SECTORS.is_disabled() { Err(Io) } else if now >= soft { Err(Io) /* no SECTORS.disable() */ } else { Ok(()) }`. The `exchange_blocking` abort predicate and `deadline_ticks` keep the hard deadline; an aborted in-flight exchange still disables `SECTORS` (unchanged). `device_failed()` returns `SECTORS.is_disabled()`.
  - `owner` loop: compute `kind` per command (`Mount`, `Append`/`Tail`/rotation → `Log`, `Domain` → `Podcast`). Replace the global teardown with `match failure_scope(SECTORS.is_disabled(), kind)`: `Bus` → disable `SECTORS`, drop `volume`, `podcasts`, `card` (today's behaviour); `Logs` → `volume = None`; `Podcasts` → `podcasts = None`. A `runtime::Error::Storage` from a domain request becomes `Reply::Domain(runtime::Reply::Offline)` when the scope is `Podcasts` (the caller does not see a bus error).
  - Log commands with `volume == None` reply a new `Reply::Offline`; map it in the frontend `append`/`report_log_tail` to `Ok(())`/a skipped tail with one `log::warn!` per boot, so the frontend `RequestGuard` completes and does not poison the bus.
  - `mount_owner` returns `(Option<Rc<MediaFs>>, Option<PodcastOwner>)`: descriptor read errors → `Err` (bus scope); log mount failure → `None` volume + `log::warn!`; catalogue/`UnitMedia` open failure → `podcasts = None` + `log::warn!`. Mount is `Err` only if the bus is poisoned.
  - Keep the legacy (no descriptor) path: LittleFS on the whole card, no podcasts.
- [ ] **Step 4: Run** `just test`, `just check`, `just lint` (prefixed) — PASS.
- [ ] **Step 5: Commit** `feat(storage): isolate log and podcast failures (t-121)`.

---

### Task 7: ADR-0020 and documentation

**Files:**
- Create: `docs/adr/0020-store-media-in-raw-allocation-units.md`
- Modify: `docs/adr/README.md` (index), `docs/adr/0019-*.md` (one line under the open limit: "Resolved by ADR-0020 for region-layout cards."), `crates/theo-storage/ARCHITECTURE.md` (v2 layout, `scope.rs`, `device_failed` extension point, invariant "bus faults disable every scope; deadline and logical errors disable one"), `crates/theo-podcasts` module docs in `lib.rs` (list `units`, `unit_media`), `AGENTS.md` only if a gotcha changes (none expected).

- [ ] **Step 1:** Write ADR-0020 (Status: Implemented in software; hardware validation pending. Related: t-121; amends ADR-0015, ADR-0018; resolves ADR-0019's open limit). Sections: Context (measured `fs_size` 137/265/522 reads at 1/2/4 MiB, ~516 prep reads per 4 MiB, ~2 ms/read), Decision (layout v2, unit store, keys and invariant, failure scopes, 20 ms margin), Alternatives (separate log region only; tuned shared LittleFS; `ekv-fs`/media in EKV), Consequences (tail waste, 1 GiB episode cap, cancellation still global, playback reads not implemented), Verification (filled in Task 8).
- [ ] **Step 2: Commit** `docs(adr): record raw media units and per-scope failures (t-121)`.

---

### Task 8: Verification, review and PR

- [ ] **Step 1:** `just fmt`, then (prefixed) `just check`, `just test`, `just lint`, and `RUSTFLAGS="-C link-arg=-Tlinkall.x" just build`. All must pass; record the test count in ADR-0020 "Verification".
- [ ] **Step 2:** Run `/review` on the branch diff against `103-fix/littlefs-first-append`; fix critical/major findings with tests; add `Reviewed-by:` to the final commit.
- [ ] **Step 3:** Push `121-fix/bounded-media-storage` (outside the sandbox) and open a PR based on `103-fix/littlefs-first-append` (or `main` if PR #7 has merged, after rebasing). Body: summary, measured costs, no board validation done.
- [ ] **Step 4:** `oku cli -a http://127.0.0.1:50051 task comment --project theo-pod --id t-121` with the PR link; leave t-121 open until merged.
