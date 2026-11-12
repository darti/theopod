# ADR-0022: Store the podcast catalogue in redb

- Status: Accepted
- Date: 2026-11-12
- Related: t-133; supersedes ADR-0011; amends ADR-0017, ADR-0021
- Spec: `docs/superpowers/specs/2026-11-12-redb-catalogue-design.md`

## Context

Podcasts must be available within a few seconds of boot. The catalogue lives
in ekv over `PageStore` (ADR-0011), and ekv's mount reads every page header
to find the newest meta page. On the provisioned card that is about 73,872
sector reads at about 2 ms each (ADR-0019): minutes, against a 2 s mount
budget. A cheaper page load and a background open were tried; neither brings
boot to seconds, because the open stays O(pages).

A host spike over a sector-counting backend measured redb 4.4.0: with
`quick_repair` on every commit, opening costs about 34 sector reads
(~70 ms), clean or after a crash, whatever the database size; a commit costs
about 230 ms with a 256 KiB cache. redb 4.4.0 builds `no_std` for
`xtensa-esp32s3-none-elf`. The `no_std` embedded key-value crates surveyed
(sequential-storage, tickv, simple-kvdb) target NOR flash and lack ordered
ranges or multi-key transactions; file-per-key designs lack atomicity.

## Decision

- The podcast catalogue is a redb database, one typed table per concern
  (episodes, identity, audio, subscriptions, shows, sync, unit pages, unit
  extents, counters). Every write transaction sets `quick_repair`.
- `theo_storage::redb_backend::RegionBackend` implements redb's
  `StorageBackend` over a card region: a length sector, then the redb file
  in 4 KiB pages aligned to sectors. It is HAL-independent.
- The database runs synchronously inside the core1 storage owner, wrapped in
  a `CoreLocal` type whose `Send + Sync` rests on never leaving that `!Send`
  owner. The redb cache is 256 KiB in PSRAM.
- Layout v3 replaces v2: a 64 MiB database region, then the log LittleFS and
  media units. Cards are re-provisioned with `just sd-provision-v3` after an
  explicitly approved erase; there is no migration, and a v2 card keeps
  podcasts offline.
- The scale target is 50 subscriptions and 5,000 episodes. Feed sync commits
  one feed page per transaction.
- `PageStore`, `vendor/ekv` and the `ekv` dependency are removed.

## Consequences

- Opening the catalogue fits in the normal mount, so the background-open
  machinery is unnecessary.
- Each commit blocks the core1 executor for about 230 ms; the SD log queue
  absorbs it.
- The catalogue depends on redb's experimental `no_std` API, pinned to an
  exact version.
- Crash safety now rests on redb's two-phase commit over our backend; host
  tests must cover crashes and torn sectors at every write of a commit
  before the board run.
- The ekv-specific ADRs (0011, the ekv parts of 0015 and 0017) describe
  history only.

## Amendments

- Heap order: PSRAM is registered before internal RAM; esp-radio and esp-rtos request `Internal` explicitly.
- Growth is not zero-filled: `set_len` rewrites only the length record (spike: 5,000 inserts with `0xA5` fill).
- Two length sectors: `set_len` writes the next generation into the copy not holding the current one, so a torn record write reopens with the old or the new length instead of `Corrupt`; the redb file starts at region sector 2.
- Provisioning creates no tables: the first `Catalogue::open` of an empty provisioned region creates them.
- v2 cards are refused like any unknown version; one log line asks to re-provision.
- Per-episode sync commits (2026-11-12): a 50-episode page commit measured ~2,500 sector commands (~5 s) at 5,000 episodes, over the 500 ms IO_BUDGET of a podcast request; each episode commits on its own (~0.2 s); a first 5,000-episode sync takes ~17–20 min.
- Catalogue::open measured 80 reads at scale (table checks); bound 96, raw redb open ≤64.
- Fixed bug: the ekv `unit_ids` range scan returned `Corrupt` on show-index keys; separate tables remove it.
- Fresh flag: `format` writes the length record with a distinct magic (`THEORDBF`, CRC-covered); while fresh, `Catalogue::open` resets a non-empty redb file without redb's magic (a cut first open) to length 0 and creates it again; the flag is cleared by a new record generation once the tables are durable, and a region that is not fresh is never reset (ADR-0015).
- Provisioning guard: `sd-provision-v3` (exact card id and size) also accepts a valid v3 card whose catalogue region is corrupt (no valid length record, or a non-empty file without redb's magic, checked on the raw sectors); a healthy v3 card stays `AlreadyV3`.
