use super::*;
use crate::test_disk::{Crash, TestDisk};
use alloc::string::ToString;
use redb::{ReadableDatabase, ReadableTable, StorageBackend};

const SECTORS: u32 = LENGTH_SECTORS + 64; // length copies + 32 KiB

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
    let disk = TestDisk::new(LENGTH_SECTORS + 16_384); // 8 MiB
    let mut d = disk.clone();
    RegionBackend::format(&mut d).unwrap();
    const T: redb::TableDefinition<u64, u64> = redb::TableDefinition::new("t");
    {
        // SAFETY: single-threaded test.
        let b = unsafe { RegionBackend::new(disk.clone(), LENGTH_SECTORS + 16_384) }.unwrap();
        let db = redb::Database::builder().create_with_backend(b).unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.set_quick_repair(true);
        tx.open_table(T).unwrap().insert(7, 42).unwrap();
        tx.commit().unwrap();
    }
    // SAFETY: single-threaded test.
    let b = unsafe { RegionBackend::new(disk, LENGTH_SECTORS + 16_384) }.unwrap();
    let db = redb::Database::builder().create_with_backend(b).unwrap();
    let tx = db.begin_read().unwrap();
    assert_eq!(
        tx.open_table(T).unwrap().get(7).unwrap().unwrap().value(),
        42
    );
}

fn reopen(disk: &TestDisk) -> Result<RegionBackend<TestDisk>, BackendError> {
    // SAFETY: single-threaded test.
    unsafe { RegionBackend::new(disk.clone(), SECTORS) }
}
fn sector(disk: &TestDisk, index: u32) -> [u8; SECTOR_SIZE] {
    let mut out = [0; SECTOR_SIZE];
    disk.clone().read_sector(index, &mut out).unwrap();
    out
}
fn put_sector(disk: &TestDisk, index: u32, data: &[u8; SECTOR_SIZE]) {
    let mut d = disk.clone();
    d.write_sector(index, data).unwrap();
    d.sync().unwrap();
}

#[test]
fn redb_file_starts_after_both_length_copies() {
    assert_eq!(LENGTH_SECTORS, 2);
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(1024).unwrap();
    b.write(0, &[7; 1024]).unwrap();
    assert_eq!(sector(&disk, LENGTH_SECTORS), [7; SECTOR_SIZE]);
    assert_eq!(sector(&disk, LENGTH_SECTORS + 1), [7; SECTOR_SIZE]);
    assert_eq!(backend(&disk).len().unwrap(), 1024);
}

#[test]
fn set_len_alternates_between_the_two_copies() {
    let disk = formatted();
    let b = backend(&disk);
    let mut previous = None;
    for len in [512, 1024, 1536, 2048] {
        let copies = [sector(&disk, 0), sector(&disk, 1)];
        b.set_len(len).unwrap();
        let changed: alloc::vec::Vec<_> = (0..2)
            .filter(|&i| sector(&disk, i) != copies[i as usize])
            .collect();
        assert_eq!(changed.len(), 1, "set_len({len}) rewrote {changed:?}");
        assert_ne!(previous, Some(changed[0]), "set_len({len}) reused a copy");
        previous = Some(changed[0]);
    }
    assert_eq!(backend(&disk).len().unwrap(), 2048);
}

#[test]
fn one_damaged_copy_falls_back_to_the_other() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(512).unwrap();
    b.set_len(1024).unwrap();
    drop(b);
    let newest = (0..LENGTH_SECTORS)
        .find(|&i| decode(&sector(&disk, i)).is_some_and(|r| r.len == 1024))
        .unwrap();
    put_sector(&disk, newest, &[0xA5; SECTOR_SIZE]);
    assert_eq!(backend(&disk).len().unwrap(), 512);
}

#[test]
fn both_copies_damaged_is_corrupt() {
    let disk = formatted();
    put_sector(&disk, 0, &[0; SECTOR_SIZE]);
    put_sector(&disk, 1, &[0; SECTOR_SIZE]);
    assert_eq!(reopen(&disk).unwrap_err(), BackendError::Corrupt);
}

#[test]
fn copy_past_capacity_is_ignored() {
    let disk = formatted();
    backend(&disk).set_len(512).unwrap();
    // A larger region accepts a length this one cannot hold.
    // SAFETY: single-threaded test.
    let big = unsafe { RegionBackend::new(disk.clone(), SECTORS * 2) }.unwrap();
    big.set_len(u64::from(SECTORS) * 512).unwrap();
    drop(big);
    assert_eq!(backend(&disk).len().unwrap(), 512);
}

// Regression (t-133): format must not leave a stale copy that outranks it.
#[test]
fn format_over_a_used_region_resets_both_copies() {
    let disk = formatted();
    let b = backend(&disk);
    for len in [512, 1024, 1536] {
        b.set_len(len).unwrap();
    }
    drop(b);
    let mut d = disk.clone();
    RegionBackend::format(&mut d).unwrap();
    assert_eq!(backend(&disk).len().unwrap(), 0);
}

// Regression (t-133): a torn length record used to make the region
// `Corrupt` for good; with two copies the previous length survives.
#[test]
fn torn_set_len_reopens_with_old_or_new_length() {
    for earlier in 0..3u64 {
        // Prefix 12 tears the length field itself.
        for prefix in [1, 12, 511] {
            for mode in [Crash::DropPending, Crash::KeepPending] {
                let disk = formatted();
                let b = backend(&disk);
                for i in 1..=earlier {
                    b.set_len(i * 512).unwrap();
                }
                let old = earlier * 512;
                let new = old + 4096;
                let next = disk.state().writes;
                disk.state().cut = Some((next, prefix));
                assert!(b.set_len(new).is_err());
                drop(b);
                disk.crash(mode);
                let len = reopen(&disk)
                    .unwrap_or_else(|e| panic!("{earlier} {prefix} {mode:?}: {e:?}"))
                    .len()
                    .unwrap();
                assert!(
                    len == old || len == new,
                    "{earlier} {prefix} {mode:?}: {len}"
                );
            }
        }
    }
}

// Regression guard (t-133): `set_len` is durable on return.
#[test]
fn set_len_survives_a_crash_dropping_pending_writes() {
    let disk = formatted();
    backend(&disk).set_len(5000).unwrap();
    disk.crash(Crash::DropPending);
    assert_eq!(backend(&disk).len().unwrap(), 5000);
}
