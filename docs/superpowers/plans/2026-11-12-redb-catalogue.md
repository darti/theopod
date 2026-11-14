# redb Podcast Catalogue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the ekv/PageStore podcast catalogue with redb over a 64 MiB card region (layout v3), so podcasts come online within a few seconds of boot.

**Architecture:** `theo_storage::redb_backend::RegionBackend` implements redb's synchronous `StorageBackend` over a card region (a length sector, then 512-byte sectors). `theo_podcasts::catalogue::Catalogue` becomes a concrete type owning a `redb::Database`, with one typed table per concern; every module loses its `F: ekv::flash::Flash` parameter. Card layout v3 gives the database a fixed 64 MiB region; provisioning writes an empty length record and the first open creates the tables. The firmware registers PSRAM before internal RAM, so redb's 256 KiB cache never lands in the internal heap that Wi-Fi uses.

**Tech Stack:** Rust `no_std` + `alloc`, redb `=4.4.0` (`experimental-api-5`, std on host), esp-alloc 0.11, embassy, `just`.

**Spec:** `docs/superpowers/specs/2026-11-12-redb-catalogue-design.md` (with the amendments in Task 0), ADR-0022.

## Global Constraints

- redb pinned `=4.4.0`, `default-features = false`, `features = ["experimental-api-5"]` for the firmware target; host (non-`target_os = "none"`) builds add `features = ["std"]`.
- Every redb write transaction calls `set_quick_repair(true)` before writing.
- redb cache: `CACHE_BYTES = 256 * 1024`.
- Database region: 64 MiB = `DATABASE_SECTORS = 131_072` sectors, starting at sector 16. Layout magic `THEOSD03`, version 3.
- Schema: `COUNTERS["schema"] == 3`. Any other value, or a missing table, is `Error::Schema`; nothing is rewritten.
- Scale target: 50 subscriptions, 5,000 episodes.
- No migration and no v2 support code: a v2 descriptor is refused like any unknown version.
- Boot never formats (ADR-0015). Only explicit provisioning writes descriptors or the length record.
- `theo-storage` and `theo-podcasts` stay HAL-independent; ESP code stays in `src/device/`.
- Regression tests live in side-by-side `*_tests.rs` files, tagged `// Regression (t-N): ...`. Until the Oku task exists, use `t-133`.
- Commit messages: Conventional Commits, `(t-133)` suffix, a `Review-waived: <reason>` or `Reviewed-by: …` trailer (commit hook), and **no** `Co-Authored-By` line.
- Host commands (`$HOST` below): `RUSTUP_TOOLCHAIN=stable cargo <cmd> --target $(rustc +stable -vV | sed -n 's/^host: //p')`. In the sandbox prefix cargo with `RUSTC_WRAPPER= CARGO_BUILD_BUILD_DIR=$PWD/target/build-dir`.
- Flashing and serial monitoring only on an explicit user request (Task 8).

## File Map

| File | Change |
|---|---|
| `Cargo.toml` | add `redb`; later remove `ekv` and its `[patch]` |
| `crates/theo-storage/src/crc.rs` | new: `crc32` moved out of `page_store.rs` |
| `crates/theo-storage/src/redb_backend.rs` (+`_tests.rs`, `_crash_tests.rs`) | new: `RegionBackend`, `BackendError`, `REGION_FULL` |
| `crates/theo-storage/src/test_disk.rs` | new (feature `test-disk`): in-memory `SectorIo` with fault injection |
| `crates/theo-storage/src/card_layout.rs` (+tests) | layout v3 |
| `crates/theo-podcasts/src/tables.rs` | new: table definitions and counter names |
| `crates/theo-podcasts/src/catalogue.rs`, `subscriptions.rs`, `units.rs` | persistence on redb |
| `crates/theo-podcasts/src/{sync,recovery,reclaim,download,media,unit_media,runtime,worker,service}.rs` | drop `F: Flash` |
| `crates/theo-podcasts/src/provision.rs`, `provision_guard.rs` | `provision_v3`, token `v3:` |
| `crates/theo-podcasts/src/test_support.rs`, `media_test_support.rs`, all `*_tests.rs` | port to `TestDisk` |
| `src/device/sd_worker.rs`, `sd_provision.rs`, `mod.rs` | redb open, provisioning, heap order |
| `justfile` | `sd-provision-v3`; drop `legacy-fat` and page-store recipes |
| deleted | `page_store*.rs`, `fat_page_file*.rs`, `vendor/ekv/` |

---

### Task 0: Set aside the background-open edits and amend the spec

The worktree holds uncommitted t-133 edits (background open, `Reply::Opening`, header-first `PageStore::load`). The design drops them; save them as a patch first.

**Files:**
- Create: `.superpowers/t133-background-open.patch` (git-ignored)
- Modify: `docs/superpowers/specs/2026-11-12-redb-catalogue-design.md`, `docs/adr/0022-store-the-podcast-catalogue-in-redb.md`

- [ ] **Step 1: Save and discard the edits**

```bash
git diff > .superpowers/t133-background-open.patch
test -s .superpowers/t133-background-open.patch
git restore crates/theo-podcasts/src/runtime.rs crates/theo-storage/src/future.rs \
  crates/theo-storage/src/page_store.rs crates/theo-storage/src/page_store_tests.rs \
  src/device/network.rs src/device/sd_provision.rs src/device/sd_worker.rs
git status --short   # expected: no tracked modifications
```

- [ ] **Step 2: Record the amendments decided while planning**

Append to the spec, before `## Risks`:

```markdown
## Amendments (2026-11-12, while planning)

- **Heap order.** esp-radio forces esp-alloc's global allocator, so redb's
  allocations cannot be routed by type. The firmware registers PSRAM before
  internal RAM: global allocations land in PSRAM, while esp-radio and
  esp-rtos keep requesting `Internal` explicitly. A smaller redb cache was
  measured and rejected: 64 KiB costs ~163 writes (~346 ms) per commit,
  32 KiB ~220 writes (~460 ms), against ~106 (~226 ms) at 256 KiB; a PSRAM
  sector cache does not reduce writes.
- **Growth is not zero-filled.** redb grows its file by doubling (1, 2, 4,
  9, 18 MiB at 0.2k, 0.5k, 1k, 2.1k, 4.3k episodes). Zero-filling 9 MiB at
  2 ms per sector would block one commit for ~36 s. The spike ran 5,000
  inserts with growth filled with `0xA5`, reopened cleanly and after a
  crash, and read every record back. `set_len` therefore only rewrites the
  length record; a regression test keeps the fill non-zero.
- **Provisioning creates no tables.** redb's backend must be `'static`, but
  provisioning only borrows the card. `provision_v3` writes the length
  record with length 0; the first `Catalogue::open` of an empty, provisioned
  region creates the tables and counters in one transaction. A sector
  without the record's magic and CRC is `Corrupt`, so a blank or damaged
  region is never formatted at boot.
- **v2 cards.** A v2 descriptor is refused like any unknown version (no
  regions mount; one log line asks to re-provision). Keeping media mounted
  would need v2 geometry code, which the no-migration decision excludes.
- **Sync batching.** `Catalogue` holds an open `WriteTransaction` across one
  feed page: `begin_batch`, `upsert` into it, `commit_batch` at the page end.
  Any other operation commits a pending batch first.
- **Fixed bug.** The ekv units range scan (`unit_ids`, prefix `u`) also
  reached the 37-byte show-index keys (`u` + show id) and returned `Corrupt`
  once fewer than 16 unit records remained after the cursor. Separate tables
  remove it; Task 4 adds a regression test.
```

Add the same five bullets, one line each, under a `## Amendments` heading at the end of ADR-0022.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-11-12-redb-catalogue-design.md docs/adr/0022-store-the-podcast-catalogue-in-redb.md
git commit -m "docs(adr): amend the redb catalogue design after planning (t-133)

Review-waived: design documents only"
```

---

### Task 1: `TestDisk` and `RegionBackend`

**Files:**
- Modify: `Cargo.toml` (workspace deps), `crates/theo-storage/Cargo.toml`, `crates/theo-storage/src/lib.rs`
- Create: `crates/theo-storage/src/crc.rs`, `crates/theo-storage/src/test_disk.rs`, `crates/theo-storage/src/redb_backend.rs`, `crates/theo-storage/src/redb_backend_tests.rs`
- Modify: `crates/theo-storage/src/page_store.rs` (import `crc32` from `crc`)

**Interfaces:**
- Produces: `theo_storage::redb_backend::{RegionBackend, BackendError, REGION_FULL, LENGTH_SECTORS}`; `RegionBackend::<D>::new(device: D, sectors: u32) -> Result<Self, BackendError>` (unsafe); `RegionBackend::format(device: &mut D) -> Result<(), littlefs_rust::Error>`; `theo_storage::test_disk::TestDisk` (feature `test-disk`); `theo_storage::crc::crc32(&[u8]) -> u32`.

- [ ] **Step 1: Dependencies**

Workspace `Cargo.toml`, `[workspace.dependencies]`:

```toml
redb = { version = "=4.4.0", default-features = false, features = ["experimental-api-5"] }
```

`crates/theo-storage/Cargo.toml`:

```toml
[dependencies]
redb.workspace = true

# Host builds (tests, clippy, miri) use redb's std mode: its no_std mode
# requires panic = "abort", which host test harnesses cannot use.
[target.'cfg(not(target_os = "none"))'.dependencies]
redb = { workspace = true, features = ["std"] }

[features]
test-disk = []
```

(keep the existing entries; add `test-disk` next to `legacy-fat`).

- [ ] **Step 2: Move `crc32`**

Create `crates/theo-storage/src/crc.rs` holding the exact `crc32` function now in `page_store.rs` (cut it from there, keep its doc comment), with `//! CRC-32 (IEEE) shared by layout descriptors and the redb length record.` In `lib.rs` add `pub mod crc;`; in `page_store.rs` and `card_layout.rs` replace the definition/import with `use crate::crc::crc32;` (keep `pub use crate::crc::crc32;` in `page_store.rs` only if a test imports `page_store::crc32`; delete it again in Task 6).

- [ ] **Step 3: `TestDisk`**

`crates/theo-storage/src/test_disk.rs`:

