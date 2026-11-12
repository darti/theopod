use super::*;
use crate::test_disk::TestDisk;
use alloc::string::ToString;
use redb::{ReadableDatabase, ReadableTable, StorageBackend};

const SECTORS: u32 = 1 + 64; // length sector + 32 KiB

fn formatted() -> TestDisk {
    let mut disk = TestDisk::new(SECTORS);
    RegionBackend::format(&mut disk).unwrap();
    disk
}
fn backend(disk: &TestDisk) -> RegionBackend<TestDisk> {
    // SAFETY: single-threaded test; the backend never leaves this thread.
    unsafe { RegionBackend::new(disk.clone(), SECTORS) }.unwrap()
}

#[test]
fn new_on_unformatted_region_is_corrupt() {
    let disk = TestDisk::new(SECTORS);
    // SAFETY: single-threaded test.
    let error = unsafe { RegionBackend::new(disk, SECTORS) }.unwrap_err();
    assert_eq!(error, BackendError::Corrupt);
}

#[test]
fn formatted_region_is_empty() {
    assert_eq!(backend(&formatted()).len().unwrap(), 0);
}

#[test]
fn set_len_persists_across_reopen() {
    let disk = formatted();
    backend(&disk).set_len(5000).unwrap();
    assert_eq!(backend(&disk).len().unwrap(), 5000);
}

#[test]
fn set_len_beyond_region_reports_region_full() {
    let disk = formatted();
    let error = backend(&disk).set_len(64 * 512 + 1).unwrap_err();
    assert!(error.to_string().contains(REGION_FULL));
    assert_eq!(backend(&disk).len().unwrap(), 0);
}

#[test]
fn unaligned_write_reads_back_and_keeps_neighbours() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(4096).unwrap();
    b.write(0, &[1; 4096]).unwrap();
    b.write(700, &[2; 1000]).unwrap();
    let mut out = [0; 4096];
    b.read(0, &mut out).unwrap();
    assert!(out[..700].iter().all(|&x| x == 1));
    assert!(out[700..1700].iter().all(|&x| x == 2));
    assert!(out[1700..].iter().all(|&x| x == 1));
}

#[test]
fn aligned_page_write_needs_no_read() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(8192).unwrap();
    disk.reset_counters();
    b.write(4096, &[3; 4096]).unwrap();
    assert_eq!(disk.state().reads, 0);
    assert_eq!(disk.state().writes, 8);
}

#[test]
fn read_or_write_past_len_fails_without_io() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(1024).unwrap();
    disk.reset_counters();
    assert!(b.read(1000, &mut [0; 100]).is_err());
    assert!(b.write(1000, &[0; 100]).is_err());
    let state = disk.state();
    assert_eq!(state.reads + state.writes, 0);
}

#[test]
fn sync_data_syncs_the_device() {
    let disk = formatted();
    let b = backend(&disk);
    disk.reset_counters();
    b.sync_data().unwrap();
    assert_eq!(disk.state().syncs, 1);
}

#[test]
fn device_failure_surfaces_as_io_error() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(512).unwrap();
    disk.state().fail_writes = true;
    assert!(b.write(0, &[0; 512]).is_err());
}

#[test]
fn redb_database_roundtrips_on_the_backend() {
    let disk = TestDisk::new(1 + 16_384); // 8 MiB
    let mut d = disk.clone();
    RegionBackend::format(&mut d).unwrap();
    const T: redb::TableDefinition<u64, u64> = redb::TableDefinition::new("t");
    {
        // SAFETY: single-threaded test.
        let b = unsafe { RegionBackend::new(disk.clone(), 1 + 16_384) }.unwrap();
        let db = redb::Database::builder().create_with_backend(b).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.set_quick_repair(true);
        tx.open_table(T).unwrap().insert(7, 42).unwrap();
        tx.commit().unwrap();
    }
    // SAFETY: single-threaded test.
    let b = unsafe { RegionBackend::new(disk, 1 + 16_384) }.unwrap();
    let db = redb::Database::builder().create_with_backend(b).unwrap();
    let tx = db.begin_read().unwrap();
    assert_eq!(
        tx.open_table(T).unwrap().get(7).unwrap().unwrap().value(),
        42
    );
}
