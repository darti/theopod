use crate::regions::*;
use alloc::{collections::BTreeMap, rc::Rc, vec, vec::Vec};
use core::{
    cell::RefCell,
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;
use littlefs_rust::{Error, Filesystem};
use theo_storage::{
    LogBuffer,
    card_layout::CardLayout,
    littlefs::{Geometry, SectorIo, SectorStorage},
    page_store::{PageStore, SectorFile},
};

#[derive(Default)]
struct Image {
    sectors: BTreeMap<u32, [u8; 512]>,
    calls: Vec<u32>,
    fail: bool,
    syncs: usize,
    mutations: usize,
    cut: Option<usize>,
    prefix: usize,
}
struct Disk(Rc<RefCell<Image>>);
impl SectorIo for Disk {
    fn read_sector(&mut self, s: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        let mut d = self.0.borrow_mut();
        d.calls.push(s);
        if d.fail {
            return Err(Error::Io);
        }
        *out = d.sectors.get(&s).copied().unwrap_or([0xff; 512]);
        Ok(())
    }
    fn write_sector(&mut self, s: u32, data: &[u8; 512]) -> Result<(), Error> {
        let mut d = self.0.borrow_mut();
        d.calls.push(s);
        if d.fail {
            return Err(Error::Io);
        }
        let cut = d.cut == Some(d.mutations);
        d.mutations += 1;
        if cut {
            let n = d.prefix;
            if n > 0 {
                d.sectors.entry(s).or_insert([0xff; 512])[..n].copy_from_slice(&data[..n]);
            }
            return Err(Error::Io);
        }
        d.sectors.insert(s, *data);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error> {
        let mut d = self.0.borrow_mut();
        d.syncs += 1;
        let cut = d.cut == Some(d.mutations);
        d.mutations += 1;
        if d.fail || cut {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}
fn setup() -> (
    RegionIo<Disk>,
    RegionIo<Disk>,
    Rc<RefCell<Image>>,
    CardLayout,
) {
    let image = Rc::new(RefCell::new(Image::default()));
    let layout = CardLayout::new(128 * 1024, 32).unwrap();
    let (db, media) = split(Disk(image.clone()), layout);
    (db, media, image, layout)
}
#[test]
fn translates_disjoint_regions_and_rejects_out_of_bounds_before_io() {
    // Regression (t-106): region adapters never cross the shared card's region bounds.
    let (mut db, mut media, image, layout) = setup();
    db.write_sector(0, &[0x42; 512]).unwrap();
    media.write_sector(0, &[0x24; 512]).unwrap();
    assert_eq!(
        image.borrow().calls,
        vec![layout.database().start(), layout.media().start()]
    );
    let calls = image.borrow().calls.len();
    assert_eq!(
        db.read_sector(layout.database().sectors(), &mut [0; 512]),
        Err(Error::Invalid)
    );
    assert_eq!(image.borrow().calls.len(), calls);
    media.read_sector(0, &mut [0; 512]).unwrap();
}
#[test]
fn error_on_one_region_disables_all_regions() {
    // Regression (t-106): a failed database operation cannot leave logging using the card.
    let (mut db, mut media, image, _) = setup();
    image.borrow_mut().fail = true;
    assert_eq!(db.write_sector(0, &[0; 512]), Err(Error::Io));
    image.borrow_mut().fail = false;
    let calls = image.borrow().calls.len();
    assert_eq!(media.read_sector(0, &mut [0; 512]), Err(Error::Io));
    assert_eq!(image.borrow().calls.len(), calls);
}
#[test]
fn async_sector_yields_only_after_releasing_the_shared_owner() {
    // Regression (t-106): pending DB work must permit a synchronous log operation between sectors.
    let (db, mut media, _, _) = setup();
    let mut file = AsyncRegion::new(db);
    let mut op = pin!(file.write_sector(0, &[1; 512]));
    assert!(matches!(
        op.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    media.write_sector(0, &[2; 512]).unwrap();
    assert!(matches!(
        op.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Ok(()))
    ));
}
#[test]
fn cancelled_sector_future_disables_both_regions() {
    // Regression (t-106): cancelling a pending DB sector disables the entire shared mount.
    let (db, mut media, image, _) = setup();
    let mut file = AsyncRegion::new(db);
    {
        let mut op = pin!(file.write_sector(0, &[1; 512]));
        assert!(matches!(
            op.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
    let calls = image.borrow().calls.len();
    assert_eq!(media.sync(), Err(Error::Io));
    assert_eq!(image.borrow().calls.len(), calls);
}
#[test]
fn deadline_hook_runs_before_every_sector_and_its_failure_poisons_session() {
    // Regression (t-106): resumed DB work must restore its own deadline after a log operation.
    let (db, mut media, image, _) = setup();
    let calls = Rc::new(core::cell::Cell::new(0));
    let seen = calls.clone();
    let mut file = AsyncRegion::with_before_io(db, move || {
        seen.set(seen.get() + 1);
        if seen.get() == 3 {
            Err(Error::Io)
        } else {
            Ok(())
        }
    });
    futures::executor::block_on(async {
        file.write_sector(0, &[1; 512]).await.unwrap();
        file.sync().await.unwrap();
        let before = image.borrow().calls.len();
        assert_eq!(file.read_sector(0, &mut [0; 512]).await, Err(Error::Io));
        assert_eq!(image.borrow().calls.len(), before);
    });
    assert_eq!(calls.get(), 3);
    assert_eq!(media.sync(), Err(Error::Io));
}
#[test]
fn actual_ekv_and_littlefs_share_one_device_and_reopen() {
    // Regression (t-106): actual engines use disjoint regions of one device and logs make progress during DB work.
    futures::executor::block_on(async {
        let (db_region, media, image, layout) = setup();
        let geometry = Geometry::new(layout.media().sectors() as u64).unwrap();
        let mut media = SectorStorage::new(media, geometry);
        Filesystem::format(&mut media, &geometry.config()).unwrap();
        let fs = Filesystem::mount(media, geometry.config())
            .map_err(|(e, _)| e)
            .unwrap();
        let store = PageStore::create(AsyncRegion::new(db_region), 32)
            .await
            .unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        db.format().await.unwrap();
        let log_progress = Rc::new(core::cell::Cell::new(0));
        let progress = log_progress.clone();
        let done = core::cell::Cell::new(false);
        let database = async {
            for version in 0u64..16 {
                let before = log_progress.get();
                let mut tx = db.write_transaction().await;
                tx.write(b"read", &version.to_le_bytes()).await.unwrap();
                tx.commit().await.unwrap();
                assert!(
                    log_progress.get() > before,
                    "logger must run between DB sectors"
                );
            }
            done.set(true);
        };
        let logger = async {
            while !done.get() {
                let mut log = LogBuffer::<64>::new();
                log.push(b"during DB\n");
                theo_storage::littlefs::append_log(&fs, &mut log, "/theo.log", "/theo.1.log")
                    .unwrap();
                progress.set(progress.get() + 1);
                embassy_futures::yield_now().await;
            }
        };
        futures::join!(database, logger);
        drop(db);
        fs.unmount().unwrap();
        let (db_region, media) = split(Disk(image), layout);
        let store = PageStore::open(AsyncRegion::new(db_region), 32)
            .await
            .unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        db.mount().await.unwrap();
        let mut out = [0; 8];
        assert_eq!(
            db.read_transaction()
                .await
                .read(b"read", &mut out)
                .await
                .unwrap(),
            8
        );
        assert_eq!(u64::from_le_bytes(out), 15);
        let fs = Filesystem::mount(SectorStorage::new(media, geometry), geometry.config())
            .map_err(|(e, _)| e)
            .unwrap();
        let tail = theo_storage::littlefs::log_tail(&fs, "/theo.log").unwrap();
        assert!(tail.size > 0);
        assert!(
            core::str::from_utf8(&tail.bytes[..tail.len])
                .unwrap()
                .contains("during DB")
        );
    });
}

#[test]
fn late_database_completion_after_media_failure_is_rejected() {
    // Regression (t-106): another region's failure must win over an already-started DB reply.
    let (db, mut media, image, _) = setup();
    let mut file = AsyncRegion::new(db);
    let mut op = pin!(SectorFile::sync(&mut file));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(op.as_mut().poll(&mut cx), Poll::Pending));
    image.borrow_mut().fail = true;
    assert_eq!(media.write_sector(0, &[2; 512]), Err(Error::Io));
    image.borrow_mut().fail = false;
    assert!(matches!(
        op.as_mut().poll(&mut cx),
        Poll::Ready(Err(Error::Io))
    ));
}

#[test]
fn legacy_mount_does_not_create_regions_or_write_descriptors() {
    // Regression (t-106): normal legacy discovery only reads; no implicit migration.
    let image = Rc::new(RefCell::new(Image::default()));
    let geometry = Geometry::new(128 * 1024).unwrap();
    let mut storage = SectorStorage::new(Disk(image.clone()), geometry);
    Filesystem::format(&mut storage, &geometry.config()).unwrap();
    let fs = Filesystem::mount(storage, geometry.config())
        .map_err(|(e, _)| e)
        .unwrap();
    let mut log = LogBuffer::<64>::new();
    log.push(b"legacy preserved\n");
    theo_storage::littlefs::append_log(&fs, &mut log, "/theo.log", "/theo.1.log").unwrap();
    fs.unmount().unwrap();
    let before = image.borrow().sectors.clone();
    let Discovered::Legacy(storage) = discover(Disk(image.clone()), 128 * 1024).ok().unwrap()
    else {
        panic!("legacy card incorrectly selected as partitioned")
    };
    let fs = Filesystem::mount(storage, geometry.config())
        .map_err(|(e, _)| e)
        .unwrap();
    assert!(
        theo_storage::littlefs::log_tail(&fs, "/theo.log")
            .unwrap()
            .size
            > 0
    );
    assert_eq!(image.borrow().sectors, before);
}

#[test]
fn explicit_image_preparation_publishes_both_real_engines_last() {
    // Regression (t-106): publication follows durable DB/schema and LittleFS initialization.
    futures::executor::block_on(async {
        let image = Rc::new(RefCell::new(Image::default()));
        let layout = CardLayout::new(128 * 1024, 32).unwrap();
        provision_new_image(Disk(image.clone()), layout)
            .await
            .unwrap();
        let Discovered::Regions(db_region, media, found) =
            discover(Disk(image.clone()), 128 * 1024).ok().unwrap()
        else {
            panic!("prepared image has no descriptor")
        };
        assert_eq!(found, layout);
        let db = ekv::Database::<_, NoopRawMutex>::new(
            PageStore::open(AsyncRegion::new(db_region), 32)
                .await
                .unwrap(),
            ekv::Config::default(),
        );
        db.mount().await.unwrap();
        let mut schema = [0; 4];
        assert_eq!(
            db.read_transaction()
                .await
                .read(b"schema", &mut schema)
                .await
                .unwrap(),
            4
        );
        assert_eq!(u32::from_le_bytes(schema), 1);
        let geometry = Geometry::new(layout.media().sectors() as u64).unwrap();
        let fs = Filesystem::mount(SectorStorage::new(media, geometry), geometry.config())
            .map_err(|(e, _)| e)
            .unwrap();
        assert!(
            theo_storage::littlefs::log_tail(&fs, "/theo.log")
                .unwrap()
                .size
                > 0
        );
    });
}

#[test]
fn image_preparation_rejects_existing_legacy_data_before_writing() {
    // Regression (t-106): an existing nonblank prefix is never authorization to replace a card.
    futures::executor::block_on(async {
        let image = Rc::new(RefCell::new(Image::default()));
        let layout = CardLayout::new(128 * 1024, 32).unwrap();
        image.borrow_mut().sectors.insert(0, [0x45; 512]);
        let before = image.borrow().sectors.clone();
        assert_eq!(
            provision_new_image(Disk(image.clone()), layout).await,
            Err(Error::Exists)
        );
        assert_eq!(image.borrow().sectors, before);
    });
}

async fn assert_discoverable_means_complete(image: Rc<RefCell<Image>>, layout: CardLayout) {
    let before = image.borrow().sectors.clone();
    match discover(Disk(image.clone()), 128 * 1024) {
        Ok(Discovered::Regions(db, media, found)) => {
            assert_eq!(found, layout);
            let db = ekv::Database::<_, NoopRawMutex>::new(
                PageStore::open(AsyncRegion::new(db), 32).await.unwrap(),
                ekv::Config::default(),
            );
            db.mount().await.unwrap();
            let mut schema = [0; 4];
            assert_eq!(
                db.read_transaction()
                    .await
                    .read(b"schema", &mut schema)
                    .await
                    .unwrap(),
                4
            );
            assert_eq!(u32::from_le_bytes(schema), 1);
            let geometry = Geometry::new(layout.media().sectors() as u64).unwrap();
            let fs = Filesystem::mount(SectorStorage::new(media, geometry), geometry.config())
                .map_err(|(e, _)| e)
                .unwrap();
            assert!(
                theo_storage::littlefs::log_tail(&fs, "/theo.log")
                    .unwrap()
                    .size
                    > 0
            );
        }
        Ok(Discovered::Legacy(storage)) => {
            let geometry = Geometry::new(128 * 1024).unwrap();
            assert!(Filesystem::mount(storage, geometry.config()).is_err());
        }
        Err(Error::Corrupt) => (),
        _ => panic!("unexpected discovery failure"),
    }
    // Both discovery and validation are read-only, even for an incomplete image.
    assert_eq!(image.borrow().sectors, before);
}

#[test]
fn every_preparation_write_and_sync_failure_keeps_unready_images_unpublished() {
    // Regression (t-106): a selected descriptor always refers to complete durable initialization.
    futures::executor::block_on(async {
        let layout = CardLayout::new(128 * 1024, 32).unwrap();
        let reference = Rc::new(RefCell::new(Image::default()));
        provision_new_image(Disk(reference.clone()), layout)
            .await
            .unwrap();
        let boundaries = reference.borrow().mutations;
        for prefix in [0, 217, 512] {
            for cut in 0..boundaries {
                let image = Rc::new(RefCell::new(Image {
                    cut: Some(cut),
                    prefix,
                    ..Image::default()
                }));
                assert!(
                    provision_new_image(Disk(image.clone()), layout)
                        .await
                        .is_err(),
                    "cut={cut} prefix={prefix}"
                );
                image.borrow_mut().cut = None;
                assert_discoverable_means_complete(image, layout).await;
            }
        }
        std::println!(
            "preparation publication: {boundaries} write/sync boundaries x 3 persisted prefixes"
        );
    });
}

#[test]
fn cancelled_preparation_never_publishes_a_descriptor() {
    // Regression (t-106): cancellation of a cooperative DB step cannot publish readiness.
    let layout = CardLayout::new(128 * 1024, 32).unwrap();
    let image = Rc::new(RefCell::new(Image::default()));
    let mut cx = Context::from_waker(Waker::noop());
    let mut total = 0;
    {
        let mut future = pin!(provision_new_image(Disk(image), layout));
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Pending => total += 1,
                Poll::Ready(result) => {
                    result.unwrap();
                    break;
                }
            }
        }
    }
    for skip in [0, 1, 8, 64, 128, total - 1] {
        let image = Rc::new(RefCell::new(Image::default()));
        {
            let mut future = pin!(provision_new_image(Disk(image.clone()), layout));
            for _ in 0..=skip {
                assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
            }
        }
        let first = image
            .borrow()
            .sectors
            .get(&0)
            .copied()
            .unwrap_or([0xff; 512]);
        let second = image
            .borrow()
            .sectors
            .get(&8)
            .copied()
            .unwrap_or([0xff; 512]);
        assert_eq!(CardLayout::select(&first, &second, 128 * 1024), Ok(None));
        futures::executor::block_on(assert_discoverable_means_complete(image, layout));
    }
    std::println!("preparation cancellation: 6 selected yield points out of {total}");
}
