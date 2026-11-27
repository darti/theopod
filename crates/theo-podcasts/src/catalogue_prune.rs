//! Episode pruning: the catalogue never grows into `Error::Full` (ADR-0030).
//!
//! redb doubles its file when it grows, and a 72 MiB request near 8,600
//! episodes exceeds the 64 MiB region. Creating an episode while
//! [`MAX_EPISODES`] exist therefore first deletes, in the same transaction,
//! the oldest episode without audio on the card of the largest subscription
//! (ADR-0032), so one large feed cannot evict a small one. The identity
//! entry stays as a tombstone, so a later sync of the same feed history
//! finds the episode "known" and does not import it again.
use super::{
    Episode, EpisodeId, EpisodeState, Error, MAX_SCAN_RECORDS, SubscriptionId, UpsertOutcome,
    db_error, decode, evict_key,
};
use crate::{
    catalogue::AudioState,
    tables::{EPISODES, PRUNABLE, SUBSCRIPTION_EPISODES, SUBSCRIPTION_SIZES, UNIT_EXTENTS},
};
use core::ops::Bound;
use redb::{ReadableTable, ReadableTableMetadata, WriteTransaction};

/// A `PRUNABLE` key: subscription, then the eviction order.
type PruneKey = (u64, u8, i64, u64);

fn prune_key(subscription: SubscriptionId, id: EpisodeId, published: Option<i64>) -> PruneKey {
    let (undated, date, id) = evict_key(id, published);
    (subscription.0, undated, date, id)
}

/// Most episodes the catalogue keeps. About 2,600 below the measured failure
/// point (about 8,600 episodes of 1 KiB), so worst-case 4 KiB records still
/// fit the 32 MiB file that precedes the doubling.
pub const MAX_EPISODES: u64 = 6_000;

/// Prune index entries examined per new episode.
const WINDOW: usize = MAX_SCAN_RECORDS;

/// Whether an episode may be pruned in `state`: no audio on the card, none
/// being fetched or deleted.
pub(crate) fn prunable(state: EpisodeState) -> bool {
    matches!(
        state.audio,
        AudioState::Missing | AudioState::Queued | AudioState::Reclaimed
    )
}

/// The outcome reported for an episode that was pruned earlier.
pub(super) fn pruned(id: EpisodeId) -> UpsertOutcome {
    UpsertOutcome {
        id,
        state: EpisodeState {
            read: true,
            audio: AudioState::Reclaimed,
        },
        created: false,
        queued: false,
    }
}

/// Keep `PRUNABLE` in step with an episode write, in its transaction.
pub(super) fn reindex(
    tx: &WriteTransaction,
    subscription: SubscriptionId,
    id: EpisodeId,
    old: Option<(EpisodeState, Option<i64>)>,
    (state, published): (EpisodeState, Option<i64>),
) -> Result<(), Error> {
    let old = old
        .filter(|&(s, _)| prunable(s))
        .map(|(_, p)| prune_key(subscription, id, p));
    let new = prunable(state).then(|| prune_key(subscription, id, published));
    if old.is_none() && new.is_none() {
        return Ok(());
    }
    let mut table = tx.open_table(PRUNABLE).map_err(db_error)?;
    if let Some(key) = old.filter(|k| Some(*k) != new) {
        table.remove(key).map_err(db_error)?;
    }
    if let Some(key) = new {
        table.insert(key, ()).map_err(db_error)?;
    }
    Ok(())
}

/// Count a created episode of `subscription` in the size tables.
pub(super) fn count_created(
    tx: &WriteTransaction,
    subscription: SubscriptionId,
) -> Result<(), Error> {
    count(tx, subscription, true)
}

/// Count a pruned episode of `subscription` out of the size tables.
fn count_pruned(tx: &WriteTransaction, subscription: SubscriptionId) -> Result<(), Error> {
    count(tx, subscription, false)
}

/// Keep `SUBSCRIPTION_EPISODES` equal to the episodes of each subscription,
/// and `SUBSCRIPTION_SIZES` mirroring it.
///
/// Self-healing (ADR-0032): a missing count row is read as 0 and is not
/// decremented below it, so a lost table degrades fairness instead of
/// failing every prune at the cap.
fn count(tx: &WriteTransaction, subscription: SubscriptionId, add: bool) -> Result<(), Error> {
    let sub = subscription.0;
    let mut counts = tx.open_table(SUBSCRIPTION_EPISODES).map_err(db_error)?;
    let old = counts.get(sub).map_err(db_error)?.map_or(0, |v| v.value());
    let new = if add {
        old.checked_add(1).ok_or(Error::Corrupt)?
    } else {
        old.saturating_sub(1)
    };
    if new == old {
        return Ok(());
    }
    let mut sizes = tx.open_table(SUBSCRIPTION_SIZES).map_err(db_error)?;
    if old > 0 {
        sizes.remove((old, sub)).map_err(db_error)?;
    }
    if new > 0 {
        sizes.insert((new, sub), ()).map_err(db_error)?;
        counts.insert(sub, new).map_err(db_error)?;
    } else {
        counts.remove(sub).map_err(db_error)?;
    }
    Ok(())
}

/// Index entries considered per subscription, so one subscription's
/// blocked entries cannot fill the window.
const PER_SUBSCRIPTION: usize = 4;

