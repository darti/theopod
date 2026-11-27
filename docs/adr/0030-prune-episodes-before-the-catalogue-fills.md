# ADR-0030: Prune episodes before the catalogue fills

- Status: Accepted
- Date: 2026-11-27
- Related: t-148, ADR-0022 (Amendments, "Real capacity"), ADR-0007
  (regression tests)

## Context

redb doubles its file when it grows. Near ~8,600 episodes a 72 MiB growth
request exceeds the 64 MiB catalogue region, the backend refuses it, and the
catalogue reports `Error::Full`, which disables podcasts until reboot. Feeds
have long histories (sync walks every page), so a few subscriptions can reach
that count.

## Decision

- **Cap.** The catalogue keeps at most `MAX_EPISODES` = 6,000 episodes
  (`catalogue_prune.rs`), about 2,600 below the failure point, so even
  worst-case ~4 KiB records fit the 32 MiB file that precedes the doubling.
- **Prune on creation.** Creating an episode while the cap is reached first
  deletes the oldest-published prunable episode, in the same write
  transaction (still one commit per request; measured 252 sector commands
  against the 500 budget). Undated episodes go last, ties by id, the order
  of `EVICTABLE`.
- **Prunable** means audio `Missing`, `Queued` or `Reclaimed` and no unit
  record. `Downloading`, `Available` and `Deleting` episodes are never
  pruned, so downloaded or in-progress audio is safe. If nothing is
  prunable the creation goes one over; the next one tries again, and `Full`
  remains the last resort.
- **Index.** `PRUNABLE` (same key as `EVICTABLE`) is maintained by `reindex`
  in the transaction of every episode write, so pruning reads at most 16
  index entries and never scans episodes. The table is created on first
  write; a database without it stays valid (schema stays 3). Episodes written
  before the table existed are not indexed. When the index yields no victim
  (empty, or its first 16 entries are stale or hold a unit record, which are
  dropped as met), pruning falls back to the 16 lowest ids, the oldest
  imported, and deletes the first prunable one. The record is encoded before
  anything is pruned, so a creation that fails deletes nothing.
- **Tombstone.** The identity entry (`EPISODE_IDENTITY`) outlives the
  episode. A sync meeting a pruned episode finds it known and returns
  `created: false`, `queued: false`, so re-syncing a history longer than the
  cap neither re-imports nor churns. Tombstones cost ~50 bytes each; those of
  episodes that left their feed are collected (ADR-0031).

## Consequences

- The unread backlog is bounded: the oldest unread episodes without audio
  disappear from the catalogue once more than 6,000 exist.
- Pruning is global and oldest-first across subscriptions, and a pruned
  episode is not imported again; a quiet feed can lose old unread episodes to
  a prolific one. Per-subscription fairness is an accepted limit (follow-up);
  tombstone collection is ADR-0031.
- Ids are never reused (`next_episode` only grows).
- An identity whose episode is missing is no longer reported as `Corrupt`;
  it is read as pruned.
- `just test-catalogue-bounds` includes `prune_at_scale_*`: 10,500 episodes
  synced (ordinary and worst-case records) never report `Full`.
