use crate::{catalogue::EpisodeId, media::*};
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
    pub after_publish: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    pub after_unlink: Option<std::rc::Rc<std::cell::Cell<bool>>>,
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
    async fn space(&mut self) -> io::Result<Space> {
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
    async fn reset_temp(&mut self, id: EpisodeId) -> io::Result<()> {
        self.operation(|this| {
            File::create(this.path(id, true))?;
            Ok(())
        })
    }
    async fn append_temp(&mut self, id: EpisodeId, bytes: &[u8]) -> io::Result<()> {
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
    async fn sync_temp(&mut self, id: EpisodeId) -> io::Result<u64> {
        self.operation(|this| {
            let f = File::open(this.path(id, true))?;
            if this.physical_sync {
                f.sync_all()?;
            }
            Ok(f.metadata()?.len())
        })
    }
    async fn publish_temp(&mut self, id: EpisodeId) -> io::Result<()> {
        let result = self.operation(|this| {
            if this.path(id, false).exists() {
                return Err(io::Error::other("refuse overwrite"));
            }
            fs::rename(this.path(id, true), this.path(id, false))?;
            this.sync_dir()
        });
        if result.is_ok()
            && let Some(flag) = &self.after_publish
        {
            flag.set(true);
        }
        result
    }
    async fn unlink_temp(&mut self, id: EpisodeId) -> io::Result<()> {
        self.operation(|this| this.unlink(id, true))
    }
    async fn unlink_audio(&mut self, id: EpisodeId) -> io::Result<()> {
        let result = self.operation(|this| this.unlink(id, false));
        if result.is_ok()
            && let Some(flag) = &self.after_unlink
        {
            flag.set(true);
        }
        result
    }
    async fn audio_len(&mut self, id: EpisodeId) -> io::Result<Option<u64>> {
        self.operation(|this| match fs::metadata(this.path(id, false)) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
    }
    async fn list_ids(
        &mut self,
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
pub(crate) async fn available(
    cat: &mut crate::catalogue::Catalogue<
        theo_storage::page_store::PageStore<crate::test_support::HostFile>,
    >,
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
        .await
        .unwrap()
        .id;
    cat.transition_audio(id, AudioState::Downloading, scratch)
        .await
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
        .await
        .unwrap();
    if read {
        cat.mark_read(id, scratch).await.unwrap();
    }
    id
}
