//! redb's two-phase commit over `RegionBackend`, cut at every sector write.
use super::*;
use crate::test_disk::{Crash, TestDisk};
use redb::{Database, ReadableDatabase, ReadableTable, StorageBackend, TableDefinition};

const SECTORS: u32 = LENGTH_SECTORS + 32_768; // 16 MiB: room for redb's 8 MiB step
const T: TableDefinition<u64, &[u8]> = TableDefinition::new("t");

fn backend(disk: &TestDisk) -> RegionBackend<TestDisk> {
    // SAFETY: single-threaded test.
    unsafe { RegionBackend::new(disk.clone(), SECTORS) }.unwrap()
}
fn try_open(disk: &TestDisk) -> Result<Database, redb::DatabaseError> {
    Database::builder()
        .set_cache_size(256 * 1024)
        .create_with_backend(backend(disk))
}
fn open(disk: &TestDisk) -> Database {
    try_open(disk).unwrap()
}
fn len(disk: &TestDisk) -> u64 {
    backend(disk).len().unwrap()
}
fn put(db: &Database, from: u64, to: u64) -> Result<(), redb::Error> {
    let mut tx = db.begin_write()?;
    tx.set_quick_repair(true);
    {
        let mut t = tx.open_table(T)?;
        for i in from..to {
            t.insert(i, [i as u8; 1500].as_slice())?;
        }
    }
    tx.commit()?;
    Ok(())
}
fn count(db: &Database) -> u64 {
    let tx = db.begin_read().unwrap();
    let t = tx.open_table(T).unwrap();
    let mut n = 0;
    for entry in t.iter().unwrap() {
        let (k, v) = entry.unwrap();
        assert!(v.value().iter().all(|&b| b == k.value() as u8));
        n += 1;
    }
    n
}

fn base() -> TestDisk {
    let mut disk = TestDisk::new(SECTORS);
    RegionBackend::format(&mut disk).unwrap();
    put(&open(&disk), 0, BASE).unwrap();
    disk
}

/// Records in the base database, and after the probe commit; the probe
/// commit crosses redb's first growth (1 -> 2 MiB, near 470 records).
const BASE: u64 = 460;
const PROBE: u64 = 480;

#[test]
fn crash_at_every_write_of_a_commit_opens_old_or_new() {
    let probe = base().snapshot();
    let first = probe.state().writes;
    {
        let db = open(&probe);
        let opened = len(&probe);
        put(&db, BASE, PROBE).unwrap();
        // The cut commit must grow the file: growth is not zero-filled.
        let committed = len(&probe);
        assert!(committed > opened, "no growth: {opened} -> {committed}");
    }
    let total = probe.state().writes - first;
    for cut in 0..total {
        for prefix in [0, 1, 511] {
            for mode in [Crash::DropPending, Crash::KeepPending] {
                let disk = base();
                let start = disk.state().writes;
                disk.state().cut = Some((start + cut, prefix));
                // The cut may land in the open (redb grows the file) or in
                // the close; a failed open is a crash before the commit.
                if let Ok(db) = try_open(&disk) {
                    let _ = put(&db, BASE, PROBE);
                }
                disk.crash(mode);
                let n = count(&open(&disk));
                assert!(
                    n == BASE || n == PROBE,
                    "cut {cut} prefix {prefix} {mode:?}: {n} records"
                );
            }
        }
    }
}

// Regression (t-133): opening after a crash stays O(1) in sector reads.
// Release only: debug builds of redb walk every allocated page on open
// (`mark_allocated_page_for_debug`), about 8,000 reads here.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "redb walks every page on open in debug builds; run with --release"
)]
fn open_after_crash_reads_at_most_64_sectors() {
    let disk = base();
    put(&open(&disk), BASE, 2000).unwrap();
    let crashed = disk.snapshot();
    crashed.crash(Crash::DropPending);
    crashed.reset_counters();
    drop(open(&crashed));
    assert!(
        crashed.state().reads <= 64,
        "{} reads",
        crashed.state().reads
    );
}

// Regression (t-133): growth is not zero-filled (fresh sectors read 0xA5);
// a database grown through several of redb's doublings reopens intact
// after a crash in either mode.
#[test]
fn growth_through_three_doublings_survives_a_crash() {
    // redb doubles at about 470, 980 and 1990 records: 1 -> 2 -> 4 -> 8 MiB.
    const RECORDS: u64 = 2100;
    let disk = base();
    let db = open(&disk);
    let mut sizes = alloc::vec![len(&disk)];
    for from in (BASE..RECORDS).step_by(10) {
        put(&db, from, from + 10).unwrap();
        let size = len(&disk);
        if size > *sizes.last().unwrap() {
            sizes.push(size);
        }
    }
    assert!(sizes.len() > 3 && sizes[3] >= 8 << 20, "{sizes:?}");
    // Crash with the database still open: no clean shutdown.
    for mode in [Crash::DropPending, Crash::KeepPending] {
        let crashed = disk.snapshot();
        crashed.crash(mode);
        let db = open(&crashed);
        assert_eq!(count(&db), RECORDS, "{mode:?}");
        let tx = db.begin_read().unwrap();
        let t = tx.open_table(T).unwrap();
        for i in 0..RECORDS {
            let v = t.get(i).unwrap().unwrap();
            assert_eq!(v.value(), [i as u8; 1500].as_slice(), "{mode:?} record {i}");
        }
    }
    drop(db);
}
