use super::*;
use crate::page_store::{PageStore, file_len};
use alloc::vec;
use futures::executor::block_on;
use hadris_fat::{FatKind, FatOptions, async_::format, embedded::MountToken};
use hadris_storage::{BlockSize, MemDevice};

// Regression guard: real FAT32 I/O must preserve file length and log neighbours across database updates.
#[test]
fn page_store_reopens_on_fat32_without_growing_file() {
    block_on(async {
        let mut bytes = vec![0u8; 64 << 20];
        let mut device = MemDevice::new(bytes.as_mut_slice(), BlockSize::new(512).unwrap());
        format(&mut device, &FatOptions::new().with_kind(FatKind::Fat32))
            .await
            .unwrap();
        let mut token = MountToken::new();
        let mut volume = Fat::<_, 4>::mount(device, &mut token).await.unwrap();
        let log = volume
            .open(
                volume.root(),
                "theo.log",
                OpenOptions::new().write().create_new(),
            )
            .await
            .unwrap();
        volume.write(&log, b"keep log").await.unwrap();
        volume.close(log).await.unwrap();
        let dbfile = volume
            .open(
                volume.root(),
                "podcast.db",
                OpenOptions::new().write().create_new(),
            )
            .await
            .unwrap();
        volume.set_len(&dbfile, file_len(32)).await.unwrap();
        volume.close(dbfile).await.unwrap();
        volume.sync().await.unwrap();
        let file = FatPageFile::open(&mut volume, "podcast.db").await.unwrap();
        let mut store = PageStore::create(file, 32).await.unwrap();
        store.replace(0, 0, b"test").await.unwrap();
        store.into_inner().close().await.unwrap();
        assert_eq!(
            volume
                .metadata(volume.root(), "podcast.db")
                .await
                .unwrap()
                .len(),
            file_len(32)
        );
        let device = volume.unmount().await.unwrap();
        let mut token = MountToken::new();
        let mut volume = Fat::<_, 4>::mount(device, &mut token).await.unwrap();
        let file = FatPageFile::open(&mut volume, "podcast.db").await.unwrap();
        let mut store = PageStore::open(file, 32).await.unwrap();
        let mut data = [0; 4];
        store.read_bytes(0, 0, &mut data).await.unwrap();
        assert_eq!(&data, b"test");
        store.into_inner().close().await.unwrap();
        let log = volume
            .open(volume.root(), "theo.log", OpenOptions::new().read())
            .await
            .unwrap();
        let mut data = [0; 8];
        assert_eq!(volume.read(&log, &mut data).await.unwrap(), 8);
        assert_eq!(&data, b"keep log");
        volume.close(log).await.unwrap();
    });
}
