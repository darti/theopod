# ADR-0019: Prepare LittleFS at mount and cache sector reads

- Status: Implemented; board-verified on a log-only volume (t-103).
- Date: 2026-11-11 (decisions made 2026-11-01 and 2026-11-02)
- Related: t-103; amends ADR-0012; budgets from ADR-0018.

## Context

On the board, the first log append after boot exceeded its 500 ms budget and
disabled storage until reboot. Two costs combined:

- LittleFS defers its consistency pass and first free-block scan to the first
  allocation, so the first append paid for them.
- Each LittleFS access is amplified into repeated single-sector RPCs. One
  failed append made 169 read RPCs for only 23 distinct sectors (one sector 20
  times). Every RPC also pays for SD chip-select changes over the shared I2C
  expander, about 2 ms per sector.

## Decision

1. **Prepare at mount.** `littlefs::mount_for_writes` mounts and then calls
   `prepare_for_writes` (`Filesystem::gc()`) before the volume is published.
   The work is charged to the existing 2 s mount request. The explicit
   provisioning build prepares its volume through the same helper. Preparation
   may write (orphan and pending-move repair); it never formats, and its
   failure disables the session like any other storage error.
2. **Cache sector reads.** `SectorStorage` keeps a 32-sector read-through LRU
   cache (`READ_CACHE_BYTES`, 16,896 bytes), reserved fallibly before any
   device I/O. The board places it in PSRAM; host tools use the global heap.
   - Writes and erases invalidate their whole range before the first device
     call. Write data never fills the cache, so LittleFS's post-write readback
     always reaches the medium.
   - Only successful physical reads populate entries.
   - Any failed device call disables the adapter, cache hits included.
   - A cache hit first calls `SectorIo::check`, which on the board applies the
     request's absolute deadline and the session's disable flag. Cached work
     is therefore bounded by the same deadline as physical I/O.
3. No deadline is increased: 500 ms per foreground request, 1.2 s shutdown and
   2 s mount stay as they are.

## Consequences

- Board samples on the existing log-only volume: mount 57.8 ms, first appends
  47–176 ms (13 physical reads). These are samples, not bounds
  (`docs/sd-validation.md`).
- On the host fixture, the first append after preparation needs 18 sector
  reads instead of 50.
- **Open limit: preparation grows with stored data.** The free-block scan reads
  every file's block list, about one sector per two data blocks (516 reads for
  4 MiB of media on the host). At about 2 ms per read, one hour-long episode
  (28–60 MB) costs roughly 7–15 s, beyond both the 2 s mount and the 500 ms
  append budgets. The scan recurs whenever the 4 MiB lookahead window runs
  out, for instance during downloads. This predates the decision: before it,
  the same scan landed on the first append, and per ADR-0018 any failed
  storage request disables both regions, podcasts included. Moving it to mount
  gives it the larger budget, not a bound. Large libraries need a follow-up
  that bounds log-allocation cost independently of media size.
  Resolved by ADR-0020 for region-layout cards.
- A cached sector stays cached for the life of the mount. A swapped card
  would be served stale sectors until the first miss or I/O failure. Card
  swap is unsupported; if it is added, the swap must invalidate the whole
  cache.
- A read that returned wrong bytes but reported success stays cached until it
  is evicted or overwritten, where an uncached adapter would reread it.
  LittleFS's metadata CRCs still detect it; nothing formats.
- Cache hits read at most 512 bytes from PSRAM while the display DMA streams
  the PSRAM framebuffer (ADR-0004). Not observed to matter; watch for panel
  bands if the cache grows.

## Alternatives

- Keep the mount plain and let the first append pay the scan: this is what
  failed on the board.
- Make preparation failure non-fatal for reads: not possible without
  weakening the poison-on-failure rule, since a scan that overruns its
  deadline leaves the session's I/O state unknown.
- Multi-sector transfers (CMD18/CMD25) would cut per-sector chip-select cost
  for misses, writes and the scan. They are a larger worker and mailbox change,
  left as a follow-up.
- A separate small log volume would make log allocation independent of media
  size. It needs a card layout change, left as a follow-up.
