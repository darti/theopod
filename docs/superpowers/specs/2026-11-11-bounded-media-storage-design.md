# Bounded media storage (t-121)

Logs and podcast media share one LittleFS volume today. LittleFS has no
persistent free map: each lookahead refill and the mount-time `gc()` read every
file's block list (about one sector read per two 4 KiB blocks, ~2 ms each on
the board). `LittlefsMedia::space()` calls `fs_size()`, which walks the same
lists, for every 4 KiB download chunk. Past ~2 MB of media every chunk overruns
its 500 ms budget, and per ADR-0018 that disables logs and podcasts until
reboot.

Goal: keep per-request storage cost for logs and downloads independent of how
much media is stored, with no deadline increase and no formatting of user media,
and stop a podcast failure from disabling SD logging.

Decisions made with the user (2026-11-11): no region-layout (v1) card exists,
so the layout may change without compatibility; failures are isolated per
region; approach A (raw allocation-unit store) is selected. `ekv-fs` 0.2 was
reviewed and rejected for media (whole-file writes from RAM, no append/seek;
EKV caps at 65,536 pages and its LSM compaction would copy audio payloads);
its metadata-plus-chunks model is kept, with chunks on raw sectors.

## 1. Card layout v2

| Range | Content | Size |
|---|---|---|
| Sectors 0 and 8 | Immutable v2 descriptors (`THEOSD02`, CRC) | — |
| From sector 16 | EKV catalogue on PageStore (unchanged) | `512 + pages * 2 * 4608` bytes |
| Log region | LittleFS holding only logs | 16 MiB (32,768 sectors) |
| Media region | Raw sectors in 1 MiB allocation units, no filesystem | remainder, whole units |

- The log region starts at the first 4096-byte boundary after the database;
  the media region starts at the first 1 MiB boundary after the log region and
  holds `floor(remaining / 1 MiB)` units. The minimum media size stays 32 MiB.
- Logs are at most two files of `LOG_FILE_LIMIT` (128 KiB), so log mount `gc()`
  and allocation scans touch a few dozen blocks regardless of media.
- `CardLayout::select` accepts only v2. v1, unknown versions, bad geometry and
  conflicting copies are rejected exactly as unknown versions are today.
  `regions::split` returns three disjoint `RegionIo` halves (database, log,
  media) sharing the one sector client. With
  no descriptor, the legacy whole-card LittleFS log mount is unchanged. Boot
  never formats (ADR-0015).
- Unit size trade-off: average tail waste 0.5 MiB per episode (~1% of a
  50 MB episode); a 3 h episode is ~170 units; one 4096-byte bitmap page
  covers 32 GiB.

## 2. Unit store

### Catalogue keys (schema 1 → 2)

Keys are written in lexical order inside a transaction (EKV contract):

| Key | Value |
|---|---|
| `b` + page `u16` BE | Bitmap page, 4096 bytes, bit set = unit allocated |
| `m/units` | Allocated unit count, `u32` LE |
| `u` + episode `u64` BE | `state u8` (0 temp, 1 published), `len u64` LE, then unit indices `u32` LE |

A unit record holds at most `(4096 - 9) / 4 = 1021` units; an episode needing
more is refused (`InvalidInput` before any allocation when the expected length
is known, otherwise when the next unit would exceed it). Schema 1 catalogues are
rejected as `Schema` (none exist; no migration).

### Invariant

Every change to bitmap pages, `m/units` and `u` records happens in one EKV
transaction, so the bitmap always equals the union of unit lists and its
popcount equals `m/units`. At open, the bitmap pages are loaded into a fallible
PSRAM allocation and the popcount is checked. The cost is one EKV read per
32 GiB of card, independent of stored data. A mismatch is `Corrupt`: podcasts
go offline, logs continue, nothing is formatted.

### Operations

- `space()`: from in-RAM counters, no I/O. Free bytes are free units times
  1 MiB plus the unused tail of the active temporary episode's last unit.
- `reset_temp(id)`: frees any existing temp record for `id`, then starts an
  empty temp in RAM (no unit is allocated until the first byte).
- `append_temp(id, bytes)` (≤ 4096 bytes): when the current unit cannot hold
  the bytes, first commit one transaction allocating the next free unit
  (next-fit cursor) and appending it to the temp `u` record; only then write
  the data sectors. A chunk may span two units. Data writes are raw sector
  writes inside owned units; no catalogue write happens between unit
  boundaries (one catalogue commit per 256 chunks).
- `sync_temp(id)`: sector writes complete before their RPC replies and there
  is no write cache; returns the temp length held in RAM (one active temp).
- `publish_temp(id)`: one transaction sets the record state to published and
  stores the final length. Refuses if a published record already exists.
  `finish_download` then writes `a`/`e` as today.
- `unlink_temp(id)` / `unlink_audio(id)`: one transaction clears the record's
  bits, decrements `m/units` and deletes the record, only if the record has
  the matching state. Idempotent when absent.
