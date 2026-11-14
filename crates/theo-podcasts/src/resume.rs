//! Same-boot resume of an interrupted audio download (t-156, ADR-0023).
//!
//! An ordinary network failure in the body of a download whose response
//! carried a strong validator keeps the temporary audio, and [`Resume`]
//! remembers it in RAM. The next attempt of the same, unchanged job asks
//! for the rest with `Range: bytes=N-` and `If-Range`. Anything else that
//! happens first (another job starts, the episode changes or is read)
//! discards the partial through [`recover_one`], as before resume existed.
//! A reboot forgets the record, so boot recovery discards the partial.
use crate::{catalogue::*, download::DownloadJob, media::*, recovery::recover_one};
use heapless::String;
use sha2::{Digest, Sha256};

/// Resumes allowed per job before the next attempt restarts from zero.
pub const MAX_RESUMES: u8 = 5;

/// The kept partial, by local identity: no 2 KB enclosure copy.
struct Kept {
    id: EpisodeId,
    /// SHA-256 of the enclosure the partial was fetched from.
    enclosure: [u8; 32],
    /// The job's announced length when it was kept.
    expected: Option<u64>,
}

/// The partial download kept for a same-boot resume, and its validator.
///
/// Owned by the storage owner's runtime (in place, PSRAM) and passed to
/// [`crate::download::DownloadSession`] at start, response and abort.
pub struct Resume {
    kept: Option<Kept>,
    /// Bytes of the kept partial.
    pub(crate) bytes: u64,
    /// Total length of the representation the partial belongs to.
    pub(crate) total: u64,
    /// Resumes already made for the kept job.
    pub(crate) resumes: u8,
    /// `If-Range` validator of the current or kept download; empty: none.
    validator: String<512>,
}

impl Default for Resume {
    fn default() -> Self {
        Self::new()
    }
}

fn digest(enclosure: &str) -> [u8; 32] {
    Sha256::digest(enclosure.as_bytes()).into()
}

impl Resume {
    pub const fn new() -> Self {
        Self {
            kept: None,
            bytes: 0,
            total: 0,
            resumes: 0,
            validator: String::new(),
        }
    }
    /// The episode whose partial is kept.
    pub fn kept(&self) -> Option<EpisodeId> {
        self.kept.as_ref().map(|kept| kept.id)
    }
    /// The `If-Range` validator of the current or kept download.
    pub fn validator(&self) -> Option<&str> {
        (!self.validator.is_empty()).then_some(self.validator.as_str())
    }
    /// Record the validator of a whole (`200`) response; one too long for
    /// the buffer counts as none.
    pub(crate) fn set_validator(&mut self, validator: Option<&str>) {
        self.validator.clear();
        if let Some(validator) = validator
            && self.validator.push_str(validator).is_err()
        {
            self.validator.clear();
        }
    }
    /// Keep `job`'s partial of `bytes` out of `total`.
    pub(crate) fn keep(&mut self, job: &DownloadJob, bytes: u64, total: u64, resumes: u8) {
        self.kept = Some(Kept {
            id: job.id,
            enclosure: digest(&job.enclosure),
            expected: job.expected_bytes,
        });
        self.bytes = bytes;
        self.total = total;
        self.resumes = resumes;
    }
    /// Hand the kept partial to the session that resumes it.
    pub(crate) fn take(&mut self) {
        self.kept = None;
    }
    /// Whether the kept partial still resumes `episode`: same job, still
    /// queued and unread, temp intact, under [`MAX_RESUMES`].
    pub(crate) fn can_resume<M: MediaStore>(&self, episode: &Episode<'_>, media: &M) -> bool {
        self.kept.as_ref().is_some_and(|kept| {
            kept.id == episode.id
                && !episode.state.read
                && episode.state.audio == AudioState::Queued
                && kept.expected == episode.metadata.enclosure_bytes
                && kept.enclosure == digest(episode.metadata.enclosure)
                && self.resumes < MAX_RESUMES
                && !media.active(episode.id)
                && media.temp_len(episode.id) == Some(self.bytes)
        })
    }
    /// Forget the kept partial and recover its identity (one commit).
    pub(crate) async fn discard<M: MediaStore>(
        &mut self,
        cat: &mut Catalogue,
        media: &mut M,
        scratch: &mut [u8],
    ) -> Result<(), MediaError<M::Error>> {
        if let Some(kept) = self.kept.take() {
            recover_one(cat, media, kept.id, scratch).await?;
        }
        Ok(())
    }
    /// The kept job, if it can still resume; otherwise discard it.
    pub async fn offer<M: MediaStore>(
        &mut self,
        cat: &mut Catalogue,
        media: &mut M,
        scratch: &mut [u8],
    ) -> Result<Option<DownloadJob>, MediaError<M::Error>> {
        let Some(id) = self.kept() else {
            return Ok(None);
        };
        if let Some(episode) = cat.get(id, scratch).map_err(MediaError::Catalogue)?
            && self.can_resume(&episode, media)
            && let Ok(job) = DownloadJob::from_episode(episode)
        {
            return Ok(Some(job));
        }
        self.discard(cat, media, scratch).await?;
        Ok(None)
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
