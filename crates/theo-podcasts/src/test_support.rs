use crate::catalogue::Catalogue;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use theo_storage::{
    card_layout::DATABASE_SECTORS,
    redb_backend::RegionBackend,
    test_disk::{Crash, TestDisk},
};

/// The real 64 MiB catalogue region; `TestDisk` stores only written sectors.
pub(crate) const SECTORS: u32 = DATABASE_SECTORS;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A provisioned catalogue region on an in-memory disk.
pub(crate) struct Image {
    pub(crate) disk: TestDisk,
    /// Unique per image: host media stores derive their directory from it.
    pub(crate) path: PathBuf,
}
impl Image {
    pub(crate) fn new() -> Self {
        let mut disk = TestDisk::new(SECTORS);
        RegionBackend::format(&mut disk).unwrap();
        let path = std::env::temp_dir().join(std::format!(
            "theo-catalogue-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        Self { disk, path }
    }
    pub(crate) fn backend(&self) -> RegionBackend<TestDisk> {
        // SAFETY: host tests use each catalogue on one thread only.
        unsafe { RegionBackend::new(self.disk.clone(), SECTORS) }.unwrap()
    }
    /// The first open of an empty region creates the catalogue.
    pub(crate) fn create(&self) -> Catalogue {
        Catalogue::open(self.backend()).unwrap()
    }
    pub(crate) fn reopen(&self) -> Catalogue {
        Catalogue::open(self.backend()).unwrap()
    }
    /// Power loss: writes after the last sync are lost.
    pub(crate) fn crash(&self) {
        self.disk.crash(Crash::DropPending);
    }
}
