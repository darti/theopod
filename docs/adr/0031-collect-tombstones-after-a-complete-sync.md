# ADR-0031: Collect tombstones after a complete sync

- Status: Accepted
- Date: 2026-11-27
- Related: t-188, ADR-0030 (tombstones), ADR-0022 (one commit per request),
  ADR-0007 (regression tests)

## Context

A pruned episode keeps its `EPISODE_IDENTITY` entry (a tombstone, ADR-0030)
so a sync does not import it again. Tombstones were never deleted, so they
grew with the feeds' total history, including episodes the feeds no longer
list. A tombstone is only useful while its feed can still list it.

## Decision

- **Seen set.** A sync session records every identity it upserts in a Bloom
  filter (`sync_seen.rs`): 8 KiB in the PSRAM `SyncSession`, 4 probes, about
  4% false positives at 10,000 identities. Positions are salted with the
  sync generation, so an identity kept by a false positive is not kept on
  every sync. There are no false negatives.
- **When.** Only a complete walk sweeps: after the last page of a 200, and
  before the checkpoint commit. A 304, a failed or abandoned sync never
  sweeps, since they did not see the whole feed. A walk that met no item
  (an empty or broken 200) or more than 10,000 (`SEEN_CAPACITY`, past
  which the filter keeps too much to be worth it) skips the sweep and only
  saves its checkpoint.
- **Order.** Sweep first, checkpoint last: an interrupted sweep (reboot,
  error, step cap) leaves the old checkpoint, so the next sync cannot be a
  304 and walks and sweeps again. Deletions already made persist.
- **What.** An identity of the synced subscription is deleted when its
  episode is gone (pruned) and the seen set rejects it. Identities with an
  episode, and those the feed still lists, stay.
- **Bounds.** `Catalogue::sweep_tombstones` examines at most 16 identities
  (`MAX_SCAN_RECORDS`) after a cursor, in key order, and deletes in one
  commit; a step that deletes nothing commits nothing. The runtime answers
  `SyncEnd` with `Reply::Pending` while steps remain, and the network task
  sends `SyncEnd` again, so each request stays at one commit; the
  checkpoint is its own last request. The scan uses a read transaction; a
  write transaction opens only to delete. The network task caps a sync at
  4,096 steps (64 Ki identities) and fails it past that (ADR-0009).
- **Alternatives rejected.** An exact seen set (sorted hashes) needs ~330 KB
  for 10,000 entries; a per-identity "last seen generation" doubles the
  write cost of every sync.

## Consequences

- Tombstones are bounded by what the feeds currently list (plus false
  positives, cleared on a later sync).
- An episode that left its feed and later comes back is a new episode with
  a new id.
- A sync of a subscription with N identities costs ceil(N / 16) + 1 extra
  requests before the checkpoint commit.
- The sweep cursor lives in RAM: a reboot mid-sweep restarts it on the next
  sync, which walks the feed again since no checkpoint was saved.
- A 200 that is truncated but still well-formed (the publisher trims its
  feed) forgets the tombstones of what it left out; an episode coming back
  later is imported again. A page that changes between the pages of one
  walk is treated the same way.
- The seen set makes `SyncSession` 8 KiB larger (about 28 KB). It is built
  in place in PSRAM; `just frame-check` bounds the stack.
- Each upserted item is hashed twice (upsert, then the seen set).
