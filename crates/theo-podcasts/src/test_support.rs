use crate::catalogue::Catalogue;
use std::{
    cell::Cell,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};
use theo_storage::page_store::{PageStore, SectorFile, file_len};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub(crate) struct Image {
    pub(crate) path: PathBuf,
    pages: usize,
    pub(crate) writes: Rc<Cell<usize>>,
    pub(crate) fail: Rc<Cell<bool>>,
    pub(crate) hang: Rc<Cell<bool>>,
    pub(crate) cut: Rc<Cell<Option<usize>>>,
    pub(crate) prefix: Rc<Cell<usize>>,
    pub(crate) sync_to_disk: Rc<Cell<bool>>,
}
impl Image {
    pub(crate) fn new() -> Self {
        Self::with_pages(32)
    }
    pub(crate) fn with_pages(pages: usize) -> Self {
        let path = std::env::temp_dir().join(std::format!(
            "theo-catalogue-{}-{}.db",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap()
            .set_len(file_len(pages))
            .unwrap();
        Self {
            path,
            pages,
            writes: Rc::new(Cell::new(0)),
            fail: Rc::new(Cell::new(false)),
            hang: Rc::new(Cell::new(false)),
            cut: Rc::new(Cell::new(None)),
            prefix: Rc::new(Cell::new(0)),
            sync_to_disk: Rc::new(Cell::new(true)),
        }
    }
    pub(crate) fn file(&self) -> HostFile {
        HostFile {
            file: OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.path)
                .unwrap(),
            writes: self.writes.clone(),
            fail: self.fail.clone(),
            hang: self.hang.clone(),
            cut: self.cut.clone(),
            prefix: self.prefix.clone(),
            sync_to_disk: self.sync_to_disk.clone(),
        }
    }
    pub(crate) async fn create(&self) -> Catalogue<PageStore<HostFile>> {
        Catalogue::create(PageStore::create(self.file(), self.pages).await.unwrap())
            .await
            .unwrap()
    }
    pub(crate) async fn reopen(&self) -> Catalogue<PageStore<HostFile>> {
        Catalogue::open(PageStore::open(self.file(), self.pages).await.unwrap())
            .await
            .unwrap()
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
pub(crate) struct HostFile {
    file: File,
    writes: Rc<Cell<usize>>,
    fail: Rc<Cell<bool>>,
    hang: Rc<Cell<bool>>,
    cut: Rc<Cell<Option<usize>>>,
    prefix: Rc<Cell<usize>>,
    sync_to_disk: Rc<Cell<bool>>,
}
impl SectorFile for HostFile {
    type Error = std::io::Error;
    fn byte_len(&self) -> u64 {
        self.file.metadata().unwrap().len()
    }
    async fn read_sector(&mut self, sector: u64, data: &mut [u8; 512]) -> Result<(), Self::Error> {
        self.file.seek(SeekFrom::Start(sector * 512))?;
        self.file.read_exact(data)
    }
    async fn write_sector(&mut self, sector: u64, data: &[u8; 512]) -> Result<(), Self::Error> {
        if self.hang.get() {
            core::future::pending::<()>().await;
        }
        if self.fail.get() {
            return Err(std::io::Error::other("injected write failure"));
        }
        let position = self.writes.get();
        self.writes.set(position + 1);
        self.file.seek(SeekFrom::Start(sector * 512))?;
        if self.cut.get() == Some(position) {
            self.file.write_all(&data[..self.prefix.get()])?;
            if self.sync_to_disk.get() {
                self.file.sync_all()?;
            }
            return Err(std::io::Error::other("simulated torn sector"));
        }
        self.file.write_all(data)
    }
    async fn sync(&mut self) -> Result<(), Self::Error> {
        if self.sync_to_disk.get() {
            self.file.sync_all()
        } else {
            Ok(())
        }
    }
}
