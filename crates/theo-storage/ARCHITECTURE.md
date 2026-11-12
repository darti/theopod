# theo-storage

HAL-independent SD storage for Theo Pod: bounded log buffering, the
LittleFS sector adapter, checked card regions and the redb storage backend.
`no_std` + `alloc`, unit-tested on the host (`just test`). Dependencies:
`littlefs-rust` (vendored, ADR-0012), `redb` (ADR-0022), `embassy-*`, `allocator-api2`.
Used by the firmware (`src/device/sd_worker.rs`) and `theo-podcasts`.

## Contents

- `lib.rs`: `LogBuffer` (bounded queue, drop counting), `LogSink`,
  `rotation_needed` / `LOG_FILE_LIMIT`, `log_enabled` (`ESP_LOG` filter).
- `littlefs.rs`: `SectorStorage` (LittleFS `Storage` over 512-byte sectors,
  32-sector read cache, disable-on-failure), `Geometry`, `mount_for_writes`
  / `prepare_for_writes` (ADR-0019), `provision_once` (explicit format only),
  `append_log`, `log_tail`, `RequestGuard` (cancellation poisons a session).
- `card_layout.rs`, `regions.rs`: versioned region descriptors (sectors 0
  and 8) and `RegionIo`, one device split into database, log and media
  ranges; `discover` is for host tooling. Layouts are created only by
  `theo_podcasts::provision::provision_v3` (ADR-0021/0022).
- `scope.rs`: failure scopes, the owner's `after_failure`, `OWNER_MARGIN_MS`.
- `sector_bus.rs`: owned-buffer mailbox from the synchronous filesystem core
  to the async SD service; late replies after cancellation are refused.
- `redb_backend.rs`: `RegionBackend`, redb's `StorageBackend` over the
  database region. Two alternating length-record sectors (generation, length,
  CRC) precede the file; growth is not zero-filled; `format` writes an empty
  record. `unsafe fn new` carries a single-execution-context contract (the
  backend claims `Send + Sync` for redb without being thread-safe).
- `test_disk.rs` (`test-disk` feature): sparse in-memory `SectorIo` with
  crash injection, shared by the storage and catalogue tests.
- `future.rs`: PSRAM placement of large futures; `run_in` polls one apart.

## Card layout v3

Only v3 descriptors are accepted: the redb database (a fixed 64 MiB,
`DATABASE_SECTORS`) from sector 16, a 16 MiB
log LittleFS (`LOG_SECTORS`), then raw 1 MiB media units (`UNIT_SECTORS`).
`regions::split` yields three disjoint `RegionIo` halves over one sector
client. Layout, unit store and failure scopes: ADR-0020.

## Extension points

- Implement `littlefs::SectorIo` to back `SectorStorage` with a device. It
  must wait for completion, enforce its own deadline, and fail `check()`
  once the current operation may not proceed (cache hits rely on it).
  Override `device_failed()` to return `false` for errors that leave the
  device usable (a passed soft deadline): shared regions then keep it.
- Pass an `allocator_api2::alloc::Allocator` to `SectorStorage::try_new_in`
  to choose where the read cache lives; `new` uses the global heap.

## Invariants

- Nothing in this crate formats except `provision_once` on a `Corrupt`
  mount; v3 layouts come only from explicit provisioning (ADR-0021).
- Any failed device call disables its adapter until reboot. `RegionIo`
  drops the shared device, disabling every region, only when
  `device_failed()`; other failures stay in their scope (`failure_scope`, `after_failure`).
