//! ID-addressed media operations supplied by the unique storage owner.
use crate::catalogue::{Catalogue, EpisodeId};
use ekv::flash::Flash;
pub const FILE_PAGE: usize = 16;
pub const AUDIO_CHUNK: usize = 4096;
#[derive(Clone, Copy, Debug)]
pub struct Space {
    pub capacity: u64,
    pub free: u64,
}
/// The greater of16MiB and1% (rounded up); never borrowed by unknown-length audio.
pub fn reserve(capacity: u64) -> u64 {
    (16 * 1024 * 1024).max(capacity / 100 + u64::from(!capacity.is_multiple_of(100)))
}
/// Methods are serialized by the existing storage owner. No server-provided
/// path enters this interface. Methods receive the unique catalogue so a store
/// may persist its own accounting in the same database; they never begin a
/// competing writer. A media error or cancelled operation must poison the
/// podcast scope (ADR-0020) until reboot, just like SD logging I/O.
/// Active leases must not be admitted for Downloading/Deleting records.
#[allow(async_fn_in_trait)]
pub trait MediaStore<F: Flash> {
    type Error;
    fn active(&self, id: EpisodeId) -> bool;
    async fn space(&mut self, cat: &mut Catalogue<F>) -> Result<Space, Self::Error>;
    async fn reset_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error>;
    async fn append_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;
    async fn sync_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<u64, Self::Error>;
    /// Publish only a previously synced temporary file and durably sync its
    /// directory/metadata before returning. Never overwrite an available file.
    async fn publish_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error>;
    /// Idempotent durable unlink, including directory/metadata synchronization.
    async fn unlink_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error>;
    async fn unlink_audio(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error>;
    async fn audio_len(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<Option<u64>, Self::Error>;
    /// Bounded sorted unique local IDs present as temporary/final files. Zero
    /// means end; resume strictly after the last returned ID. Never remote paths.
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
