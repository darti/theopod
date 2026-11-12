# redb podcast catalogue (layout v3) — design

- Date: 2026-11-12
- Related: t-133 (follow-up task to create), ADR-0022, ADR-0011, ADR-0017,
  ADR-0019, ADR-0020, ADR-0021

## Problem

Podcasts must be available within a few seconds of boot. The ekv catalogue
cannot do that on the card: ekv's mount reads every page header to find the
newest meta page, so opening is O(pages). On the provisioned v2 card that is
about 73,872 sector reads at about 2 ms each (chip select goes through the
I2C expander, ADR-0019): minutes, against a 2 s mount budget. Reading headers
before payloads (tested) only shaves a constant factor.

## Spike results (host model, 2 ms per sector command)

A throwaway spike ran redb 4.4.0 over a sector-counting `StorageBackend`,
with 1500 B episode records; each commit held an episode, its identity key
and the counter. The xtensa `no_std` build of redb 4.4.0 also compiles.

| Measurement | Result |
|---|---|
| Open, clean or after a crash, `quick_repair` on | ~34 reads (~70 ms), independent of size |
| Open after a crash, `quick_repair` off (5000 episodes) | 42,439 reads (~85 s) |
| Commit with `quick_repair`, 256 KiB cache | ~106 writes, 2 syncs, ~7 reads (~226 ms) |
| Commit with a 64 KiB cache | ~572 ms (over the 500 ms `IO_BUDGET`) |
| First get + identity lookup | 48 reads |
| Range of 16 records | 80 reads |
| File size at 5000 episodes | 17.9 MiB |

Rejected alternatives: sequential-storage, tickv, simple-kvdb (NOR-oriented,
no ordered ranges or transactions); littlefs or FAT files (no multi-key
atomicity); tachyon-db, btree-store (std or file-only); sanakirja, bptree
(unproven).

## Scope and targets

- Maximum: 50 subscriptions, 5,000 episodes. Tests cover that scale.
- Database region: 64 MiB.
- Existing v2 cards are re-provisioned to layout v3; there is no migration.
  The current card's catalogue holds no real data.

## 1. Tables

| Table | Key → value | Replaces (ekv key) |
|---|---|---|
| `EPISODES` | `u64` → `&[u8]` episode record | `e`+id |
| `EPISODE_IDENTITY` | `(u64 subscription, u8 kind, [u8; 32] sha256)` → `u64` episode | `i`+… |
| `AUDIO` | `u64` episode → `u64` published length | `a`+id |
| `SUBSCRIPTIONS` | `u64` → `&[u8]` feed record | `f`+id |
| `SHOWS` | `[u8; 36]` show id → `u64` subscription | `u`+show |
| `SYNC` | `u64` subscription → `&[u8]` sync state | `s`+id |
| `UNIT_PAGES` | `u16` → `&[u8]` 4 KiB allocation bitmap | `b`+page |
| `UNIT_EXTENTS` | `u64` episode → `&[u8]` state, length, extents | `u`+id |
| `COUNTERS` | `&str` → `u64`: `next_episode`, `next_subscription`, `units_used`, `schema` | `m/*` |

- redb's ordered key types replace the hand-written big-endian `*_key()`
  builders; range order is unchanged.
- Separate tables remove today's shared `u` prefix between the show index
  (36-byte show id) and unit extents (8-byte episode id), which the units
  range scan only tells apart by key length.
- Value byte layouts and bounds (`MAX_RECORD_BYTES`, the `MAX_*_BYTES`
  limits) stay, so record parsers and their tests stay.
- `COUNTERS["schema"]` is 3. Any other value: podcasts offline, re-provision.
- Each operation atomic today (follow; insert episode with identity and
  counter; publish audio; unit allocation) is one `WriteTransaction` with
  `set_quick_repair(true)`.

## 2. Storage backend on core1

`theo_storage::redb_backend::RegionBackend<D: SectorIo>` implements redb's
`StorageBackend` over one card region. It is HAL-independent and host-tested.

- The region's first sector is a length record (magic, logical length,
  CRC). The redb file is the rest of the region.
- `len()` returns the logical length: 0 on a freshly provisioned region, so
  redb creates its database. `set_len()` writes the length sector
  synchronously before redb uses the new space; growing past the region
  returns a full error.
