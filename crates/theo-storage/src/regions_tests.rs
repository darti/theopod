use super::*;
use crate::{
    LogBuffer,
    card_layout::CardLayout,
    littlefs::{Geometry, SectorIo, SectorStorage},
};
use alloc::{collections::BTreeMap, rc::Rc, vec, vec::Vec};
use core::cell::RefCell;
use littlefs_rust::{Error, Filesystem};

/// Sectors of the synthetic card behind every test layout.
const TEST_CARD_SECTORS: u64 = 300_000;

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
    fn check(&mut self) -> Result<(), Error> {
        if self.0.borrow().fail {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}
type Setup3 = (
    RegionIo<Disk>,
    RegionIo<Disk>,
    RegionIo<Disk>,
    Rc<RefCell<Image>>,
    CardLayout,
);
fn setup3() -> Setup3 {
    let image = Rc::new(RefCell::new(Image::default()));
    let layout = CardLayout::new(TEST_CARD_SECTORS).unwrap();
    let (db, log, media) = split(Disk(image.clone()), layout);
    (db, log, media, image, layout)
}
/// Database and log handles; the media handle is not needed by most tests.
/// The log region plays the role of the "other" region sharing the device.
type Setup = (
    RegionIo<Disk>,
    RegionIo<Disk>,
    Rc<RefCell<Image>>,
    CardLayout,
);
fn setup() -> Setup {
    let (db, log, _media, image, layout) = setup3();
    (db, log, image, layout)
}
// Regression (t-121): three handles translate to database, log and media starts.
#[test]
fn split_yields_database_log_and_media_handles() {
    let (mut db, mut log, mut media, image, layout) = setup3();
    db.write_sector(0, &[1; 512]).unwrap();
    log.write_sector(0, &[2; 512]).unwrap();
    media.write_sector(0, &[3; 512]).unwrap();
    let image = image.borrow();
    assert_eq!(image.sectors[&layout.database().start()], [1; 512]);
    assert_eq!(image.sectors[&layout.log().start()], [2; 512]);
    assert_eq!(image.sectors[&layout.media().start()], [3; 512]);
    assert_eq!(
        image.calls,
        vec![
            layout.database().start(),
            layout.log().start(),
            layout.media().start()
        ]
    );
}
#[test]
fn translates_disjoint_regions_and_rejects_out_of_bounds_before_io() {
    // Regression (t-106): region adapters never cross the shared card's region bounds.
    let (mut db, mut media, image, layout) = setup();
    db.write_sector(0, &[0x42; 512]).unwrap();
    media.write_sector(0, &[0x24; 512]).unwrap();
    assert_eq!(
        image.borrow().calls,
        vec![layout.database().start(), layout.log().start()]
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
    crate::littlefs::append_log(&fs, &mut log, "/theo.log", "/theo.1.log").unwrap();
    fs.unmount().unwrap();
    let before = image.borrow().sectors.clone();
    let Discovered::Legacy(storage) = discover(Disk(image.clone()), 128 * 1024).ok().unwrap()
    else {
        panic!("legacy card incorrectly selected as partitioned")
    };
    let fs = Filesystem::mount(storage, geometry.config())
        .map_err(|(e, _)| e)
        .unwrap();
    assert!(crate::littlefs::log_tail(&fs, "/theo.log").unwrap().size > 0);
    assert_eq!(image.borrow().sectors, before);
}

/// A device that fails while `expired` is set, reporting `device_failed()`
/// as `poisoned`: a soft (deadline) or hard (bus) failure.
struct Timed {
    disk: Disk,
    expired: Rc<core::cell::Cell<bool>>,
    poisoned: bool,
}
impl Timed {
    fn live(&self) -> Result<(), Error> {
        if self.expired.get() {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}
impl SectorIo for Timed {
    fn read_sector(&mut self, s: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        self.live()?;
        self.disk.read_sector(s, out)
    }
    fn write_sector(&mut self, s: u32, data: &[u8; 512]) -> Result<(), Error> {
        self.live()?;
        self.disk.write_sector(s, data)
    }
    fn sync(&mut self) -> Result<(), Error> {
        self.live()?;
        self.disk.sync()
    }
    fn check(&mut self) -> Result<(), Error> {
        self.live()
    }
    fn device_failed(&self) -> bool {
        self.poisoned
    }
}
type TimedSetup = (
    RegionIo<Timed>,
    RegionIo<Timed>,
    RegionIo<Timed>,
    Rc<core::cell::Cell<bool>>,
);
fn timed(poisoned: bool) -> TimedSetup {
    let expired = Rc::new(core::cell::Cell::new(false));
    let device = Timed {
        disk: Disk(Rc::new(RefCell::new(Image::default()))),
        expired: expired.clone(),
        poisoned,
    };
    let (db, log, media) = split(device, CardLayout::new(TEST_CARD_SECTORS).unwrap());
    (db, log, media, expired)
}

// Regression (t-121): a deadline error in one region must not take the shared device.
#[test]
fn soft_failure_keeps_shared_device_for_other_regions() {
    let (_db, mut log, mut media, expired) = timed(false);
    expired.set(true);
    assert_eq!(media.read_sector(0, &mut [0; 512]), Err(Error::Io));
    assert_eq!(media.check(), Err(Error::Io));
    assert!(!media.device_failed());
    expired.set(false);
    log.read_sector(0, &mut [0; 512]).unwrap();
    media.read_sector(0, &mut [0; 512]).unwrap();
}

// Regression (t-121): a bus-level failure still disables every region.
#[test]
fn hard_failure_still_disables_every_region() {
    let (_db, mut log, mut media, expired) = timed(true);
    expired.set(true);
    assert_eq!(media.read_sector(0, &mut [0; 512]), Err(Error::Io));
    expired.set(false);
    assert_eq!(log.read_sector(0, &mut [0; 512]), Err(Error::Io));
    assert!(log.device_failed());
}

/// (sector reads, sector writes) of a log-region mount plus one append, after
/// `media_sectors` sectors were written through the media half.
fn log_mount_and_append_cost(media_sectors: u32) -> (usize, usize) {
    let (_db, log, mut media, image, layout) = setup3();
    let geometry = Geometry::new(layout.log().sectors() as u64).unwrap();
    let mut storage = SectorStorage::new(log, geometry);
    Filesystem::format(&mut storage, &geometry.config()).unwrap();
    let fs = crate::littlefs::mount_for_writes(storage, geometry).unwrap();
    let mut buffer = LogBuffer::<64>::new();
    buffer.push(b"before\n");
    crate::littlefs::append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").unwrap();
    fs.unmount().unwrap();
    for sector in 0..media_sectors.min(layout.media().sectors()) {
        media.write_sector(sector, &[0xA5; 512]).unwrap();
    }
    // A fresh split, as after a reboot.
    let (_db, log, _media) = split(Disk(image.clone()), layout);
    let (calls, mutations, syncs) = {
        let image = image.borrow();
        (image.calls.len(), image.mutations, image.syncs)
    };
    let fs =
        crate::littlefs::mount_for_writes(SectorStorage::new(log, geometry), geometry).unwrap();
    let mut buffer = LogBuffer::<64>::new();
    buffer.push(b"after\n");
    crate::littlefs::append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").unwrap();
    let image = image.borrow();
    let writes = (image.mutations - mutations) - (image.syncs - syncs);
    (image.calls.len() - calls - writes, writes)
}

// Regression (t-121): log mount and append cost do not depend on media
// written to the separate media region.
#[test]
fn log_mount_and_append_cost_is_independent_of_media() {
    let without = log_mount_and_append_cost(0);
    let with = log_mount_and_append_cost(u32::MAX);
    assert!(without.0 > 0 && without.1 > 0, "{without:?}");
    assert_eq!(without, with);
}
// Regression (t-153): a batch is translated into its region and written in
// order, one sector after the other, by the default `write_sectors`.
#[test]
fn batch_write_translates_into_the_region_in_order() {
    let (_db, _log, mut media, image, layout) = setup3();
    let mut data = vec![0u8; 3 * 512];
    for (i, sector) in data.as_chunks_mut::<512>().0.iter_mut().enumerate() {
        sector.fill(i as u8 + 1);
    }
    media.write_sectors(5, &data).unwrap();
    let start = layout.media().start();
    let image = image.borrow();
    assert_eq!(image.calls, vec![start + 5, start + 6, start + 7]);
    for i in 0..3u32 {
        assert_eq!(image.sectors[&(start + 5 + i)], [i as u8 + 1; 512]);
    }
}
// Regression (t-153): a batch whose last sector is past the region, or that
// is not whole sectors, fails before any device I/O.
#[test]
fn batch_write_out_of_bounds_or_partial_fails_before_io() {
    let (mut db, _log, image, layout) = setup();
    let last = layout.database().sectors() - 1;
    assert_eq!(db.write_sectors(last, &[0; 1024]), Err(Error::Invalid));
    assert_eq!(db.write_sectors(0, &[0; 100]), Err(Error::Invalid));
    assert_eq!(db.write_sectors(0, &[]), Err(Error::Invalid));
    assert!(image.borrow().calls.is_empty());
}
