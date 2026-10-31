//! Replayable bounded history reconciliation and durable all-pages checkpoints.
use crate::{
    catalogue::{Catalogue, Error, MAX_URL_BYTES, SubscriptionId},
    radio_france::{FeedError, RadioFranceFeed},
};
use ekv::flash::Flash;
use heapless::String;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub feed: String<256>,
    pub etag: String<512>,
    pub last_modified: String<128>,
    pub pages: u32,
    pub generation: u64,
}
#[derive(Clone, Copy, Default)]
pub struct Validators<'a> {
    pub etag: Option<&'a str>,
    pub last_modified: Option<&'a str>,
}
#[derive(Debug)]
pub enum SyncError<E> {
    Feed(FeedError),
    Catalogue(Error<E>),
    Protocol,
    Incomplete,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub entries: u64,
    pub created: u64,
    pub queued: u64,
}
use crate::radio_france::FeedUrl;
/// Each call processes at most one transport buffer, each upsert at most one
/// bounded record. Multi-page responses are always fetched unconditionally:
/// an unchanged first page does not certify older pages are unchanged.
pub const MAX_CHUNK_BYTES: usize = 4096;
pub const MAX_HISTORY_PAGES: u32 = 4096;
#[derive(PartialEq, Eq)]
enum Phase {
    Response,
    Body,
    NotModified,
    Ready,
    Failed,
}
pub struct SyncSession {
    id: SubscriptionId,
    url: String<MAX_URL_BYTES>,
    checkpoint: Checkpoint,
    prior: Option<Checkpoint>,
    parser: RadioFranceFeed,
    phase: Phase,
    page: u32,
    progress: Progress,
    unchanged: bool,
}
impl SyncSession {
    pub fn new(
        id: SubscriptionId,
        url: &str,
        prior: Option<Checkpoint>,
    ) -> Result<Self, FeedError> {
        if id.0 == 0
            || FeedUrl::parse(url)?.page != 0
            || prior
                .as_ref()
                .is_some_and(|p| p.feed != url || p.pages == 0 || p.generation == 0)
        {
            return Err(FeedError::Source);
        }
        let checkpoint = Checkpoint {
            feed: String::try_from(url).map_err(|_| FeedError::Limit)?,
            etag: String::new(),
            last_modified: String::new(),
            pages: 0,
            generation: prior
                .as_ref()
                .map_or(Some(1), |p| p.generation.checked_add(1))
                .ok_or(FeedError::Limit)?,
        };
        Ok(Self {
            id,
            url: String::try_from(url).map_err(|_| FeedError::Limit)?,
            checkpoint,
            prior,
            parser: RadioFranceFeed::new(),
            phase: Phase::Response,
            page: 0,
            progress: Progress::default(),
            unchanged: false,
        })
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn conditional(&self) -> Validators<'_> {
        if self.page == 0
            && self.phase == Phase::Response
            && let Some(prior) = &self.prior
            && prior.pages == 1
        {
            return Validators {
                etag: (!prior.etag.is_empty()).then_some(prior.etag.as_str()),
                last_modified: (!prior.last_modified.is_empty())
                    .then_some(prior.last_modified.as_str()),
            };
        }
        Validators::default()
    }
    pub fn response<E>(
        &mut self,
        status: u16,
        validators: Validators<'_>,
    ) -> Result<(), SyncError<E>> {
        let valid = self.phase == Phase::Response;
        let conditional = self.conditional();
        let can_304 = conditional.etag.is_some() || conditional.last_modified.is_some();
        self.phase = Phase::Failed;
        if !valid {
            return Err(SyncError::Protocol);
        }
        if status == 304 && can_304 {
            self.unchanged = true;
            self.phase = Phase::NotModified;
            return Ok(());
        }
        if status != 200 {
            return Err(SyncError::Protocol);
        }
        // Validate every page's headers, but publish only first-page validators.
        let etag = header::<512>(validators.etag).map_err(SyncError::Feed)?;
        let last_modified = header::<128>(validators.last_modified).map_err(SyncError::Feed)?;
        if self.page == 0 {
            self.checkpoint.etag = etag;
            self.checkpoint.last_modified = last_modified;
        }
        self.phase = Phase::Body;
        Ok(())
    }
    pub async fn push<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        bytes: &[u8],
        scratch: &mut [u8],
    ) -> Result<(), SyncError<F::Error>> {
        let valid = self.phase == Phase::Body;
        self.phase = Phase::Failed;
        if !valid || bytes.len() > MAX_CHUNK_BYTES {
            return Err(SyncError::Protocol);
        }
        for &byte in bytes {
            if let Some(item) = self.parser.push(byte).map_err(SyncError::Feed)? {
                let outcome = cat
                    .upsert(self.id, item.metadata(), scratch)
                    .await
                    .map_err(SyncError::Catalogue)?;
                self.progress.entries += 1;
                self.progress.created += u64::from(outcome.created);
                self.progress.queued += u64::from(outcome.queued);
            }
        }
        self.phase = Phase::Body;
        Ok(())
    }
    /// Returns true when `url()` names the next required page. False means the
    /// session can commit. End-of-document alone never publishes a checkpoint.
    pub fn end_page<E>(&mut self) -> Result<bool, SyncError<E>> {
        let old = core::mem::replace(&mut self.phase, Phase::Failed);
        if old == Phase::NotModified {
            self.phase = Phase::Ready;
            return Ok(false);
        }
        if old != Phase::Body {
            return Err(SyncError::Protocol);
        }
        match self.parser.finish().map_err(SyncError::Feed)? {
            Some(next) => {
                if self.page + 1 >= MAX_HISTORY_PAGES {
                    return Err(SyncError::Feed(FeedError::Limit));
                }
                FeedUrl::parse(&self.url)
                    .and_then(|url| url.next(next))
                    .map_err(SyncError::Feed)?;
                self.url = String::try_from(next).map_err(|_| SyncError::Feed(FeedError::Limit))?;
                self.page += 1;
                self.parser = RadioFranceFeed::new();
                self.phase = Phase::Response;
                Ok(true)
            }
            None => {
                self.checkpoint.pages = self.page + 1;
                self.phase = Phase::Ready;
                Ok(false)
            }
        }
    }
    pub async fn complete<F: Flash>(
        self,
        cat: &mut Catalogue<F>,
        scratch: &mut [u8],
    ) -> Result<Progress, SyncError<F::Error>> {
        if self.phase != Phase::Ready {
            return Err(SyncError::Incomplete);
        }
        if !self.unchanged {
            cat.save_sync_checkpoint(self.id, &self.checkpoint, scratch)
                .await
                .map_err(SyncError::Catalogue)?;
        }
        Ok(self.progress)
    }
}
fn header<const N: usize>(s: Option<&str>) -> Result<String<N>, FeedError> {
    let s = s.unwrap_or("");
    if s.bytes().any(|b| b < 32 || b == 127) {
        return Err(FeedError::Metadata);
    }
    String::try_from(s).map_err(|_| FeedError::Limit)
}
impl Checkpoint {
    pub(crate) fn encode(&self, buf: &mut [u8]) -> Option<usize> {
        if self.pages == 0
            || self.pages > MAX_HISTORY_PAGES
            || self.generation == 0
            || FeedUrl::parse(&self.feed).ok()?.page != 0
        {
            return None;
        }
        let n = 19 + self.feed.len() + self.etag.len() + self.last_modified.len();
        let buf = buf.get_mut(..n)?;
        buf[0] = 1;
        buf[1..5].copy_from_slice(&self.pages.to_le_bytes());
        buf[5..13].copy_from_slice(&self.generation.to_le_bytes());
        let mut at = 19;
        for (i, value) in [
            self.feed.as_str(),
            self.etag.as_str(),
            self.last_modified.as_str(),
        ]
        .iter()
        .enumerate()
        {
            buf[13 + i * 2..15 + i * 2].copy_from_slice(&(value.len() as u16).to_le_bytes());
            buf[at..at + value.len()].copy_from_slice(value.as_bytes());
            at += value.len();
        }
        Some(n)
    }
    pub(crate) fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 19 || buf[0] != 1 {
            return None;
        }
        let mut at = 19;
        let mut field = |i: usize| {
            let n = u16::from_le_bytes(buf.get(13 + i * 2..15 + i * 2)?.try_into().ok()?) as usize;
            let s = core::str::from_utf8(buf.get(at..at + n)?).ok()?;
            at += n;
            Some(s)
        };
        let feed = String::try_from(field(0)?).ok()?;
        let etag = header(Some(field(1)?)).ok()?;
        let last_modified = header(Some(field(2)?)).ok()?;
        if at != buf.len() {
            return None;
        }
        let out = Self {
            feed,
            etag,
            last_modified,
            pages: u32::from_le_bytes(buf[1..5].try_into().ok()?),
            generation: u64::from_le_bytes(buf[5..13].try_into().ok()?),
        };
        if out.pages == 0
            || out.pages > MAX_HISTORY_PAGES
            || out.generation == 0
            || FeedUrl::parse(&out.feed).ok()?.page != 0
        {
            return None;
        }
        Some(out)
    }
}
#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
