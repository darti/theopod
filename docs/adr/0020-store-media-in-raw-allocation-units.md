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
   `u` + episode `u64` BE: `state u8` (0 temp, 1 published), `len u64` LE,
   then extents `(start u32 LE, count u32 LE)`, at most 510 per record.
   Invariant: bitmap pages, `m/units` and `u` records change only in one EKV
   transaction, so the bitmap equals the union of the records' extents and its
   popcount equals `m/units`; open checks the popcount and a mismatch is
   `Corrupt`.
   - `space()` reads in-RAM counters only.
   - `reset_temp(id, expected)` reserves `ceil(expected / 1 MiB)` units up
     front in one transaction when the length is known, so streaming it makes
     no catalogue commit. With an unknown length, `append_temp` owns a batch of
     units (as many as already owned, 1 to 64, next-fit with a byte-wise
     bitmap scan) in one transaction before writing any data sector to them.
     Adjacent units merge into one extent, so a record's size depends on
     fragmentation, not on episode length. A crash can leave a partially
     written owned unit, never an unowned one; existing recovery returns it
     through `unlink_temp`.
   - `publish_temp` releases the owned units past `ceil(len / 1 MiB)` in the
     transaction that publishes the record; unlink is one transaction.
   - Capacity cap: the download layer refuses an episode larger than the media
     region before any eviction or allocation (known length at start, an
     announced length at the headers, a growing body before the append) and
     sets it `Missing`, so it is not retried.
   - Error policy: input and state errors (`Invalid`, including a growth that
     would need a 511th extent, `Exists`, `Full`) are detected before any
     change and leave the store enabled. Catalogue, device and allocation
     errors disable it until reopen, since RAM may no longer match the card.
   - `MediaStore<F>` takes the catalogue; `LittlefsMedia` is removed. Raw data
     I/O uses the existing `theo_storage::littlefs::SectorIo` on the media
     `RegionIo`, not the `UnitDevice` trait the spec first proposed.
   - Trust assumption: open checks the bitmap against the counter, not against
     the records; records are trusted to match the bitmap (unlink refuses an
     extent whose units are not allocated as `Corrupt`). Verifying records
     against a shadow bitmap at open is tracked in t-135.
   - Playback: `active()` is always false. Playback must not read units
     (`read_at`) before leases exist, or reclaim could free units being read.
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

- Log mount and append cost no longer depend on media (`regions_tests.rs`
  compares log mount plus one append with and without media written), and
  download chunks cost a bounded number of operations. The bound test
  (`unit_media_tests.rs`) stores 1, 16 and 64 episodes on a two-page bitmap;
  after the one reservation commit, `space()` and two 4 KiB appends cost no
  catalogue read, write or commit and 16 device writes in every case.
- The crash-cut test samples cut points (operation boundaries, gap midpoints,
  every 97th write) rather than every write index, and checks after each cut
  that the bitmap equals the union of the records.
- Tail waste averages 0.5 MiB per episode and is trimmed at publish. The only
  episode cap is the card capacity.
- A v1 (`THEOSD01`) card is rejected as a whole: the mount refuses it, so its
  logs go offline too, not just podcasts. No such card exists.
- Caller-side cancellation stays global: without reply sequence numbers a late
  reply could be read by the next request.
- Known limit: when the last live scope goes offline, the shared sector client
  is dropped and the bus poisoned, so later requests answer an I/O error
  instead of `Offline`. The end state, everything offline, is the same.
- The owner-loop wiring and the 20 ms margin are compile-verified only (a
  const assert ties the margin to the 500 ms and 2 s budgets; the owner's
  transition is the host-tested `scope::after_failure`). Board validation
  needs an explicit request.
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

Results (2026-11-11, host): `just fmt` clean; `just check`, `just lint` and
`RUSTFLAGS="-C link-arg=-Tlinkall.x" just build` pass; `just test` passes
(theo-podcasts 109, 1 ignored; theo-storage 80, 5 ignored; drivers 308;
frame-dump 22; frame-png 3; prepare_image 3). The unit-store bound test measures zero
catalogue commits, reads and writes for `space()` and appends inside reserved
units with 1, 16 and 64 stored episodes. Not yet validated on the board.
