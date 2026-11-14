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

/// The current length record as reopen sees it: (length, fresh).
fn state(disk: &TestDisk) -> (u64, bool) {
    let b = backend(disk);
    (b.len().unwrap(), b.is_fresh())
}

// Regression (t-133): provisioning marks the region fresh, so the first
// catalogue open may redo an interrupted database creation.
#[test]
fn formatted_region_is_fresh_until_cleared() {
    let disk = formatted();
    let b = backend(&disk);
    assert!(b.is_fresh());
    b.set_len(1024).unwrap();
    assert_eq!(state(&disk), (1024, true), "set_len keeps fresh-ness");
    b.fresh_flag().unwrap().clear().unwrap();
    assert!(!b.is_fresh());
    assert!(b.fresh_flag().is_none());
    assert_eq!(state(&disk), (1024, false));
    b.set_len(2048).unwrap();
    assert_eq!(state(&disk), (2048, false));
}

// Regression guard (t-133): a record written before the fresh flag existed
// (`THEORDB1`) reads as created, never as fresh.
#[test]
fn created_record_magic_is_not_fresh() {
    let disk = TestDisk::new(SECTORS);
    put_sector(&disk, 0, &record(1, 0, false));
    assert_eq!(&sector(&disk, 0)[..8], b"THEORDB1");
    assert_eq!(state(&disk), (0, false));
    assert_eq!(&sector(&formatted(), 0)[..8], b"THEORDBF");
}

#[test]
fn reset_unfinished_creation_resets_only_a_fresh_file_without_magic() {
    // Fresh, header written without its magic: reset to empty, still fresh.
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(4096).unwrap();
    b.write(0, &[0; 512]).unwrap();
    assert!(b.reset_unfinished_creation().unwrap());
    assert_eq!(b.len().unwrap(), 0);
    assert_eq!(state(&disk), (0, true));
    // Fresh and empty: nothing to undo.
    disk.reset_counters();
    assert!(!b.reset_unfinished_creation().unwrap());
    assert_eq!(disk.state().writes, 0);
    // Fresh with redb's magic: the creation finished.
    b.set_len(4096).unwrap();
    let mut head = [0; 512];
    head[..REDB_MAGIC.len()].copy_from_slice(&REDB_MAGIC);
    b.write(0, &head).unwrap();
    disk.reset_counters();
    assert!(!b.reset_unfinished_creation().unwrap());
    assert_eq!(disk.state().writes, 0);
    assert_eq!(b.len().unwrap(), 4096);
}

// Regression (t-133): boot never resets a region that is not fresh
// (ADR-0015), even when its file lacks redb's magic.
#[test]
fn reset_unfinished_creation_never_touches_a_created_region() {
    let disk = formatted();
    let b = backend(&disk);
    b.set_len(4096).unwrap();
    b.fresh_flag().unwrap().clear().unwrap();
    b.write(0, &[0x5a; 4096]).unwrap();
    b.sync_data().unwrap();
    disk.reset_counters();
    assert!(!b.reset_unfinished_creation().unwrap());
    assert_eq!(disk.state().writes, 0);
    assert_eq!(state(&disk), (4096, false));
}

// Regression (t-133): a torn length record write around the fresh to
// created transition reopens with the old or the new state, fresh-ness
// included.
#[test]
fn torn_record_writes_keep_old_or_new_fresh_state() {
    type Step = fn(&RegionBackend<TestDisk>) -> Result<(), io::Error>;
    /// (length, fresh) before and after the step.
    type State = (u64, bool);
    let steps: [(&str, Step, State, State); 3] = [
        (
            "grow while fresh",
            |b| b.set_len(4096),
            (512, true),
            (4096, true),
        ),
        (
            "clear",
            |b| b.fresh_flag().ok_or_else(bounds)?.clear(),
            (512, true),
            (512, false),
        ),
        (
            "reset",
            |b| b.reset_unfinished_creation().map(drop),
            (512, true),
            (0, true),
        ),
    ];
    for (name, step, old, new) in steps {
        for prefix in [1, 7, 12, 511] {
            for mode in [Crash::DropPending, Crash::KeepPending] {
                let disk = formatted();
                let b = backend(&disk);
                b.set_len(512).unwrap();
                let next = disk.state().writes;
                disk.state().cut = Some((next, prefix));
                assert!(step(&b).is_err(), "{name}");
                drop(b);
                disk.crash(mode);
                let got = reopen(&disk)
                    .map(|b| (b.len().unwrap(), b.is_fresh()))
                    .unwrap_or_else(|e| panic!("{name} {prefix} {mode:?}: {e:?}"));
                assert!(
                    got == old || got == new,
                    "{name} {prefix} {mode:?}: {got:?}"
                );
            }
        }
    }
}

// Regression guard (t-133): `REDB_MAGIC` is what redb 4.4.0 writes first in
// a database it creates; fail loudly if a redb upgrade changes it.
#[test]
fn redb_magic_matches_a_created_database() {
    let disk = TestDisk::new(LENGTH_SECTORS + 4096);
    let mut d = disk.clone();
    RegionBackend::format(&mut d).unwrap();
    // SAFETY: single-threaded test.
    let b = unsafe { RegionBackend::new(disk.clone(), LENGTH_SECTORS + 4096) }.unwrap();
    drop(redb::Database::builder().create_with_backend(b).unwrap());
    let head = sector(&disk, LENGTH_SECTORS);
    assert_eq!(head[..REDB_MAGIC.len()], REDB_MAGIC);
}

/// The guard's view of a region: both record copies and the file start.
fn head(disk: &TestDisk) -> [[u8; SECTOR_SIZE]; 3] {
    [
        sector(disk, 0),
        sector(disk, 1),
        sector(disk, LENGTH_SECTORS),
    ]
}

#[test]
fn region_is_corrupt_without_record_or_with_a_file_without_magic() {
    // Regression (t-133): the provisioning guard re-provisions a v3 card
    // only when its catalogue region is corrupt.
    let blank = TestDisk::new(SECTORS);
    assert!(region_is_corrupt(&head(&blank), SECTORS));
    let disk = formatted();
    assert!(
        !region_is_corrupt(&head(&disk), SECTORS),
        "empty is healthy"
    );
    let b = backend(&disk);
    b.set_len(4096).unwrap();
    b.write(0, &[0x5a; 512]).unwrap();
    b.sync_data().unwrap();
    assert!(region_is_corrupt(&head(&disk), SECTORS));
    let mut file = [0x5a; 512];
    file[..REDB_MAGIC.len()].copy_from_slice(&REDB_MAGIC);
    b.write(0, &file).unwrap();
    b.sync_data().unwrap();
    assert!(!region_is_corrupt(&head(&disk), SECTORS));
    // A length past this region's capacity is not a valid record: the
    // older copy (empty) is current, as `RegionBackend::new` would pick it.
    assert!(!region_is_corrupt(&head(&disk), LENGTH_SECTORS + 4));
    assert!(region_is_corrupt(&head(&disk), LENGTH_SECTORS));
}
