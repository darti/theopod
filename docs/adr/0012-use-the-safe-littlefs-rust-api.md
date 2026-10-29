# ADR-0012: Use the safe littlefs-rust API for SD files

- Status: Implemented; host/build validation passed, hardware validation pending.
- Date: 2026-10-29
- Supersedes: the FAT filesystem selection in ADR-0010 and the FAT backing-file direction in ADR-0011. The legacy FAT file adapter remains available behind theo-storage/legacy-fat for host tests.

## Context

Podcast audio and logs need an embedded filesystem designed to recover after
interrupted writes. The user requires a pure Rust implementation and selected
`littlefs-rust`, including its public safe wrapper from the upstream `main`
branch. This is a different package from `littlefs2-rust` and from the C-backed
`littlefs2` wrapper. The board's shared async I2C bus still controls SD chip
select, so a synchronous filesystem cannot simply block the existing executor.

## Decision

Use the public `littlefs-rust` API, rather than calling `littlefs-rust-core`
directly. The no_std firmware uses:

```toml
littlefs-rust = { git = "https://github.com/light-player/littlefs-rust", branch = "main", default-features = false, features = ["alloc"] }
```

Cargo.lock records upstream main at 732d038b684803edb713a0f87d00b6878674276a. Do not silently replace main with a fork. The repository
currently redirects to `PhotomancerArt/littlefs-rust`.

Formatting is an explicit initial provisioning operation. Subsequent starts
mount the existing filesystem. A mount error must never trigger automatic
formatting; the UI and serial logger must remain usable without the card.
Formatting the existing card and flashing the board require a separate user
request.

Retain the single filesystem owner, bounded request queues, shutdown budget
and failed/cancelled mount policy from ADR-0010. Core 1 runs the synchronous
LittleFS owner with a reserved 32 KiB internal stack and a separate executor.
Core 0 retains the non-Send async SD HAL and I2C handles. The filesystem proxy
sends owned sectors through a capacity-one mailbox to an async core-0 service.
A unique sector client serializes operations. No filesystem/driver references
or unsafe Send implementations cross cores. Core 1 can wait while core 0
continues touch/IMU I/O; it never blocks the executor that releases shared I2C.

Every request carries an absolute deadline. Cancellation/error permanently
poisons the session; a late response cannot permit reuse. The callback and
core-0 service check poison/deadline before I/O. Failed or timed-out HAL futures
drop the entire device. The UI-side wait remains bounded and no boot path calls
format. The host-only `just sd-format` command explicitly provisions an existing
unmounted device/image with exactly the same geometry.

Revisit EKV placement before database runtime integration: LittleFS copies a file's tail when
overwriting its beginning, making a large random-access database backing file
expensive. A separate raw sector region is a candidate, not an implemented
partition layout. The existing PageStore tests remain useful evidence for its
sector recovery contract.

## Evidence and limits

Safe API regressions reproduced aliasing errors on upstream main at
`732d038b684803edb713a0f87d00b6878674276a`, including normal file operations
after formatting. Formatting only once does not avoid those later errors.
[Upstream PR #12](https://github.com/PhotomancerArt/littlefs-rust/pull/12)
proposes the fixes. Its two regression scenarios pass Miri Stacked Borrows and
Tree Borrows, `just fci` passes, and all 22 C/Rust compatibility tests pass.
This covers those scenarios, not a complete safety audit. These fixes are not
yet part of the selected upstream main revision.

Earlier host experiments with the published 0.1.0 port verified large sequential
files and simulated interrupted renames. The new adapter's host tests cover whole-sector geometry, actual wrapper
format/mount/reopen, logs/rotation, torn sectors at every append write, failed
sync and cancelled/late cross-thread RPCs. These do not validate the physical
SD adapter. LittleFS recovery also depends on the SD device honoring
sector writes and durability barriers; physical power-loss testing remains
necessary.

## Consequences and alternatives

The public wrapper owns handles and buffers, while the implementation stays
pure Rust and no_std with allocation. LittleFS provides metadata recovery
machinery and a bounded allocator. The card will no longer mount as an ordinary
FAT volume on a desktop, and the second core is now reserved for synchronous filesystem work.

- Keep FAT32: already working and desktop-readable, but rejected for the new
  filesystem direction because interrupted metadata writes remain a concern.
- C-backed `littlefs2`: rejected by the pure Rust requirement.
- Independent `littlefs2-rust`: not selected; its allocation bitmap scales
  with card capacity and host experiments also found Miri errors.

Software verification: `just fmt`, `just check`, `just test` (363 tests),
`just lint` (all firmware variants and host crates), and `just build` pass.
`just audit` reports the pre-existing pulldown-cmark 0.13.4 advisory
RUSTSEC-2026-0414 via Slint; licenses and sources pass. Provisioning was exercised
only on a temporary 4 MiB image. The board has not been flashed or formatted.

Oku t-99 tracks this migration; implementation subtasks stay open until merge
into main. Board tests and the pending upstream aliasing fix remain separate
follow-ups.
