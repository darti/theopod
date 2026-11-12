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

/// Episode pages one reclaim step scans. Two pages of about 1 KiB records
/// measured at most 128 reads at the scale target, within a request's IO
/// budget of 250 commands (`scale_tests`, ADR-0022).
pub const RECLAIM_SCAN_PAGES: usize = 2;

/// What one [`make_space`] step achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceStep {
    /// The space is free: go on, nothing was committed by this step.
    Ready,
    /// Work done (a bounded scan, or one commit): repeat the request.
    Pending,
    /// Nothing more can be evicted: wait for a space change.
    Unavailable,
}

/// An eviction round carried across requests, so each request reads a
/// bounded number of sectors and makes at most one commit.
///
/// The round scans every episode from import order, keeping the policy's
/// best candidate ([`choose_reclaim`]), then commits its `Deleting` intent,
/// then unlinks its audio and marks it `Reclaimed` in one commit. Records
/// change between requests: the chosen candidate is checked again before
/// eviction (a stale one restarts the round), but an episode that becomes
/// eligible behind the cursor waits for the next round, so it may be evicted
/// after a newer one (ADR-0022).
#[derive(Debug, Default)]
pub struct Reclaim {
    after: Option<EpisodeId>,
    best: Option<ReclaimCandidate>,
    scanned: bool,
    /// A committed `Deleting` intent; the next step unlinks its audio.
    deleting: Option<EpisodeId>,
}
impl Reclaim {
    pub fn new() -> Self {
        Self::default()
    }
    /// Start the next scan from the first episode.
    fn restart(&mut self) {
        self.after = None;
        self.best = None;
        self.scanned = false;
    }
}

/// Make room for one actual pending append/download, one bounded step at a
/// time: repeat while it returns [`SpaceStep::Pending`]. No clock or purge
/// path.
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
        reclaim.restart();
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
        reclaim.restart();
        return Ok(SpaceStep::Unavailable);
    };
    let deficit = target.saturating_sub(space.free);
    if deficit == 0 {
        reclaim.restart();
        return Ok(SpaceStep::Ready);
    }
    if !reclaim.scanned {
        for _ in 0..RECLAIM_SCAN_PAGES {
            let mut best = reclaim.best;
            let page = cat
                .scan(reclaim.after, scratch, |episode| {
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
                .map_err(MediaError::Catalogue)?;
            reclaim.best = best;
            reclaim.after = page.resume_after.or(reclaim.after);
            if !page.has_more {
                reclaim.scanned = true;
                break;
            }
        }
        if reclaim.scanned && reclaim.best.is_none() {
            reclaim.restart();
            return Ok(SpaceStep::Unavailable);
        }
        // Evict in the next step: these reads and a commit could exceed
        // the request's IO budget together.
        return Ok(SpaceStep::Pending);
    }
    let candidate = reclaim.best.take();
    reclaim.restart();
    let Some(candidate) = candidate else {
        return Ok(SpaceStep::Unavailable);
    };
    // Scanned in earlier requests: check it is still eligible.
    let eligible = cat
        .get(candidate.id, scratch)
        .map_err(MediaError::Catalogue)?
        .is_some_and(|e| e.state.read && e.state.audio == AudioState::Available);
    if !eligible || media.active(candidate.id) {
        return Ok(SpaceStep::Pending);
    }
    cat.transition_audio(candidate.id, AudioState::Deleting, scratch)
        .map_err(MediaError::Catalogue)?;
    reclaim.deleting = Some(candidate.id);
    Ok(SpaceStep::Pending)
}
