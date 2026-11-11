//! ID-only files on the filesystem held by the unique service owner.
use crate::{
    catalogue::{Catalogue, EpisodeId},
    media::*,
};
use alloc::rc::Rc;
use core::fmt::Write;
use ekv::flash::Flash;
use littlefs_rust::{Error, Filesystem, OpenFlags, Storage};
/// The Rc stays on the storage core; it permits log operations between domain
/// requests without constructing a second mount or exposing file handles.
pub struct LittlefsMedia<S: Storage> {
    pub fs: Rc<Filesystem<S>>,
    capacity: u64,
}
impl<S: Storage> LittlefsMedia<S> {
    pub fn new(fs: Rc<Filesystem<S>>, capacity: u64) -> Self {
        Self { fs, capacity }
    }
    fn path(id: EpisodeId, temp: bool) -> Result<heapless::String<32>, Error> {
        if id.0 == 0 {
            return Err(Error::Invalid);
        }
        let mut path = heapless::String::new();
        write!(
            path,
            "/{:016x}.{}",
            id.0,
            if temp { "part" } else { "audio" }
        )
        .map_err(|_| Error::Invalid)?;
        Ok(path)
    }
    fn unlink(&self, id: EpisodeId, temp: bool) -> Result<(), Error> {
        match self.fs.remove(&Self::path(id, temp)?) {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(e),
        }
    }
}
impl<S: Storage, F: Flash> MediaStore<F> for LittlefsMedia<S> {
    type Error = Error;
    fn active(&self, _: EpisodeId) -> bool {
        false
    } // Playback/leases are not yet implemented.
    async fn space(&mut self, _: &mut Catalogue<F>) -> Result<Space, Error> {
        let used = self.fs.fs_size()? as u64 * 4096;
        Ok(Space {
            capacity: self.capacity,
            free: self.capacity.saturating_sub(used),
        })
    }
    async fn reset_temp(&mut self, _: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Error> {
        self.fs
            .open(
                &Self::path(id, true)?,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNC,
            )?
            .close()
    }
    async fn append_temp(
        &mut self,
        _: &mut Catalogue<F>,
        id: EpisodeId,
        bytes: &[u8],
    ) -> Result<(), Error> {
        if bytes.is_empty() || bytes.len() > AUDIO_CHUNK {
            return Err(Error::Invalid);
        }
        let file = self
            .fs
            .open(&Self::path(id, true)?, OpenFlags::WRITE | OpenFlags::APPEND)?;
        if file.write(bytes)? as usize != bytes.len() {
            return Err(Error::Io);
        }
        file.close()
    }
    async fn sync_temp(&mut self, _: &mut Catalogue<F>, id: EpisodeId) -> Result<u64, Error> {
        let file = self
            .fs
            .open(&Self::path(id, true)?, OpenFlags::READ | OpenFlags::WRITE)?;
        file.sync()?;
        let len = file.size() as u64;
        file.close()?;
        Ok(len)
    }
    async fn publish_temp(&mut self, _: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Error> {
        let target = Self::path(id, false)?;
        match self.fs.stat(&target) {
            Err(Error::NoEntry) => (),
            Ok(_) => return Err(Error::Exists),
            Err(e) => return Err(e),
        }
        // littlefs rename/remove commit their metadata and sync the backend
        // before success; the card backend has no additional write cache.
        self.fs.rename(&Self::path(id, true)?, &target)
    }
    async fn unlink_temp(&mut self, _: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Error> {
        self.unlink(id, true)
    }
    async fn unlink_audio(&mut self, _: &mut Catalogue<F>, id: EpisodeId) -> Result<(), Error> {
        self.unlink(id, false)
    }
    async fn audio_len(
        &mut self,
        _: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<Option<u64>, Error> {
        match self.fs.stat(&Self::path(id, false)?) {
            Ok(meta) => Ok(Some(meta.size as u64)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(e),
        }
    }
    async fn list_ids(
        &mut self,
        _: &mut Catalogue<F>,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> Result<usize, Error> {
        let mut ids = heapless::Vec::<u64, FILE_PAGE>::new();
        let mut dir = self.fs.read_dir("/")?;
        for entry in dir.by_ref() {
            let entry = entry?;
            let Some((name, extension)) = entry.name.rsplit_once('.') else {
                continue;
            };
            if name.len() != 16 || !matches!(extension, "part" | "audio") {
                continue;
            }
            let Ok(id) = u64::from_str_radix(name, 16) else {
                continue;
            };
            if id == 0 || after.is_some_and(|a| id <= a.0) || ids.contains(&id) {
                continue;
            }
            let at = ids.partition_point(|other| *other < id);
            if at < FILE_PAGE {
                if ids.is_full() {
                    ids.pop();
                }
                ids.insert(at, id).map_err(|_| Error::Invalid)?;
            }
        }
        dir.close()?;
        for (out, id) in out.iter_mut().zip(ids.iter()) {
            *out = EpisodeId(*id);
        }
        Ok(ids.len())
    }
}
#[cfg(test)]
#[path = "littlefs_media_tests.rs"]
mod tests;
