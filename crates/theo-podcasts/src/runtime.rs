//! Bounded domain protocol, executed only by the unique filesystem/database owner.
use crate::{
    catalogue::{self, *},
    download::*,
    media::*,
    reclaim::Reclaim,
    recovery::Recovery,
    subscriptions::Subscription,
    sync::*,
};
use heapless::{String, Vec};
use theo_storage::future::apart;
pub type Chunk = Vec<u8, AUDIO_CHUNK>;
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
    /// `SyncChunk`: bytes consumed; send the rest again.
    Consumed(usize),
    /// `DownloadStart` or `DownloadChunk` made one bounded step (a commit
    /// or a reclaim scan): send the same command again.
    Pending,
}
/// The one sync session, kept in place in the runtime (PSRAM): starting or
/// ending a sync never moves its 20 KB through core1's stack (t-133).
struct SyncSlot {
    session: SyncSession,
    active: bool,
}
impl SyncSlot {
    fn get(&self) -> Option<&SyncSession> {
        self.active.then_some(&self.session)
    }
    fn get_mut(&mut self) -> Option<&mut SyncSession> {
        self.active.then_some(&mut self.session)
    }
    fn is_some(&self) -> bool {
        self.active
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Storage,
}
pub struct Runtime<M: MediaStore> {
    catalogue: Catalogue,
    pub media: M,
    recovery: Recovery,
    /// The eviction in progress, carried across `DownloadStart`/`DownloadChunk`
    /// requests (each makes at most one commit, ADR-0022).
    reclaim: Reclaim,
    sync: SyncSlot,
    download: Option<DownloadSession>,
    enabled: bool,
}
impl<M: MediaStore> Runtime<M> {
    pub fn new(catalogue: Catalogue, media: M) -> Self {
        Self {
            catalogue,
            media,
            recovery: Recovery::new(),
            reclaim: Reclaim::new(),
            sync: SyncSlot {
                session: SyncSession::idle(),
                active: false,
            },
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
        // Borrowed: the command stays in this future's state (PSRAM), never
        // copied through core1's stack into an arm (t-133).
        let result = self.execute(&command, scratch).await;
        self.enabled = result.as_ref().err() != Some(&Error::Storage);
        result
    }
    async fn execute(&mut self, command: &Command, scratch: &mut [u8]) -> Result<Reply, Error> {
        // Each arm is polled in a frame of its own (`apart`): inlined together,
        // their temporaries shared one 53 KB frame and overflowed core1 (t-133).
        match command {
            Command::Recover => {
                apart(async move {
                    Ok(Reply::Recovered(
                        self.recovery
                            .step(&mut self.catalogue, &mut self.media, scratch)
                            .await
                            .map_err(media_error::<M>)?,
                    ))
                })
                .await
            }
            Command::Follow(url) => {
                apart(async move {
                    Ok(Reply::Followed(
                        self.catalogue.follow(url).map_err(cat_error)?,
                    ))
                })
                .await
            }
            Command::MarkRead(id) => {
                apart(async move {
                    self.catalogue.mark_read(*id, scratch).map_err(cat_error)?;
                    Ok(Reply::Done)
                })
                .await
            }
            Command::Subscription(after) => {
                apart(async move {
                    Ok(Reply::Subscription(
                        self.catalogue
                            .next_subscription(*after)
                            .map_err(cat_error)?,
                    ))
                })
                .await
            }
            Command::SyncStart(id, url) => {
                apart(async move {
                    if self.download.is_some() || self.sync.is_some() {
                        return Err(Error::Invalid);
                    }
                    let prior = self
                        .catalogue
                        .sync_checkpoint(*id, scratch)
                        .map_err(cat_error)?;
                    self.sync
                        .session
                        .restart(*id, url, prior)
                        .map_err(|_| Error::Invalid)?;
                    self.sync.active = true;
                    Ok(Reply::Done)
                })
                .await
            }
            Command::SyncConditionals => {
                apart(async move {
                    let validators = self.sync.get().ok_or(Error::Invalid)?.conditional();
                    Ok(Reply::Conditionals {
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
                    })
                })
                .await
            }
            Command::SyncHeaders {
                status,
                etag,
                modified,
            } => {
                apart(async move {
                    self.sync
                        .get_mut()
                        .ok_or(Error::Invalid)?
                        .response(
                            *status,
                            Validators {
                                etag: (!etag.is_empty()).then_some(etag.as_str()),
                                last_modified: (!modified.is_empty()).then_some(modified.as_str()),
                            },
                        )
                        .map_err(sync_error)?;
                    Ok(Reply::Done)
                })
                .await
            }
            Command::SyncChunk(bytes) => {
                apart(async move {
                    Ok(Reply::Consumed(
                        self.sync
                            .get_mut()
                            .ok_or(Error::Invalid)?
                            .push(&mut self.catalogue, bytes, scratch)
                            .map_err(sync_error)?,
                    ))
                })
                .await
            }
            Command::SyncEnd => {
                apart(async move {
                    let sync = self.sync.get_mut().ok_or(Error::Invalid)?;
                    let reply = match sync.end_page() {
                        Ok(true) => String::try_from(sync.url())
                            .map(Reply::Url)
                            .map_err(|_| Error::Invalid),
                        Ok(false) => sync
                            .complete(&mut self.catalogue, scratch)
                            .map(Reply::Synced)
                            .map_err(sync_error),
                        Err(error) => Err(sync_error(error)),
                    };
                    // Only a next page keeps the session.
                    if !matches!(reply, Ok(Reply::Url(_))) {
                        self.sync.active = false;
                    }
                    reply
                })
                .await
            }
            Command::NextJob(after) => {
                apart(async move {
                    let mut job = None;
                    let page = self
                        .catalogue
                        .scan(*after, scratch, |episode| {
                            if job.is_none()
                                && !episode.state.read
                                && episode.state.audio == AudioState::Queued
                            {
                                job = DownloadJob::from_episode(episode).ok();
                            }
                        })
                        .map_err(cat_error)?;
                    // A scan may contain several queued episodes. Resume after the
                    // emitted job, not after records the caller has not received.
                    let after = job.as_ref().map(|job| job.id).or(page.resume_after);
                    let done = job.is_none() && !page.has_more;
                    Ok(Reply::Job { job, after, done })
                })
                .await
            }
            Command::DownloadStart(job) => {
                apart(async move {
                    if self.download.is_some() || self.sync.is_some() {
                        return Err(Error::Invalid);
                    }
                    Ok(
                        match DownloadSession::start(
                            &mut self.catalogue,
                            &mut self.media,
                            &mut self.reclaim,
                            job,
                            scratch,
                        )
                        .await
                        .map_err(media_error::<M>)?
                        {
                            Start::Ready(session) => {
                                self.download = Some(session);
                                Reply::Ready
                            }
                            Start::Pending => Reply::Pending,
                            Start::WaitingForSpace => {
                                Reply::Downloaded(DownloadOutcome::WaitingForSpace)
                            }
                            Start::Skip => Reply::Downloaded(DownloadOutcome::RetryLater),
                        },
                    )
                })
                .await
            }
            Command::DownloadHeaders {
                status,
                length,
                partial,
            } => {
                apart(async move {
                    self.download
                        .as_mut()
                        .ok_or(Error::Invalid)?
                        .response(*status, *length, partial.then_some("partial"))
                        .map_err(|_| Error::Invalid)?;
                    Ok(Reply::Done)
                })
                .await
            }
            Command::DownloadChunk(bytes) => {
                apart(async move {
                    Ok(
                        match self
                            .download
                            .as_mut()
                            .ok_or(Error::Invalid)?
                            .push(
                                &mut self.catalogue,
                                &mut self.media,
                                &mut self.reclaim,
                                bytes,
                                scratch,
                            )
                            .await
                            .map_err(media_error::<M>)?
                        {
                            ChunkOutcome::Written => Reply::Done,
                            ChunkOutcome::Pending => Reply::Pending,
                            ChunkOutcome::WaitingForSpace => {
                                Reply::Downloaded(DownloadOutcome::WaitingForSpace)
                            }
                        },
                    )
                })
                .await
            }
            Command::DownloadEnd(complete) => {
                apart(async move {
                    Ok(Reply::Downloaded(
                        self.download
                            .take()
                            .ok_or(Error::Invalid)?
                            .finish(&mut self.catalogue, &mut self.media, *complete, scratch)
                            .await
                            .map_err(media_error::<M>)?,
                    ))
                })
                .await
            }
            Command::Abort => {
                apart(async move {
                    self.sync.active = false;
                    if let Some(download) = self.download.take() {
                        download
                            .abort(&mut self.catalogue, &mut self.media, scratch)
                            .await
                            .map_err(media_error::<M>)?;
                    }
                    Ok(Reply::Done)
                })
                .await
            }
        }
    }
}
fn cat_error(error: catalogue::Error) -> Error {
    match error {
        catalogue::Error::FieldTooLong
        | catalogue::Error::InvalidMetadata
        | catalogue::Error::BufferTooSmall
        | catalogue::Error::NotFound
        | catalogue::Error::InvalidState => Error::Invalid,
        _ => Error::Storage,
    }
}
fn sync_error(error: SyncError) -> Error {
    match error {
        SyncError::Catalogue(e) => cat_error(e),
        _ => Error::Invalid,
    }
}
/// A non-fatal store error left the store enabled: reject the request only.
fn media_error<M: MediaStore>(error: MediaError<M::Error>) -> Error {
    match error {
        MediaError::Catalogue(e) => cat_error(e),
        MediaError::Media(e) if M::is_fatal(&e) => Error::Storage,
        _ => Error::Invalid,
    }
}
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
