# ADR-0020: Store media in raw allocation units and isolate failures per scope

- Status: Implemented in software; hardware validation pending.
- Date: 2026-11-11
- Related: t-121; amends ADR-0015 and ADR-0018; resolves the open limit of ADR-0019.

## Context

Logs and podcast media shared one LittleFS volume. LittleFS keeps no
persistent free map, so every lookahead refill, the mount-time `gc()` and
`Filesystem::fs_size()` read every file's block list. Measured on the 8 MiB
host volume:

- `fs_size()` costs 137, 265 and 522 sector reads with 1, 2 and 4 MiB stored,
  unchanged on a second call.
- Preparation costs about 516 extra reads per 4 MiB of media.
- A board sector read costs about 2 ms.
- `LittlefsMedia::space()` called `fs_size()` for every download chunk, so past
  about 2 MB each chunk overran its 500 ms budget. Per ADR-0018 that disabled
  logs and podcasts together until reboot.

No region-layout (v1) card exists, so the layout may change without migration.

## Decision

1. **Card layout v2.** After the descriptors (sectors 0 and 8, `THEOSD02`,
   CRC; sectors 0..16 reserved), the EKV catalogue starts at sector 16,
   followed by a 16 MiB log region (`LOG_SECTORS`, LittleFS holding only logs)
   and a media region of raw 1 MiB allocation units (`UNIT_SECTORS` = 2,048,
   no filesystem, minimum 32 MiB). The log region starts on a 4096-byte
   boundary and the media region on a 1 MiB boundary. `CardLayout::select`
   accepts only v2; v1 and unknown versions are rejected. `regions::split`
   returns three disjoint `RegionIo` halves over one sector client. Without a
   descriptor the legacy whole-card log mount is unchanged. Boot never formats.
2. **Unit store.** The catalogue (schema 2) holds `b` + page `u16` BE (4096-byte
   bitmap page, bit set = allocated), `m/units` (`u32` LE count) and
   `u` + episode `u64` BE (state, length, unit indices). Invariant: bitmap
   pages, `m/units` and `u` records change only in one EKV transaction, so the
   bitmap equals the union of the unit lists and its popcount equals
   `m/units`; open checks the popcount and a mismatch is `Corrupt`.
   - `space()` reads in-RAM counters only.
   - `append_temp` commits one catalogue transaction per newly allocated unit,
     before writing any data sector to it; data sectors are raw writes.
     A crash can leave a partially written owned unit, never an unowned one;
     existing recovery returns it through `unlink_temp`.
   - `publish_temp` and unlink are single transactions.
   - `MediaStore<F>` takes the catalogue; `LittlefsMedia` is removed.
3. **Failure scopes** (`theo_storage::scope`). A bus fault (card or HAL error,
   aborted in-flight sector RPC) or caller-side cancellation disables every
   scope. A LittleFS error, or a request reaching the owner after its soft
   deadline, disables only logs; an EKV or unit-store error, `Corrupt`, or a
   late podcast request disables only podcasts. A `before_io` hook failure
   stays global. An offline log scope answers `Offline` without I/O and warns
   once per boot.
4. **Owner margin.** The owner stops `OWNER_MARGIN_MS` = 20 ms before each
   request deadline so its error reply precedes the caller's own deadline. The
   500 ms, 1.2 s and 2 s budgets are unchanged.

## Consequences

- Log mount and append cost no longer depend on media, and download chunks
  cost a bounded number of operations. The bound test (`unit_media_tests.rs`)
  measures (catalogue commits, device reads, device writes) = (1, 0, 16) for
  two 4 KiB appends with 1, 4 and 64 MiB already stored. It counts catalogue
  commits, not raw EKV sector writes (EKV's merge schedule depends on commit
  history, not stored media), and does not yet count catalogue reads.
- The crash-cut test samples cut points (operation boundaries, gap midpoints,
  every 97th write) rather than every write index.
- Tail waste averages 0.5 MiB per episode. An episode is capped at 1,021 units
  (about 1 GiB); a larger one is refused.
- Caller-side cancellation stays global: without reply sequence numbers a late
  reply could be read by the next request.
- Known limit: when the last live scope goes offline, the shared sector client
  is dropped and the bus poisoned, so later requests answer an I/O error
  instead of `Offline`. The end state, everything offline, is the same.
- The owner-loop wiring and the 20 ms margin are compile-verified only. Board
  validation needs an explicit request.
- On-device v2 provisioning is tracked separately (Oku t-133).
- Out of scope: playback reads (`read_at`), reply sequence numbers, and
  multi-sector transfers (t-122).

## Alternatives

- A separate log region only, media still on LittleFS: fixes log cost but not
  `space()` or the free-block scan on media.
- Tune the shared LittleFS (cache `fs_size`, larger lookahead): the scan still
  grows with stored data and a failure still affects both users.
- `ekv-fs` 0.2 or media in EKV: whole-file writes from RAM with no append or
  seek, a 65,536-page cap, and LSM compaction that would copy audio payloads.
  Its metadata-plus-chunks idea is kept, with chunks on raw sectors.

## Verification

Commands: `just fmt`, `just check`, `just test`, `just lint`, `just build`.

Results: recorded at merge (Task 8).
