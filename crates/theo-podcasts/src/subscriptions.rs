//! Persisted Radio France follows, independent of completed sync checkpoints.
use crate::{
    catalogue::*,
    radio_france::FeedUrl,
    tables::{COUNTERS, NEXT_SUBSCRIPTION, SHOWS, SUBSCRIPTIONS},
};
use core::ops::Bound;
use redb::{ReadableDatabase, ReadableTable};
#[derive(Debug)]
pub struct Subscription {
    pub id: SubscriptionId,
    pub feed: heapless::String<256>,
}
impl Catalogue {
    /// Following page zero and its query-free spelling addresses the same show.
    /// The ID and URL become durable in one transaction before any network work.
    pub fn follow(&mut self, feed: &str) -> Result<SubscriptionId, Error> {
        let parsed = FeedUrl::parse(feed).map_err(|_| Error::InvalidMetadata)?;
        if parsed.page != 0 {
            return Err(Error::InvalidMetadata);
        }
        let canonical = feed.split('?').next().ok_or(Error::InvalidMetadata)?;
        let show: [u8; 36] = parsed
            .show
            .as_bytes()
            .try_into()
            .map_err(|_| Error::InvalidMetadata)?;
        self.end_batch();
        self.begin()?;
        let result = self.follow_inner(canonical, show);
        self.finish(result)
    }
    fn follow_inner(&mut self, canonical: &str, show: [u8; 36]) -> Result<SubscriptionId, Error> {
        // An existing follow is answered from a read transaction: no commit.
        {
            let tx = self.db.begin_read().map_err(db_error)?;
            let shows = tx.open_table(SHOWS).map_err(db_error)?;
            if let Some(id) = number(&shows, show)? {
                return if id == 0 {
                    Err(Error::Corrupt)
                } else {
                    Ok(SubscriptionId(id))
                };
            }
        }
        let tx = self.write()?;
        let id = {
            let mut counters = tx.open_table(COUNTERS).map_err(db_error)?;
            let id = number(&counters, NEXT_SUBSCRIPTION)?
                .filter(|&n| n != 0)
                .ok_or(Error::Corrupt)?;
            let next = id.checked_add(1).ok_or(Error::IdExhausted)?;
            counters.insert(NEXT_SUBSCRIPTION, next).map_err(db_error)?;
            id
        };
        tx.open_table(SUBSCRIPTIONS)
            .map_err(db_error)?
            .insert(id, canonical)
            .map_err(db_error)?;
        tx.open_table(SHOWS)
            .map_err(db_error)?
            .insert(show, id)
            .map_err(db_error)?;
        self.done()?;
        Ok(SubscriptionId(id))
    }
    /// One subscription per request, with no retained database cursor.
    pub fn next_subscription(
        &mut self,
        after: Option<SubscriptionId>,
    ) -> Result<Option<Subscription>, Error> {
        self.end_batch();
        self.begin()?;
        let result = self.next_subscription_inner(after);
        self.finish(result)
    }
    fn next_subscription_inner(
        &self,
        after: Option<SubscriptionId>,
    ) -> Result<Option<Subscription>, Error> {
        let tx = self.db.begin_read().map_err(db_error)?;
        let table = tx.open_table(SUBSCRIPTIONS).map_err(db_error)?;
        let lower = Bound::Excluded(after.unwrap_or(SubscriptionId(0)).0);
        let Some(entry) = table
            .range((lower, Bound::Unbounded))
            .map_err(db_error)?
            .next()
        else {
            return Ok(None);
        };
        let (id, feed) = entry.map_err(db_error)?;
        let (id, feed) = (SubscriptionId(id.value()), feed.value());
        if id.0 == 0 || FeedUrl::parse(feed).map_err(|_| Error::Corrupt)?.page != 0 {
            return Err(Error::Corrupt);
        }
        Ok(Some(Subscription {
            id,
            feed: heapless::String::try_from(feed).map_err(|_| Error::Corrupt)?,
        }))
    }
}
#[cfg(test)]
#[path = "subscriptions_tests.rs"]
mod tests;