```rust
//! In-memory sector device with fault injection, for host tests.
//!
//! Writes since the last `sync` stay pending: [`TestDisk::crash`] either
//! drops them (nothing after the last barrier reached the card) or keeps
//! them (everything up to the failure did, torn sector included).

extern crate std;

use crate::littlefs::{SECTOR_SIZE, SectorIo};
use alloc::rc::Rc;
use core::cell::{RefCell, RefMut};
use littlefs_rust::Error;
use std::collections::BTreeMap;

type Sector = [u8; SECTOR_SIZE];

/// Shared handle: clones see the same sectors and counters.
#[derive(Clone, Debug)]
pub struct TestDisk(Rc<RefCell<State>>);

#[derive(Debug)]
pub struct State {
    /// Sectors never written read as this byte (`0xA5` by default: redb
    /// must not rely on zero-filled growth).
    pub fill: u8,
    pub sectors: u32,
    durable: BTreeMap<u32, Sector>,
    pending: BTreeMap<u32, Sector>,
    pub reads: usize,
    pub writes: usize,
    pub syncs: usize,
    /// Every write fails while set.
    pub fail_writes: bool,
    /// `(n, prefix)`: write number `n` (0-based, counting from creation)
    /// stores only its first `prefix` bytes, then fails.
    pub cut: Option<(usize, usize)>,
}

/// What a crash keeps of the writes after the last sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Crash {
    DropPending,
    KeepPending,
}

impl TestDisk {
    pub fn new(sectors: u32) -> Self {
        Self(Rc::new(RefCell::new(State {
            fill: 0xA5,
            sectors,
            durable: BTreeMap::new(),
            pending: BTreeMap::new(),
            reads: 0,
            writes: 0,
            syncs: 0,
            fail_writes: false,
            cut: None,
        })))
    }
    pub fn state(&self) -> RefMut<'_, State> {
        self.0.borrow_mut()
    }
    /// Power loss: resolve pending writes and clear faults.
    pub fn crash(&self, mode: Crash) {
        let mut s = self.state();
        let pending = core::mem::take(&mut s.pending);
        if mode == Crash::KeepPending {
            s.durable.extend(pending);
        }
        s.fail_writes = false;
        s.cut = None;
    }
    /// An independent copy of the durable and pending sectors, counters reset.
    pub fn snapshot(&self) -> Self {
        let s = self.0.borrow();
        let copy = Self::new(s.sectors);
        {
            let mut c = copy.state();
            c.fill = s.fill;
            c.durable = s.durable.clone();
            c.pending = s.pending.clone();
        }
        copy
    }
    pub fn reset_counters(&self) {
        let mut s = self.state();
        s.reads = 0;
        s.writes = 0;
        s.syncs = 0;
    }
}

impl SectorIo for TestDisk {
    fn read_sector(&mut self, sector: u32, out: &mut Sector) -> Result<(), Error> {
        let mut s = self.state();
        if sector >= s.sectors {
            return Err(Error::Io);
        }
        s.reads += 1;
        *out = match s.pending.get(&sector).or_else(|| s.durable.get(&sector)) {
            Some(data) => *data,
            None => [s.fill; SECTOR_SIZE],
        };
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &Sector) -> Result<(), Error> {
        let mut s = self.state();
        if sector >= s.sectors || s.fail_writes {
            return Err(Error::Io);
        }
        let n = s.writes;
        s.writes += 1;
        if let Some((cut, prefix)) = s.cut
            && cut == n
        {
            let mut torn = match s.pending.get(&sector).or_else(|| s.durable.get(&sector)) {
                Some(old) => *old,
                None => [s.fill; SECTOR_SIZE],
            };
            torn[..prefix].copy_from_slice(&data[..prefix]);
            s.pending.insert(sector, torn);
            s.fail_writes = true;
            return Err(Error::Io);
        }
        s.pending.insert(sector, *data);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error> {
        let mut s = self.state();
        if s.fail_writes {
            return Err(Error::Io);
        }
        s.syncs += 1;
        let pending = core::mem::take(&mut s.pending);
        s.durable.extend(pending);
        Ok(())
    }
    fn check(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
```

In `lib.rs`: `#[cfg(any(test, feature = "test-disk"))] pub mod test_disk;` (the crate's own tests get it through `test`; `theo-podcasts` enables the feature as a dev-dependency).

- [ ] **Step 4: Write the failing backend tests**

`crates/theo-storage/src/redb_backend_tests.rs`:

```rust
use super::*;
use crate::test_disk::TestDisk;
use redb::StorageBackend;

const SECTORS: u32 = 1 + 64; // length sector + 32 KiB

fn formatted() -> TestDisk {
    let mut disk = TestDisk::new(SECTORS);
    RegionBackend::format(&mut disk).unwrap();
    disk
}
fn backend(disk: &TestDisk) -> RegionBackend<TestDisk> {
    // SAFETY: single-threaded test; the backend never leaves this thread.
    unsafe { RegionBackend::new(disk.clone(), SECTORS) }.unwrap()
}

#[test]
fn new_on_unformatted_region_is_corrupt() {
    let disk = TestDisk::new(SECTORS);
    // SAFETY: single-threaded test.
    let error = unsafe { RegionBackend::new(disk, SECTORS) }.unwrap_err();
    assert_eq!(error, BackendError::Corrupt);
}

#[test]
fn formatted_region_is_empty() {
    assert_eq!(backend(&formatted()).len().unwrap(), 0);
}

#[test]
fn set_len_persists_across_reopen() {
    let disk = formatted();
    backend(&disk).set_len(5000).unwrap();
    assert_eq!(backend(&disk).len().unwrap(), 5000);
}

#[test]
fn set_len_beyond_region_reports_region_full() {
    let disk = formatted();
    let error = backend(&disk).set_len(64 * 512 + 1).unwrap_err();
    assert!(error.to_string().contains(REGION_FULL));
    assert_eq!(backend(&disk).len().unwrap(), 0);
}

#[test]
fn unaligned_write_reads_back_and_keeps_neighbours() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(4096).unwrap();
    b.write(0, &[1; 4096]).unwrap();
    b.write(700, &[2; 1000]).unwrap();
    let mut out = [0; 4096];
    b.read(0, &mut out).unwrap();
    assert!(out[..700].iter().all(|&x| x == 1));
    assert!(out[700..1700].iter().all(|&x| x == 2));
    assert!(out[1700..].iter().all(|&x| x == 1));
}

#[test]
fn aligned_page_write_needs_no_read() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(8192).unwrap();
    disk.reset_counters();
    b.write(4096, &[3; 4096]).unwrap();
    assert_eq!(disk.state().reads, 0);
    assert_eq!(disk.state().writes, 8);
}

#[test]
fn read_or_write_past_len_fails_without_io() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(1024).unwrap();
    disk.reset_counters();
    assert!(b.read(1000, &mut [0; 100]).is_err());
    assert!(b.write(1000, &[0; 100]).is_err());
    assert_eq!(disk.state().reads + disk.state().writes, 0);
}

#[test]
fn sync_data_syncs_the_device() {
    let disk = formatted();
    let b = backend(&disk);
    disk.reset_counters();
    b.sync_data().unwrap();
    assert_eq!(disk.state().syncs, 1);
}

#[test]
fn device_failure_surfaces_as_io_error() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(512).unwrap();
    disk.state().fail_writes = true;
    assert!(b.write(0, &[0; 512]).is_err());
}

#[test]
fn redb_database_roundtrips_on_the_backend() {
    let disk = TestDisk::new(1 + 16_384); // 8 MiB
    let mut d = disk.clone();
    RegionBackend::format(&mut d).unwrap();
    const T: redb::TableDefinition<u64, u64> = redb::TableDefinition::new("t");
    {
        // SAFETY: single-threaded test.
        let b = unsafe { RegionBackend::new(disk.clone(), 1 + 16_384) }.unwrap();
        let db = redb::Database::builder().create_with_backend(b).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.set_quick_repair(true);
        tx.open_table(T).unwrap().insert(7, 42).unwrap();
        tx.commit().unwrap();
    }
    // SAFETY: single-threaded test.
    let b = unsafe { RegionBackend::new(disk, 1 + 16_384) }.unwrap();
    let db = redb::Database::builder().create_with_backend(b).unwrap();
    let tx = db.begin_read().unwrap();
    assert_eq!(tx.open_table(T).unwrap().get(7).unwrap().unwrap().value(), 42);
}
```

- [ ] **Step 5: Run to verify failure**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-storage --target $HOST redb_backend`
Expected: compile error, `redb_backend` not found.

- [ ] **Step 6: Implement**

`crates/theo-storage/src/redb_backend.rs`:

```rust
//! redb storage over one card region (ADR-0022).
//!
//! Sector 0 of the region holds the length record (magic, logical length,
//! CRC); the redb file is the rest, byte `n` in region sector `1 + n / 512`.
//! Growth is not zero-filled: redb tolerates arbitrary bytes past its last
//! write (measured; the tests fill fresh sectors with `0xA5`).

use crate::{
    crc::crc32,
    littlefs::{SECTOR_SIZE, SectorIo},
};
use core::{cell::RefCell, fmt};
use littlefs_rust::Error as DeviceError;
use redb::{StorageBackend, io};

const MAGIC: &[u8; 8] = b"THEORDB1";
/// Sectors before the redb file: the length record.
pub const LENGTH_SECTORS: u32 = 1;
/// Message of the error `set_len` returns past the region; the catalogue
/// maps it to `Full`.
pub const REGION_FULL: &str = "theo: database region full";
const SECTOR: u64 = SECTOR_SIZE as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendError {
    /// The region is smaller than the length record.
    Geometry,
    /// No valid length record: not provisioned, or damaged.
    Corrupt,
    Device(DeviceError),
}

/// redb's [`StorageBackend`] over a region device.
pub struct RegionBackend<D> {
    inner: RefCell<Inner<D>>,
}
struct Inner<D> {
    device: D,
    capacity: u64,
    len: u64,
}

// SAFETY: `RegionBackend::new` is unsafe and requires every use and the
// drop of the backend, and of any `redb::Database` built on it, to happen
// on the thread (core) that created it. Under that contract no `&self`
// reaches another thread, so the `RefCell` is never borrowed concurrently
// and `D` never moves between threads.
unsafe impl<D> Send for RegionBackend<D> {}
// SAFETY: see `Send` above.
unsafe impl<D> Sync for RegionBackend<D> {}

impl<D> fmt::Debug for RegionBackend<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegionBackend")
    }
}