- redb pages are 4 KiB = 8 aligned sectors, written through directly. Only
  writes not aligned to sectors (redb's header) read-modify-write their edge
  sectors.
- `sync_data()` flushes the CardIo sector cache (ADR-0019); sector writes
  are already complete on the card.

`Send + Sync`: `StorageBackend` requires both and `RegionIo<CardIo>` holds
`Rc`. A `CoreLocal<D>` wrapper (`RefCell` inside, no lock) carries an
`unsafe impl Send + Sync` whose SAFETY argument is that the `Database` is
created, used and dropped only inside the storage owner on core1; the owner
(`PodcastOwner`) is itself `!Send`, so the compiler keeps the database there.
A re-entrant borrow panics.

redb's API is synchronous and `CardIo` is already a blocking `SectorIo`, so
no async adapter is needed.

- Opening (~70 ms) fits within `MOUNT_BUDGET`: podcasts come online in the
  normal mount. The uncommitted t-133 background-open work (`Reply::Opening`,
  `pending`/`opening` in the owner, `Error::Opening` in the network task) is
  dropped.
- A commit (~230 ms) blocks the core1 executor; the SD log task waits, its
  queue is bounded. CardIo's per-operation deadline still applies.
- redb's cache: 256 KiB from the PSRAM heap through `alloc`. The plan checks
  the heap configuration has room for it.
- redb's B-tree code runs synchronously on the owner's 48 KiB stack;
  recursion depth follows tree height (3–4 levels at 5,000 episodes). The
  high-water mark is measured on the board with the existing stack painting.

Retired: `PageStore`, `vendor/ekv` and its `THEO-PATCH.md`, the uncommitted
header-first `PageStore::load()` change, and the `ekv` workspace dependency.

## 3. API, modules and provisioning

- `Catalogue` becomes concrete: `Catalogue { db: redb::Database }`.
  `Error<E>` becomes `Error`; redb errors map to `Storage`, `Corrupt`,
  `Full`.
- Catalogue methods become synchronous `fn`s; each mutating method commits
  one write transaction. Range reads keep `MAX_SCAN_RECORDS` (16).
- `units`, `subscriptions`, `sync`, `recovery`, `reclaim`, `download`,
  `media`, `unit_media` and `runtime` lose `F: Flash`: `MediaStore<F>`
  becomes `MediaStore`, `Runtime<F, M>` becomes `Runtime<M>`. Functions stay
  `async` only where they await media unit IO or the network.
- `sync` commits each episode on its own (episode, identity and counter
  together): a page commit would exceed a request's 500 ms IO budget. An
  interrupted sync keeps the committed episodes and resumes from the last
  persisted `SYNC` state; re-upserting is idempotent through the identity
  table.
- One commit per podcast request (`PODCAST_BUDGET`, 1 s: 500 sector
  commands at 2 ms). `Catalogue::begin_batch`/`end_batch` hold several operations'
  writes in one transaction (reads see them; a fatal error drops them and
  disables the owner). `SyncChunk` returns `Consumed(n)`: a push stops after
  the first committing upsert, or after `MAX_CHUNK_ITEMS` (4) known
  episodes, and the network task resends the rest. `DownloadEnd` publishes
  and marks `Available` in one batch; `recover_one` unlinks both files and
  settles the state in one batch. `DownloadStart` and `DownloadChunk`
  answer `Pending` after one bounded step (this job's orphan unlinks, one
  reclaim step, or the start batch) and are resent unchanged.
- Reclaim reads the `EVICTABLE` index (key: no date last, date, import
  order = id; one entry per read `Available` episode, written in the
  episode's own transaction) instead of scanning: its first entry not
  leased is the policy's choice. Each step makes one commit (the
  `Deleting` intent, then unlink and `Reclaimed` in one batch); the
  runtime's `Reclaim` carries the intent between requests.

Layout v3:

- Descriptor version 3: 64 MiB database region, then the 16 MiB log
  LittleFS, then media units.
- `provision_v2` becomes `provision_v3`, in ADR-0021's order: zero
  sectors 0 and 8; format the log; write an empty length sector and create
  the database with every table and `COUNTERS["schema"] = 3` in one
  transaction; sync; write both descriptors last. The blank-card guarantee
  of an interrupted run is unchanged.
- The `just sd-provision-v2` recipe becomes `sd-provision-v3`, and the
  `provision_guard` token goes from `v2:` to `v3:`. The explicitly approved
  erase stays mandatory (ADR-0015).
- A v2 descriptor at boot: media stays mounted, podcasts stay offline, one
  log line says to re-provision.
- ADR-0021 is amended for v3. ADR-0022 records the redb decision and
  supersedes ADR-0011.

## 4. Errors, crashes and testing

Errors:

- A redb `StorageError` from CardIo (timeout, bus) → `Error::Storage`; the
  mount is disabled until reboot, as today.
- `DatabaseAlreadyOpen`, `UpgradeRequired`, a schema other than 3 or a bad
  length sector → `Error::Corrupt`: podcasts offline, a log line, nothing
  rewritten.
- Region full → `Error::Full`, already handled by download and sync.
- A redb panic aborts (`panic=abort`); the watchdog reboots and
  `quick_repair` recovers the last commit on the next open.

Crash consistency (host tests in `theo-storage`, using the spike's model of
a durable copy at each sync):

- A crash at every sector write of a commit opens to either the previous or
  the new state.
- Torn sectors (a sector write interrupted partway) at every sector of a
  commit, including the length sector.
- Opening after a crash costs at most 64 sector reads: a
  `// Regression (t-N)` guard on the spike's ~34.

Host tests:

- `redb_backend_tests.rs`: sector mapping, edge read-modify-write, growth
  through `set_len`, region-full errors.
- The catalogue, subscriptions, units, sync and other suites move from the
  ekv mock to `RegionBackend` over an in-memory disk, assertions unchanged.
- A scale test at 5,000 episodes and 50 subscriptions bounds open, a
  16-record range and a commit in sector counts, and each changed podcast
  request (sync chunk, `DownloadEnd`, `recover_one`, every `make_space`
  step) to 500 commands (`just test-catalogue-bounds`), warm and with a
  cache emptied by a reopen and a full scan (sync push, eviction).

Target checks: `just check`, `just lint`, `just build` on xtensa with redb
pinned `default-features = false`, `features = ["experimental-api-5"]`;
`just audit` for the new dependency; `just miri` for the `CoreLocal`
`unsafe`.

On the board, only on explicit request: `sd-provision-v3`, then flash and
boot; "podcasts online" within a few seconds; core1 stack high-water mark;
follow a feed and sync a page; power off during a sync, then reopen fast and
intact.

## Amendments (2026-11-12, while planning)

- **Heap order.** esp-radio forces esp-alloc's global allocator, so redb's
  allocations cannot be routed by type. The firmware registers PSRAM before
  internal RAM: global allocations land in PSRAM, while esp-radio and
  esp-rtos keep requesting `Internal` explicitly. A smaller redb cache was
  measured and rejected: 64 KiB costs ~163 writes (~346 ms) per commit,
  32 KiB ~220 writes (~460 ms), against ~106 (~226 ms) at 256 KiB; a PSRAM
  sector cache does not reduce writes.
- **Growth is not zero-filled.** redb grows its file by doubling (1, 2, 4,
  9, 18 MiB at 0.2k, 0.5k, 1k, 2.1k, 4.3k episodes). Zero-filling 9 MiB at
  2 ms per sector would block one commit for ~36 s. The spike ran 5,000
  inserts with growth filled with `0xA5`, reopened cleanly and after a
  crash, and read every record back. `set_len` therefore only rewrites the
  length record; a regression test keeps the fill non-zero.
- **Provisioning creates no tables.** redb's backend must be `'static`, but
  provisioning only borrows the card. `provision_v3` writes the length
  record with length 0; the first `Catalogue::open` of an empty, provisioned
  region creates the tables and counters in one transaction. A sector
  without the record's magic and CRC is `Corrupt`, so a blank or damaged
  region is never formatted at boot.
- **v2 cards.** A v2 descriptor is refused like any unknown version (no
  regions mount; one log line asks to re-provision). Keeping media mounted
  would need v2 geometry code, which the no-migration decision excludes.
- **Per-episode sync commits (2026-11-12).** a 50-episode page commit measured ~2,500 sector commands (~5 s) at 5,000 episodes, over the 500 ms IO_BUDGET of a podcast request; each episode commits on its own (~0.2 s); a first 5,000-episode sync takes ~17–20 min.
- **One commit per podcast request (final review).** One commit per
  podcast request; sync chunk resend; bounded reclaim cursor (see §3).
  Measured at 5,000 episodes: sync push 42 + 114, `DownloadEnd` 2 + 107,
  `recover_one` 2 + 106.
- **Eviction index (final review).** `EVICTABLE` (policy-ordered key →
  episode) is maintained in the transaction of every episode write, so
  reclaim reads its first entry instead of scanning 5,000 episodes; schema
  stays 3 (v3 not deployed). Eviction steps measured 26 + 98 and 26 + 138
  warm, 74 + 98 and 162 + 130 cold.
- **PODCAST_BUDGET 1 s (final review, user-approved).** A redb commit after
  a scan that emptied the page cache measured ~250 commands (e.g. 122 +
  130), at the old 500 ms bound; podcast requests get 1 s (500 commands),
  other requests keep theirs.
- **Open cost.** Catalogue::open measured 80 reads at scale (table checks); bound 96, raw redb open ≤64.
- **Fixed bug.** The ekv units range scan (`unit_ids`, prefix `u`) also
  reached the 37-byte show-index keys (`u` + show id) and returned `Corrupt`
  once fewer than 16 unit records remained after the cursor. Separate tables
  remove it; Task 4 adds a regression test.
- **Fresh flag (final review).** Cutting the first open before redb wrote
  its magic left "Not a redb database" (`Corrupt`) on a card the guard
  refused. `format` now writes a fresh record (magic `THEORDBF`); while
  fresh, `Catalogue::open` resets a non-empty file without redb's magic to
  length 0 and creates it again, then clears the flag (new record
  generation) once the tables are durable. A region that is not fresh is
  never reset.
- **Guard accepts a corrupt v3 catalogue (final review).** `sd-provision-v3`
  also authorizes the exact target card when its v3 layout is valid but its
  catalogue region is corrupt (no valid length record, or a non-empty file
  without redb's magic, read from the raw sectors); a healthy v3 card stays
  `AlreadyV3`.

## Risks

- redb's `no_std` mode is experimental (`experimental-api-5`); the version is
  pinned exactly and upgrades are deliberate.
- Torn-sector behaviour was not in the spike; the crash tests above decide
  it before any board run.
- Commits block core1 for ~230 ms each; a sync holds the SD
  log task back for the duration of each commit.
