//! Episode pruning: the catalogue never grows into `Error::Full` (ADR-0030).
//!
//! redb doubles its file when it grows, and a 72 MiB request near 8,600
//! episodes exceeds the 64 MiB region. Creating an episode while
//! [`MAX_EPISODES`] exist therefore first deletes the oldest episode that has
//! no audio on the card, in the same transaction. The identity entry stays
//! as a tombstone, so a later sync of the same feed history finds the
//! episode "known" and does not import it again.
use super::{
    Episode, EpisodeId, EpisodeState, Error, MAX_SCAN_RECORDS, UpsertOutcome, db_error, decode,
    evict_key,
};
use crate::{
    catalogue::AudioState,
    tables::{EPISODES, PRUNABLE, UNIT_EXTENTS},
};
use redb::{ReadableTable, ReadableTableMetadata, WriteTransaction};

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
    id: EpisodeId,
    old: Option<(EpisodeState, Option<i64>)>,
    (state, published): (EpisodeState, Option<i64>),
) -> Result<(), Error> {
    let old = old
        .filter(|&(s, _)| prunable(s))
        .map(|(_, p)| evict_key(id, p));
    let new = prunable(state).then(|| evict_key(id, published));
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

/// Before an episode is created: at [`MAX_EPISODES`], delete the oldest
/// prunable episode among the first `WINDOW` index entries, else among the
/// oldest imported. Finding none leaves the catalogue one over.
pub(super) fn make_room(
    tx: &WriteTransaction,
    limit: u64,
    scratch: &mut [u8],
) -> Result<(), Error> {
    let count = tx
        .open_table(EPISODES)
        .map_err(db_error)?
        .len()
        .map_err(db_error)?;
    if count < limit {
        return Ok(());
    }
    let mut keys = [None; WINDOW];
    {
        let table = tx.open_table(PRUNABLE).map_err(db_error)?;
        for (slot, entry) in keys.iter_mut().zip(table.iter().map_err(db_error)?) {
            *slot = Some(entry.map_err(db_error)?.0.value());
        }
    }
    for key in keys.into_iter().flatten() {
        if remove(tx, key, scratch)? {
            return Ok(());
        }
    }
    // The index may be empty (episodes written before `PRUNABLE` existed) or
    // blocked: fall back to the oldest-imported episodes.
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
                state, metadata, ..
            } = decode(buf).ok_or(Error::Corrupt)?;
            if prunable(state) {
                *slot = Some(evict_key(EpisodeId(id.value()), metadata.published));
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
fn remove(tx: &WriteTransaction, key: (u8, i64, u64), scratch: &mut [u8]) -> Result<bool, Error> {
    let id = key.2;
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
                    state, metadata, ..
                } = decode(buf).ok_or(Error::Corrupt)?;
                Some((state, metadata.published))
            }
        }
    };
    let eligible = state.is_some_and(|(s, p)| prunable(s) && evict_key(EpisodeId(id), p) == key);
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
    if eligible {
        tx.open_table(EPISODES)
            .map_err(db_error)?
            .remove(id)
            .map_err(db_error)?;
    }
    Ok(eligible)
}

#[cfg(test)]
#[path = "catalogue_prune_tests.rs"]
mod tests;