impl<D: SectorIo> RegionBackend<D> {
    /// Write an empty length record: the region holds an empty database.
    ///
    /// Destructive; explicit provisioning only (ADR-0021).
    pub fn format(device: &mut D) -> Result<(), DeviceError> {
        device.write_sector(0, &record(0))?;
        device.sync()
    }

    /// Open a provisioned region of `sectors` sectors.
    ///
    /// # Safety
    ///
    /// The backend and any `redb::Database` built on it must be used and
    /// dropped only on the thread (core) that calls this: the backend claims
    /// `Send + Sync` for redb without being thread-safe.
    pub unsafe fn new(mut device: D, sectors: u32) -> Result<Self, BackendError> {
        if sectors <= LENGTH_SECTORS {
            return Err(BackendError::Geometry);
        }
        let capacity = u64::from(sectors - LENGTH_SECTORS) * SECTOR;
        let mut sector = [0; SECTOR_SIZE];
        device.read_sector(0, &mut sector).map_err(BackendError::Device)?;
        let len = decode(&sector)
            .filter(|&len| len <= capacity)
            .ok_or(BackendError::Corrupt)?;
        Ok(Self {
            inner: RefCell::new(Inner { device, capacity, len }),
        })
    }
}

fn record(len: u64) -> [u8; SECTOR_SIZE] {
    let mut sector = [0; SECTOR_SIZE];
    sector[..8].copy_from_slice(MAGIC);
    sector[8..16].copy_from_slice(&len.to_le_bytes());
    let crc = crc32(&sector[..16]);
    sector[16..20].copy_from_slice(&crc.to_le_bytes());
    sector
}
fn decode(sector: &[u8; SECTOR_SIZE]) -> Option<u64> {
    let crc = u32::from_le_bytes(sector[16..20].try_into().ok()?);
    (sector[..8] == *MAGIC && crc == crc32(&sector[..16]))
        .then(|| u64::from_le_bytes(sector[8..16].try_into().ok()?))
        .flatten()
}
fn device(error: DeviceError) -> io::Error {
    let _ = error;
    io::Error::other("theo: sector device failed")
}
fn bounds() -> io::Error {
    io::Error::other("theo: access past the database length")
}

impl<D: SectorIo> Inner<D> {
    fn check(&self, offset: u64, len: usize) -> Result<(), io::Error> {
        match offset.checked_add(len as u64) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(bounds()),
        }
    }
    fn sector(offset: u64) -> Result<u32, io::Error> {
        u32::try_from(offset / SECTOR + u64::from(LENGTH_SECTORS)).map_err(|_| bounds())
    }
}

