//! ID-addressed media operations supplied by the unique storage owner.
use crate::catalogue::{Catalogue, EpisodeId};
pub const FILE_PAGE: usize = 16;
pub const AUDIO_CHUNK: usize = 4096;
/// Media capacity and free bytes, as `MediaStore::space` reports them.
#[derive(Clone, Copy, Debug)]
pub struct Space {
    /// Bytes the store can hold in total.
    pub capacity: u64,
    /// Bytes still available, including the unwritten part of space already
    /// owned by the in-progress temporary audio.
    pub free: u64,
}
/// The greater of 16 MiB and 1% (rounded up); never borrowed by unknown-length audio.
pub fn reserve(capacity: u64) -> u64 {
    (16 * 1024 * 1024).max(capacity / 100 + u64::from(!capacity.is_multiple_of(100)))
}
/// ID-addressed audio storage for the podcast domain.
///
/// Implement this trait to give the download, reclaim and recovery paths a
/// place to stream, publish and delete episode audio by local id.
///
/// Methods are serialized by the existing storage owner. No server-provided
/// path enters this interface. Methods receive the unique catalogue so a store
/// may persist its own accounting in the same database; they never begin a
/// competing writer. A media error or cancelled operation must poison the
/// podcast scope (ADR-0020) until reboot, just like SD logging I/O.
/// Active leases must not be admitted for Downloading/Deleting records.
#[expect(
    async_fn_in_trait,
    reason = "implemented and awaited only inside this firmware's single-threaded storage owner"
)]
pub trait MediaStore {
    /// The store's error.
    type Error;
    /// Whether `error` may have left the store inconsistent, so it is now
    /// disabled. A non-fatal error was detected before any change: the
    /// store is unchanged and still enabled, and only the request failed.
    /// The default treats every error as fatal.
    fn is_fatal(error: &Self::Error) -> bool {
        let _ = error;
        true
    }
    /// Whether `id`'s audio is leased for playback.
    fn active(&self, id: EpisodeId) -> bool;
    /// Length of `id`'s temporary audio while it is the store's current
    /// one, from RAM without I/O; `None` otherwise. A partial kept for a
    /// same-boot resume continues from it (t-156).
    fn temp_len(&self, id: EpisodeId) -> Option<u64>;
    /// Capacity and free bytes.
    async fn space(&mut self, cat: &mut Catalogue) -> Result<Space, Self::Error>;
    /// Discard any temporary audio (of `id` or of the previous download) and
    /// start an empty one for `id`. `expected` is the announced length, if
    /// known: a store may reserve that much space up front. Refuses when
    /// published audio of `id` exists.
    async fn reset_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        expected: Option<u64>,
    ) -> Result<(), Self::Error>;
    /// Append at most `AUDIO_CHUNK` bytes to `id`'s temporary audio.
    async fn append_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;
    /// Make the temporary audio durable; returns its length.
    async fn sync_temp(&mut self, cat: &mut Catalogue, id: EpisodeId) -> Result<u64, Self::Error>;
    /// Durably turn a previously synced temporary audio into published audio
    /// before returning. Never overwrite published audio.
    async fn publish_temp(&mut self, cat: &mut Catalogue, id: EpisodeId)
    -> Result<(), Self::Error>;
    /// Durably delete `id`'s temporary audio; idempotent when absent.
    async fn unlink_temp(&mut self, cat: &mut Catalogue, id: EpisodeId) -> Result<(), Self::Error>;
    /// Durably delete `id`'s published audio; idempotent when absent.
    async fn unlink_audio(&mut self, cat: &mut Catalogue, id: EpisodeId)
    -> Result<(), Self::Error>;
    /// Length of `id`'s published audio, if any.
    async fn audio_len(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<Option<u64>, Self::Error>;
    /// Bounded sorted unique local IDs holding temporary or published audio.
    /// Zero means end; resume strictly after the last returned ID. Never
    /// remote paths.
    async fn list_ids(
        &mut self,
        cat: &mut Catalogue,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> Result<usize, Self::Error>;
}
#[derive(Debug)]
pub enum MediaError<M> {
    Catalogue(crate::catalogue::Error),
    Media(M),
    InvalidState,
    InvalidInput,
}

/// Close a batch begun with `Catalogue::begin_batch` around `result`: one
/// commit when it succeeded, or failed with an error detected before any
/// change (the earlier operations' writes are kept, as without a batch). A
/// failure to commit, or held writes dropped after a fatal error, is the
/// error (it disabled the catalogue).
pub(crate) fn end_batch<M: MediaStore, T>(
    cat: &mut Catalogue,
    result: Result<T, MediaError<M::Error>>,
) -> Result<T, MediaError<M::Error>> {
    let commit = match &result {
        Ok(_) | Err(MediaError::InvalidState | MediaError::InvalidInput) => true,
        Err(MediaError::Catalogue(e)) => !crate::catalogue::is_fatal(*e),
        Err(MediaError::Media(e)) => !M::is_fatal(e),
    };
    match (cat.end_batch(commit), result) {
        (Ok(()), result) => result,
        (Err(error), _) => Err(MediaError::Catalogue(error)),
    }
}
