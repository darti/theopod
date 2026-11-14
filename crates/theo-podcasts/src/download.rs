//! Streaming audio publication, resumable within one boot.
//!
//! A download starts from byte 0 with an unconditional GET, unless the
//! previous attempt of the same job failed on the network mid-body and
//! [`Resume`] kept its partial: it then continues from there with `Range`
//! and `If-Range` (t-156, ADR-0023). A `206` must carry exactly the range
//! asked for; a `200` restarts from 0 in the same response.
use crate::{
    catalogue::*,
    media::*,
    network_policy::ContentRange,
    reclaim::{Reclaim, SpaceStep, make_space},
    resume::Resume,
};
use heapless::String;
use theo_storage::future::apart;
#[derive(Clone, Debug)]
pub struct DownloadJob {
    pub id: EpisodeId,
    pub enclosure: String<MAX_URL_BYTES>,
    pub expected_bytes: Option<u64>,
}
#[derive(Debug)]
pub struct InvalidJob;
impl DownloadJob {
    pub fn from_episode(episode: Episode<'_>) -> Result<Self, InvalidJob> {
        if episode.id.0 == 0 || episode.metadata.enclosure.is_empty() {
            return Err(InvalidJob);
        }
        Ok(Self {
            id: episode.id,
            enclosure: String::try_from(episode.metadata.enclosure).map_err(|_| InvalidJob)?,
            expected_bytes: episode.metadata.enclosure_bytes,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadOutcome {
    Complete,
    WaitingForSpace,
    RetryLater,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkOutcome {
    Written,
    WaitingForSpace,
    /// Space is being made: send the same chunk again.
    Pending,
}
#[expect(
    clippy::large_enum_variant,
    reason = "caller-owned bounded session: boxing would hide allocator/placement policy"
)]
pub enum Start {
    Ready(DownloadSession),
    WaitingForSpace,
    /// One bounded step done (a commit or a reclaim scan): start the same
    /// job again.
    Pending,
    Skip,
}
/// What a start step decided; [`DownloadSession::start`] builds the session.
enum Admit {
    Ready {
        from: u64,
        expected: Option<u64>,
        resumes: u8,
        capacity: u64,
    },
    Pending,
    WaitingForSpace,
    Skip,
}
#[derive(PartialEq, Eq)]
enum Phase {
    Headers,
    Body,
    /// A resume was answered with the whole representation: the next push
    /// discards the partial first.
    Restart,
    Waiting,
    Rejected,
    /// The announced length exceeds the media capacity; abort marks the
    /// episode `Missing` instead of re-queuing it.
    Oversize,
    /// The body grew past the media capacity: the episode is already
    /// `Missing` and its units freed, so abort and finish do nothing more.
    Refused,
    Poisoned,
}
pub struct DownloadSession {
    job: DownloadJob,
    phase: Phase,
    /// The `Range` start: the kept bytes resumed, 0 for the whole body.
    from: u64,
    /// Resumes made for this job, this one included.
    resumes: u8,
    bytes: u64,
    expected: Option<u64>,
    /// Media capacity at start: no episode can be larger.
    capacity: u64,
}
/// An episode larger than the whole media region can never be stored: free
/// what it owns and set it `Missing`, so it is not retried.
async fn refuse_oversize<M: MediaStore>(
    cat: &mut Catalogue,
    media: &mut M,
    id: EpisodeId,
    scratch: &mut [u8],
) -> Result<(), MediaError<M::Error>> {
    cat.begin_batch().map_err(MediaError::Catalogue)?;
    let result = async {
        media
            .unlink_temp(cat, id)
            .await
            .map_err(MediaError::Media)?;
        cat.transition_audio(id, AudioState::Missing, scratch)
            .map_err(MediaError::Catalogue)
    }
    .await;
    end_batch::<M, _>(cat, result)
}
impl DownloadSession {
    /// Resume the partial `resume` kept for this unchanged job (see
    /// [`Self::from`]), or discard partials and start from byte 0.
    ///
    /// Each call makes at most one catalogue commit (ADR-0022): discarding
    /// a kept partial or this job's files, one reclaim step, or the start
    /// itself. Repeat with the same job, `reclaim` and `resume` while it
    /// returns [`Start::Pending`].
    pub async fn start<M: MediaStore>(
        cat: &mut Catalogue,
        media: &mut M,
        reclaim: &mut Reclaim,
        resume: &mut Resume,
        job: &DownloadJob,
        scratch: &mut [u8],
    ) -> Result<Start, MediaError<M::Error>> {
        // The steps return a small `Admit`, so the 2 KB session is built
        // once, here: each awaited `Start` would add its own copy to the
        // caller's frame on core1 (t-133).
        Ok(
            match apart(Self::admit(cat, media, reclaim, resume, job, scratch)).await? {
                Admit::Ready {
                    from,
                    expected,
                    resumes,
                    capacity,
                } => Start::Ready(Self {
                    job: job.clone(),
                    phase: Phase::Headers,
                    from,
                    resumes,
                    bytes: from,
                    expected,
                    capacity,
                }),
                Admit::Pending => Start::Pending,
                Admit::WaitingForSpace => Start::WaitingForSpace,
                Admit::Skip => Start::Skip,
            },
        )
    }
    /// One bounded step of [`Self::start`].
    async fn admit<M: MediaStore>(
        cat: &mut Catalogue,
        media: &mut M,
        reclaim: &mut Reclaim,
        resume: &mut Resume,
        job: &DownloadJob,
        scratch: &mut [u8],
    ) -> Result<Admit, MediaError<M::Error>> {
        if let Some(kept) = resume.kept() {
            let resumable = kept == job.id
                && cat
                    .get(job.id, scratch)
                    .map_err(MediaError::Catalogue)?
                    .is_some_and(|episode| {
                        resume.can_resume(&episode, media)
                            && episode.metadata.enclosure == job.enclosure
                            && episode.metadata.enclosure_bytes == job.expected_bytes
                    });
            if resumable {
                // Back to `Downloading` (one commit), temp untouched.
                let capacity = apart(media.space(cat))
                    .await
                    .map_err(MediaError::Media)?
                    .capacity;
                cat.transition_audio(job.id, AudioState::Downloading, scratch)
                    .map_err(MediaError::Catalogue)?;
                resume.take();
                return Ok(Admit::Ready {
                    from: resume.bytes,
                    expected: Some(resume.total),
                    resumes: resume.resumes.saturating_add(1),
                    capacity,
                });
            }
            let commits = cat.commits();
            apart(resume.discard(cat, media, scratch)).await?;
            if cat.commits() != commits {
                return Ok(Admit::Pending);
            }
        }
        let episode = cat
            .get(job.id, scratch)
            .map_err(MediaError::Catalogue)?
            .ok_or(MediaError::InvalidInput)?;
        if episode.state.read || episode.state.audio != AudioState::Queued || media.active(job.id) {
            return Ok(Admit::Skip);
        }
        if episode.metadata.enclosure != job.enclosure
            || episode.metadata.enclosure_bytes != job.expected_bytes
            || job.expected_bytes == Some(0)
        {
            return Err(MediaError::InvalidInput);
        }
        // Refuse before any unlink, eviction or allocation (t-121).
        // Each step below is polled in a frame of its own (core1, t-133).
        let capacity = apart(media.space(cat))
            .await
            .map_err(MediaError::Media)?
            .capacity;
        if job.expected_bytes.is_some_and(|n| n > capacity) {
            cat.transition_audio(job.id, AudioState::Missing, scratch)
                .map_err(MediaError::Catalogue)?;
            return Err(MediaError::InvalidInput);
        }
        // Discard only this non-Available job's orphaned files first. Their
        // capacity must be recovered before considering playable read audio.
        let commits = cat.commits();
        cat.begin_batch().map_err(MediaError::Catalogue)?;
        let result = apart(async {
            media
                .unlink_temp(cat, job.id)
                .await
                .map_err(MediaError::Media)?;
            media
                .unlink_audio(cat, job.id)
                .await
                .map_err(MediaError::Media)
        })
        .await;
        end_batch::<M, _>(cat, result)?;
        if cat.commits() != commits {
            return Ok(Admit::Pending);
        }
        let required = job.expected_bytes.unwrap_or(AUDIO_CHUNK as u64);
        match apart(make_space(cat, media, reclaim, required, scratch)).await? {
            SpaceStep::Ready => (),
            SpaceStep::Pending => return Ok(Admit::Pending),
            SpaceStep::Unavailable => return Ok(Admit::WaitingForSpace),
        }
        let expected = job.expected_bytes;
        // Reserve first: its `Full` and `Invalid` refusals change nothing,
        // so they leave the batch empty and the owner enabled.
        cat.begin_batch().map_err(MediaError::Catalogue)?;
        let result = apart(async {
            media
                .reset_temp(cat, job.id, expected)
                .await
                .map_err(MediaError::Media)?;
            cat.transition_audio(job.id, AudioState::Downloading, scratch)
                .map_err(MediaError::Catalogue)
        })
        .await;
        end_batch::<M, _>(cat, result)?;
        Ok(Admit::Ready {
            from: 0,
            expected,
            resumes: 0,
            capacity,
        })
    }
    /// Where the request's `Range` starts: 0 asks for the whole body.
    pub fn from(&self) -> u64 {
        self.from
    }
    /// Check the response to this session's request. `range` is its
    /// parsed `Content-Range`; `validator` its
    /// [`crate::network_policy::range_validator`], kept in `resume` from a
    /// whole response so that a later failure can resume.
    pub fn response(
        &mut self,
        status: u16,
        length: Option<u64>,
        range: Option<ContentRange>,
        validator: Option<&str>,
        resume: &mut Resume,
    ) -> Result<(), DownloadOutcome> {
        if self.phase == Phase::Poisoned {
            return Err(DownloadOutcome::RetryLater);
        }
        let valid = self.phase == Phase::Headers;
        self.phase = Phase::Rejected;
        if !valid {
            return Err(DownloadOutcome::RetryLater);
        }
        if self.from > 0 && status == 206 {
            // Exactly the kept bytes' tail of the same representation.
            let total = self.expected.ok_or(DownloadOutcome::RetryLater)?;
            let asked = ContentRange {
                first: self.from,
                last: total - 1,
                total,
            };
            if range != Some(asked) || length.is_some_and(|n| n != total - self.from) {
                return Err(DownloadOutcome::RetryLater);
            }
            self.phase = Phase::Body;
            return Ok(());
        }
        if self.from > 0 {
            // A whole response replaces the kept representation: only the
            // feed's length binds the new one.
            self.expected = self.job.expected_bytes;
        }
        if status != 200
            || range.is_some()
            || length == Some(0)
            || length.zip(self.expected).is_some_and(|(a, b)| a != b)
        {
            return Err(DownloadOutcome::RetryLater);
        }
        if length.is_some_and(|n| n > self.capacity) {
            self.phase = Phase::Oversize;
            return Err(DownloadOutcome::RetryLater);
        }
        resume.set_validator(validator);
        self.expected = length.or(self.expected);
        // A resume answered in full (If-Range did not match): start over.
        self.phase = if self.from > 0 {
            Phase::Restart
        } else {
            Phase::Body
        };
        self.from = 0;
        self.resumes = 0;
        Ok(())
    }
    /// Whether a resume was answered in full: call [`Self::restart`]
    /// before the first [`Self::push`].
    pub fn restarting(&self) -> bool {
        self.phase == Phase::Restart
    }
    /// Discard the kept bytes before the whole body: one commit. Kept out
    /// of `push` so that the store's reset is not polled under push's
    /// frame on core1 (t-133).
    pub async fn restart<M: MediaStore>(
        &mut self,
        cat: &mut Catalogue,
        media: &mut M,
    ) -> Result<(), MediaError<M::Error>> {
        if self.phase != Phase::Restart {
            return Err(MediaError::InvalidInput);
        }
        self.phase = Phase::Poisoned;
        let expected = self.expected;
        cat.begin_batch().map_err(MediaError::Catalogue)?;
        let result = media
            .reset_temp(cat, self.job.id, expected)
            .await
            .map_err(MediaError::Media);
        end_batch::<M, _>(cat, result)?;
        self.bytes = 0;
        self.phase = Phase::Body;
        Ok(())
    }
    /// Written consumes all input. WaitingForSpace consumes none: retain this
    /// exact chunk until a relevant space/state change or explicit retry.
    /// Pending consumes none either: one reclaim step was made, send the
    /// same chunk again with the same `reclaim`.
    pub async fn push<M: MediaStore>(
        &mut self,
        cat: &mut Catalogue,
        media: &mut M,
        reclaim: &mut Reclaim,
        bytes: &[u8],
        scratch: &mut [u8],
    ) -> Result<ChunkOutcome, MediaError<M::Error>> {
        let valid = matches!(self.phase, Phase::Body | Phase::Waiting);
        self.phase = Phase::Poisoned;
        if !valid || bytes.is_empty() || bytes.len() > AUDIO_CHUNK {
            return Err(MediaError::InvalidInput);
        }
        let end = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or(MediaError::InvalidInput)?;
        if self.expected.is_some_and(|n| end > n) {
            return Err(MediaError::InvalidInput);
        }
        if end > self.capacity {
            refuse_oversize(cat, media, self.job.id, scratch).await?;
            self.phase = Phase::Refused;
            return Err(MediaError::InvalidInput);
        }
        let required = if self.expected.is_some() {
            bytes.len() as u64
        } else {
            AUDIO_CHUNK as u64
        };
        match apart(make_space(cat, media, reclaim, required, scratch)).await? {
            SpaceStep::Ready => (),
            SpaceStep::Pending => {
                self.phase = Phase::Body;
                return Ok(ChunkOutcome::Pending);
            }
            SpaceStep::Unavailable => {
                self.phase = Phase::Waiting;
                return Ok(ChunkOutcome::WaitingForSpace);
            }
        }
        media
            .append_temp(cat, self.job.id, bytes)
            .await
            .map_err(MediaError::Media)?;
        self.bytes = end;
        self.phase = Phase::Body;
        Ok(ChunkOutcome::Written)
    }
    /// Ordinary HTTP/network failure can retry without reboot: mid-body,
    /// with a validator and bytes written, the partial is kept in `resume`
    /// and the episode is `Queued` again (one commit); otherwise it is
    /// recovered. A storage-failed or cancelled session is left to mount
    /// invalidation and startup recovery.
    pub async fn abort<M: MediaStore>(
        self,
        cat: &mut Catalogue,
        media: &mut M,
        resume: &mut Resume,
        scratch: &mut [u8],
    ) -> Result<DownloadOutcome, MediaError<M::Error>> {
        // Mid-body with bytes written, or a resume that failed before its
        // response (reconnect, TLS, 429): that one does not count.
        let resumes = match self.phase {
            Phase::Body if self.bytes > 0 => Some(self.resumes),
            Phase::Headers if self.from > 0 => Some(self.resumes.saturating_sub(1)),
            _ => None,
        };
        if let Some(resumes) = resumes
            && let Some(total) = self.expected
            && self.bytes < total
            && resume.validator().is_some()
            && !media.active(self.job.id)
            && media.temp_len(self.job.id) == Some(self.bytes)
        {
            // Read meanwhile: only recovery may move it (to `Missing`).
            let downloading = cat
                .get(self.job.id, scratch)
                .map_err(MediaError::Catalogue)?
                .is_some_and(|e| !e.state.read && e.state.audio == AudioState::Downloading);
            if downloading {
                cat.transition_audio(self.job.id, AudioState::Queued, scratch)
                    .map_err(MediaError::Catalogue)?;
                resume.keep(&self.job, self.bytes, total, resumes);
                return Ok(DownloadOutcome::RetryLater);
            }
        }
        self.discard(cat, media, scratch).await
    }
    /// Recover the job's files and state as before resume existed.
    async fn discard<M: MediaStore>(
        self,
        cat: &mut Catalogue,
        media: &mut M,
        scratch: &mut [u8],
    ) -> Result<DownloadOutcome, MediaError<M::Error>> {
        if matches!(self.phase, Phase::Poisoned | Phase::Refused) {
            return Ok(DownloadOutcome::RetryLater);
        }
        if media.active(self.job.id) {
            return Err(MediaError::InvalidState);
        }
        if self.phase == Phase::Oversize {
            refuse_oversize(cat, media, self.job.id, scratch).await?;
            return Ok(DownloadOutcome::RetryLater);
        }
        crate::recovery::recover_one(cat, media, self.job.id, scratch).await?;
        Ok(DownloadOutcome::RetryLater)
    }
    /// `body_complete` must come from successful transport framing, never an
    /// arbitrary socket EOF. A failed/short/stalled body remains non-Available.
    pub async fn finish<M: MediaStore>(
        self,
        cat: &mut Catalogue,
        media: &mut M,
        body_complete: bool,
        scratch: &mut [u8],
    ) -> Result<DownloadOutcome, MediaError<M::Error>> {
        if self.phase != Phase::Body
            || !body_complete
            || self.bytes == 0
            || self.expected.is_some_and(|n| n != self.bytes)
        {
            return self.discard(cat, media, scratch).await;
        }
        let episode = cat
            .get(self.job.id, scratch)
            .map_err(MediaError::Catalogue)?
            .ok_or(MediaError::InvalidState)?;
        if episode.state.audio != AudioState::Downloading
            || episode.metadata.enclosure != self.job.enclosure
            || episode.metadata.enclosure_bytes != self.job.expected_bytes
            || media.active(self.job.id)
        {
            return self.discard(cat, media, scratch).await;
        }
        let bytes = media
            .sync_temp(cat, self.job.id)
            .await
            .map_err(MediaError::Media)?;
        if bytes != self.bytes {
            return self.discard(cat, media, scratch).await;
        }
        // Publication and the Available state are one commit (ADR-0022).
        cat.begin_batch().map_err(MediaError::Catalogue)?;
        let result = async {
            media
                .publish_temp(cat, self.job.id)
                .await
                .map_err(MediaError::Media)?;
            cat.finish_download(self.job.id, bytes, scratch)
                .map_err(MediaError::Catalogue)
        }
        .await;
        end_batch::<M, _>(cat, result)?;
        Ok(DownloadOutcome::Complete)
    }
}
#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
