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
  and 8) and `RegionIo`, one device split into database, log and media
  ranges; `discover` is for host tooling.
- `scope.rs`: `failure_scope`, which scope (bus, logs, podcasts) a failed
  request disables, and `OWNER_MARGIN_MS`.
- `sector_bus.rs`: owned-buffer mailbox from the synchronous filesystem core
  to the async SD service; late replies after cancellation are refused.
- `page_store.rs`: copy-on-write EKV pages on raw sectors (ADR-0011/0015).
- `future.rs`: fallible PSRAM placement of large futures.
- `fat_page_file.rs` (`legacy-fat` feature): the former FAT page transport.

## Card layout v2

Only v2 descriptors (`THEOSD02`, sectors 0 and 8) are accepted; v1 is
rejected. After the descriptors (sectors 0..16): the EKV database from sector
16, a 16 MiB log region (`LOG_SECTORS`, LittleFS holding only logs), then the
media region from the next 1 MiB boundary (`UNIT_SECTORS` = 2,048 sectors per
raw allocation unit, no filesystem; minimum 32 MiB). Log work therefore does
not scale with stored media. `regions::split` yields three disjoint
`RegionIo` halves over one shared sector client. See ADR-0020.

## Extension points

- Implement `littlefs::SectorIo` to back `SectorStorage` with a device. It
  must wait for completion, enforce its own deadline, and fail `check()`
  once the current operation may not proceed (cache hits rely on it).
  Override `device_failed()` to return `false` for errors that leave the
  device usable (a passed soft deadline): shared regions then keep it.
- Pass an `allocator_api2::alloc::Allocator` to `SectorStorage::try_new_in`
  to choose where the read cache lives; `new` uses the global heap.

## Invariants

- Nothing formats except `provision_once` on a `Corrupt` mount.
- Any failed device call disables its adapter until reboot. `RegionIo`
  drops the shared device, disabling every region, only when
  `device_failed()`; otherwise the failure stays in its scope
  (`failure_scope`, t-121). Cancelling an in-flight `AsyncRegion` sector
  still disables every region.
- Bus faults (card or HAL error, aborted in-flight sector RPC, caller-side
  cancellation) disable every scope; a missed soft deadline between sector
  RPCs and logical errors (LittleFS, EKV, unit store) disable one scope.
  When the last live scope goes offline the shared client is dropped, so
  later requests may report an I/O error instead of Offline.
