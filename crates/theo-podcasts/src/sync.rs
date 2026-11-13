//! Replayable bounded history reconciliation and durable all-pages checkpoints.
use crate::{
    catalogue::{Catalogue, Error, MAX_URL_BYTES, SubscriptionId},
    radio_france::{FeedError, RadioFranceFeed},
};
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
pub enum SyncError {
    Feed(FeedError),
    Catalogue(Error),
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
/// Most episodes one [`SyncSession::push`] upserts. Only the last may commit;
/// the others are known and unchanged, so they only read (ADR-0022).
pub const MAX_CHUNK_ITEMS: usize = 4;
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
        let mut session = Self::idle();
        session.restart(id, url, prior)?;
        Ok(session)
    }
    /// A session holding no sync; [`Self::restart`] starts one.
    pub fn idle() -> Self {
        Self {
            id: SubscriptionId(0),
            url: String::new(),
            checkpoint: Checkpoint {
                feed: String::new(),
                etag: String::new(),
                last_modified: String::new(),
                pages: 0,
                generation: 0,
            },
            prior: None,
            parser: RadioFranceFeed::new(),
            phase: Phase::Failed,
            page: 0,
            progress: Progress::default(),
            unchanged: false,
        }
    }
    /// Starts a sync in place, as [`Self::new`] would; on error `self` is
    /// unchanged. Building a new session is a 20 KB stack temporary, which
    /// core1's SD owner cannot afford (t-133).
    pub fn restart(
        &mut self,
        id: SubscriptionId,
        url: &str,
        prior: Option<Checkpoint>,
    ) -> Result<(), FeedError> {
        if id.0 == 0
            || FeedUrl::parse(url)?.page != 0
            || prior
                .as_ref()
                .is_some_and(|p| p.feed != url || p.pages == 0 || p.generation == 0)
        {
            return Err(FeedError::Source);
        }
        let feed = String::try_from(url).map_err(|_| FeedError::Limit)?;
        let url = String::try_from(url).map_err(|_| FeedError::Limit)?;
        let generation = prior
            .as_ref()
            .map_or(Some(1), |p| p.generation.checked_add(1))
            .ok_or(FeedError::Limit)?;
        let Self {
            id: session_id,
            url: session_url,
            checkpoint,
            prior: session_prior,
            parser,
            phase,
            page,
            progress,
            unchanged,
        } = self;
        *session_id = id;
        *session_url = url;
        let Checkpoint {
            feed: checkpoint_feed,
            etag,
            last_modified,
            pages,
            generation: checkpoint_generation,
        } = checkpoint;
        *checkpoint_feed = feed;
        etag.clear();
        last_modified.clear();
        *pages = 0;
        *checkpoint_generation = generation;
        *session_prior = prior;
        parser.reset();
        *phase = Phase::Response;
        *page = 0;
        *progress = Progress::default();
        *unchanged = false;
        Ok(())
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
    pub fn response(&mut self, status: u16, validators: Validators<'_>) -> Result<(), SyncError> {
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
    /// Feed body bytes; returns how many were consumed. The caller sends
    /// the rest again (a non-empty input always consumes at least one byte).
    ///
    /// Each upsert commits on its own (episode, identity and counter
    /// together), and one call makes at most one commit, so a request stays
    /// within its IO budget: it stops after the first episode whose upsert
    /// committed, or after [`MAX_CHUNK_ITEMS`] episodes. A failure mid-page
    /// keeps the earlier episodes; the checkpoint is only saved by
    /// [`Self::complete`], so a resync re-upserts the page and the identity
    /// table makes that idempotent (ADR-0022).
    pub fn push(
        &mut self,
        cat: &mut Catalogue,
        bytes: &[u8],
        scratch: &mut [u8],
    ) -> Result<usize, SyncError> {
        let valid = self.phase == Phase::Body;
        self.phase = Phase::Failed;
        if !valid || bytes.len() > MAX_CHUNK_BYTES {
            return Err(SyncError::Protocol);
        }
        let mut items = 0;
        for (at, &byte) in bytes.iter().enumerate() {
            if let Some(item) = self.parser.push(byte).map_err(SyncError::Feed)? {
                let commits = cat.commits();
                let outcome = cat
                    .upsert(self.id, item.metadata(), scratch)
                    .map_err(SyncError::Catalogue)?;
                self.progress.entries += 1;
                self.progress.created += u64::from(outcome.created);
                self.progress.queued += u64::from(outcome.queued);
                items += 1;
                if cat.commits() != commits || items == MAX_CHUNK_ITEMS {
                    self.phase = Phase::Body;
                    return Ok(at + 1);
                }
            }
        }
        self.phase = Phase::Body;
        Ok(bytes.len())
    }
    /// Returns true when `url()` names the next required page. False means the
    /// session can commit. End-of-document alone never publishes a checkpoint.
    pub fn end_page(&mut self) -> Result<bool, SyncError> {
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
                self.parser.reset();
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
    /// Saves the checkpoint.
    /// Saves the checkpoint once: the session is then spent until `restart`.
    pub fn complete(
        &mut self,
        cat: &mut Catalogue,
        scratch: &mut [u8],
    ) -> Result<Progress, SyncError> {
        if self.phase != Phase::Ready {
            return Err(SyncError::Incomplete);
        }
        if !self.unchanged {
            cat.save_sync_checkpoint(self.id, &self.checkpoint, scratch)
                .map_err(SyncError::Catalogue)?;
        }
        self.phase = Phase::Failed;
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