/// The `PRUNABLE` keys of subscription `sub`, oldest first.
fn subscription_range(sub: u64) -> (Bound<PruneKey>, Bound<PruneKey>) {
    (
        Bound::Included((sub, 0, i64::MIN, 0)),
        Bound::Included((sub, u8::MAX, i64::MAX, u64::MAX)),
    )
}

/// At [`MAX_EPISODES`], delete one episode before another is created.
///
/// The victim is the oldest prunable episode of the largest subscription
/// that has one (ties: the highest subscription id), among the first
/// `PER_SUBSCRIPTION` index entries of the `WINDOW / PER_SUBSCRIPTION`
/// largest subscriptions; else the first prunable of the oldest imported.
/// Finding none leaves the catalogue one over.
pub(super) fn make_room(
    tx: &WriteTransaction,
    limit: u64,
    scratch: &mut [u8],
) -> Result<(), Error> {
    let episodes = tx
        .open_table(EPISODES)
        .map_err(db_error)?
        .len()
        .map_err(db_error)?;
    if episodes < limit {
        return Ok(());
    }
    let mut keys = [None; WINDOW];
    {
        let sizes = tx.open_table(SUBSCRIPTION_SIZES).map_err(db_error)?;
        let index = tx.open_table(PRUNABLE).map_err(db_error)?;
        let largest = sizes
            .iter()
            .map_err(db_error)?
            .rev()
            .take(WINDOW / PER_SUBSCRIPTION);
        // Largest first: one range seek per subscription.
        for (entry, slots) in largest.zip(keys.chunks_mut(PER_SUBSCRIPTION)) {
            let (_, sub) = entry.map_err(db_error)?.0.value();
            let range = index.range(subscription_range(sub)).map_err(db_error)?;
            for (slot, item) in slots.iter_mut().zip(range) {
                *slot = Some(item.map_err(db_error)?.0.value());
            }
        }
    }
    for key in keys.into_iter().flatten() {
        if remove(tx, key, scratch)? {
            return Ok(());
        }
    }
    // The index may be blocked (every entry stale or with a unit record):
    // fall back to the oldest-imported episodes.
    fallback(tx, scratch)?;
    Ok(())
}

/// Delete the first prunable episode among the `WINDOW` lowest ids, which
/// were imported first; it needs no index.
fn fallback(tx: &WriteTransaction, scratch: &mut [u8]) -> Result<bool, Error> {
    let mut keys = [None; WINDOW];
    {
        let table = tx.open_table(EPISODES).map_err(db_error)?;
        for (slot, entry) in keys.iter_mut().zip(table.iter().map_err(db_error)?) {
            let (id, value) = entry.map_err(db_error)?;
            let value = value.value();
            let buf = scratch.get_mut(..value.len()).ok_or(Error::Corrupt)?;
            buf.copy_from_slice(value);
            let Episode {
                state,
                metadata,
                subscription,
                ..
            } = decode(buf).ok_or(Error::Corrupt)?;
            if prunable(state) {
                *slot = Some(prune_key(
                    subscription,
                    EpisodeId(id.value()),
                    metadata.published,
                ));
            }
        }
    }
    for key in keys.into_iter().flatten() {
        if remove(tx, key, scratch)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Delete the episode of index entry `key` if it is still prunable and has
/// no unit record; drop an entry whose episode is gone.
fn remove(tx: &WriteTransaction, key: PruneKey, scratch: &mut [u8]) -> Result<bool, Error> {
    let id = key.3;
    if tx
        .open_table(UNIT_EXTENTS)
        .map_err(db_error)?
        .get(id)
        .map_err(db_error)?
        .is_some()
    {
        // Drop the entry so it cannot fill the window; the next write of the
        // episode indexes it again.
        tx.open_table(PRUNABLE)
            .map_err(db_error)?
            .remove(key)
            .map_err(db_error)?;
        return Ok(false);
    }
    let state = {
        let table = tx.open_table(EPISODES).map_err(db_error)?;
        match table.get(id).map_err(db_error)? {
            None => None,
            Some(value) => {
                let value = value.value();
                let buf = scratch.get_mut(..value.len()).ok_or(Error::Corrupt)?;
                buf.copy_from_slice(value);
                let Episode {
                    state,
                    metadata,
                    subscription,
                    ..
                } = decode(buf).ok_or(Error::Corrupt)?;
                Some((state, metadata.published, subscription))
            }
        }
    };
    let eligible =
        state.is_some_and(|(s, p, sub)| prunable(s) && prune_key(sub, EpisodeId(id), p) == key);
    if state.is_some() && !eligible {
        // Stale entry (state or date changed): drop it too.
        tx.open_table(PRUNABLE)
            .map_err(db_error)?
            .remove(key)
            .map_err(db_error)?;
        return Ok(false);
    }
    tx.open_table(PRUNABLE)
        .map_err(db_error)?
        .remove(key)
        .map_err(db_error)?;
    if let Some((_, _, subscription)) = state.filter(|_| eligible) {
        tx.open_table(EPISODES)
            .map_err(db_error)?
            .remove(id)
            .map_err(db_error)?;
        count_pruned(tx, subscription)?;
    }
    Ok(eligible)
}

#[cfg(test)]
#[path = "catalogue_prune_tests.rs"]
mod tests;
