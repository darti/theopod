// Regression guard: exercise the public LittleFS API on whole SD sectors.
use alloc::{rc::Rc, vec, vec::Vec};
use core::cell::RefCell;
use littlefs_rust::{Error, Filesystem, Storage};

use crate::littlefs::{Geometry, SectorIo, SectorStorage, append_log, log_tail};

#[derive(Clone)]
struct Disk(Rc<RefCell<Image>>);
struct Image {
    bytes: Vec<u8>,
    writes: usize,
    reads: usize,
    read_budget: Option<usize>,
    fail: bool,
    fail_sync: bool,
    ignore_writes: bool,
    expired: bool,
    tear_at: Option<usize>,
}
impl Disk {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(Image {
            bytes: vec![0xff; 4096 * 64],
            writes: 0,
            reads: 0,
            read_budget: None,
            fail: false,
            fail_sync: false,
            ignore_writes: false,
            expired: false,
            tear_at: None,
        })))
    }
    fn adapter(&self) -> SectorStorage<Self> {
        SectorStorage::new(self.clone(), Geometry::new(512).unwrap())
    }
}
impl SectorIo for Disk {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        let mut image = self.0.borrow_mut();
        if image.fail
            || image
                .read_budget
                .is_some_and(|budget| image.reads >= budget)
        {
            return Err(Error::Io);
        }
        image.reads += 1;
        out.copy_from_slice(&image.bytes[sector as usize * 512..][..512]);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        let mut image = self.0.borrow_mut();
        if image.fail {
            return Err(Error::Io);
        }
        image.writes += 1;
        if image.ignore_writes {
            return Ok(());
        }
        if image.tear_at == Some(image.writes) {
            image.bytes[sector as usize * 512..][..256].copy_from_slice(&data[..256]);
            return Err(Error::Io);
        }
        image.bytes[sector as usize * 512..][..512].copy_from_slice(data);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error> {
        if self.0.borrow().fail || self.0.borrow().fail_sync {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
    fn check(&mut self) -> Result<(), Error> {
        let image = self.0.borrow();
        if image.fail || image.expired {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}

fn populated_log() -> (Disk, Geometry) {
    let disk = Disk::new();
    disk.0.borrow_mut().bytes.resize(1024 * 1024, 0xff);
    let geometry = Geometry::new(2048).unwrap();
    let mut adapter = SectorStorage::new(disk.clone(), geometry);
    Filesystem::format(&mut adapter, &geometry.config()).unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    fs.write_file("/theo.1.log", &vec![b'R'; 131072]).unwrap();
    fs.write_file("/sdtest2.1", &vec![b'R'; 131072]).unwrap();
    fs.write_file("/sdtest2.log", &vec![b'R'; 4096]).unwrap();
    fs.write_file("/theo.log", &vec![b'A'; 215]).unwrap();
    for _ in 0..14 {
        let mut batch = crate::LogBuffer::<1024>::new();
        batch.push(&[b'A'; 1000]);
        append_log(&fs, &mut batch, "/theo.log", "/theo.1.log").unwrap();
    }
    drop(fs.unmount().unwrap());
    (disk, geometry)
}

/// Sector reads used by a first 4 KiB append on a cold adapter.
fn first_append_reads(disk: &Disk, geometry: Geometry, prepared: bool) -> usize {
    let adapter = SectorStorage::new(disk.clone(), geometry);
    let fs = if prepared {
        crate::littlefs::mount_for_writes(adapter, geometry).unwrap()
    } else {
        Filesystem::mount(adapter, geometry.config()).ok().unwrap()
    };
    disk.0.borrow_mut().reads = 0;
    let mut pending = crate::LogBuffer::<4096>::new();
    pending.push(&[b'B'; 4096]);
    append_log(&fs, &mut pending, "/theo.log", "/theo.1.log").unwrap();
    let reads = disk.0.borrow().reads;
    drop(fs);
    reads
}

// Regression (t-103): startup allocation scanning must not consume the first append budget.
#[test]
fn mounted_volume_primes_allocator_before_first_log_append() {
    let (disk, geometry) = populated_log();
    let baseline = disk.0.borrow().bytes.clone();
    // Compared on identical images so the read cache cannot mask a missing
    // preparation step: with the cache, 18 reads prepared vs 50 unprepared.
    let unprepared = first_append_reads(&disk, geometry, false);
    disk.0.borrow_mut().bytes = baseline.clone();
    let prepared = first_append_reads(&disk, geometry, true);
    assert!(
        prepared * 2 < unprepared,
        "prepared={prepared} unprepared={unprepared}"
    );
    // A fixture-specific budget between the two, not a latency guarantee.
    assert!(prepared <= 32, "prepared={prepared}");

    disk.0.borrow_mut().bytes = baseline;
    let adapter = SectorStorage::new(disk.clone(), geometry);
    let fs = crate::littlefs::mount_for_writes(adapter, geometry).unwrap();
    assert_eq!(log_tail(&fs, "/theo.log").unwrap().size, 14215);
    disk.0.borrow_mut().reads = 0;
    disk.0.borrow_mut().read_budget = Some(32);
    let mut pending = crate::LogBuffer::<4096>::new();
    pending.push(&[b'B'; 4096]);
    assert_eq!(
        append_log(&fs, &mut pending, "/theo.log", "/theo.1.log"),
        Ok(())
    );
    assert!(pending.is_empty());
    disk.0.borrow_mut().read_budget = None;
    let adapter = fs.unmount().unwrap();
    let fs = crate::littlefs::mount_for_writes(adapter, geometry).unwrap();
    let mut expected = vec![b'A'; 14215];
    expected.extend_from_slice(&[b'B'; 4096]);
    assert_eq!(fs.read_to_vec("/theo.log").unwrap(), expected);
    assert_eq!(fs.read_to_vec("/theo.1.log").unwrap(), vec![b'R'; 131072]);
    assert_eq!(fs.read_to_vec("/sdtest2.1").unwrap(), vec![b'R'; 131072]);
    assert_eq!(fs.read_to_vec("/sdtest2.log").unwrap(), vec![b'R'; 4096]);
}

// Regression (t-103): failed startup maintenance must not expose a writable mount or format it.
#[test]
fn mount_preparation_io_failure_preserves_existing_volume() {
    let disk = Disk::new();
    let geometry = Geometry::new(512).unwrap();
    let mut adapter = disk.adapter();
    Filesystem::format(&mut adapter, &geometry.config()).unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    fs.write_file("/keep", &[b'K'; 8192]).unwrap();
    drop(fs.unmount().unwrap());
    disk.0.borrow_mut().reads = 0;
    let fs = Filesystem::mount(disk.adapter(), geometry.config())
        .ok()
        .unwrap();
    let mount_reads = disk.0.borrow().reads;
    drop(fs.unmount().unwrap());
    // The raw mount fits this budget, so the failure below is preparation's.
    disk.0.borrow_mut().reads = 0;
    disk.0.borrow_mut().read_budget = Some(mount_reads);
    let fs = Filesystem::mount(disk.adapter(), geometry.config())
        .ok()
        .unwrap();
    drop(fs);
    let before = disk.0.borrow().bytes.clone();
    let writes = disk.0.borrow().writes;
    disk.0.borrow_mut().reads = 0;
    assert!(matches!(
        crate::littlefs::mount_for_writes(disk.adapter(), geometry),
        Err(Error::Io)
    ));
    assert_eq!(disk.0.borrow().writes, writes);
    assert_eq!(disk.0.borrow().bytes, before);
    disk.0.borrow_mut().read_budget = None;
    let fs = crate::littlefs::mount_for_writes(disk.adapter(), geometry).unwrap();
    assert_eq!(fs.read_to_vec("/keep").unwrap(), &[b'K'; 8192]);
}

// Regression guard (t-103): preparation cost grows with stored file data
// (ADR-0019). Pins the documented limit so a change to it is noticed.
#[test]
fn mount_preparation_reads_grow_with_stored_media() {
    let geometry = Geometry::new(16384).unwrap(); // 8 MiB volume.
    let prepare_reads = |media: usize| {
        let disk = Disk::new();
        disk.0.borrow_mut().bytes.resize(8 << 20, 0xff);
        let mut adapter = SectorStorage::new(disk.clone(), geometry);
        Filesystem::format(&mut adapter, &geometry.config()).unwrap();
        let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
        fs.write_file("/theo.log", &[b'A'; 4096]).unwrap();
        if media > 0 {
            fs.write_file("/episode", &vec![b'M'; media]).unwrap();
        }
        drop(fs.unmount().unwrap());
        disk.0.borrow_mut().reads = 0;
        let fs = Filesystem::mount(
            SectorStorage::new(disk.clone(), geometry),
            geometry.config(),
        )
        .ok()
        .unwrap();
        let mounted = disk.0.borrow().reads;
        crate::littlefs::prepare_for_writes(&fs).unwrap();
        disk.0.borrow().reads - mounted
    };
    let small = prepare_reads(0);
    let large = prepare_reads(4 << 20); // 1,024 blocks of media.
    // About one read per 2 data blocks (measured 516), ~2 ms each on board.
    assert!(large >= small + 500, "small={small} large={large}");
}

// Regression (t-100): sector geometry rejects invalid requests without io.
#[test]
fn sector_geometry_rejects_invalid_requests_without_io() {
    assert!(Geometry::new(15).is_err());
    assert!(Geometry::new(u32::MAX as u64 + 1).is_err());
    let geometry = Geometry::new(519).unwrap();
    assert_eq!(geometry.block_count(), 64);
    let disk = Disk::new();
    let mut adapter = disk.adapter();
    assert_eq!(adapter.write(64, 0, &[0; 512]), Err(Error::Invalid));
    assert_eq!(adapter.write(0, 1, &[0; 512]), Err(Error::Invalid));
    assert_eq!(adapter.read(0, 4096, &mut [0; 512]), Err(Error::Invalid));
    assert_eq!(disk.0.borrow().writes, 0);
    adapter.write(3, 512, &[0; 512]).unwrap();
    adapter.erase(3).unwrap();
    assert!(
        disk.0.borrow().bytes[3 * 4096..4 * 4096]
            .iter()
            .all(|b| *b == 0xff)
    );
}

// Regression (t-100): failed mount does not format existing media.
#[test]
fn failed_mount_does_not_format_existing_media() {
    let disk = Disk::new();
    disk.0.borrow_mut().bytes.fill(0x5a);
    let before = disk.0.borrow().bytes.clone();
    assert!(
        crate::littlefs::mount_for_writes(disk.adapter(), Geometry::new(512).unwrap()).is_err()
    );
    assert_eq!(disk.0.borrow().writes, 0);
    assert_eq!(disk.0.borrow().bytes, before);
}

// Regression (t-103): explicit provisioning preserves an existing LittleFS volume.
#[test]
fn provision_once_preserves_existing_files_and_refuses_io_errors() {
    use crate::littlefs::provision_once;
    let disk = Disk::new();
    let geometry = Geometry::new(512).unwrap();
    let (fs, formatted) = provision_once(disk.adapter(), geometry).ok().unwrap();
    assert!(formatted);
    fs.write_file("/keep", b"existing").unwrap();
    drop(fs.unmount().unwrap());
    let writes = disk.0.borrow().writes;
    let (fs, formatted) = provision_once(disk.adapter(), geometry).ok().unwrap();
    assert!(!formatted);
    assert_eq!(fs.read_to_vec("/keep").unwrap(), b"existing");
    assert_eq!(disk.0.borrow().writes, writes);
    drop(fs.unmount().unwrap());
    disk.0.borrow_mut().fail = true;
    assert!(provision_once(disk.adapter(), geometry).is_err());
    assert_eq!(disk.0.borrow().writes, writes);
}

// Regression (t-100): log append reopen and rotation preserve bytes.
#[test]
fn log_append_reopen_and_rotation_preserve_bytes() {
    let disk = Disk::new();
    let geometry = Geometry::new(512).unwrap();
    let mut adapter = disk.adapter();
    Filesystem::format(&mut adapter, &geometry.config()).unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    let mut buffer = crate::LogBuffer::<4096>::new();
    buffer.push(b"first\n");
    append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").unwrap();
    assert!(buffer.is_empty());
    let adapter = fs.unmount().unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    let tail = log_tail(&fs, "/theo.log").unwrap();
    assert_eq!(&tail.bytes[..tail.len], b"first\n");
    fs.write_file("/theo.log", &vec![b'R'; crate::LOG_FILE_LIMIT as usize])
        .unwrap();
    buffer.push(b"next\n");
    append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").unwrap();
    assert_eq!(
        fs.stat("/theo.1.log").unwrap().size as u64,
        crate::LOG_FILE_LIMIT
    );
    assert_eq!(fs.read_to_vec("/theo.log").unwrap(), b"next\n");
    fs.unmount().unwrap();
}

// Regression (t-100): io failure disables adapter and keeps pending log.
#[test]
fn io_failure_disables_adapter_and_keeps_pending_log() {
    let disk = Disk::new();
    let geometry = Geometry::new(512).unwrap();
    let mut adapter = disk.adapter();
    // Regression (t-103): a failed sync also refuses a previously cached sector.
    adapter.read(0, 0, &mut [0; 512]).unwrap();
    disk.0.borrow_mut().fail = true;
    assert_eq!(adapter.sync(), Err(Error::Io));
    disk.0.borrow_mut().fail = false;
    assert_eq!(adapter.read(0, 0, &mut [0; 512]), Err(Error::Io));
    let mut adapter = disk.adapter();
    Filesystem::format(&mut adapter, &geometry.config()).unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    let mut buffer = crate::LogBuffer::<64>::new();
    buffer.push(b"keep me\n");
    disk.0.borrow_mut().fail = true;
    assert!(append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").is_err());
    assert_eq!(buffer.pending(), b"keep me\n");
}

// Regression (t-101): cancelled request forbids late reply reuse.
#[test]
fn cancelled_request_forbids_late_reply_reuse() {
    use crate::littlefs::RequestGuard;
    use core::sync::atomic::{AtomicBool, Ordering};
    let poison = AtomicBool::new(false);
    RequestGuard::begin(&poison).unwrap().complete().unwrap();
    assert!(!poison.load(Ordering::Acquire));
    drop(RequestGuard::begin(&poison).unwrap());
    assert!(poison.load(Ordering::Acquire));
    assert!(RequestGuard::begin(&poison).is_err());
    // A worker reporting success after cancellation cannot re-enable storage.
    let poison = AtomicBool::new(false);
    let request = RequestGuard::begin(&poison).unwrap();
    poison.store(true, Ordering::Release);
    assert!(request.complete().is_err());
    assert!(RequestGuard::begin(&poison).is_err());
}

// Regression guard: a torn SD sector cannot publish a partial log append.
// Regression (t-100): each torn sector during append recovers previous or complete log.
#[test]
fn each_torn_sector_during_append_recovers_previous_or_complete_log() {
    let geometry = Geometry::new(512).unwrap();
    let original = Disk::new();
    let mut adapter = original.adapter();
    Filesystem::format(&mut adapter, &geometry.config()).unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    let old = vec![b'A'; 8192];
    fs.write_file("/theo.log", &old).unwrap();
    fs.write_file("/neighbor", b"preserve").unwrap();
    drop(fs.unmount().unwrap());
    let baseline = original.0.borrow().bytes.clone();
    let pending = vec![b'B'; 1024];
    let mut complete = old.clone();
    complete.extend_from_slice(&pending);
    let copy = || {
        let disk = Disk::new();
        disk.0.borrow_mut().bytes = baseline.clone();
        disk
    };
    let disk = copy();
    let fs = Filesystem::mount(disk.adapter(), geometry.config())
        .ok()
        .unwrap();
    let mut buffer = crate::LogBuffer::<1024>::new();
    buffer.push(&pending);
    append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").unwrap();
    let writes = disk.0.borrow().writes;
    assert!(writes > 0);
    drop(fs.unmount().unwrap());
    for cut in 1..=writes {
        let disk = copy();
        let fs = Filesystem::mount(disk.adapter(), geometry.config())
            .ok()
            .unwrap();
        disk.0.borrow_mut().tear_at = Some(cut);
        let mut buffer = crate::LogBuffer::<1024>::new();
        buffer.push(&pending);
        assert!(
            append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").is_err(),
            "cut={cut}"
        );
        assert_eq!(buffer.pending(), pending);
        drop(fs);
        disk.0.borrow_mut().tear_at = None;
        let fs = Filesystem::mount(disk.adapter(), geometry.config())
            .ok()
            .unwrap();
        let recovered = fs.read_to_vec("/theo.log").unwrap();
        assert!(recovered == old || recovered == complete, "cut={cut}");
        assert_eq!(fs.read_to_vec("/neighbor").unwrap(), b"preserve");
        fs.unmount().unwrap();
    }
}

// Regression guard: sync failure never acknowledges a buffered log batch.
// Regression (t-100): failed sync keeps entire log batch pending.
#[test]
fn failed_sync_keeps_entire_log_batch_pending() {
    let disk = Disk::new();
    let geometry = Geometry::new(512).unwrap();
    let mut adapter = disk.adapter();
    Filesystem::format(&mut adapter, &geometry.config()).unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    fs.write_file("/theo.log", b"old").unwrap();
    let mut buffer = crate::LogBuffer::<64>::new();
    buffer.push(b"pending");
    disk.0.borrow_mut().fail_sync = true;
    assert!(append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log").is_err());
    assert_eq!(buffer.pending(), b"pending");
}

// Regression (t-103): exercise actual LittleFS allocator exhaustion, not an injected I/O error.
#[test]
fn full_littlefs_volume_keeps_failed_append_pending() {
    let disk = Disk::new();
    // A real 128 KiB filesystem fills before the 128 KiB log rotation limit.
    let geometry = Geometry::new(256).unwrap();
    let mut adapter = SectorStorage::new(disk, geometry);
    Filesystem::format(&mut adapter, &geometry.config()).unwrap();
    let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
    fs.write_file("/theo.log", &vec![b'A'; 8192]).unwrap();
    for _ in 0..64 {
        let previous = fs.read_to_vec("/theo.log").unwrap();
        let mut buffer = crate::LogBuffer::<4096>::new();
        buffer.push(&[b'B'; 4096]);
        match append_log(&fs, &mut buffer, "/theo.log", "/theo.1.log") {
            Ok(()) => assert!(buffer.is_empty()),
            Err(Error::NoSpace) => {
                assert_eq!(buffer.pending(), &[b'B'; 4096]);
                assert_eq!(fs.read_to_vec("/theo.log").unwrap(), previous);
                return;
            }
            Err(error) => panic!("unexpected full-volume error: {error:?}"),
        }
    }
    panic!("bounded filesystem should have filled");
}

// Regression (t-103): metadata rereads across journal history must not exhaust a sector-read budget.
#[test]
fn repeated_cold_mount_appends_and_rotation_fit_read_budget() {
    let (disk, geometry) = populated_log();
    let mut expected = vec![b'A'; 14215];
    let mut archive = vec![b'R'; 131072];
    for round in 0..64u8 {
        let fs =
            crate::littlefs::mount_for_writes(SectorStorage::new(disk.clone(), geometry), geometry)
                .unwrap();
        assert_eq!(
            log_tail(&fs, "/theo.log").unwrap().size as usize,
            expected.len()
        );
        disk.0.borrow_mut().reads = 0;
        disk.0.borrow_mut().read_budget = Some(64);
        let payload = [round; 2048];
        let mut pending = crate::LogBuffer::<2048>::new();
        pending.push(&payload);
        assert_eq!(
            append_log(&fs, &mut pending, "/theo.log", "/theo.1.log"),
            Ok(()),
            "round {round}"
        );
        assert!(pending.is_empty());
        disk.0.borrow_mut().read_budget = None;
        if expected.len() + 2048 > 131072 {
            archive = core::mem::take(&mut expected);
        }
        expected.extend_from_slice(&payload);
        drop(fs.unmount().unwrap());
        // Cold cache after each remount: committed data, not cached bytes, is checked.
        let fs =
            crate::littlefs::mount_for_writes(SectorStorage::new(disk.clone(), geometry), geometry)
                .unwrap();
        assert_eq!(
            fs.read_to_vec("/theo.log").unwrap(),
            expected,
            "round {round}"
        );
        assert_eq!(
            fs.read_to_vec("/theo.1.log").unwrap(),
            archive,
            "round {round}"
        );
        assert_eq!(fs.read_to_vec("/sdtest2.1").unwrap(), vec![b'R'; 131072]);
        drop(fs.unmount().unwrap());
    }
}

// Regression (t-103): read caching must never replace LittleFS's real post-write media validation.
#[test]
fn cached_reads_never_hide_write_readback() {
    let disk = Disk::new();
    let mut adapter = disk.adapter();
    let mut out = [0; 512];
    adapter.read(2, 0, &mut out).unwrap();
    adapter.read(2, 0, &mut out).unwrap();
    assert_eq!(
        disk.0.borrow().reads,
        1,
        "unchanged media should only be read once"
    );
    disk.0.borrow_mut().ignore_writes = true;
    adapter.write(2, 0, &[0x5a; 512]).unwrap();
    adapter.read(2, 0, &mut out).unwrap();
    assert_eq!(
        out, [0xff; 512],
        "acknowledged but missing write must be visible"
    );
    assert_eq!(disk.0.borrow().reads, 2, "write must force a real readback");
    disk.0.borrow_mut().ignore_writes = false;
    adapter.write(2, 0, &[0x5a; 512]).unwrap();
    adapter.read(2, 0, &mut out).unwrap();
    assert_eq!(out, [0x5a; 512]);
}

// Regression (t-103): an erase must force real readback of every sector it covers.
#[test]
fn cached_reads_never_hide_erase_readback() {
    let disk = Disk::new();
    let mut adapter = disk.adapter();
    adapter.write(2, 0, &[0x5a; 4096]).unwrap();
    let mut block = [0; 4096];
    adapter.read(2, 0, &mut block).unwrap();
    disk.0.borrow_mut().reads = 0;
    adapter.erase(2).unwrap();
    adapter.read(2, 0, &mut block).unwrap();
    assert_eq!(block, [0xff; 4096]);
    assert_eq!(disk.0.borrow().reads, 8, "every erased sector is reread");
}

// Regression (t-103): a multi-sector write invalidates exactly the sectors it covers.
#[test]
fn multi_sector_write_invalidates_its_whole_range_only() {
    let disk = Disk::new();
    let mut adapter = disk.adapter();
    let mut block = [0; 4096];
    adapter.read(2, 0, &mut block).unwrap();
    disk.0.borrow_mut().reads = 0;
    disk.0.borrow_mut().ignore_writes = true;
    // Sectors 1..=3 of block 2; the medium silently keeps its old bytes.
    adapter.write(2, 512, &[0x5a; 1536]).unwrap();
    disk.0.borrow_mut().ignore_writes = false;
    let mut sector = [0; 512];
    for offset in [512, 1024, 1536] {
        adapter.read(2, offset, &mut sector).unwrap();
        assert_eq!(sector, [0xff; 512], "offset {offset} must be reread");
    }
    assert_eq!(disk.0.borrow().reads, 3);
    for offset in [0, 2048, 3584] {
        adapter.read(2, offset, &mut sector).unwrap();
    }
    assert_eq!(
        disk.0.borrow().reads,
        3,
        "sectors outside the write stay cached"
    );
}

// Regression (t-103): a failed partial erase poisons even cached reads of unrelated sectors.
#[test]
fn failed_erase_cannot_return_cached_bytes() {
    let disk = Disk::new();
    let mut adapter = disk.adapter();
    let mut out = [0; 512];
    adapter.read(3, 0, &mut out).unwrap();
    adapter.read(4, 0, &mut out).unwrap();
    disk.0.borrow_mut().tear_at = Some(2);
    assert_eq!(adapter.erase(3), Err(Error::Io));
    assert_eq!(adapter.read(4, 0, &mut out), Err(Error::Io));
}

// Regression (t-103): a cache hit honours the operation deadline and disables on expiry.
#[test]
fn cache_hit_after_expired_deadline_fails_and_disables() {
    let disk = Disk::new();
    let mut adapter = disk.adapter();
    let mut out = [0; 512];
    adapter.read(3, 0, &mut out).unwrap();
    disk.0.borrow_mut().expired = true;
    assert_eq!(adapter.read(3, 0, &mut out), Err(Error::Io));
    disk.0.borrow_mut().expired = false;
    assert_eq!(adapter.read(3, 0, &mut out), Err(Error::Io));
    assert_eq!(disk.0.borrow().reads, 1);
}

// Regression (t-103): a failed physical read never populates the cache.
#[test]
fn failed_read_is_not_cached() {
    let disk = Disk::new();
    disk.0.borrow_mut().fail = true;
    let mut adapter = disk.adapter();
    assert_eq!(adapter.read(3, 0, &mut [0; 512]), Err(Error::Io));
    disk.0.borrow_mut().fail = false;
    let mut adapter = disk.adapter();
    adapter.read(3, 0, &mut [0; 512]).unwrap();
    assert_eq!(disk.0.borrow().reads, 1);
}

// Regression (t-103): eviction drops the least recently used sector, not the oldest fill.
#[test]
fn cache_evicts_least_recently_used_sector() {
    let disk = Disk::new();
    let mut adapter = disk.adapter();
    let mut out = [0; 512];
    let read = |adapter: &mut SectorStorage<Disk>, sector: u32, out: &mut [u8; 512]| {
        adapter.read(sector / 8, (sector % 8) * 512, out).unwrap();
    };
    for sector in 0..32 {
        read(&mut adapter, sector, &mut out);
    }
    read(&mut adapter, 0, &mut out); // Refresh the first fill.
    read(&mut adapter, 32, &mut out); // Evicts sector 1.
    disk.0.borrow_mut().reads = 0;
    read(&mut adapter, 0, &mut out);
    assert_eq!(
        disk.0.borrow().reads,
        0,
        "recently used sector stays cached"
    );
    read(&mut adapter, 1, &mut out);
    assert_eq!(
        disk.0.borrow().reads,
        1,
        "least recently used sector was evicted"
    );
}

mod allocation {
    use super::*;
    use allocator_api2::alloc::{AllocError, Allocator, Global, Layout};
    use core::{cell::Cell, ptr::NonNull};

    /// Counts allocations, optionally refusing every request.
    struct Counting<'a> {
        count: &'a Cell<usize>,
        refuse: bool,
    }

    // SAFETY: forwards to `Global`, which upholds the `Allocator` contract;
    // refusing returns an error and never hands out memory.
    unsafe impl Allocator for Counting<'_> {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            self.count.set(self.count.get() + 1);
            if self.refuse {
                return Err(AllocError);
            }
            Global.allocate(layout)
        }

        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            // SAFETY: every live block came from `Global.allocate` above.
            unsafe { Global.deallocate(ptr, layout) }
        }
    }

    // Regression (t-103): a refused cache reservation fails before any device I/O.
    #[test]
    fn refused_cache_reservation_fails_without_device_io() {
        let disk = Disk::new();
        let count = Cell::new(0);
        let allocator = Counting {
            count: &count,
            refuse: true,
        };
        let result =
            SectorStorage::try_new_in(disk.clone(), Geometry::new(512).unwrap(), allocator);
        assert!(matches!(result, Err(Error::NoMemory)));
        assert_eq!(disk.0.borrow().reads, 0);
        assert_eq!(disk.0.borrow().writes, 0);
    }

    // Regression (t-103): the cache is reserved once, up front; reads never allocate.
    #[test]
    fn cache_reserves_once_and_reads_never_allocate() {
        let disk = Disk::new();
        let count = Cell::new(0);
        let allocator = Counting {
            count: &count,
            refuse: false,
        };
        let mut adapter =
            SectorStorage::try_new_in(disk.clone(), Geometry::new(512).unwrap(), allocator)
                .ok()
                .unwrap();
        assert_eq!(count.get(), 1);
        let mut block = [0; 4096];
        for block_index in 0..8 {
            adapter.read(block_index, 0, &mut block).unwrap();
        }
        assert_eq!(disk.0.borrow().reads, 64);
        assert_eq!(count.get(), 1, "64 distinct reads must not allocate");
    }
}
