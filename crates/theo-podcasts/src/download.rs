//! Restart-only streaming audio publication.
use crate::{catalogue::*, media::*, reclaim::make_space};
use ekv::flash::Flash;
use heapless::String;
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
}
// Caller-owned bounded session: boxing would hide allocator/placement policy.
#[allow(clippy::large_enum_variant)]
pub enum Start {
    Ready(DownloadSession),
    WaitingForSpace,
    Skip,
}
#[derive(PartialEq, Eq)]
enum Phase {
    Headers,
    Body,
    Waiting,
    Rejected,
    Poisoned,
}
pub struct DownloadSession {
    job: DownloadJob,
    phase: Phase,
    bytes: u64,
    expected: Option<u64>,
}
impl DownloadSession {
    /// Restart only: discard partials before issuing an unconditional GET. Never
    /// issue Range/If-Range;200 therefore always begins a fresh file at offset0.
    pub async fn start<F: Flash, M: MediaStore>(
        cat: &mut Catalogue<F>,
        media: &mut M,
        job: DownloadJob,
        scratch: &mut [u8],
    ) -> Result<Start, MediaError<F::Error, M::Error>> {
        let episode = cat
            .get(job.id, scratch)
            .await
            .map_err(MediaError::Catalogue)?
            .ok_or(MediaError::InvalidInput)?;
        if episode.state.read || episode.state.audio != AudioState::Queued || media.active(job.id) {
            return Ok(Start::Skip);
        }
        if episode.metadata.enclosure != job.enclosure
            || episode.metadata.enclosure_bytes != job.expected_bytes
            || job.expected_bytes == Some(0)
        {
            return Err(MediaError::InvalidInput);
        }
        // Discard only this non-Available job's orphaned files first. Their
        // capacity must be recovered before considering playable read audio.
        media.unlink_temp(job.id).await.map_err(MediaError::Media)?;
        media
            .unlink_audio(job.id)
            .await
            .map_err(MediaError::Media)?;
        let required = job.expected_bytes.unwrap_or(AUDIO_CHUNK as u64);
        if !make_space(cat, media, required, scratch).await? {
            return Ok(Start::WaitingForSpace);
        }
        cat.transition_audio(job.id, AudioState::Downloading, scratch)
            .await
            .map_err(MediaError::Catalogue)?;
        media.reset_temp(job.id).await.map_err(MediaError::Media)?;
        let expected = job.expected_bytes;
        Ok(Start::Ready(Self {
            job,
            phase: Phase::Headers,
            bytes: 0,
            expected,
        }))
    }
    pub fn response(
        &mut self,
        status: u16,
        length: Option<u64>,
        content_range: Option<&str>,
    ) -> Result<(), DownloadOutcome> {
        if self.phase == Phase::Poisoned {
            return Err(DownloadOutcome::RetryLater);
        }
        let valid = self.phase == Phase::Headers;
        self.phase = Phase::Rejected;
        if !valid
            || status != 200
            || content_range.is_some()
            || length == Some(0)
            || length.zip(self.expected).is_some_and(|(a, b)| a != b)
        {
            return Err(DownloadOutcome::RetryLater);
        }
        self.expected = length.or(self.expected);
        self.phase = Phase::Body;
        Ok(())
    }
    /// Written consumes all input. WaitingForSpace consumes none: retain this
    /// exact chunk until a relevant space/state change or explicit retry.
    pub async fn push<F: Flash, M: MediaStore>(
        &mut self,
        cat: &mut Catalogue<F>,
        media: &mut M,
        bytes: &[u8],
        scratch: &mut [u8],
    ) -> Result<ChunkOutcome, MediaError<F::Error, M::Error>> {
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
        let required = if self.expected.is_some() {
            bytes.len() as u64
        } else {
            AUDIO_CHUNK as u64
        };
        if !make_space(cat, media, required, scratch).await? {
            self.phase = Phase::Waiting;
            return Ok(ChunkOutcome::WaitingForSpace);
        }
        media
            .append_temp(self.job.id, bytes)
            .await
            .map_err(MediaError::Media)?;
        self.bytes = end;
        self.phase = Phase::Body;
        Ok(ChunkOutcome::Written)
    }
    /// Ordinary HTTP/network failure can retry without reboot. A storage-failed
    /// or cancelled session is left to mount invalidation and startup recovery.
    pub async fn abort<F: Flash, M: MediaStore>(
        self,
        cat: &mut Catalogue<F>,
        media: &mut M,
        scratch: &mut [u8],
    ) -> Result<DownloadOutcome, MediaError<F::Error, M::Error>> {
        if self.phase == Phase::Poisoned {
            return Ok(DownloadOutcome::RetryLater);
        }
        if media.active(self.job.id) {
            return Err(MediaError::InvalidState);
        }
        crate::recovery::recover_one(cat, media, self.job.id, scratch).await?;
        Ok(DownloadOutcome::RetryLater)
    }
    /// `body_complete` must come from successful transport framing, never an
    /// arbitrary socket EOF. A failed/short/stalled body remains non-Available.
    pub async fn finish<F: Flash, M: MediaStore>(
        self,
        cat: &mut Catalogue<F>,
        media: &mut M,
        body_complete: bool,
        scratch: &mut [u8],
    ) -> Result<DownloadOutcome, MediaError<F::Error, M::Error>> {
        if self.phase != Phase::Body
            || !body_complete
            || self.bytes == 0
            || self.expected.is_some_and(|n| n != self.bytes)
        {
            return self.abort(cat, media, scratch).await;
        }
        let episode = cat
            .get(self.job.id, scratch)
            .await
            .map_err(MediaError::Catalogue)?
            .ok_or(MediaError::InvalidState)?;
        if episode.state.audio != AudioState::Downloading
            || episode.metadata.enclosure != self.job.enclosure
            || episode.metadata.enclosure_bytes != self.job.expected_bytes
            || media.active(self.job.id)
        {
            return self.abort(cat, media, scratch).await;
        }
        let bytes = media
            .sync_temp(self.job.id)
            .await
            .map_err(MediaError::Media)?;
        if bytes != self.bytes {
            return self.abort(cat, media, scratch).await;
        }
        media
            .publish_temp(self.job.id)
            .await
            .map_err(MediaError::Media)?;
        cat.finish_download(self.job.id, bytes, scratch)
            .await
            .map_err(MediaError::Catalogue)?;
        Ok(DownloadOutcome::Complete)
    }
}
#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
