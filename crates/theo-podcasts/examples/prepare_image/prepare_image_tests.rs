use super::*;
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "theopod-image-{}-{}.img",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
#[test]
fn new_image_publishes_only_initialized_catalogue_and_media() {
    // Regression (t-110): descriptors must reopen a real catalogue and LittleFS.
    let path = path();
    create_image(&path, 262_144).unwrap();
    let mut io = ImageIo::open(&path).unwrap();
    let mut first = [0; 512];
    let mut second = [0; 512];
    theo_storage::littlefs::SectorIo::read_sector(&mut io, 0, &mut first).unwrap();
    theo_storage::littlefs::SectorIo::read_sector(&mut io, 8, &mut second).unwrap();
    let layout = theo_storage::card_layout::CardLayout::select(&first, &second, 262_144)
        .unwrap()
        .unwrap();
    assert_eq!(layout.pages(), 4096);
    let (db, media) = theo_storage::regions::split(io, layout);
    futures::executor::block_on(async {
        let pages = theo_storage::page_store::PageStore::open(
            theo_storage::regions::AsyncRegion::new(db),
            layout.pages(),
        )
        .await
        .unwrap();
        let mut cat = theo_podcasts::catalogue::Catalogue::open(pages)
            .await
            .unwrap();
        assert!(cat.next_subscription(None).await.unwrap().is_none());
    });
    let geometry = theo_storage::littlefs::Geometry::new(layout.media().sectors() as u64).unwrap();
    let fs = littlefs_rust::Filesystem::mount(
        theo_storage::littlefs::SectorStorage::new(media, geometry),
        geometry.config(),
    )
    .map_err(|(e, _)| e)
    .unwrap();
    assert_eq!(fs.stat("/theo.log").unwrap().size, 0);
    fs.unmount().unwrap();
    fs::remove_file(path).unwrap();
}
#[test]
fn existing_file_and_bad_capacity_are_preserved() {
    // Regression (t-110): provisioning never overwrites files, symlinks or devices.
    let path = path();
    fs::write(&path, b"do not overwrite").unwrap();
    assert!(create_image(&path, 262_144).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"do not overwrite");
    fs::remove_file(&path).unwrap();
    assert!(create_image(&path, 1).is_err());
    assert!(!path.exists());
}
