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
    media::{MediaError, MediaStore, end_batch, reserve},
};

/// What one [`make_space`] step achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceStep {
    /// The space is free: go on, nothing was committed by this step.
    Ready,
    /// One commit made: repeat the request.
    Pending,
    /// Nothing more can be evicted: wait for a space change.
    Unavailable,
}

/// An eviction carried across requests, so each request makes at most one
/// commit: the `Deleting` intent of the policy's first evictable episode
/// (read from the `EVICTABLE` index, no scan), then its unlink and
/// `Reclaimed` in one commit (ADR-0022).
#[derive(Debug, Default)]
pub struct Reclaim {
    /// A committed `Deleting` intent; the next step unlinks its audio.
    deleting: Option<EpisodeId>,
}
impl Reclaim {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Make room for one actual pending append/download, one commit at a time:
/// repeat while it returns [`SpaceStep::Pending`]. No clock or purge path.
pub async fn make_space<M: MediaStore>(
    cat: &mut Catalogue,
    media: &mut M,
    reclaim: &mut Reclaim,
    required: u64,
    scratch: &mut [u8],
) -> Result<SpaceStep, MediaError<M::Error>> {
    if let Some(id) = reclaim.deleting.take() {
        // Lease admission for Deleting is forbidden by the storage-owner
        // contract; startup recovery finishes an intent left here.
        if media.active(id) {
            return Ok(SpaceStep::Unavailable);
        }
        cat.begin_batch().map_err(MediaError::Catalogue)?;
        let result = async {
            media
                .unlink_audio(cat, id)
                .await
                .map_err(MediaError::Media)?;
            cat.transition_audio(id, AudioState::Reclaimed, scratch)
                .map_err(MediaError::Catalogue)
        }
        .await;
        end_batch::<M, _>(cat, result)?;
        return Ok(SpaceStep::Pending);
    }
    if required == 0 {
        return Ok(SpaceStep::Ready);
    }
    let space = media.space(cat).await.map_err(MediaError::Media)?;
    if space.free > space.capacity {
        return Err(MediaError::InvalidState);
    }
    let Some(target) = reserve(space.capacity)
        .checked_add(required)
        .filter(|&n| n <= space.capacity)
    else {
        return Ok(SpaceStep::Unavailable);
    };
    if target <= space.free {
        return Ok(SpaceStep::Ready);
    }
    // The index holds exactly the read `Available` episodes, in the
    // policy's order (`choose_reclaim`); leased ones are skipped.
    let Some(id) = cat
        .first_evictable(|id| media.active(id))
        .map_err(MediaError::Catalogue)?
    else {
        return Ok(SpaceStep::Unavailable);
    };
    cat.transition_audio(id, AudioState::Deleting, scratch)
        .map_err(MediaError::Catalogue)?;
    reclaim.deleting = Some(id);
    Ok(SpaceStep::Pending)
}