- `audio_len(id)`: published record length. `list_ids`: paged range scan of
  the `u` prefix (`FILE_PAGE` ids, strictly after the cursor).
- `active(id)`: false, unchanged (playback is not implemented).

### Crash consistency

A unit is recorded as owned by a temp record before any data is written to it.
A crash can leave a partially written owned unit, never an unowned one. On
restart the existing `recover_one`/`recover_all` paths call `unlink_temp` for
non-Available episodes and return their units. A crash between
`publish_temp` and `finish_download` leaves the same window the LittleFS rename
leaves today and is handled by the same recovery.

### Interface change

The store needs the catalogue database, which the `Runtime` already owns next
to the media store. `MediaStore` becomes `MediaStore<F: Flash>` and each method
takes `cat: &mut Catalogue<F>`; `download`, `reclaim`, `recovery` and `runtime`
already hold the catalogue at every call. Unit-store persistence lives in a
catalogue submodule (`pub(crate)` methods on `Catalogue`) so the single EKV
writer is preserved. Raw data I/O goes through a small `UnitDevice` trait
(read/write sectors relative to the media region) implemented on `RegionIo`'s
media half. `LittlefsMedia` is deleted; the test media store is adapted.

## 3. Failure isolation

| Scope | Triggers | Effect |
|---|---|---|
| Bus (`SECTORS`, unchanged) | core0 SD error or timeout, failed sector RPC, caller-side cancellation (dropped `RequestGuard`, shutdown) | everything disabled until reboot |
| Logs | LittleFS error, or the owner seeing the deadline pass during a log request or log mount | `volume = None`; logs answer `Error::Offline`; podcasts continue |
| Podcasts | EKV or unit-store error, `Corrupt`, bitmap mismatch, or the owner seeing the deadline pass during a domain request or catalogue open | `podcasts = None`; domain answers `Offline`; logs continue |

- `CardIo::ensure_live` no longer disables the bus on deadline expiry; it
  returns an error that fails only the current domain. It runs between sector
  RPCs, when none is in flight, so no late sector reply can exist.
- The owner stops 20 ms before the request deadline (`OWNER_MARGIN`), so its
  error reply reaches the caller before the caller's own deadline fires and
  drops its guard. No deadline increases: the owner's effective I/O budget
  becomes 480 ms of the unchanged 500 ms; the margin covers one in-flight
  sector RPC (~2 ms measured) and the reply. A core0 overrun remains a bus
  failure. The mount uses the same margin within its 2 s.
- Mount order: descriptors (bus scope), log LittleFS mount and `gc()` (log
  scope), catalogue open and bitmap load (podcast scope). One scope failing
  leaves the other mounted.
- Caller-side cancellation stays global: without reply sequence numbers a late
  reply could be read by the next request. Sequence numbers are out of scope.
- The owner/scope decision is a pure function in `theo-storage` so it is
  host-tested; `sd_worker.rs` keeps only the ESP wiring.

## 4. Testing

Regression tests are tagged `// Regression (t-121): ...` in side-by-side
`*_tests.rs` files.

- `card_layout`: v2 round trip and canonical recomputation; v1, unknown
  version, conflicting copies rejected; regions disjoint; media 1 MiB aligned.
- Unit store, over an in-memory device and catalogue that count reads and
  writes:
  - `space()` and `append_temp` read/write counts are identical with 1, 4 and
    64 MiB already stored (the counterpart of the `fs_size()` growth test);
  - exactly one catalogue commit per allocated unit;
  - bitmap = counter = union of records after mixed allocate/free histories;
  - a cut after each step (allocation, data write, publish, unlink) followed by
    reopen and recovery leaks no unit;
  - bitmap/counter mismatch → `Corrupt`; >1021 units refused; full card →
    `WaitingForSpace`.
- LittleFS: log mount and append counts on the 16 MiB log region do not depend
  on media (separate regions).
- Existing `download`, `reclaim`, `recovery`, `runtime`, `worker` tests move to
  `MediaStore<F>`.
- Isolation: a domain overrun leaves logs usable and vice versa; caller-side
  cancellation still poisons everything; owner margin ordering.

## 5. Tooling and documentation

- `prepare_image` (`just prepare-image`) writes v2: catalogue schema 2 with an
  empty bitmap and `m/units = 0`, formatted log LittleFS, inode sync, then the
  two descriptors. Create-new file semantics unchanged. `just sd-provision`
  (legacy whole card) unchanged.
- ADR-0020 records layout v2, the unit store and failure isolation; it amends
  ADR-0015/0018 and closes ADR-0019's open limit. Update
  `crates/theo-storage/ARCHITECTURE.md` and podcast module docs.

## Out of scope

- Audio playback reads (`read_at`); the record format makes it one record read
  plus address arithmetic.
- Reply sequence numbers to isolate caller-side cancellation.
- Multi-sector transfers (t-122).
- Board validation, which needs an explicit request to flash.
