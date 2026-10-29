# ADR-0015: Correct EKV publication and describe checked SD regions

- Status: Accepted local engine correction and descriptor API; runtime region provisioning/mount integration is not implemented.
- Date: 2026-10-29
- Related: t-105, t-106, t-116; refines ADR-0011/0012 and the recovery stop condition in ADR-0014.

## Context

The original t-106 PageStore experiment demonstrated loss of committed keys
when EKV 1.0.0 publishes a replacement metadata page header before committing
its metadata chunk. PageStore makes individual logical writes recoverable;
it cannot repair ordering across engine writes. The isolated correction and
fault evidence are retained under `docs/experiments/t116/`.

Subsequent model tests cover 192 committed/reopened mixed-history transactions,
24 episode keys, variable-length values, deletes and a persistent read/reclaimed
record. The history causes three metadata rollovers, 787 erases and page reuse.
The unpatched engine fails the complete-catalogue old/new assertion at the first
rollover. The correction passes 3238 logical before/after write/erase cuts at
six checkpoints and 2964 sector/sync fault cases at generation 96, in addition
to the earlier 8112 first-rollover sector/sync fault cases. A further regression
continues 12 recovered histories through twelve 4096-byte-value commits and
reopens them again. These are distinct, specified histories, not exhaustive
proofs over all transactions or physical SD fault behaviour.

Two read-only reviews found no blocking defect in the ordering correction or
model assertions. They emphasized that the eventual card adapter must provide
ordered durability and that recovery testing should include subsequent writes.

## Decision: local EKV correction

Keep EKV **1.0.0**, including its existing configuration and on-disk format.
Use a crates.io override to `vendor/ekv`, retaining the published source and
MIT/Apache-2.0 licenses. The only engine source changes are in `src/file.rs`:
commit metadata records before publishing the higher-sequence page header;
return rollover metadata I/O errors instead of unwrapping them. Provenance,
exact patch and removal criteria are in `vendor/ekv/THEO-PATCH.md`.

This is an explicit local correction, not a switch of engines or a claim that
upstream has accepted a fix. Remove it only when an upstream replacement passes
the retained recovery tests. Normal `just test` covers the short commit and
metadata-publication regression. `just test-storage-recovery` runs all long
injection histories in release mode without board access. Run that recipe when
changing EKV, PageStore, its format or a sector backend.

## Decision: checked layout description, without writing a card

`theo_storage::card_layout` defines immutable v1 descriptors in sectors 0 and 8.
The database begins at sector 16. Its extent is exactly PageStore's fixed size
`512 + pages * 2 * 4608` bytes, for 32–4096 logical pages. LittleFS starts at
the next 4096-byte boundary and receives the remaining complete 4096-byte
blocks. At least 32 MiB of media space is required; download reserve policy is
additional. Exact card size must match the descriptor. Addressing remains
bounded by the existing u32 sector service.

At 4096 pages the database has 16 MiB logical space, occupies 37,749,248 bytes
(73729 sectors), and media starts at sector 73752. The small alignment gap is
unallocated. Regions cannot overlap; every relative sector translation is
checked. The descriptor contains a version, sector size, exact capacities,
offsets, page count, zero reserved bytes and CRC. Decoding recomputes the entire
canonical layout rather than trusting supplied ranges.

Two matching valid copies select the layout. One torn/absent copy may be
recovered from a valid copy. Unknown versions, incompatible geometry and two
conflicting valid copies reject region mounting. With no descriptor, the only
permitted fallback is trying the existing whole-card LittleFS mount; absence
is never permission to format, migrate or initialize a database.

No runtime code reads or publishes these descriptors yet. Future explicit
provisioning must initialize and sync both regions before publishing either
immutable descriptor. It must require a separate destructive provisioning
request, preserve normal legacy boot, and never automatically carve sectors
out of a mounted whole-card filesystem. There is no in-place migration here.
The pure parser/geometry API is independently useful for implementing and
verifying that later provisioning path.

## Decision: explicit page-buffer allocation

PageStore accepts an allocator through `create_in`/`open_in`, while existing
`create`/`open` calls retain their global-allocator behaviour. Both buffers use
the selected allocator. Fallible exact reservation occurs before any storage
I/O; allocation failure returns `PageError::Allocation` without writes.
`buffer_bytes` reports reserved payload bytes, excluding allocator metadata.
This does not alter PageStore bytes, publication ordering or recovery policy.

For firmware, pass `&esp_alloc::ExternalMemory` to place the page and version
buffers explicitly in PSRAM. A reference is used because ExternalMemory itself
does not implement Clone. Both constructor paths compile for Xtensa in the
retained API probe. Do not rely on the current internal-first allocator spill.

## Resource assessment and remaining integration gate

Measured Xtensa layouts remain: Option<Version> 24 bytes; EKV plus a zero-size
file backend 10148 bytes; StackResources<4> 3120; TLS connection 480 excluding
borrowed buffers; verifier 4336. At 4096 pages PageStore buffer payload is
102400 bytes. The t-106 fixed transport configuration totals 49152 bytes,
including its network objects; do not double-count those objects.

Those two payload groups total **151552 bytes**, before the database object,
parser, queues, async futures/stacks, crypto scratch and allocator metadata.
They fit within the board's 8 MiB PSRAM capacity but are not a combined runtime
budget. The existing framebuffer consumes 368 * 448 * 2 = 329728 bytes in PSRAM.
No additional internal heap is provisioned by this change. StackResources and
any atomics must remain in internal memory: ESP32-S3 PSRAM does not support
ordinary atomic instructions, so blindly moving an entire service to PSRAM is
not an allocation strategy.

esp-radio beta.1's default controller uses 10 static RX, 32 dynamic RX and
32 dynamic TX buffers. Its own quick start provisions 64 KiB reclaimed plus
36 KiB ordinary internal heap. The current firmware supplies 64 KiB ordinary
internal heap, shared with existing work. Therefore network startup **must not
be enabled on the assumption that the default controller fits**. The integration
must size its controller explicitly, link the combined firmware, account for
internal allocations/stacks and retain measured margins. The existing API-only
probe does not prove any of these. A reclaimed-heap or allocation-policy change
needs its own linked verification, not an undocumented assumption.

The engine recovery blocker is corrected in source, with regression coverage.
The overall t-106 positive persistence/network gate remains incomplete until
single-owner region I/O, provisioning/publication, combined memory and latency,
trusted setup and bounded scheduling are implemented/validated. No podcast DB
is initialized, no networking is enabled, and no card or firmware was flashed.

## Limits

CRC is corruption detection, not cryptographic authenticity. Descriptor copies
are immutable and do not implement online repartitioning. Fault tests assume
failed writes do not damage other sectors and completed barriers order durable
writes; the controller's caches, arbitrary reordering and physical power loss
are outside that model. Read-state recovery and hardware durability must not
be inferred from compilation or from these selected histories alone.

## Verification

`just fmt`, `just check`, `just test` (374 passing, five explicit long-sweep
ignores), `just test-storage-recovery` (all seven matching tests passing,
including all five ignores), `just lint`, `just audit` (advisories/licenses/
sources) and `just build` pass. The explicit allocator constructors compile
with `&esp_alloc::ExternalMemory` on Xtensa. A separate final read-only review
found no actionable issue. No driver unsafe code changed, and no hardware
operation was performed. Tasks remain open until merge into main.
