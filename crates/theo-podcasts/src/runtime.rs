//! Bounded domain protocol, executed only by the unique filesystem/database owner.
use crate::{
    catalogue::{self, *},
    download::*,
    media::*,
    recovery::Recovery,
    subscriptions::Subscription,
    sync::*,
};
use ekv::flash::Flash;
use heapless::{String, Vec};
pub type Chunk = Vec<u8, AUDIO_CHUNK>;
#[allow(clippy::large_enum_variant)]
pub enum Command {
    Recover,
    Follow(String<256>),
    MarkRead(EpisodeId),
    Subscription(Option<SubscriptionId>),
    SyncStart(SubscriptionId, String<256>),
    SyncConditionals,
    SyncHeaders {
        status: u16,
        etag: String<512>,
        modified: String<128>,
    },
    SyncChunk(Chunk),
    SyncEnd,
    NextJob(Option<EpisodeId>),
    DownloadStart(DownloadJob),
    DownloadHeaders {
        status: u16,
        length: Option<u64>,
        partial: bool,
    },
    DownloadChunk(Chunk),
    DownloadEnd(bool),
    Abort,
}
#[allow(clippy::large_enum_variant)]
pub enum Reply {
    Offline,
    Rejected,
    Done,
    Recovered(bool),
    Followed(SubscriptionId),
    Subscription(Option<Subscription>),
    Url(String<MAX_URL_BYTES>),
    Synced(Progress),
    Conditionals {
        etag: String<512>,
        modified: String<128>,
    },
    Job {
        job: Option<DownloadJob>,
        after: Option<EpisodeId>,
        done: bool,
    },
    Ready,
    Downloaded(DownloadOutcome),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Storage,
}
pub struct Runtime<F: Flash, M: MediaStore<F>> {
    catalogue: Catalogue<F>,
    pub media: M,
    recovery: Recovery,
    sync: Option<SyncSession>,
    download: Option<DownloadSession>,
    enabled: bool,
}
impl<F: Flash, M: MediaStore<F>> Runtime<F, M> {
    pub fn new(catalogue: Catalogue<F>, media: M) -> Self {
        Self {
            catalogue,
            media,
            recovery: Recovery::new(),
            sync: None,
            download: None,
            enabled: true,
        }
    }
    pub async fn handle(&mut self, command: Command, scratch: &mut [u8]) -> Result<Reply, Error> {
        if !self.enabled {
            return Err(Error::Storage);
        }
        // Cancellation cannot leave this domain owner reusable. The board also
        // drops its one shared-sector session guard on cancelled requests.
        self.enabled = false;
        let result = self.execute(command, scratch).await;
        self.enabled = result.as_ref().err() != Some(&Error::Storage);
        result
    }
    async fn execute(&mut self, command: Command, scratch: &mut [u8]) -> Result<Reply, Error> {
        Ok(match command {
            Command::Recover => Reply::Recovered(
                self.recovery
                    .step(&mut self.catalogue, &mut self.media, scratch)
                    .await
                    .map_err(media_error)?,
            ),
            Command::Follow(url) => {
                Reply::Followed(self.catalogue.follow(&url).await.map_err(cat_error)?)
            }
            Command::MarkRead(id) => {
                self.catalogue
                    .mark_read(id, scratch)
                    .await
                    .map_err(cat_error)?;
                Reply::Done
            }
            Command::Subscription(after) => Reply::Subscription(
                self.catalogue
                    .next_subscription(after)
                    .await
                    .map_err(cat_error)?,
            ),
            Command::SyncStart(id, url) => {
                if self.download.is_some() || self.sync.is_some() {
                    return Err(Error::Invalid);
                }
                let prior = self
                    .catalogue
                    .sync_checkpoint(id, scratch)
                    .await
                    .map_err(cat_error)?;
                self.sync = Some(SyncSession::new(id, &url, prior).map_err(|_| Error::Invalid)?);
                Reply::Done
            }
            Command::SyncConditionals => {
                let validators = self.sync.as_ref().ok_or(Error::Invalid)?.conditional();
                Reply::Conditionals {
                    etag: validators
                        .etag
                        .unwrap_or("")
                        .try_into()
                        .map_err(|_| Error::Invalid)?,
                    modified: validators
                        .last_modified
                        .unwrap_or("")
                        .try_into()
                        .map_err(|_| Error::Invalid)?,
                }
            }
            Command::SyncHeaders {
                status,
                etag,
                modified,
            } => {
                self.sync
                    .as_mut()
                    .ok_or(Error::Invalid)?
                    .response::<F::Error>(
                        status,
                        Validators {
                            etag: (!etag.is_empty()).then_some(etag.as_str()),
                            last_modified: (!modified.is_empty()).then_some(modified.as_str()),
                        },
                    )
                    .map_err(sync_error)?;
                Reply::Done
            }
            Command::SyncChunk(bytes) => {
                self.sync
                    .as_mut()
                    .ok_or(Error::Invalid)?
                    .push(&mut self.catalogue, &bytes, scratch)
                    .await
                    .map_err(sync_error)?;
                Reply::Done
            }
            Command::SyncEnd => {
                let mut sync = self.sync.take().ok_or(Error::Invalid)?;
                if sync.end_page::<F::Error>().map_err(sync_error)? {
                    let url = String::try_from(sync.url()).map_err(|_| Error::Invalid)?;
                    self.sync = Some(sync);
                    Reply::Url(url)
                } else {
                    Reply::Synced(
                        sync.complete(&mut self.catalogue, scratch)
                            .await
                            .map_err(sync_error)?,
                    )
                }
            }
            Command::NextJob(after) => {
                let mut job = None;
                let page = self
                    .catalogue
                    .scan(after, scratch, |episode| {
                        if job.is_none()
                            && !episode.state.read
                            && episode.state.audio == AudioState::Queued
                        {
                            job = DownloadJob::from_episode(episode).ok();
                        }
                    })
                    .await
                    .map_err(cat_error)?;
                // A scan may contain several queued episodes. Resume after the
                // emitted job, not after records the caller has not received.
                let after = job.as_ref().map(|job| job.id).or(page.resume_after);
                let done = job.is_none() && !page.has_more;
                Reply::Job { job, after, done }
            }
            Command::DownloadStart(job) => {
                if self.download.is_some() || self.sync.is_some() {
                    return Err(Error::Invalid);
                }
                match DownloadSession::start(&mut self.catalogue, &mut self.media, job, scratch)
                    .await
                    .map_err(media_error)?
                {
                    Start::Ready(session) => {
                        self.download = Some(session);
                        Reply::Ready
                    }
                    Start::WaitingForSpace => Reply::Downloaded(DownloadOutcome::WaitingForSpace),
                    Start::Skip => Reply::Downloaded(DownloadOutcome::RetryLater),
                }
            }
            Command::DownloadHeaders {
                status,
                length,
                partial,
            } => {
                self.download
                    .as_mut()
                    .ok_or(Error::Invalid)?
                    .response(status, length, partial.then_some("partial"))
                    .map_err(|_| Error::Invalid)?;
                Reply::Done
            }
            Command::DownloadChunk(bytes) => {
                match self
                    .download
                    .as_mut()
                    .ok_or(Error::Invalid)?
                    .push(&mut self.catalogue, &mut self.media, &bytes, scratch)
                    .await
                    .map_err(media_error)?
                {
                    ChunkOutcome::Written => Reply::Done,
                    ChunkOutcome::WaitingForSpace => {
                        Reply::Downloaded(DownloadOutcome::WaitingForSpace)
                    }
                }
            }
            Command::DownloadEnd(complete) => Reply::Downloaded(
                self.download
                    .take()
                    .ok_or(Error::Invalid)?
                    .finish(&mut self.catalogue, &mut self.media, complete, scratch)
                    .await
                    .map_err(media_error)?,
            ),
            Command::Abort => {
                self.sync = None;
                if let Some(download) = self.download.take() {
                    download
                        .abort(&mut self.catalogue, &mut self.media, scratch)
                        .await
                        .map_err(media_error)?;
                }
                Reply::Done
            }
        })
    }
}
fn cat_error<E>(error: catalogue::Error<E>) -> Error {
    match error {
        catalogue::Error::FieldTooLong
        | catalogue::Error::InvalidMetadata
        | catalogue::Error::BufferTooSmall
        | catalogue::Error::NotFound
        | catalogue::Error::InvalidState => Error::Invalid,
        _ => Error::Storage,
    }
}
fn sync_error<E>(error: SyncError<E>) -> Error {
    match error {
        SyncError::Catalogue(e) => cat_error(e),
        _ => Error::Invalid,
    }
}
fn media_error<C, M>(error: MediaError<C, M>) -> Error {
    match error {
        MediaError::Catalogue(e) => cat_error(e),
        MediaError::Media(_) => Error::Storage,
        _ => Error::Invalid,
    }
}
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
