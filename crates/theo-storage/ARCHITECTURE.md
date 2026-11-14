# theo-storage

HAL-independent SD storage for Theo Pod: bounded log buffering, the
LittleFS sector adapter, checked card regions and the EKV page store.
`no_std` + `alloc`, unit-tested on the host (`just test`). Dependencies:
`littlefs-rust` (vendored, ADR-0012), `ekv`, `embassy-*`, `allocator-api2`.
Used by the firmware (`src/device/sd_worker.rs`) and `theo-podcasts`.

## Contents

- `lib.rs`: `LogBuffer` (bounded queue, drop counting), `LogSink`,
  `rotation_needed` / `LOG_FILE_LIMIT`, `log_enabled` (`ESP_LOG` filter).
- `littlefs.rs`: `SectorStorage` (LittleFS `Storage` over 512-byte sectors,
  32-sector read cache, disable-on-failure), `Geometry`, `mount_for_writes`
  / `prepare_for_writes` (ADR-0019), `provision_once` (explicit format only),
  `append_log`, `log_tail`, `RequestGuard` (cancellation poisons a session).
- `card_layout.rs`, `regions.rs`: versioned region descriptors (sectors 0
  and 8) and `RegionIo`, one device split into database and media ranges;
  `discover` is for host tooling.
- `sector_bus.rs`: owned-buffer mailbox from the synchronous filesystem core
  to the async SD service; late replies after cancellation are refused.
- `page_store.rs`: copy-on-write EKV pages on raw sectors (ADR-0011/0015).
- `future.rs`: fallible PSRAM placement of large futures.
- `fat_page_file.rs` (`legacy-fat` feature): the former FAT page transport.

## Extension points

- Implement `littlefs::SectorIo` to back `SectorStorage` with a device. It
  must wait for completion, enforce its own deadline, and fail `check()`
  once the current operation may not proceed (cache hits rely on it).
- Pass an `allocator_api2::alloc::Allocator` to `SectorStorage::try_new_in`
  to choose where the read cache lives; `new` uses the global heap.

## Invariants

- Nothing formats except `provision_once` on a `Corrupt` mount.
- Any failed device call disables its adapter until reboot; the firmware
  then disables both regions (ADR-0018).
