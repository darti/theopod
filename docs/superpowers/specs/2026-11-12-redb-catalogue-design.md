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
- `sync` commits one feed page per transaction (its episodes, identities and
  counter together). Per-episode commits would take ~19 minutes for a
  5,000-episode first sync. An interrupted sync resumes after the last
  committed page from `SYNC` state.

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
  16-record range and a commit in sector counts.

Target checks: `just check`, `just lint`, `just build` on xtensa with redb
pinned `default-features = false`, `features = ["experimental-api-5"]`;
`just audit` for the new dependency; `just miri` for the `CoreLocal`
`unsafe`.

On the board, only on explicit request: `sd-provision-v3`, then flash and
boot; "podcasts online" within a few seconds; core1 stack high-water mark;
follow a feed and sync a page; power off during a sync, then reopen fast and
intact.

## Risks

- redb's `no_std` mode is experimental (`experimental-api-5`); the version is
  pinned exactly and upgrades are deliberate.
- Torn-sector behaviour was not in the spike; the crash tests above decide
  it before any board run.
- Commits block core1 for ~230 ms each; a sync of many pages holds the SD
  log task back for the duration of each commit.
