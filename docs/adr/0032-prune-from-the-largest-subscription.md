# ADR-0032: Prune from the largest subscription

- Status: Accepted; amends ADR-0030
- Date: 2026-11-27
- Related: t-189, ADR-0030 (pruning), ADR-0022 (bounded requests),
  ADR-0021 (card provisioning), ADR-0007 (regression tests)

## Context

ADR-0030 prunes the oldest prunable episode across the whole catalogue. A
prolific feed therefore evicts the older episodes of quiet feeds: one feed
with thousands of episodes can empty a small one.

## Decision

- **Victim.** At the cap, creating an episode deletes the oldest prunable
  episode (no audio on the card, no unit record) of the *largest*
  subscription that has one. A feed that grows past the others pays for its
  own growth; a small feed loses episodes only once it is the largest.
- **Tables (schema 4).**
  - `PRUNABLE` ("subscription_prunable") is keyed `(subscription,
    eviction order)`, so a subscription's oldest prunable episode is one
    range seek.
  - `SUBSCRIPTION_EPISODES`: subscription → episode count.
  - `SUBSCRIPTION_SIZES`: `(count, subscription)` → (), so the largest is
    the last entry.
  All three are written in the transaction of the episode write that changes
  them (create, prune, state change), like `EVICTABLE`.
  All three are created with the catalogue but not opened by `validate`:
  each costs ~8 sector reads and boot open stays within its 96-read bound
  (t-133). A table lost to corruption is recreated empty by the next write.
  Counting is self-healing: a missing count reads as 0 and never goes below
  it, so a lost table only skews which subscription is pruned (`remove`
  re-checks every candidate against its stored episode) instead of failing
  every creation at the cap.
- **Bounds.** Pruning considers the 4 largest subscriptions (ties: highest
  id first), one range seek each, and at most 4 index entries of each: 16
  entries (`MAX_SCAN_RECORDS`) in all. The per-subscription share keeps one
  subscription's blocked entries (unit record) from filling the window.
  Stale or blocked entries are dropped as met (ADR-0030); when the 4 largest
  yield no victim, the oldest-imported fallback stays the last resort, and
  it is not fair.
- **Migration.** None. The schema becomes 4; a schema 3 catalogue is refused
  as `Schema` at boot and its card must be provisioned again (`sd-provision`,
  ADR-0021), which erases it. Exact counts from the first write were
  preferred over a resumable backfill.
- **Measured.** `just test-catalogue-bounds` passes unchanged (pruning at
  scale, sector budgets). The 2 MiB `full_region` test region now reaches
  `Full` later (about 11,300 episodes instead of under 10,000), so the 6,000
  cap of ADR-0030 keeps at least its margin.

## Alternatives rejected

- **Resumable backfill** of counts and index on schema 3 cards, in bounded
  steps: no erase, but much more code, and pruning stays unfair until it
  completes.
- **Lazy, approximate** tables filled only by new writes: counts undercount
  the existing episodes, so fairness arrives only as they age out.
- **Counts scanned, no size index**: one table less to keep in step, but
  finding the largest is a scan of every subscription, unbounded in the
  number of subscriptions.
- **Fair share** (`cap / subscriptions`, the creating feed pays while over
  it): needs the subscription count and gives the same result as "largest"
  in the cases that matter, with more rules.

## Consequences

- A small feed keeps its episodes while larger feeds exist. Sizes count all
  episodes, downloaded ones included, so the largest feeds converge towards
  equal sizes only as far as their episodes are prunable.
- Existing cards must be reprovisioned before this firmware boots with them;
  their catalogue and downloaded audio are lost.
- Each created episode writes two more small tables (count and size entry);
  each pruned one too.
- A subscription whose every episode has audio on the card is skipped; the
  next largest gives way.
