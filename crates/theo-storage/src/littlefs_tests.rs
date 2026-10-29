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
    fail: bool,
    fail_sync: bool,
    tear_at: Option<usize>,
}
impl Disk {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(Image {
            bytes: vec![0xff; 4096 * 64],
            writes: 0,
            fail: false,
            fail_sync: false,
            tear_at: None,
        })))
    }
    fn adapter(&self) -> SectorStorage<Self> {
        SectorStorage::new(self.clone(), Geometry::new(512).unwrap())
    }
}
impl SectorIo for Disk {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        let image = self.0.borrow();
        if image.fail {
            return Err(Error::Io);
        }
        out.copy_from_slice(&image.bytes[sector as usize * 512..][..512]);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        let mut image = self.0.borrow_mut();
        if image.fail {
            return Err(Error::Io);
        }
        image.writes += 1;
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
    assert!(Filesystem::mount(disk.adapter(), Geometry::new(512).unwrap().config()).is_err());
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
