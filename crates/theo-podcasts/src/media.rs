//! ID-addressed media operations supplied by the unique storage owner.
use crate::catalogue::{Catalogue, EpisodeId};
use ekv::flash::Flash;
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
pub trait MediaStore<F: Flash> {
    /// The store's error.
    type Error;
    /// Whether `id`'s audio is leased for playback.
    fn active(&self, id: EpisodeId) -> bool;
    /// Capacity and free bytes.
    async fn space(&mut self, cat: &mut Catalogue<F>) -> Result<Space, Self::Error>;
    /// Discard any temporary audio (of `id` or of the previous download) and
    /// start an empty one for `id`. `expected` is the announced length, if
    /// known: a store may reserve that much space up front. Refuses when
    /// published audio of `id` exists.
    async fn reset_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
        expected: Option<u64>,
    ) -> Result<(), Self::Error>;
    /// Append at most `AUDIO_CHUNK` bytes to `id`'s temporary audio.
    async fn append_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;
    /// Make the temporary audio durable; returns its length.
    async fn sync_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<u64, Self::Error>;
    /// Durably turn a previously synced temporary audio into published audio
    /// before returning. Never overwrite published audio.
    async fn publish_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error>;
    /// Durably delete `id`'s temporary audio; idempotent when absent.
    async fn unlink_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error>;
    /// Durably delete `id`'s published audio; idempotent when absent.
    async fn unlink_audio(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error>;
    /// Length of `id`'s published audio, if any.
    async fn audio_len(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<Option<u64>, Self::Error>;
    /// Bounded sorted unique local IDs holding temporary or published audio.
    /// Zero means end; resume strictly after the last returned ID. Never
    /// remote paths.
    async fn list_ids(
        &mut self,
        cat: &mut Catalogue<F>,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> Result<usize, Self::Error>;
}
#[derive(Debug)]
pub enum MediaError<C, M> {
    Catalogue(crate::catalogue::Error<C>),
    Media(M),
    InvalidState,
    InvalidInput,
}
