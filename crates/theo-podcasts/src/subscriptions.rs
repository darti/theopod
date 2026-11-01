//! Persisted Radio France follows, independent of completed sync checkpoints.
use crate::{catalogue::*, radio_france::FeedUrl};
use ekv::flash::Flash;
const NEXT: &[u8] = b"m/subnext";
#[derive(Debug)]
pub struct Subscription {
    pub id: SubscriptionId,
    pub feed: heapless::String<256>,
}
impl<F: Flash> Catalogue<F> {
    /// Following page zero and its query-free spelling addresses the same show.
    /// The ID and URL become durable in one transaction before any network work.
    pub async fn follow(&mut self, feed: &str) -> Result<SubscriptionId, Error<F::Error>> {
        let parsed = FeedUrl::parse(feed).map_err(|_| Error::InvalidMetadata)?;
        if parsed.page != 0 {
            return Err(Error::InvalidMetadata);
        }
        let canonical = feed.split('?').next().ok_or(Error::InvalidMetadata)?;
        let mut index = [b'u'; 37];
        index[1..].copy_from_slice(parsed.show.as_bytes());
        self.begin()?;
        let result = async {
            let tx = self.db.read_transaction().await;
            let mut bytes = [0; 8];
            match tx.read(&index, &mut bytes).await {
                Ok(8) => {
                    let id = SubscriptionId(u64::from_le_bytes(bytes));
                    if id.0 == 0 {
                        return Err(Error::Corrupt);
                    }
                    return Ok(id);
                }
                Err(ekv::ReadError::KeyNotFound) => (),
                Ok(_) => return Err(Error::Corrupt),
                Err(e) => return Err(read_error(e)),
            }
            let id = match tx.read(NEXT, &mut bytes).await {
                Ok(8) => u64::from_le_bytes(bytes),
                Err(ekv::ReadError::KeyNotFound) => 1,
                Ok(_) => return Err(Error::Corrupt),
                Err(e) => return Err(read_error(e)),
            };
            let next = id
                .checked_add(1)
                .filter(|_| id != 0)
                .ok_or(Error::IdExhausted)?;
            drop(tx);
            let mut tx = self.db.write_transaction().await;
            tx.write(&key(SubscriptionId(id)), canonical.as_bytes())
                .await
                .map_err(write_error)?;
            tx.write(NEXT, &next.to_le_bytes())
                .await
                .map_err(write_error)?;
            tx.write(&index, &id.to_le_bytes())
                .await
                .map_err(write_error)?;
            tx.commit().await.map_err(commit_error)?;
            Ok(SubscriptionId(id))
        }
        .await;
        self.finish(result)
    }
    /// One subscription per request, with no retained database cursor.
    pub async fn next_subscription(
        &mut self,
        after: Option<SubscriptionId>,
    ) -> Result<Option<Subscription>, Error<F::Error>> {
        self.begin()?;
        let result = async {
            let tx = self.db.read_transaction().await;
            let lower = key(after.unwrap_or(SubscriptionId(0)));
            let mut cursor = tx
                .read_range((
                    core::ops::Bound::Excluded(lower.as_slice()),
                    core::ops::Bound::Excluded(b"g".as_slice()),
                ))
                .await
                .map_err(|e| match e {
                    ekv::Error::Flash(e) => Error::Storage(e),
                    _ => Error::Corrupt,
                })?;
            let mut k = [0; 9];
            let mut value = [0; 256];
            let Some((kn, n)) = cursor.next(&mut k, &mut value).await.map_err(|e| match e {
                ekv::CursorError::Flash(e) => Error::Storage(e),
                _ => Error::Corrupt,
            })?
            else {
                return Ok(None);
            };
            if kn != 9 || k[0] != b'f' {
                return Err(Error::Corrupt);
            }
            let id = SubscriptionId(u64::from_be_bytes(
                k[1..].try_into().map_err(|_| Error::Corrupt)?,
            ));
            let feed = core::str::from_utf8(&value[..n]).map_err(|_| Error::Corrupt)?;
            if id.0 == 0 || FeedUrl::parse(feed).map_err(|_| Error::Corrupt)?.page != 0 {
                return Err(Error::Corrupt);
            }
            Ok(Some(Subscription {
                id,
                feed: heapless::String::try_from(feed).map_err(|_| Error::Corrupt)?,
            }))
        }
        .await;
        self.finish(result)
    }
}
fn key(id: SubscriptionId) -> [u8; 9] {
    let mut key = [b'f'; 9];
    key[1..].copy_from_slice(&id.0.to_be_bytes());
    key
}
#[cfg(test)]
#[path = "subscriptions_tests.rs"]
mod tests;
