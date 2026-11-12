use super::*;
use crate::catalogue::Catalogue;
use allocator_api2::alloc::Global;
use std::collections::HashMap;
use theo_storage::littlefs::Geometry;

/// Smallest convenient v3 card: database, log and 66 media units.
const CARD: u64 = 300_000;

/// Sparse in-memory card; unwritten sectors read as zero.
#[derive(Default)]
struct Card {
    sectors: HashMap<u32, [u8; 512]>,
    writes: usize,
    syncs: usize,
    /// Writes at or past this sector fail.
    fail_from: Option<u32>,
    /// The write after this many successful ones fails, and every later one.
    fail_after: Option<usize>,
    fail_sync: bool,
}
impl SectorIo for Card {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        *out = self.sectors.get(&sector).copied().unwrap_or([0; 512]);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        if self.fail_from.is_some_and(|from| sector >= from)
            || self.fail_after.is_some_and(|after| self.writes >= after)
        {
            return Err(Error::Io);
        }
        self.writes += 1;
        self.sectors.insert(sector, *data);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error> {
        if self.fail_sync {
            return Err(Error::Io);
        }
        self.syncs += 1;
        Ok(())
    }
    fn check(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
fn descriptors(card: &mut Card) -> ([u8; 512], [u8; 512]) {
    let (mut first, mut second) = ([0; 512], [0; 512]);
    card.read_sector(DESCRIPTOR_SECTORS[0], &mut first).unwrap();
    card.read_sector(DESCRIPTOR_SECTORS[1], &mut second)
        .unwrap();
    (first, second)
}

#[test]
fn provision_v3_publishes_a_layout_with_catalogue_and_log() {
    // Regression (t-133): the on-device and host provisioners share this
    // path; the published layout must reopen a catalogue and a log volume.
    let mut card = Card::default();
    let layout = provision_v3(&mut card, CARD, Global).unwrap();
    let (first, second) = descriptors(&mut card);
    assert_eq!(
        CardLayout::select(&first, &second, CARD).unwrap(),
        Some(layout)
    );
    let (db, log, _media) = split(card, layout);
    let log = {
        // SAFETY: single-threaded test.
        let backend = unsafe { RegionBackend::new(db, layout.database().sectors()) }.unwrap();
        let mut cat = Catalogue::open(backend).unwrap();
        assert!(cat.next_subscription(None).unwrap().is_none());
        let mut scratch = [0; crate::catalogue::MAX_RECORD_BYTES];
        assert_eq!(cat.scan(None, &mut scratch, |_| ()).unwrap().records, 0);
        log
    };
    let geometry = Geometry::new(layout.log().sectors() as u64).unwrap();
    let fs = littlefs_rust::Filesystem::mount(SectorStorage::new(log, geometry), geometry.config())
        .map_err(|(e, _)| e)
        .unwrap();
    assert_eq!(fs.stat("/theo.log").unwrap().size, 0);
    fs.unmount().unwrap();
}

#[test]
fn provision_v3_failure_leaves_a_blank_card() {
    // Regression (t-133): an interrupted run must stay undiscoverable and
    // must not leave the old whole-card LittleFS superblocks mountable.
    let layout = CardLayout::new(CARD).unwrap();
    let mut card = Card {
        fail_from: Some(layout.log().start()),
        ..Card::default()
    };
    let old = [0x5a; 512];
    card.sectors.insert(DESCRIPTOR_SECTORS[0], old);
    card.sectors.insert(DESCRIPTOR_SECTORS[1], old);
    assert!(matches!(
        provision_v3(&mut card, CARD, Global),
        Err(ProvisionError::Log(_))
    ));
    assert_eq!(descriptors(&mut card), ([0; 512], [0; 512]));
}

#[test]
fn provision_v3_interrupted_at_any_write_publishes_nothing_or_a_complete_layout() {
    // Regression (t-133): covers the log, database length record and
    // descriptor phases; only the last write (second descriptor copy) may
    // fail after a layout is already published.
    let mut complete = Card::default();
    let layout = provision_v3(&mut complete, CARD, Global).unwrap();
    let total = complete.writes;
    let mut points: std::vec::Vec<usize> = (0..total).step_by(total / 64 + 1).collect();
    points.extend(total - 3..total);
    for after in points {
        let mut card = Card {
            fail_after: Some(after),
            ..Card::default()
        };
        assert!(provision_v3(&mut card, CARD, Global).is_err());
        let (first, second) = descriptors(&mut card);
        let published = CardLayout::select(&first, &second, CARD).unwrap();
        let expected = (after == total - 1).then_some(layout);
        assert_eq!(
            published, expected,
            "failure after {after} of {total} writes"
        );
    }
}

#[test]
fn provision_v3_sync_failure_publishes_no_descriptor() {
    let mut card = Card {
        fail_sync: true,
        ..Card::default()
    };
    assert!(matches!(
        provision_v3(&mut card, CARD, Global),
        Err(ProvisionError::Unpublish(_))
    ));
    assert_eq!(descriptors(&mut card), ([0; 512], [0; 512]));
}

#[test]
fn provision_v3_syncs_only_at_its_barriers() {
    // Unpublish, regions, then one per descriptor copy.
    let mut card = Card::default();
    provision_v3(&mut card, CARD, Global).unwrap();
    assert_eq!(card.syncs, 4);
}

#[test]
fn provision_v3_rejects_a_card_too_small_without_writing() {
    // Regression (t-133): a capacity the layout cannot hold writes nothing.
    let mut card = Card::default();
    let result = provision_v3(&mut card, 1024, Global);
    assert!(matches!(result, Err(ProvisionError::Layout(_))));
    assert_eq!(card.writes, 0);
}
