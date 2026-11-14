use crate::{
    catalogue::{Catalogue, EpisodeId},
    media::*,
};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
pub(crate) struct HostMedia {
    pub root: PathBuf,
    pub capacity: u64,
    pub other_bytes: u64,
    pub active: BTreeSet<u64>,
    pub enabled: bool,
    pub steps: usize,
    pub fail_at: Option<usize>,
    pub physical_sync: bool,
    pub hang_append: bool,
    /// Catalogue disk whose writes start failing after a publish.
    pub after_publish: Option<theo_storage::test_disk::TestDisk>,
    /// Catalogue disk whose writes start failing after an audio unlink.
    pub after_unlink: Option<theo_storage::test_disk::TestDisk>,
}
impl HostMedia {
    pub fn new(root: PathBuf) -> Self {
        fs::create_dir(&root).unwrap();
        Self {
            root,
            capacity: 64 * 1024 * 1024,
            other_bytes: 0,
            active: BTreeSet::new(),
            enabled: true,
            steps: 0,
            fail_at: None,
            physical_sync: true,
            hang_append: false,
            after_publish: None,
            after_unlink: None,
        }
    }
    pub fn path(&self, id: EpisodeId, temp: bool) -> PathBuf {
        self.root.join(std::format!(
            "{:016x}.{}",
            id.0,
            if temp { "part" } else { "audio" }
        ))
    }
    pub fn reopen(&mut self) {
        self.enabled = true;
        self.fail_at = None;
        self.steps = 0;
        self.hang_append = false;
        self.after_publish = None;
        self.after_unlink = None;
    }
    fn operation<T>(&mut self, f: impl FnOnce(&Self) -> io::Result<T>) -> io::Result<T> {
        if !self.enabled {
            return Err(io::Error::other("disabled"));
        }
        self.enabled = false;
        self.boundary()?;
        let out = f(self)?;
        self.boundary()?;
        self.enabled = true;
        Ok(out)
    }
    fn boundary(&mut self) -> io::Result<()> {
        let at = self.steps;
        self.steps += 1;
        if self.fail_at == Some(at) {
            Err(io::Error::other("injected media error"))
        } else {
            Ok(())
        }
    }
    fn sync_dir(&self) -> io::Result<()> {
        if self.physical_sync {
            File::open(&self.root)?.sync_all()?;
        }
        Ok(())
    }
    fn unlink(&self, id: EpisodeId, temp: bool) -> io::Result<()> {
        match fs::remove_file(self.path(id, temp)) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        self.sync_dir()
    }
}
impl Drop for HostMedia {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl MediaStore for HostMedia {
    type Error = io::Error;
    fn active(&self, id: EpisodeId) -> bool {
        self.active.contains(&id.0)
    }
    fn temp_len(&self, id: EpisodeId) -> Option<u64> {
        fs::metadata(self.path(id, true)).ok().map(|m| m.len())
    }
    async fn space(&mut self, _: &mut Catalogue) -> io::Result<Space> {
        self.operation(|this| {
            let mut used = this.other_bytes;
            for entry in fs::read_dir(&this.root)? {
                used = used
                    .checked_add(entry?.metadata()?.len())
                    .ok_or_else(|| io::Error::other("overflow"))?;
            }
            Ok(Space {
                capacity: this.capacity,
                free: this.capacity.saturating_sub(used),
            })
        })
    }
    async fn reset_temp(
        &mut self,
        _: &mut Catalogue,
        id: EpisodeId,
        _: Option<u64>,
    ) -> io::Result<()> {
        self.operation(|this| {
            File::create(this.path(id, true))?;
            Ok(())
        })
    }
    async fn append_temp(
        &mut self,
        _: &mut Catalogue,
        id: EpisodeId,
        bytes: &[u8],
    ) -> io::Result<()> {
        if self.hang_append {
            self.enabled = false;
            core::future::pending::<()>().await;
        }
        self.operation(|this| {
            OpenOptions::new()
                .append(true)
                .open(this.path(id, true))?
                .write_all(bytes)
        })
    }
    async fn sync_temp(&mut self, _: &mut Catalogue, id: EpisodeId) -> io::Result<u64> {
        self.operation(|this| {
            let f = File::open(this.path(id, true))?;
            if this.physical_sync {
                f.sync_all()?;
            }
            Ok(f.metadata()?.len())
        })
    }
    async fn publish_temp(&mut self, _: &mut Catalogue, id: EpisodeId) -> io::Result<()> {
        let result = self.operation(|this| {
            if this.path(id, false).exists() {
                return Err(io::Error::other("refuse overwrite"));
            }
            fs::rename(this.path(id, true), this.path(id, false))?;
            this.sync_dir()
        });
        if result.is_ok()
            && let Some(disk) = &self.after_publish
        {
            disk.state().fail_writes = true;
        }
        result
    }
    async fn unlink_temp(&mut self, _: &mut Catalogue, id: EpisodeId) -> io::Result<()> {
        self.operation(|this| this.unlink(id, true))
    }
    async fn unlink_audio(&mut self, _: &mut Catalogue, id: EpisodeId) -> io::Result<()> {
        let result = self.operation(|this| this.unlink(id, false));
        if result.is_ok()
            && let Some(disk) = &self.after_unlink
        {
            disk.state().fail_writes = true;
        }
        result
    }
    async fn audio_len(&mut self, _: &mut Catalogue, id: EpisodeId) -> io::Result<Option<u64>> {
        self.operation(|this| match fs::metadata(this.path(id, false)) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
    }
    async fn list_ids(
        &mut self,
        _: &mut Catalogue,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> io::Result<usize> {
        self.operation(|this| {
            let mut ids = BTreeSet::new();
            for entry in fs::read_dir(&this.root)? {
                let name = entry?.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some((id, suffix)) = name.split_once('.') else {
                    continue;
                };
                if id.len() != 16 || !matches!(suffix, "part" | "audio") {
                    continue;
                }
                if let Ok(id) = u64::from_str_radix(id, 16)
                    && after.is_none_or(|a| id > a.0)
                {
                    ids.insert(id);
                }
            }
            let n = ids.len().min(FILE_PAGE);
            for (to, id) in out.iter_mut().zip(ids.into_iter().take(n)) {
                *to = EpisodeId(id);
            }
            Ok(n)
        })
    }
}
pub(crate) fn root_for(image: &crate::test_support::Image) -> PathBuf {
    Path::new(&image.path).with_extension("media")
}
pub(crate) fn available(
    cat: &mut crate::catalogue::Catalogue,
    media: &mut HostMedia,
    guid: &str,
    date: Option<i64>,
    read: bool,
    bytes: &[u8],
    scratch: &mut [u8],
) -> EpisodeId {
    use crate::catalogue::*;
    let id = cat
        .upsert(
            SubscriptionId(1),
            EpisodeMetadata {
                guid: Some(guid),
                title: guid,
                enclosure: "https://cdn.example/audio",
                description: None,
                published: date,
                enclosure_bytes: None,
                duration_seconds: None,
            },
            scratch,
        )
        .unwrap()
        .id;
    cat.transition_audio(id, AudioState::Downloading, scratch)
        .unwrap();
    fs::write(media.path(id, false), bytes).unwrap();
    if media.physical_sync {
        File::open(media.path(id, false))
            .unwrap()
            .sync_all()
            .unwrap();
        media.sync_dir().unwrap();
    }
    cat.finish_download(id, bytes.len() as u64, scratch)
        .unwrap();
    if read {
        cat.mark_read(id, scratch).unwrap();
    }
    id
}

/// Raw media device for `UnitMedia` tests: bounds-checked, counting, storing
/// sector contents only when `keep`.
pub(crate) struct Disk {
    pub sectors: std::collections::BTreeMap<u32, [u8; 512]>,
    pub keep: bool,
    pub reads: usize,
    pub writes: usize,
    /// Highest sector written, if any.
    pub highest: Option<u32>,
    pub limit: u32,
    /// `(first sector, sectors)` of each `write_sectors` call.
    pub batches: std::vec::Vec<(u32, usize)>,
}
impl Disk {
    pub fn counting(limit: u32) -> Self {
        Self {
            sectors: std::collections::BTreeMap::new(),
            keep: false,
            reads: 0,
            writes: 0,
            highest: None,
            limit,
            batches: std::vec::Vec::new(),
        }
    }
    pub fn storing(limit: u32) -> Self {
        Self {
            keep: true,
            ..Self::counting(limit)
        }
    }
}
impl theo_storage::littlefs::SectorIo for Disk {
    fn read_sector(
        &mut self,
        sector: u32,
        out: &mut [u8; 512],
    ) -> Result<(), littlefs_rust::Error> {
        if sector >= self.limit {
            return Err(littlefs_rust::Error::Io);
        }
        self.reads += 1;
        *out = self.sectors.get(&sector).copied().unwrap_or([0; 512]);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), littlefs_rust::Error> {
        if sector >= self.limit {
            return Err(littlefs_rust::Error::Io);
        }
        self.writes += 1;
        self.highest = self.highest.max(Some(sector));
        if self.keep {
            self.sectors.insert(sector, *data);
        }
        Ok(())
    }
    fn write_sectors(&mut self, first: u32, data: &[u8]) -> Result<(), littlefs_rust::Error> {
        self.batches.push((first, data.len() / 512));
        for (i, sector) in data.as_chunks::<512>().0.iter().enumerate() {
            self.write_sector(first + i as u32, sector)?;
        }
        Ok(())
    }
    fn sync(&mut self) -> Result<(), littlefs_rust::Error> {
        Ok(())
    }
    fn check(&mut self) -> Result<(), littlefs_rust::Error> {
        Ok(())
    }
}
// Lets a test keep the disk across a simulated reboot.
impl theo_storage::littlefs::SectorIo for &mut Disk {
    fn read_sector(
        &mut self,
        sector: u32,
        out: &mut [u8; 512],
    ) -> Result<(), littlefs_rust::Error> {
        (**self).read_sector(sector, out)
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), littlefs_rust::Error> {
        (**self).write_sector(sector, data)
    }
    fn write_sectors(&mut self, first: u32, data: &[u8]) -> Result<(), littlefs_rust::Error> {
        (**self).write_sectors(first, data)
    }
    fn sync(&mut self) -> Result<(), littlefs_rust::Error> {
        Ok(())
    }
    fn check(&mut self) -> Result<(), littlefs_rust::Error> {
        Ok(())
    }
}

/// Repeat one-step `make_space` until it settles: true when space is free.
pub(crate) async fn make_space_all<M: MediaStore>(
    cat: &mut Catalogue,
    media: &mut M,
    required: u64,
    scratch: &mut [u8],
) -> Result<bool, MediaError<M::Error>> {
    use crate::reclaim::{Reclaim, SpaceStep, make_space};
    let mut reclaim = Reclaim::new();
    loop {
        match make_space(cat, media, &mut reclaim, required, scratch).await? {
            SpaceStep::Ready => return Ok(true),
            SpaceStep::Unavailable => return Ok(false),
            SpaceStep::Pending => (),
        }
    }
}

/// Repeat `DownloadSession::start` while it is pending, as the worker does.
pub(crate) async fn start_all<M: MediaStore>(
    cat: &mut Catalogue,
    media: &mut M,
    job: crate::download::DownloadJob,
    scratch: &mut [u8],
) -> Result<crate::download::Start, MediaError<M::Error>> {
    start_with(cat, media, &mut crate::resume::Resume::new(), job, scratch).await
}

/// `start_all` that may resume the partial `resume` kept.
pub(crate) async fn start_with<M: MediaStore>(
    cat: &mut Catalogue,
    media: &mut M,
    resume: &mut crate::resume::Resume,
    job: crate::download::DownloadJob,
    scratch: &mut [u8],
) -> Result<crate::download::Start, MediaError<M::Error>> {
    use crate::download::{DownloadSession, Start};
    let mut reclaim = crate::reclaim::Reclaim::new();
    loop {
        match DownloadSession::start(cat, media, &mut reclaim, resume, &job, scratch).await? {
            Start::Pending => (),
            start => return Ok(start),
        }
    }
}

/// Upsert an unread queued episode `guid` and return its download job.
pub(crate) fn queued(
    cat: &mut crate::catalogue::Catalogue,
    guid: &str,
    expected: Option<u64>,
    scratch: &mut [u8],
) -> crate::download::DownloadJob {
    let id = cat
        .upsert(
            crate::catalogue::SubscriptionId(1),
            crate::catalogue::EpisodeMetadata {
                guid: Some(guid),
                title: guid,
                enclosure: "https://cdn.example/audio",
                description: None,
                published: Some(1),
                enclosure_bytes: expected,
                duration_seconds: None,
            },
            scratch,
        )
        .unwrap()
        .id;
    crate::download::DownloadJob::from_episode(cat.get(id, scratch).unwrap().unwrap()).unwrap()
}
