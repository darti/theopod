//! Recovery by persisted local identity, never by remote URL or filename.
use crate::{catalogue::*, media::*};
use ekv::flash::Flash;
pub async fn recover_one<F: Flash, M: MediaStore>(
    cat: &mut Catalogue<F>,
    media: &mut M,
    id: EpisodeId,
    scratch: &mut [u8],
) -> Result<(), MediaError<F::Error, M::Error>> {
    if media.active(id) {
        return Ok(());
    }
    let episode = cat.get(id, scratch).await.map_err(MediaError::Catalogue)?;
    let state = episode.map(|e| e.state);
    if state.is_some_and(|s| s.audio == AudioState::Available) {
        let expected = cat
            .completed_audio_len(id)
            .await
            .map_err(MediaError::Catalogue)?;
        let actual = media.audio_len(id).await.map_err(MediaError::Media)?;
        if expected.is_some() && expected == actual {
            media.unlink_temp(id).await.map_err(MediaError::Media)?;
            return Ok(());
        }
    }
    if state.is_some_and(|s| s.audio == AudioState::Deleting && !s.read) {
        return Err(MediaError::InvalidState);
    }
    media.unlink_temp(id).await.map_err(MediaError::Media)?;
    media.unlink_audio(id).await.map_err(MediaError::Media)?;
    let Some(state) = state else {
        return Ok(());
    };
    match state.audio {
        AudioState::Deleting => {
            cat.transition_audio(id, AudioState::Reclaimed, scratch)
                .await
                .map_err(MediaError::Catalogue)?;
        }
        AudioState::Reclaimed => (),
        AudioState::Queued if !state.read => (),
        _ => {
            cat.transition_audio(id, AudioState::Missing, scratch)
                .await
                .map_err(MediaError::Catalogue)?;
            if !state.read {
                cat.transition_audio(id, AudioState::Queued, scratch)
                    .await
                    .map_err(MediaError::Catalogue)?;
            }
        }
    }
    Ok(())
}
pub async fn recover_all<F: Flash, M: MediaStore>(
    cat: &mut Catalogue<F>,
    media: &mut M,
    scratch: &mut [u8],
) -> Result<(), MediaError<F::Error, M::Error>> {
    let mut after = None;
    loop {
        let mut ids = heapless::Vec::<EpisodeId, MAX_SCAN_RECORDS>::new();
        let page = cat
            .scan(after, scratch, |e| {
                let _ = ids.push(e.id);
            })
            .await
            .map_err(MediaError::Catalogue)?;
        for id in ids {
            recover_one(cat, media, id, scratch).await?;
        }
        if !page.has_more {
            break;
        }
        after = page.resume_after;
    }
    // Include orphan files without DB entries; persisted intent may have failed
    // before first file creation, so DB traversal alone is insufficient.
    let mut after = None;
    let mut ids = [EpisodeId(0); FILE_PAGE];
    loop {
        let n = media
            .list_ids(after, &mut ids)
            .await
            .map_err(MediaError::Media)?;
        if n > FILE_PAGE {
            return Err(MediaError::InvalidState);
        }
        if n == 0 {
            break;
        }
        for &id in &ids[..n] {
            if after.is_some_and(|a| id.0 <= a.0) {
                return Err(MediaError::InvalidState);
            }
            recover_one(cat, media, id, scratch).await?;
            after = Some(id);
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
