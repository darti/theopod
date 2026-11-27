//! Tombstone collection: forget pruned episodes that left their feed
//! (ADR-0031).
//!
//! A pruned episode keeps its identity entry, so a sync does not import it
//! again (ADR-0030). Once a complete sync no longer lists it, the entry can
//! go.
use super::{Catalogue, Error, IdentityKey, MAX_SCAN_RECORDS, SubscriptionId, db_error};
use crate::tables::{EPISODE_IDENTITY, EPISODES};
use core::ops::Bound;
use redb::{ReadableDatabase, ReadableTable};

impl Catalogue {
    /// One bounded step of a sweep of `subscription`: examine its next
    /// identities after `after` (at most [`MAX_SCAN_RECORDS`]) and delete,
    /// in one commit, those `seen` rejects whose episode is gone. Returns the
    /// cursor of the next step, or `None` once every identity was examined.
    /// The scan is a read transaction; a step that deletes nothing commits
    /// nothing.
    pub(crate) fn sweep_tombstones(
        &mut self,
        subscription: SubscriptionId,
        after: Option<IdentityKey>,
        seen: impl Fn(&IdentityKey) -> bool,
    ) -> Result<Option<IdentityKey>, Error> {
        if subscription.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        // Outside a batch every write is committed, so a read sees it all.
        if self.batch || self.tx.is_some() {
            return Err(Error::InvalidState);
        }
        self.begin()?;
        let result = self.sweep_step(subscription.0, after, seen);
        self.finish(result)
    }

    fn sweep_step(
        &mut self,
        subscription: u64,
        after: Option<IdentityKey>,
        seen: impl Fn(&IdentityKey) -> bool,
    ) -> Result<Option<IdentityKey>, Error> {
        let mut doomed = [None; MAX_SCAN_RECORDS];
        let mut examined = 0;
        let mut last = None;
        {
            let tx = self.db.begin_read().map_err(db_error)?;
            let identities = tx.open_table(EPISODE_IDENTITY).map_err(db_error)?;
            let episodes = tx.open_table(EPISODES).map_err(db_error)?;
            let start = match after {
                Some((kind, hash)) => Bound::Excluded((subscription, kind, hash)),
                None => Bound::Included((subscription, 0, [0; 32])),
            };
            let end = Bound::Included((subscription, u8::MAX, [u8::MAX; 32]));
            let range = identities.range((start, end)).map_err(db_error)?;
            // `doomed` first: zip stops before taking a 17th entry, so the
            // cursor never skips one.
            for (slot, entry) in doomed.iter_mut().zip(range) {
                let (key, id) = entry.map_err(db_error)?;
                let (_, kind, hash) = key.value();
                let gone = episodes.get(id.value()).map_err(db_error)?.is_none();
                if gone && !seen(&(kind, hash)) {
                    *slot = Some((subscription, kind, hash));
                }
                examined += 1;
                last = Some((kind, hash));
            }
        }
        if doomed.iter().any(Option::is_some) {
            let tx = self.write()?;
            let mut identities = tx.open_table(EPISODE_IDENTITY).map_err(db_error)?;
            for key in doomed.into_iter().flatten() {
                identities.remove(key).map_err(db_error)?;
            }
            drop(identities);
            self.done()?;
        }
        Ok(if examined == MAX_SCAN_RECORDS {
            last
        } else {
            None
        })
    }
}

#[cfg(test)]
#[path = "catalogue_sweep_tests.rs"]
mod tests;
