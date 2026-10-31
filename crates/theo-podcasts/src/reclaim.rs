//! Space-pressure-only reclamation policy.
use crate::catalogue::{AudioState, EpisodeId};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReclaimCandidate {
    pub id: EpisodeId,
    pub read: bool,
    pub audio: AudioState,
    pub published: Option<i64>,
    pub import_order: u64,
    pub active: bool,
    pub bytes: u64,
}
pub fn choose_reclaim(
    candidates: impl IntoIterator<Item = ReclaimCandidate>,
    deficit: u64,
) -> Option<ReclaimCandidate> {
    if deficit == 0 {
        return None;
    }
    candidates
        .into_iter()
        .filter(|c| c.read && c.audio == AudioState::Available && !c.active)
        .min_by_key(|c| {
            (
                c.published.is_none(),
                c.published.unwrap_or(0),
                c.import_order,
                c.id.0,
            )
        })
}
#[cfg(test)]
#[path = "reclaim_tests.rs"]
mod tests;
use crate::{
    catalogue::Catalogue,
    media::{MediaError, MediaStore},
};
use ekv::flash::Flash;
/// Make room for one actual pending append/download. No clock or purge path.
pub async fn make_space<F: Flash, M: MediaStore>(
    cat: &mut Catalogue<F>,
    media: &mut M,
    required: u64,
    scratch: &mut [u8],
) -> Result<bool, MediaError<F::Error, M::Error>> {
    use crate::catalogue::AudioState;
    use crate::media::reserve;
    if required == 0 {
        return Ok(true);
    }
    loop {
        let space = media.space().await.map_err(MediaError::Media)?;
        if space.free > space.capacity {
            return Err(MediaError::InvalidState);
        }
        let Some(target) = reserve(space.capacity)
            .checked_add(required)
            .filter(|&n| n <= space.capacity)
        else {
            return Ok(false);
        };
        let deficit = target.saturating_sub(space.free);
        if deficit == 0 {
            return Ok(true);
        }
        let mut after = None;
        let mut best = None;
        loop {
            let page = cat
                .scan(after, scratch, |episode| {
                    let candidate = ReclaimCandidate {
                        id: episode.id,
                        read: episode.state.read,
                        audio: episode.state.audio,
                        published: episode.metadata.published,
                        import_order: episode.id.0,
                        active: media.active(episode.id),
                        bytes: 0,
                    };
                    best = choose_reclaim(
                        best.into_iter().chain(core::iter::once(candidate)),
                        deficit,
                    );
                })
                .await
                .map_err(MediaError::Catalogue)?;
            if !page.has_more {
                break;
            }
            after = page.resume_after;
        }
        let Some(candidate) = best else {
            return Ok(false);
        };
        if media.active(candidate.id) {
            return Ok(false);
        }
        cat.transition_audio(candidate.id, AudioState::Deleting, scratch)
            .await
            .map_err(MediaError::Catalogue)?;
        // Lease admission for Deleting is forbidden by the storage-owner contract.
        if media.active(candidate.id) {
            return Ok(false);
        }
        media
            .unlink_audio(candidate.id)
            .await
            .map_err(MediaError::Media)?;
        cat.transition_audio(candidate.id, AudioState::Reclaimed, scratch)
            .await
            .map_err(MediaError::Catalogue)?;
    }
}