impl<D: SectorIo + 'static> StorageBackend for RegionBackend<D> {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.inner.borrow().len)
    }

    fn read(&self, mut offset: u64, mut out: &mut [u8]) -> Result<(), io::Error> {
        let mut inner = self.inner.borrow_mut();
        inner.check(offset, out.len())?;
        let mut sector = [0; SECTOR_SIZE];
        while !out.is_empty() {
            let at = (offset % SECTOR) as usize;
            let n = out.len().min(SECTOR_SIZE - at);
            inner
                .device
                .read_sector(Inner::<D>::sector(offset)?, &mut sector)
                .map_err(device)?;
            out[..n].copy_from_slice(&sector[at..at + n]);
            out = &mut out[n..];
            offset += n as u64;
        }
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        let mut inner = self.inner.borrow_mut();
        if len > inner.capacity {
            return Err(io::Error::other(REGION_FULL));
        }
        // Durable before redb writes into the new space.
        inner.device.write_sector(0, &record(len)).map_err(device)?;
        inner.device.sync().map_err(device)?;
        inner.len = len;
        Ok(())
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        self.inner.borrow_mut().device.sync().map_err(device)
    }

    fn write(&self, mut offset: u64, mut data: &[u8]) -> Result<(), io::Error> {
        let mut inner = self.inner.borrow_mut();
        inner.check(offset, data.len())?;
        let mut sector = [0; SECTOR_SIZE];
        while !data.is_empty() {
            let at = (offset % SECTOR) as usize;
            let n = data.len().min(SECTOR_SIZE - at);
            let index = Inner::<D>::sector(offset)?;
            if n == SECTOR_SIZE {
                sector.copy_from_slice(&data[..SECTOR_SIZE]);
            } else {
                // Edge sector: keep the bytes redb does not overwrite.
                inner.device.read_sector(index, &mut sector).map_err(device)?;
                sector[at..at + n].copy_from_slice(&data[..n]);
            }
            inner.device.write_sector(index, &sector).map_err(device)?;
            data = &data[n..];
            offset += n as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "redb_backend_tests.rs"]
mod tests;
```

`lib.rs`: `pub mod redb_backend;`. Drop the unused `let _ = error;` if clippy prefers `|_| io::Error::other(..)`; keep one helper.

- [ ] **Step 7: Run tests**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-storage --target $HOST redb_backend`
Expected: 11 passed.

- [ ] **Step 8: Firmware target builds redb**

Run: `just check`
Expected: success. If redb raises its `panic = "abort"` `compile_error!`, add `panic = "abort"` to `[profile.release]` and `[profile.dev]` in the workspace `Cargo.toml` and rerun.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock crates/theo-storage
git commit -m "feat(storage): add the redb region backend and test disk (t-133)

Review-waived: per-task review runs in the plan executor"
```

---

### Task 2: Crash and torn-sector tests for redb on the backend

**Files:**
- Create: `crates/theo-storage/src/redb_backend_crash_tests.rs`
- Modify: `crates/theo-storage/src/redb_backend.rs` (add the `mod` line)

**Interfaces:**
- Consumes: `TestDisk`, `Crash`, `RegionBackend` from Task 1.

- [ ] **Step 1: Write the tests**

```rust
//! redb's two-phase commit over `RegionBackend`, cut at every sector write.
use super::*;
use crate::test_disk::{Crash, TestDisk};
use redb::{Database, ReadableDatabase, TableDefinition};

const SECTORS: u32 = 1 + 16_384;
const T: TableDefinition<u64, &[u8]> = TableDefinition::new("t");

fn open(disk: &TestDisk) -> Database {
    // SAFETY: single-threaded test.
    let backend = unsafe { RegionBackend::new(disk.clone(), SECTORS) }.unwrap();
    Database::builder()
        .set_cache_size(256 * 1024)
        .create_with_backend(backend)
        .unwrap()
}
fn put(db: &Database, from: u64, to: u64) -> Result<(), redb::Error> {
    let mut tx = db.begin_write()?;
    tx.set_quick_repair(true);
    {
        let mut t = tx.open_table(T)?;
        for i in from..to {
            t.insert(i, [i as u8; 1500].as_slice())?;
        }
    }
    tx.commit()?;
    Ok(())
}
fn count(db: &Database) -> u64 {
    let tx = db.begin_read().unwrap();
    let t = tx.open_table(T).unwrap();
    let mut n = 0;
    for entry in t.iter().unwrap() {
        let (k, v) = entry.unwrap();
        assert!(v.value().iter().all(|&b| b == k.value() as u8));
        n += 1;
    }
    n
}
fn base() -> TestDisk {
    let mut disk = TestDisk::new(SECTORS);
    RegionBackend::format(&mut disk).unwrap();
    put(&open(&disk), 0, 50).unwrap();
    disk
}

#[test]
fn crash_at_every_write_of_a_commit_opens_old_or_new() {
    let disk = base();
    let probe = disk.snapshot();
    let first = probe.state().writes;
    put(&open(&probe), 50, 60).unwrap();
    let total = probe.state().writes - first;
    for cut in 0..total {
        for prefix in [0, 1, 511] {
            for mode in [Crash::DropPending, Crash::KeepPending] {
                let disk = base();
                let start = disk.state().writes;
                disk.state().cut = Some((start + cut, prefix));
                {
                    let db = open(&disk);
                    let _ = put(&db, 50, 60);
                }
                disk.crash(mode);
                let n = count(&open(&disk));
                assert!(n == 50 || n == 60, "cut {cut} prefix {prefix} {mode:?}: {n} records");
            }
        }
    }
}

// Regression (t-133): opening after a crash stays O(1) in sector reads.
#[test]
fn open_after_crash_reads_at_most_64_sectors() {
    let disk = base();
    put(&open(&disk), 50, 2000).unwrap();
    let crashed = disk.snapshot();
    crashed.crash(Crash::DropPending);
    crashed.reset_counters();
    drop(open(&crashed));
    assert!(crashed.state().reads <= 64, "{} reads", crashed.state().reads);
}
```

Note: `open(&disk)` returns the `Database` inside the inner block; the database drops before `crash`, and its `close` may write. That is acceptable: `fail_writes` stays set after a cut, so nothing more reaches the disk.

- [ ] **Step 2: Wire and run**

Add to `redb_backend.rs`: `#[cfg(test)] #[path = "redb_backend_crash_tests.rs"] mod crash_tests;`

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-storage --release --target $HOST redb_backend`
Expected: all pass. If `crash_at_every_write…` fails, **stop**: redb is not crash-safe over this backend; report the cut, prefix and mode to the user. Do not weaken the assertion.

- [ ] **Step 3: Commit**

```bash
git add crates/theo-storage/src
git commit -m "test(storage): cut redb commits at every sector write (t-133)

Review-waived: per-task review runs in the plan executor"
```

---

### Task 3: Card layout v3

**Files:**
- Modify: `crates/theo-storage/src/card_layout.rs`, `crates/theo-storage/src/card_layout_tests.rs`, `crates/theo-storage/src/regions_tests.rs` (callers of `CardLayout::new`)

**Interfaces:**
- Produces: `CardLayout::new(card_sectors: u64) -> Result<CardLayout, LayoutError>`; `card_layout::DATABASE_SECTORS: u32 = 131_072`. `CardLayout::pages()` is removed.

- [ ] **Step 1: Failing tests**

In `card_layout_tests.rs`, replace every `CardLayout::new(x, pages)` with `CardLayout::new(x)` and add:

```rust
#[test]
fn v3_database_region_is_64_mib_from_sector_16() {
    let layout = CardLayout::new(122_265_600).unwrap();
    assert_eq!(layout.database().start(), 16);
    assert_eq!(layout.database().sectors(), DATABASE_SECTORS);
    assert_eq!(layout.database().byte_len(), 64 * 1024 * 1024);
    assert_eq!(layout.log().start() % 8, 0);
    assert_eq!(layout.media().start() % UNIT_SECTORS, 0);
}

#[test]
fn v2_descriptor_is_refused_as_unknown_version() {
    let layout = CardLayout::new(122_265_600).unwrap();
    let mut v2 = layout.encode();
    v2[..8].copy_from_slice(b"THEOSD02");
    v2[8..12].copy_from_slice(&2u32.to_le_bytes());
    let crc = crate::crc::crc32(&v2[..508]);
    v2[508..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(CardLayout::select(&v2, &v2, 122_265_600), Err(LayoutError::Version));
}
```

Delete tests that only vary `pages` (geometry rejection of `pages` outside `32..=4096`).

- [ ] **Step 2: Run to verify failure**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-storage --target $HOST card_layout`
Expected: compile errors on `CardLayout::new`.

- [ ] **Step 3: Implement**

In `card_layout.rs`:
- Module doc: "Layout v3 places … the database (redb, fixed 64 MiB, ADR-0022) …".
- Constants: `const MAGIC: &[u8; 8] = b"THEOSD03"; const VERSION: u32 = 3;` add `pub const DATABASE_SECTORS: u32 = 131_072;` and `const OLD_MAGICS: [&[u8; 8]; 2] = [b"THEOSD01", b"THEOSD02"];` replacing `V1_MAGIC`.
- Remove the `pages` field, the `pages()` getter and the `file_len` import; `use crate::crc::crc32;`.
- `new(card_sectors: u64)`: reject `card_sectors > u32::MAX`; `database_sectors = DATABASE_SECTORS as u64`; rest unchanged.
- `encode`: bytes 72..76 stay zero (reserved).
- `decode`: `let old = OLD_MAGICS.iter().any(|m| &bytes[..8] == *m);` then `if &bytes[..8] != MAGIC && !old { return Ok(None) }`; after the CRC check `if old || bytes[8..12] != VERSION.to_le_bytes() { return Err(LayoutError::Version) }`; build with `Self::new(card_sectors)?`; keep the canonical `encode() != *bytes` check.

Update `regions_tests.rs` and any other `CardLayout::new(_, _)` caller in `theo-storage` the same way.

- [ ] **Step 4: Run tests**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-storage --target $HOST --features test-disk`
Expected: all pass (theo-podcasts does not compile yet; Task 4 ports it).

- [ ] **Step 5: Commit**

```bash
git add crates/theo-storage
git commit -m "feat(storage)!: card layout v3 with a fixed 64 MiB database region (t-133)

Review-waived: per-task review runs in the plan executor"
```

---

### Task 4: Catalogue, subscriptions and units on redb; drop `F: Flash` everywhere in theo-podcasts

The crate compiles only once every module is ported, so this task covers all of `theo-podcasts`. Commit at the end; the firmware build stays broken until Task 5.

**Files:**
- Create: `crates/theo-podcasts/src/tables.rs`
- Modify: `crates/theo-podcasts/Cargo.toml`, `lib.rs`, `catalogue.rs`, `subscriptions.rs`, `units.rs`, `sync.rs`, `recovery.rs`, `reclaim.rs`, `download.rs`, `media.rs`, `unit_media.rs`, `runtime.rs`, `worker.rs`, `service.rs`, `provision.rs`, `provision_guard.rs`, `test_support.rs`, `media_test_support.rs`, every `*_tests.rs`, `examples/prepare_image/*`

**Interfaces:**
- Consumes: `RegionBackend`, `REGION_FULL`, `TestDisk`, `Crash`, `CardLayout::new(card_sectors)`.
- Produces:
  - `catalogue::Error` (no type parameter): `Storage, Corrupt, Schema, FieldTooLong, InvalidMetadata, BufferTooSmall, NotFound, IdExhausted, IdentityCollision, InvalidState, Full, Disabled`.
  - `Catalogue::open(backend: impl redb::StorageBackend) -> Result<Catalogue, Error>` (creates the tables when the backend's length is 0).
  - Sync methods with unchanged names and arguments: `get, upsert, scan, mark_read, transition_audio, completed_audio_len, sync_checkpoint, follow, next_subscription`; crate-private `finish_download, save_sync_checkpoint, load_units, commit_units, unit_record, unit_ids, begin_batch, commit_batch`.
  - `MediaStore` (no parameter), `Runtime<M>`, `UnitMedia<D, A>::open_in(cat: &mut Catalogue, device: D, units: u32, alloc: A)`, `UnitError` without parameter.
  - `provision::provision_v3<D: SectorIo, A: Allocator + Clone>(device: &mut D, card_sectors: u64, allocator: A) -> Result<CardLayout, ProvisionError>` (synchronous); `ProvisionError::{Layout, Unpublish, Log, Database, Publish}`.

- [ ] **Step 1: Dependencies**

`crates/theo-podcasts/Cargo.toml`: remove `ekv.workspace = true` and `embassy-sync-06`; add

```toml
redb.workspace = true

[target.'cfg(not(target_os = "none"))'.dependencies]
redb = { workspace = true, features = ["std"] }

[dev-dependencies]
theo-storage = { workspace = true, features = ["test-disk"] }
```

(keep `futures` in dev-dependencies).

- [ ] **Step 2: Tables**

`crates/theo-podcasts/src/tables.rs`:

```rust
//! redb tables of the podcast catalogue (ADR-0022). Values keep the byte
//! layouts of schema 2; keys are typed, so ranges keep their order.
use redb::TableDefinition;

pub(crate) const SCHEMA: u64 = 3;

/// Episode id → episode record.
pub(crate) const EPISODES: TableDefinition<u64, &[u8]> = TableDefinition::new("episodes");
/// (subscription, 1 = guid / 2 = enclosure, SHA-256) → episode id.
pub(crate) const EPISODE_IDENTITY: TableDefinition<(u64, u8, [u8; 32]), u64> =
    TableDefinition::new("episode_identity");
/// Episode id → published audio length (bytes, non-zero).
pub(crate) const AUDIO: TableDefinition<u64, u64> = TableDefinition::new("audio");
/// Subscription id → canonical feed URL.
pub(crate) const SUBSCRIPTIONS: TableDefinition<u64, &str> = TableDefinition::new("subscriptions");
/// Radio France show id → subscription id.
pub(crate) const SHOWS: TableDefinition<[u8; 36], u64> = TableDefinition::new("shows");
/// Subscription id → last reconciled sync checkpoint.
pub(crate) const SYNC: TableDefinition<u64, &[u8]> = TableDefinition::new("sync");
/// Bitmap page → 4 KiB of unit allocation bits.
pub(crate) const UNIT_PAGES: TableDefinition<u16, &[u8]> = TableDefinition::new("unit_pages");
/// Episode id → unit record (state, length, extents).
pub(crate) const UNIT_EXTENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("unit_extents");
pub(crate) const COUNTERS: TableDefinition<&str, u64> = TableDefinition::new("counters");

pub(crate) const NEXT_EPISODE: &str = "next_episode";
pub(crate) const NEXT_SUBSCRIPTION: &str = "next_subscription";
pub(crate) const UNITS_USED: &str = "units_used";
pub(crate) const SCHEMA_KEY: &str = "schema";
```

`lib.rs`: add `mod tables;`; update the crate doc to "`provision::provision_v3` initializes a v3 card".

- [ ] **Step 3: Test support on `TestDisk`**

Replace `test_support.rs` with:

```rust
use crate::catalogue::Catalogue;
use theo_storage::{
    redb_backend::RegionBackend,
    test_disk::{Crash, TestDisk},
};

/// 16 MiB catalogue region: room for the scale test.
pub(crate) const SECTORS: u32 = 1 + 32_768;

/// A provisioned catalogue region on an in-memory disk.
pub(crate) struct Image {
    pub(crate) disk: TestDisk,
}
impl Image {
    pub(crate) fn new() -> Self {
        let mut disk = TestDisk::new(SECTORS);
        RegionBackend::format(&mut disk).unwrap();
        Self { disk }
    }
    pub(crate) fn backend(&self) -> RegionBackend<TestDisk> {
        // SAFETY: host tests use each catalogue on one thread only.
        unsafe { RegionBackend::new(self.disk.clone(), SECTORS) }.unwrap()
    }
    /// The first open of an empty region creates the catalogue.
    pub(crate) fn create(&self) -> Catalogue {
        Catalogue::open(self.backend()).unwrap()
    }
    pub(crate) fn reopen(&self) -> Catalogue {
        Catalogue::open(self.backend()).unwrap()
    }
    /// Power loss: writes after the last sync are lost.
    pub(crate) fn crash(&self) {
        self.disk.crash(Crash::DropPending);
    }
}
```

Test rewrite rules (apply in every `*_tests.rs`):
- `image.create().await` → `image.create()`, `image.reopen().await` → `image.reopen()`; drop `block_on` wrappers that only awaited catalogue calls (keep them around media or runtime futures that stay async).
- `image.fail.set(true)` → `image.disk.state().fail_writes = true`; `image.reads.get()` / `writes.get()` → `image.disk.state().reads` / `.writes`; `image.cut.set(Some(n)); image.prefix.set(p)` → `image.disk.state().cut = Some((n, p))`; `image.sync_to_disk.set(false)` + reopen → `image.crash()` + reopen.
- `Error::Storage(_)` → `Error::Storage`; `Catalogue<PageStore<HostFile>>` → `Catalogue`; `MediaStore<F>` impls → `MediaStore`.
- Delete tests whose subject is ekv/async cancellation and cannot happen with a synchronous catalogue: `cancelled_operation_permanently_disables_catalogue_owner`, every test that sets `hang`, and `open_into_*` tests (no `open_into` any more). List each deleted test in the commit body.
- Tests asserting exact ekv sector counts become assertions on behaviour (record present / absent after reopen), not counts.
- `catalogue_create_tests.rs`: test that a formatted region opens empty (`scan` returns no records, `next_subscription(None)` is `None`), that a second open sees the same counters, and that an unformatted region (`TestDisk::new(SECTORS)` without `format`) fails with `Error::Corrupt`.

- [ ] **Step 4: Write the new failing tests in `catalogue_tests.rs`**

```rust
// Regression (t-133): a missing schema counter is refused, never recreated.
#[test]
fn schema_mismatch_is_refused_without_writes() {
    let image = Image::new();
    drop(image.create());
    {
        let db = redb::Database::builder()
            .create_with_backend(image.backend())
            .unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.set_quick_repair(true);
        tx.open_table(crate::tables::COUNTERS)
            .unwrap()
            .insert(crate::tables::SCHEMA_KEY, 2)
            .unwrap();
        tx.commit().unwrap();
    }
    image.disk.reset_counters();
    assert!(matches!(Catalogue::open(image.backend()), Err(Error::Schema)));
    assert_eq!(image.disk.state().writes, 0);
}

#[test]
fn batch_upserts_commit_together_and_survive_reopen() {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.begin_batch();
    for i in 0..10 {
        let guid = std::format!("g{i}");
        cat.upsert(SubscriptionId(1), metadata(Some(&guid), "t", "https://x/a.mp3"), &mut scratch)
            .unwrap();
    }
    cat.commit_batch().unwrap();
    drop(cat);
    let mut cat = image.reopen();
    let page = cat.scan(None, &mut scratch, |_| ()).unwrap();
    assert_eq!(page.records, 10);
}

#[test]
fn crash_before_batch_commit_loses_only_the_batch() {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.upsert(SubscriptionId(1), metadata(Some("kept"), "t", "https://x/a.mp3"), &mut scratch)
        .unwrap();
    cat.begin_batch();
    cat.upsert(SubscriptionId(1), metadata(Some("lost"), "t", "https://x/b.mp3"), &mut scratch)
        .unwrap();
    image.crash();
    drop(cat);
    let mut cat = image.reopen();
    assert_eq!(cat.scan(None, &mut scratch, |_| ()).unwrap().records, 1);
}

#[test]
fn duplicate_guid_inside_a_batch_is_one_episode() {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.begin_batch();
    let a = cat
        .upsert(SubscriptionId(1), metadata(Some("g"), "t", "https://x/a.mp3"), &mut scratch)
        .unwrap();
    let b = cat
        .upsert(SubscriptionId(1), metadata(Some("g"), "t2", "https://x/a.mp3"), &mut scratch)
        .unwrap();
    cat.commit_batch().unwrap();
    assert_eq!(a.id, b.id);
    assert!(!b.created);
}

#[test]
fn other_operations_commit_a_pending_batch_first() {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.begin_batch();
    let id = cat
        .upsert(SubscriptionId(1), metadata(Some("g"), "t", "https://x/a.mp3"), &mut scratch)
        .unwrap()
        .id;
    cat.mark_read(id, &mut scratch).unwrap();
    image.crash();
    drop(cat);
    let mut cat = image.reopen();
    assert!(cat.get(id, &mut scratch).unwrap().unwrap().state.read);
}

#[test]
fn full_region_maps_to_full_and_disables() {
    // A region too small for growth: 1 MiB.
    let mut disk = theo_storage::test_disk::TestDisk::new(1 + 2048);
    theo_storage::redb_backend::RegionBackend::format(&mut disk).unwrap();
    // SAFETY: single-threaded test.
    let backend = unsafe { theo_storage::redb_backend::RegionBackend::new(disk, 1 + 2048) }.unwrap();
    let mut cat = Catalogue::open(backend).unwrap();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let mut result = Ok(());
    for i in 0..10_000 {
        let guid = std::format!("guid-{i}");
        if let Err(e) = cat.upsert(SubscriptionId(1), metadata(Some(&guid), "t", "https://x/a.mp3"), &mut scratch) {
            result = Err(e);
            break;
        }
    }
    assert!(matches!(result, Err(Error::Full)));
    assert!(matches!(cat.get(EpisodeId(1), &mut scratch), Err(Error::Disabled)));
}
```

In `units_tests.rs`:

```rust
// Regression (t-133): the ekv units scan also read the show index (`u` +
// 36-byte show id) and returned Corrupt. Separate tables cannot collide.
#[test]
fn unit_ids_ignore_subscriptions() {
    let image = Image::new();
    let mut cat = image.create();
    cat.follow("https://rss-rf.aerion.me/rss/00000000-0000-0000-0000-000000000002")
        .unwrap();
    let mut t = UnitTable::new_in(100, Global).unwrap();
    cat.load_units(&mut t).unwrap();
    let (unit, _) = allocate(&mut t, 1).unwrap();
    let mut rec = [0; 4096];
    let n = encode_record(RecordState::Temp, 0, &[Extent { start: unit, count: 1 }], &mut rec)
        .unwrap();
    cat.commit_units(&t, &[t.page_of(unit)], EpisodeId(3), Some(&rec[..n]))
        .unwrap();
    let mut out = [EpisodeId(0); crate::media::FILE_PAGE];
    assert_eq!(cat.unit_ids(None, &mut out).unwrap(), 1);
    assert_eq!(out[0], EpisodeId(3));
}
```

(`unit_ids` loses its `buf` parameter in Step 8.)

- [ ] **Step 5: Run to verify failure**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-podcasts --target $HOST`
Expected: compile errors (catalogue still on ekv).

- [ ] **Step 6: Catalogue core**

Replace the ekv parts of `catalogue.rs` (keep the public types, constants, `mark_read`/`should_queue`, `valid_transition`, `normalized`, `same_identity`, `Writer`/`Reader`, `encode`/`decode` exactly). Remove `SCHEMA`, `NEXT_KEY`, `SCHEMA_KEY`, the ekv `const _` assert, `episode_key`, `identity_key`, `read_error`/`write_error`/`commit_error`, `sync_key`, `audio_key`. New code:

```rust
//! Persistent identities and local state in a redb database (ADR-0022).
use crate::tables::*;
use redb::{Database, ReadableDatabase, ReadableTable, StorageBackend, WriteTransaction};
use sha2::{Digest, Sha256};

/// redb page cache; smaller caches cost more writes per commit (ADR-0022).
pub const CACHE_BYTES: usize = 256 * 1024;
const AUDIO_OFFSET: usize = 18;
const READ_OFFSET: usize = 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Storage,
    Corrupt,
    Schema,
    FieldTooLong,
    InvalidMetadata,
    BufferTooSmall,
    NotFound,
    IdExhausted,
    IdentityCollision,
    InvalidState,
    Full,
    Disabled,
}

/// Map any redb error: region growth refused → `Full`, device failure →
/// `Storage`, unknown layout → `Schema`, anything else → `Corrupt`.
pub(crate) fn db_error(error: impl Into<redb::Error>) -> Error {
    use alloc::string::ToString;
    match error.into() {
        redb::Error::Io(e) if e.to_string().contains(theo_storage::redb_backend::REGION_FULL) => {
            Error::Full
        }
        redb::Error::Io(_) | redb::Error::PreviousIo | redb::Error::DatabaseClosed => {
            Error::Storage
        }
        redb::Error::TableDoesNotExist(_) | redb::Error::UpgradeRequired(_) => Error::Schema,
        _ => Error::Corrupt,
    }
}

/// Unique database owner. Metadata views borrow the caller's scratch buffer,
/// never the database. Storage, format and capacity errors disable this
/// owner until it is dropped and reopened.
pub struct Catalogue {
    pub(crate) db: Database,
    next: u64,
    enabled: bool,
    /// Open write transaction: a sync batch, or the current operation.
    tx: Option<WriteTransaction>,
    batching: bool,
}

impl Catalogue {
    /// Open the catalogue; create its tables if `backend` is empty.
    ///
    /// Only an explicitly provisioned region is empty (`RegionBackend`
    /// refuses a missing length record), so this never formats user data.
    pub fn open(backend: impl StorageBackend) -> Result<Self, Error> {
        let fresh = backend.len().map_err(|_| Error::Storage)? == 0;
        let db = Database::builder()
            .set_cache_size(CACHE_BYTES)
            .create_with_backend(backend)
            .map_err(db_error)?;
        let mut catalogue = Self { db, next: 0, enabled: true, tx: None, batching: false };
        if fresh {
            catalogue.create_tables()?;
        }
        catalogue.validate()?;
        Ok(catalogue)
    }

    fn create_tables(&mut self) -> Result<(), Error> {
        let mut tx = self.db.begin_write().map_err(db_error)?;
        tx.set_quick_repair(true);
        {
            tx.open_table(EPISODES).map_err(db_error)?;
            tx.open_table(EPISODE_IDENTITY).map_err(db_error)?;
            tx.open_table(AUDIO).map_err(db_error)?;
            tx.open_table(SUBSCRIPTIONS).map_err(db_error)?;
            tx.open_table(SHOWS).map_err(db_error)?;
            tx.open_table(SYNC).map_err(db_error)?;
            tx.open_table(UNIT_PAGES).map_err(db_error)?;
            tx.open_table(UNIT_EXTENTS).map_err(db_error)?;
            let mut counters = tx.open_table(COUNTERS).map_err(db_error)?;
            counters.insert(SCHEMA_KEY, SCHEMA).map_err(db_error)?;
            counters.insert(NEXT_EPISODE, 1).map_err(db_error)?;
            counters.insert(NEXT_SUBSCRIPTION, 1).map_err(db_error)?;
            counters.insert(UNITS_USED, 0).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)
    }

    /// Read-only schema check. Never formats or migrates.
    fn validate(&mut self) -> Result<(), Error> {
        let tx = self.db.begin_read().map_err(db_error)?;
        let counters = tx.open_table(COUNTERS).map_err(db_error)?;
        let get = |key| -> Result<Option<u64>, Error> {
            Ok(counters.get(key).map_err(db_error)?.map(|v| v.value()))
        };
        if get(SCHEMA_KEY)? != Some(SCHEMA) {
            return Err(Error::Schema);
        }
        let next = get(NEXT_EPISODE)?.filter(|&n| n != 0).ok_or(Error::Corrupt)?;
        get(NEXT_SUBSCRIPTION)?.filter(|&n| n != 0).ok_or(Error::Corrupt)?;
        get(UNITS_USED)?.ok_or(Error::Corrupt)?;
        tx.open_table(EPISODES).map_err(db_error)?;
        tx.open_table(EPISODE_IDENTITY).map_err(db_error)?;
        tx.open_table(AUDIO).map_err(db_error)?;
        tx.open_table(SUBSCRIPTIONS).map_err(db_error)?;
        tx.open_table(SHOWS).map_err(db_error)?;
        tx.open_table(SYNC).map_err(db_error)?;
        tx.open_table(UNIT_PAGES).map_err(db_error)?;
        tx.open_table(UNIT_EXTENTS).map_err(db_error)?;
        self.next = next;
        Ok(())
    }

    /// Start a sync batch: upserts share one write transaction until
    /// [`Self::commit_batch`]. Any other operation commits it first.
    pub(crate) fn begin_batch(&mut self) {
        self.batching = true;
    }
    /// Commit and end the current batch.
    pub(crate) fn commit_batch(&mut self) -> Result<(), Error> {
        self.batching = false;
        self.begin()?;
        let result = self.commit();
        self.finish(result)
    }

    /// The open write transaction, begun with quick repair if none is open.
    pub(crate) fn write(&mut self) -> Result<&mut WriteTransaction, Error> {
        if self.tx.is_none() {
            let mut tx = self.db.begin_write().map_err(db_error)?;
            tx.set_quick_repair(true);
            self.tx = Some(tx);
        }
        self.tx.as_mut().ok_or(Error::Corrupt)
    }
    /// Commit the open write transaction, if any.
    pub(crate) fn commit(&mut self) -> Result<(), Error> {
        match self.tx.take() {
            Some(tx) => tx.commit().map_err(db_error),
            None => Ok(()),
        }
    }
    /// End a single operation's writes: commit unless a batch is running.
    pub(crate) fn done(&mut self) -> Result<(), Error> {
        if self.batching { Ok(()) } else { self.commit() }
    }

    /// Enter an operation that is not a batched upsert: commit a pending
    /// batch first so reads see it.
    pub(crate) fn begin(&mut self) -> Result<(), Error> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        self.enabled = false;
        if self.tx.is_some() && !self.batching {
            let result = self.commit();
            if result.is_err() {
                return result;
            }
        }
        Ok(())
    }
    pub(crate) fn finish<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        if matches!(
            result,
            Err(Error::Storage | Error::Corrupt | Error::Schema | Error::IdentityCollision
                | Error::Full | Error::Disabled)
        ) {
            // Abort: the database rolls back whatever this operation wrote.
            self.tx = None;
            self.batching = false;
        } else {
            self.enabled = true;
        }
        result
    }
    fn check(&self, scratch: &[u8]) -> Result<(), Error> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        if scratch.len() < MAX_RECORD_BYTES {
            return Err(Error::BufferTooSmall);
        }
        Ok(())
    }
}
```

`begin()` must not commit a pending batch for `upsert` while batching: `upsert` calls `begin()` too, but with `self.batching` true the commit is skipped. Operations other than `upsert` that run while `batching` is true must commit the batch: implement this by having every non-upsert public method call `self.end_batch()?` (sets `batching = false`) before `self.begin()`:

```rust
    /// Leave batch mode so `begin` commits the pending batch.
    fn end_batch(&mut self) {
        self.batching = false;
    }
```

Then each method follows one shape. Reads go through a read transaction (after `begin` committed any batch); writes through `self.write()` + `self.done()`. Implement:

```rust
    pub fn get<'a>(&mut self, id: EpisodeId, scratch: &'a mut [u8])
        -> Result<Option<Episode<'a>>, Error> {
        self.check(scratch)?;
        self.end_batch();
        self.begin()?;
        let result = self.load(id, scratch).map(|r| r.map(|(e, _)| e));
        self.finish(result)
    }

    /// Read through the open write transaction when there is one, so a
    /// batch sees its own upserts.
    fn load<'a>(&mut self, id: EpisodeId, scratch: &'a mut [u8])
        -> Result<Option<(Episode<'a>, usize)>, Error> {
        let next = self.next;
        let n = match self.tx.as_ref() {
            Some(tx) => copy(&tx.open_table(EPISODES).map_err(db_error)?, id.0, scratch)?,
            None => {
                let tx = self.db.begin_read().map_err(db_error)?;
                copy(&tx.open_table(EPISODES).map_err(db_error)?, id.0, scratch)?
            }
        };
        let Some(n) = n else { return Ok(None) };
        let episode = decode(&scratch[..n]).ok_or(Error::Corrupt)?;
        if episode.id != id || id.0 >= next {
            return Err(Error::Corrupt);
        }
        Ok(Some((episode, n)))
    }
```

with free helpers:

```rust
/// Copy a byte value into `scratch`; its length, or `None` if absent.
pub(crate) fn copy<K: redb::Key + 'static>(
    table: &impl ReadableTable<K, &'static [u8]>,
    key: impl core::borrow::Borrow<K::SelfType<'_>>,
    scratch: &mut [u8],
) -> Result<Option<usize>, Error> {
    let Some(value) = table.get(key).map_err(db_error)? else { return Ok(None) };
    let value = value.value();
    let out = scratch.get_mut(..value.len()).ok_or(Error::Corrupt)?;
    out.copy_from_slice(value);
    Ok(Some(value.len()))
}
fn identity(subscription: SubscriptionId, metadata: EpisodeMetadata<'_>) -> (u64, u8, [u8; 32]) {
    (
        subscription.0,
        if metadata.guid.is_some() { 1 } else { 2 },
        Sha256::digest(metadata.guid.unwrap_or(metadata.enclosure).as_bytes()).into(),
    )
}
```

Note: `tx.open_table(..)` on a `WriteTransaction` borrows it mutably in redb (`&self` in 4.x — check; if `open_table` takes `&self`, `self.tx.as_ref()` works; otherwise use `as_mut()` and restructure `load` to take `&mut self`). Fix borrow shapes to compile without changing behaviour.

`upsert` (the only operation allowed inside a batch):

```rust
    pub fn upsert(&mut self, subscription: SubscriptionId, metadata: EpisodeMetadata<'_>,
        scratch: &mut [u8]) -> Result<UpsertOutcome, Error> {
        self.check(scratch)?;
        let metadata = normalized(metadata)?;
        if subscription.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        self.begin()?;
        let result = self.upsert_inner(subscription, metadata, scratch);
        self.finish(result)
    }
    fn upsert_inner(&mut self, subscription: SubscriptionId, metadata: EpisodeMetadata<'_>,
        scratch: &mut [u8]) -> Result<UpsertOutcome, Error> {
        let key = identity(subscription, metadata);
        self.write()?;
        let existing = {
            let tx = self.tx.as_ref().ok_or(Error::Corrupt)?;
            let index = tx.open_table(EPISODE_IDENTITY).map_err(db_error)?;
            index.get(&key).map_err(db_error)?.map(|v| EpisodeId(v.value()))
        };
        // Same decision logic as before (collision, unchanged metadata,
        // new id from `self.next`, queueing); copy it verbatim from the
        // ekv version, replacing `self.load(id, scratch).await?` with
        // `self.load(id, scratch)?`. On the unchanged-metadata early
        // return, call `self.done()?` first.
        // ... compute (id, state, created, queued) and `n = encode(..)` ...
        {
            let tx = self.tx.as_ref().ok_or(Error::Corrupt)?;
            tx.open_table(EPISODES).map_err(db_error)?
                .insert(id.0, &scratch[..n]).map_err(db_error)?;
            if created {
                tx.open_table(EPISODE_IDENTITY).map_err(db_error)?
                    .insert(&key, id.0).map_err(db_error)?;
                tx.open_table(COUNTERS).map_err(db_error)?
                    .insert(NEXT_EPISODE, self.next + 1).map_err(db_error)?;
            }
        }
        if created {
            self.next += 1;
        }
        self.done()?;
        Ok(UpsertOutcome { id, state, created, queued })
    }
```

The middle section is the existing decision code from `upsert_inner` (lines "let (id, mut state, created) = …" through "let n = encode(…)"), unchanged except for the `.await` removals named in the comment. Write it out in full in the file. If a batch is aborted by `finish` (error), `self.next` may be ahead of the database: reset it by making `finish` call `self.reload_next()` (read `NEXT_EPISODE` from a read transaction; on failure leave the owner disabled) whenever it drops `self.tx`.

`scan`:

```rust
    pub fn scan(&mut self, after: Option<EpisodeId>, scratch: &mut [u8],
        mut emit: impl for<'record> FnMut(Episode<'record>)) -> Result<ScanPage, Error> {
        self.check(scratch)?;
        self.end_batch();
        self.begin()?;
        let result = (|| {
            let next = self.next;
            let tx = self.db.begin_read().map_err(db_error)?;
            let table = tx.open_table(EPISODES).map_err(db_error)?;
            let range = match after {
                Some(a) => table.range((core::ops::Bound::Excluded(a.0), core::ops::Bound::Unbounded)),
                None => table.range::<u64>(..),
            }
            .map_err(db_error)?;
            let mut page = ScanPage { resume_after: None, has_more: false, records: 0, bytes: 0 };
            for entry in range {
                let (k, v) = entry.map_err(db_error)?;
                let (id, value) = (EpisodeId(k.value()), v.value());
                // Byte accounting keeps the schema-2 key size (9 bytes).
                if page.records == MAX_SCAN_RECORDS || page.bytes + 9 + value.len() > MAX_SCAN_BYTES {
                    page.has_more = true;
                    break;
                }
                let n = value.len();
                scratch.get_mut(..n).ok_or(Error::Corrupt)?.copy_from_slice(value);
                let episode = decode(&scratch[..n]).ok_or(Error::Corrupt)?;
                if episode.id != id || id.0 >= next || id.0 == 0 {
                    return Err(Error::Corrupt);
                }
                emit(episode);
                page.resume_after = Some(id);
                page.records += 1;
                page.bytes += 9 + n;
            }
            Ok(page)
        })();
        self.finish(result)
    }
```

`mark_read`, `transition_audio` → `end_batch(); begin()`; `state_inner` loads with `self.load`, then writes through `self.write()`:

```rust
        let tx = self.write()?;
        if matches!(state.audio, AudioState::Missing | AudioState::Reclaimed) {
            tx.open_table(AUDIO).map_err(db_error)?.remove(id.0).map_err(db_error)?;
        }
        tx.open_table(EPISODES).map_err(db_error)?
            .insert(id.0, &scratch[..n]).map_err(db_error)?;
        self.done()
```

`completed_audio_len` reads `AUDIO` (`Some(0)` → `Corrupt`); `finish_download` writes `AUDIO` + `EPISODES` in one `write()`/`done()`; `sync_checkpoint` reads `SYNC` via `copy`; `save_sync_checkpoint` inserts into `SYNC`. All call `end_batch(); begin()` and `finish`. All lose `async`.

- [ ] **Step 7: Subscriptions on redb**

`subscriptions.rs`: remove `NEXT`, `key()`, ekv imports. `follow`:

```rust
    pub fn follow(&mut self, feed: &str) -> Result<SubscriptionId, Error> {
        let parsed = FeedUrl::parse(feed).map_err(|_| Error::InvalidMetadata)?;
        if parsed.page != 0 {
            return Err(Error::InvalidMetadata);
        }
        let canonical = feed.split('?').next().ok_or(Error::InvalidMetadata)?;
        let show: [u8; 36] = parsed.show.as_bytes().try_into().map_err(|_| Error::InvalidMetadata)?;
        self.end_batch();
        self.begin()?;
        let result = (|| {
            let tx = self.write()?;
            if let Some(id) = tx.open_table(SHOWS).map_err(db_error)?.get(&show).map_err(db_error)? {
                let id = id.value();
                return if id == 0 { Err(Error::Corrupt) } else { Ok(SubscriptionId(id)) };
            }
            let mut counters = tx.open_table(COUNTERS).map_err(db_error)?;
            let id = counters.get(NEXT_SUBSCRIPTION).map_err(db_error)?
                .map(|v| v.value()).filter(|&n| n != 0).ok_or(Error::Corrupt)?;
            let next = id.checked_add(1).ok_or(Error::IdExhausted)?;
            counters.insert(NEXT_SUBSCRIPTION, next).map_err(db_error)?;
            drop(counters);
            tx.open_table(SUBSCRIPTIONS).map_err(db_error)?.insert(id, canonical).map_err(db_error)?;
            tx.open_table(SHOWS).map_err(db_error)?.insert(&show, id).map_err(db_error)?;
            Ok(SubscriptionId(id))
        })();
        let result = result.and_then(|id| self.done().map(|()| id));
        self.finish(result)
    }
```

(The existing-follow path commits an empty transaction through `done()`; that is cheap enough and keeps one code path. If the borrow checker rejects the closure capturing `self`, use a private `fn follow_inner(&mut self, …)`.)

`next_subscription`: `end_batch(); begin()`; read transaction; `SUBSCRIPTIONS.range((Excluded(after.unwrap_or(SubscriptionId(0)).0), Unbounded))`, take the first entry; same validation (`id != 0`, `FeedUrl::parse(feed)?.page == 0`, `heapless::String::try_from`).

- [ ] **Step 8: Units on redb**

`units.rs`: remove `UNITS_KEY`, `page_key`, `record_key`, ekv imports; keep `UnitTable`, `RecordView`, constants. Rewrite the `impl Catalogue` block:
- `load_units`: `end_batch(); begin()`; read tx; `UNITS_USED` counter (`u64`, must fit `u32`, else `Corrupt`); for each page `p` in `0..table.pages()` read `UNIT_PAGES.get(p)`: present → length must be `PAGE_BYTES`, copy; absent → fill 0. Same recount check.
- `commit_units`: same `dirty` validation; `end_batch(); begin()`; `let tx = self.write()?;` insert each dirty page, `COUNTERS.insert(UNITS_USED, u64::from(table.used()))`, then `UNIT_EXTENTS.insert(id.0, r)` or `.remove(id.0)`; `self.done()`.
- `unit_record`: read `UNIT_EXTENTS` via `copy` into `buf`, then `RecordView::decode` as before.
- `unit_ids`: range over `UNIT_EXTENTS` after `after`, up to `FILE_PAGE` ids, `id == 0` → `Corrupt`. The `buf` parameter becomes unused: remove it and update the callers in `unit_media.rs`.

- [ ] **Step 9: Drop `F: Flash` in the other modules**

Mechanical rules for `sync.rs, recovery.rs, reclaim.rs, download.rs, media.rs, unit_media.rs, runtime.rs, worker.rs, service.rs, media_test_support.rs, provision.rs, examples/prepare_image`:
- Remove `use ekv::flash::Flash;` and every `<F: Flash>` / `F: Flash` bound; `Catalogue<F>` → `Catalogue`; `Error<F::Error>` → `Error`; `SyncError<F::Error>`, `UnitError<F::Error>` and the like lose their type parameter (`SyncError::Catalogue(Error)`, `UnitError::Catalogue(Error)`); `MediaStore<F>` → `MediaStore`; `Runtime<F, M>` → `Runtime<M>`.
- Remove `.await` after calls to catalogue methods (they are synchronous). A function stays `async` only if it still awaits something else (media device IO, network, timers); otherwise make it a plain `fn` and remove `.await` at its call sites.
- `Error::Storage(e)` patterns → `Error::Storage`; delete arms for `Error::Cancelled`.
- `sync.rs`: in `push`, call `cat.begin_batch()` before the first upsert of the call; in `end_page`'s callers and in `complete`, call `cat.commit_batch()` — concretely, give `end_page` a `cat: &mut Catalogue` parameter and commit the batch before returning `Ok(..)` (a page ends there); `complete` commits before saving the checkpoint. Update `sync_tests.rs` callers. A failed sync (`SyncError`) must not commit: drop the session, and the next catalogue operation commits nothing because `finish` cleared the transaction on error; on network errors (no catalogue error) the pending batch is committed by the next operation — upserts are idempotent, so that is safe.
- `runtime.rs`: `Reply` loses nothing (the uncommitted `Opening` variant was discarded in Task 0).

- [ ] **Step 10: Provisioning v3**

`provision.rs`:

```rust
//! Destructive initialization of a v3 card layout (ADR-0021, ADR-0022).
//!
//! [`provision_v3`] unpublishes the old contents, formats the log volume,
//! writes an empty database length record, and only then publishes both
//! descriptors. The first catalogue open creates the tables. An interrupted
//! run leaves a blank card. The host `prepare_image` tool and the explicit
//! `sd-provision-v3` firmware build both call it; [`Target`] decides which
//! card the firmware may erase. Nothing else may call it.
```

- Remove `PAGES`, `PageStore`/`PageError`/`AsyncRegion`/`Catalogue` imports; `ProvisionError::Pages` and `::Catalogue` become one `Database(littlefs_rust::Error)` ("writing the database length record failed").
- `pub fn provision_v3<D: SectorIo, A: Allocator + Clone>(device: &mut D, card_sectors: u64, allocator: A) -> Result<CardLayout, ProvisionError>` (not `async`): `CardLayout::new(card_sectors)`; same unpublish; inside the `split(Deferred(..))` block: `format_log(log, …)?; RegionBackend::format(&mut db).map_err(ProvisionError::Database)?;` (`db` must be `mut`); same publish.
- Error `Display` for `Layout`: "card too small for layout v3".
- `provision_guard.rs`: token `v2:` → `v3:` (the `[b'v', b'2', b':', rest @ ..]` pattern and its docs/tests); rename any `v2` wording.
- `provision_tests.rs`: port to `TestDisk` sized to a small card. If `CardLayout::new` needs more sectors than a test can afford, use `TestDisk` (sparse) with `card_sectors = 16 + 131_072 + 32_768 + 65_536 + UNIT_SECTORS` rounded. Assert: descriptors written last, an interrupted run (cut at each write before the descriptors) leaves both descriptor sectors zero, and after success `Catalogue::open(RegionBackend::new(db region))` succeeds with an empty catalogue.
- `examples/prepare_image`: call `provision_v3` without `.await`; the image file is `O_EXCL`, unchanged.

- [ ] **Step 11: Run the crate's tests**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-podcasts --target $HOST && RUSTUP_TOOLCHAIN=stable cargo test -p theo-podcasts --example prepare_image --target $HOST`
Expected: all pass. Run `RUSTUP_TOOLCHAIN=stable cargo clippy -p theo-podcasts --target $HOST --all-targets -- -D warnings`: clean.

- [ ] **Step 12: Commit**

```bash
git add crates/theo-podcasts crates/theo-storage Cargo.lock
git commit -m "feat(podcasts)!: store the catalogue in redb with typed tables (t-133)

Deleted tests (async/ekv-only): <list>.

Review-waived: per-task review runs in the plan executor"
```

---

### Task 5: Firmware on redb, PSRAM-first heap

**Files:**
- Modify: `src/device/sd_worker.rs`, `src/device/sd_provision.rs`, `src/device/mod.rs`, `justfile`, `features`/`build.rs` if they mention `v2:`

**Interfaces:**
- Consumes: `Catalogue::open`, `RegionBackend::new`, `provision_v3`, `CardLayout::new(card_sectors)`, `UnitMedia::open_in(&mut Catalogue, …)`.

- [ ] **Step 1: Heap order**

In `Device::init` (`src/device/mod.rs`), move both `esp_alloc::heap_allocator!` lines after `esp_alloc::psram_allocator!(…)`, and replace the comment with:

```rust
        // PSRAM is registered first, so general allocations (redb's page
        // cache above all, ADR-0022) land in PSRAM; esp-radio and esp-rtos
        // request `Internal` memory explicitly and keep the internal heap.
```

Before moving, confirm nothing between the old and new position allocates: `logging::init` and `esp_hal::init` use statics only (`grep -n "Box\|Vec\|alloc" src/device/logging.rs`). If `logging::init` allocates, keep it after the heap registration (move it below the allocators).

- [ ] **Step 2: Open the catalogue in `open_podcasts`**

In `sd_worker.rs`:
- `type DbFlash` is removed; `type PodcastOwner = Runtime<UnitStore>;`; `PodcastParts = (RegionIo<CardIo>, RegionIo<CardIo>, CardLayout)` stays.
- Replace the PageStore/`open_into` section of `open_podcasts` with:

```rust
    let sectors = layout.database().sectors();
    // SAFETY: the storage owner creates, uses and drops the catalogue and
    // this backend on core1 only; `PodcastOwner` holds `Rc`s, so it is
    // `!Send` and the compiler keeps the database on this core.
    let backend = match unsafe { RegionBackend::new(db, sectors) } {
        Ok(backend) => backend,
        Err(error) => {
            log::warn!("podcasts offline: database region unusable: {error:?}");
            return None;
        }
    };
    let mut catalogue = match Catalogue::open(backend) {
        Ok(catalogue) => catalogue,
        Err(error) => {
            log::warn!("podcasts offline: catalogue open failed: {error:?}");
            return None;
        }
    };
```

  then `UnitMedia::open_in(&mut catalogue, media, layout.media_units(), &esp_alloc::ExternalMemory)` through `apart` as today, and `install(catalogue, units)` taking the catalogue by value. Change the success log to milliseconds: `"podcasts online: catalogue opened in {} ms"`, `started.elapsed().as_millis()`. Update the function doc (no 10 KB catalogue any more; the runtime is still built in PSRAM).
- Mount: where the descriptor `select` result is handled, add a dedicated log for `Err(LayoutError::Version)`: `log::warn!("microSD layout is not v3: re-provision with `just sd-provision-v3`")`, behaviour otherwise unchanged (no regions mount).
- Remove now-unused imports (`PageStore`, `PageError`, `AsyncRegion` if unused).

- [ ] **Step 3: Provisioning build**

`sd_provision.rs`: call `provision_v3(&mut card, sectors, &esp_alloc::ExternalMemory)` without `.await`; rename log texts "v2" → "v3". `justfile`: rename the recipe and its log path to `sd-provision-v3`, token `"v3:"`. Search the repo for `sd-provision-v2` and `THEO_SD_PROVISION=v2` (`grep -rn "provision-v2\|v2:" justfile src build.rs AGENTS.md README.md docs/adr/0021*`) and update each.

- [ ] **Step 4: Verify**

Run: `just fmt && just check && just lint && just build`
Expected: all succeed. Then check the core1 owner frame (AGENTS.md gotcha): disassemble and read the `l32r` literal after `entry` in `__worker_task…::poll`:

```bash
xtensa-esp32s3-elf-objdump -d target/xtensa-esp32s3-none-elf/release/theo-pod | grep -A3 "worker_task.*poll>:" | head
```

Expected: the frame does not grow compared with the same measurement on the commit before Task 1 (build it in a temporary `git worktree add`; record both numbers in the commit body). If it grows past 8 KiB, move `Catalogue::open` into an `#[inline(never)]` helper.

- [ ] **Step 5: Commit**

```bash
git add src justfile
git commit -m "feat(storage)!: open the redb catalogue at mount, PSRAM-first heap (t-133)

Owner frame: <before> -> <after> bytes.

Review-waived: per-task review runs in the plan executor"
```

---

### Task 6: Remove ekv, PageStore and the FAT page transport

**Files:**
- Delete: `crates/theo-storage/src/page_store.rs`, `page_store_tests.rs`, `page_store_rollover_tests.rs`, `page_store_history_tests.rs`, `fat_page_file.rs`, `fat_page_file_tests.rs`, `vendor/ekv/`
- Modify: `Cargo.toml` (remove `ekv` dependency, its `[patch.crates-io]` entry and the `exclude` item; remove `hadris-*` workspace deps if nothing else uses them), `crates/theo-storage/Cargo.toml` (remove `ekv`, `hadris-*`, `legacy-fat`), `crates/theo-storage/src/lib.rs`, `crates/theo-storage/src/regions.rs` (drop `AsyncRegion`/`SectorFile` glue if only PageStore used it), `justfile` (`--features theo-storage/legacy-fat` in `test` and `lint`, recipes `test-storage-recovery`, `test-catalogue-recovery`), `crates/theo-storage/ARCHITECTURE.md`, `README.md`, `AGENTS.md`, `docs/adr/README.md`, `docs/adr/0021-provision-layout-v2-on-the-device.md`, `deny.toml` if it lists ekv.

- [ ] **Step 1: Delete and fix references**

```bash
git rm -r crates/theo-storage/src/page_store*.rs crates/theo-storage/src/fat_page_file*.rs vendor/ekv
grep -rn "ekv\|page_store\|PageStore\|fat_page_file\|legacy-fat\|AsyncRegion\|SectorFile" \
  --include='*.rs' --include='*.toml' --include='*.md' --include=justfile . | grep -v '^./target\|^./docs/adr/00\(1[0-9]\|2[01]\)'
```

Fix every hit outside historical ADRs: remove code that only served PageStore (`AsyncRegion`, `with_before_io`, `Guard`, `SectorFile`, `file_len`) after checking with grep that nothing else uses it. `ARCHITECTURE.md`: replace the `page_store.rs` and `fat_page_file.rs` bullets with `redb_backend.rs` (`RegionBackend`, length record, no zero-fill, unsafe single-core contract) and `test_disk.rs`; "Card layout v3": redb database 64 MiB from sector 16. `AGENTS.md`: mention `just sd-provision-v3`. ADR-0021: status line "Amended by ADR-0022: layout v3, `provision_v3` writes an empty redb length record". `docs/adr/README.md`: ADR-0021 status "Amended by ADR-0022 (layout v3)".

- [ ] **Step 2: Verify**

Run: `just fmt && just check && just lint && just test && just build`
Expected: all succeed. `cargo tree -i ekv` (host and esp) reports no package.

- [ ] **Step 3: Commit**

```bash
git add -A crates Cargo.toml Cargo.lock justfile README.md AGENTS.md docs deny.toml
git commit -m "refactor(storage)!: remove ekv, PageStore and the FAT page transport (t-133)

Review-waived: per-task review runs in the plan executor"
```

---

### Task 7: Scale test and the full verification gate

**Files:**
- Create: `crates/theo-podcasts/src/scale_tests.rs`
- Modify: `crates/theo-podcasts/src/lib.rs` (`#[cfg(test)] mod scale_tests;`)

- [ ] **Step 1: Scale test**

```rust
//! The scale target of ADR-0022: 50 subscriptions, 5,000 episodes, bounded
//! in sector commands (2 ms each on the board).
use crate::{catalogue::*, test_support::Image};

const FEEDS: u64 = 50;
const EPISODES: u64 = 5_000;

fn fill() -> Image {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let description = "d".repeat(900);
    for page in 0..EPISODES / 50 {
        cat.begin_batch();
        for i in page * 50..(page + 1) * 50 {
            let guid = std::format!("guid-{i}");
            let url = std::format!("https://media.radiofrance-podcast.net/{i}.mp3");
            let metadata = EpisodeMetadata {
                guid: Some(&guid),
                title: "A title of moderate length for a podcast episode",
                enclosure: &url,
                description: Some(&description),
                published: Some(1_700_000_000),
                enclosure_bytes: Some(30_000_000),
                duration_seconds: Some(3_000),
            };
            cat.upsert(SubscriptionId(1 + i % FEEDS), metadata, &mut scratch).unwrap();
        }
        cat.commit_batch().unwrap();
    }
    image
}

// Regression (t-133): boot-time open stays O(1) at the scale target.
#[test]
fn open_at_scale_reads_at_most_64_sectors() {
    let image = fill();
    image.crash();
    image.disk.reset_counters();
    drop(image.reopen());
    let reads = image.disk.state().reads;
    assert!(reads <= 64, "{reads} reads");
}

#[test]
fn scan_page_and_commit_stay_within_the_io_budget() {
    let image = fill();
    let mut cat = image.reopen();
    let mut scratch = [0; MAX_RECORD_BYTES];
    image.disk.reset_counters();
    cat.scan(Some(EpisodeId(2_500)), &mut scratch, |_| ()).unwrap();
    let scan = image.disk.state().reads;
    assert!(scan <= 160, "scan: {scan} reads");
    image.disk.reset_counters();
    cat.mark_read(EpisodeId(2_500), &mut scratch).unwrap();
    let s = image.disk.state();
    // IO_BUDGET 500 ms at 2 ms per command.
    assert!(s.reads + s.writes <= 250, "commit: {} reads + {} writes", s.reads, s.writes);
}
```

- [ ] **Step 2: Run**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p theo-podcasts --release --target $HOST scale_tests`
Expected: pass. If a bound fails, **stop** and report the measured counts to the user; do not raise a bound without their agreement (no deadline increases without agreement).

- [ ] **Step 3: Full gate**

Run, in order: `just fmt`, `just check`, `just lint`, `just test`, `just miri` (unsafe `RegionBackend` impls; ~5 min, run in background), `just audit` (new dependency redb), `just build`.
Expected: all succeed. Record `just audit` output for redb in the commit body.

- [ ] **Step 4: Commit**

```bash
git add crates/theo-podcasts/src
git commit -m "test(podcasts): bound open, scan and commit at the scale target (t-133)

Review-waived: per-task review runs in the plan executor"
```

---

### Task 8: Board validation (only on the user's explicit request)

Do not run any of this without the user saying so in chat. `sd-provision-v3` erases the card.

- [ ] **Step 1:** Ask the user for approval to erase card `c9000031f6` and provision v3: `just sd-provision-v3 122265600 c9000031f6` (bounded with `perl -e 'setpgrp(0,0); exec @ARGV' just … &` then `kill -- -PID`; worktree needs `RUSTFLAGS="-C link-arg=-Tlinkall.x"`).
- [ ] **Step 2:** Flash the normal firmware with `just flash-dump`; confirm `podcasts online: catalogue opened in N ms` with N in the hundreds of ms, and the boot-to-online delay within a few seconds.
- [ ] **Step 3:** Read the heap stats line (`heap usage:`) and confirm internal free memory stays comparable to before (PSRAM-first) and the UI stays fluid (ask the user to look at the panel).
- [ ] **Step 4:** Measure the core1 stack high-water mark through the existing stack painting log; it must leave at least 8 KiB of the 48 KiB.
- [ ] **Step 5:** Follow a feed and let one page sync; then cut power during a sync (user action), boot with `just monitor-passive`, and confirm the catalogue reopens in milliseconds with the committed pages intact.
- [ ] **Step 6:** Update ADR-0022's status to "Implemented; board-validated" with the measured numbers; commit with `Review-waived: documentation of board measurements`.
