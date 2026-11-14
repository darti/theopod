use super::*;
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
/// Smallest convenient v3 card: database, log and 66 media units.
const CARD: u64 = 300_000;
fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "theopod-image-{}-{}.img",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
/// Open the image's catalogue region; the first open creates the tables.
fn open_catalogue<D: SectorIo + 'static>(
    db: D,
    layout: theo_storage::card_layout::CardLayout,
) -> theo_podcasts::catalogue::Catalogue {
    // SAFETY: the test uses the catalogue on its own thread only.
    let backend =
        unsafe { theo_storage::redb_backend::RegionBackend::new(db, layout.database().sectors()) }
            .unwrap();
    theo_podcasts::catalogue::Catalogue::open(backend).unwrap()
}
#[test]
fn new_image_publishes_only_initialized_catalogue_and_media() {
    // Regression (t-110): descriptors must reopen a real catalogue and LittleFS.
    let path = path();
    create_image(&path, CARD).unwrap();
    let mut io = ImageIo::open(&path).unwrap();
    let mut first = [0; 512];
    let mut second = [0; 512];
    theo_storage::littlefs::SectorIo::read_sector(&mut io, 0, &mut first).unwrap();
    theo_storage::littlefs::SectorIo::read_sector(&mut io, 8, &mut second).unwrap();
    let layout = theo_storage::card_layout::CardLayout::select(&first, &second, CARD)
        .unwrap()
        .unwrap();
    let (db, log, _media) = theo_storage::regions::split(io, layout);
    let mut cat = open_catalogue(db, layout);
    assert!(cat.next_subscription(None).unwrap().is_none());
    drop(cat);
    let geometry = theo_storage::littlefs::Geometry::new(layout.log().sectors() as u64).unwrap();
    let fs = littlefs_rust::Filesystem::mount(
        theo_storage::littlefs::SectorStorage::new(log, geometry),
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
    assert!(create_image(&path, CARD).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"do not overwrite");
    fs::remove_file(&path).unwrap();
    assert!(create_image(&path, 1).is_err());
    assert!(!path.exists());
}
#[test]
fn prepared_image_has_v3_log_region_and_empty_units() {
    // Regression (t-121): a fresh image mounts logs on the log region and opens an empty unit store.
    use theo_podcasts::media::MediaStore;
    use theo_storage::card_layout::{CardLayout, LOG_SECTORS};
    let path = path();
    create_image(&path, CARD).unwrap();
    let mut io = ImageIo::open(&path).unwrap();
    let mut first = [0; 512];
    let mut second = [0; 512];
    theo_storage::littlefs::SectorIo::read_sector(&mut io, 0, &mut first).unwrap();
    theo_storage::littlefs::SectorIo::read_sector(&mut io, 8, &mut second).unwrap();
    let layout = CardLayout::select(&first, &second, CARD).unwrap().unwrap();
    assert_eq!(layout.log().sectors(), LOG_SECTORS);
    let (db, log, media) = theo_storage::regions::split(io, layout);
    let geometry = theo_storage::littlefs::Geometry::new(layout.log().sectors() as u64).unwrap();
    let storage = theo_storage::littlefs::SectorStorage::new(log, geometry);
    let fs = theo_storage::littlefs::mount_for_writes(storage, geometry).unwrap();
    fs.unmount().unwrap();
    let mut cat = open_catalogue(db, layout);
    futures::executor::block_on(async {
        let mut units = theo_podcasts::unit_media::UnitMedia::open_in(
            &mut cat,
            media,
            layout.media_units(),
            allocator_api2::alloc::Global,
        )
        .unwrap();
        let space = units.space(&mut cat).await.unwrap();
        let bytes = layout.media_units() as u64 * theo_podcasts::unit_media::UNIT_BYTES;
        assert_eq!(space.capacity, bytes);
        assert_eq!(space.free, bytes);
    });
    fs::remove_file(path).unwrap();
}
