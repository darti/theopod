use super::*;
use crate::test_support::{Image, SECTORS};
use redb::{ReadableDatabase, StorageBackend};
use theo_storage::{
    redb_backend::{BackendError, RegionBackend},
    test_disk::{Crash, TestDisk},
};

#[test]
fn formatted_region_opens_as_an_empty_catalogue() {
    // Regression (t-133): provisioning writes only the length record; the
    // first open creates the tables and counters.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let page = cat.scan(None, &mut scratch, |_| ()).unwrap();
    assert_eq!(page.records, 0);
    assert!(!page.has_more);
    assert!(cat.next_subscription(None).unwrap().is_none());
}

#[test]
fn second_open_sees_the_same_counters() {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let first = cat
        .upsert(
            SubscriptionId(1),
            EpisodeMetadata {
                guid: Some("g"),
                title: "t",
                enclosure: "https://x/a.mp3",
                description: None,
                published: None,
                enclosure_bytes: None,
                duration_seconds: None,
            },
            &mut scratch,
        )
        .unwrap();
    assert_eq!(first.id, EpisodeId(1));
    drop(cat);
    let mut cat = image.reopen();
    let second = cat
        .upsert(
            SubscriptionId(1),
            EpisodeMetadata {
                guid: Some("h"),
                title: "t",
                enclosure: "https://x/b.mp3",
                description: None,
                published: None,
                enclosure_bytes: None,
                duration_seconds: None,
            },
            &mut scratch,
        )
        .unwrap();
    assert_eq!(second.id, EpisodeId(2));
}

#[test]
fn unformatted_region_is_corrupt() {
    // Regression (t-133): a region without a length record is never
    // formatted at boot: the backend refuses it before redb sees it.
    let disk = TestDisk::new(SECTORS);
    // SAFETY: single-threaded test.
    let backend = unsafe { RegionBackend::new(disk.clone(), SECTORS) };
    assert!(matches!(backend, Err(BackendError::Corrupt)));
    assert_eq!(disk.state().writes, 0);
}

/// Overwrite the start of the redb file with bytes that are not redb's.
fn damage_header(image: &Image) {
    let backend = image.backend();
    if backend.len().unwrap() < 4096 {
        backend.set_len(4096).unwrap();
    }
    backend.write(0, &[0x5a; 4096]).unwrap();
    backend.sync_data().unwrap();
}

/// A usable empty catalogue: schema 3, every table present (`open`
/// validates them), nothing stored, and the region no longer fresh.
fn assert_empty(image: &Image, cat: &mut Catalogue, context: &str) {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let page = cat.scan(None, &mut scratch, |_| ()).unwrap();
    assert_eq!(page.records, 0, "{context}");
    assert!(cat.next_subscription(None).unwrap().is_none(), "{context}");
    {
        let tx = cat.db.begin_read().unwrap();
        let counters = tx.open_table(crate::tables::COUNTERS).unwrap();
        let schema = number(&counters, crate::tables::SCHEMA_KEY).unwrap();
        assert_eq!(schema, Some(crate::tables::SCHEMA), "{context}");
        assert_eq!(crate::tables::SCHEMA, 3);
    }
    assert!(!image.backend().is_fresh(), "{context}");
}

#[test]
fn region_without_redb_magic_is_corrupt() {
    // Regression (t-133): a non-empty region that is not a redb database is
    // a corrupt catalogue, not a device failure; once the catalogue was
    // created, boot never resets it (ADR-0015): no write at all.
    let image = Image::new();
    drop(image.create());
    assert!(!image.backend().is_fresh());
    damage_header(&image);
    image.disk.reset_counters();
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Corrupt)
    ));
    assert_eq!(image.disk.state().writes, 0);
    assert!(!image.backend().is_fresh());
}

#[test]
fn fresh_region_without_redb_magic_is_created_again() {
    // Regression (t-133): a creation cut before redb wrote its magic left
    // "Not a redb database" (`Corrupt`) on a v3 card the provisioning guard
    // refused: the card was stuck. A fresh region is reset and created.
    let image = Image::new();
    damage_header(&image);
    assert!(image.backend().is_fresh());
    let mut cat = Catalogue::open(image.backend()).unwrap();
    assert_empty(&image, &mut cat, "reset");
}

#[test]
fn first_open_cut_at_every_write_reopens_an_empty_catalogue() {
    // Regression (t-133): cuts 1 and 2 of the first open (redb's header
    // without, then with its magic) used to leave `Corrupt` for good.
    let probe = Image::new();
    let start = probe.disk.state().writes;
    drop(probe.create());
    let total = probe.disk.state().writes - start;
    assert!(total > 3, "{total} writes");
    for cut in 0..total {
        for prefix in [0, 1, 511] {
            for mode in [Crash::DropPending, Crash::KeepPending] {
                let context = std::format!("cut {cut}/{total} prefix {prefix} {mode:?}");
                let image = Image::new();
                let start = image.disk.state().writes;
                image.disk.state().cut = Some((start + cut, prefix));
                // The cut fails this open, or its drop: either is a crash.
                drop(Catalogue::open(image.backend()));
                image.disk.crash(mode);
                let mut cat =
                    Catalogue::open(image.backend()).unwrap_or_else(|e| panic!("{context}: {e:?}"));
                assert_empty(&image, &mut cat, &context);
                drop(cat);
                let mut cat = image.reopen();
                assert_empty(&image, &mut cat, &context);
            }
        }
    }
}

// Regression (t-133): the first open of a fresh region is one creation
// commit plus the fresh flag's clearing, well inside MOUNT_BUDGET (2 s at
// about 2 ms per sector command).
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn first_open_on_a_fresh_region_stays_within_the_mount_budget() {
    let image = Image::new();
    image.disk.reset_counters();
    drop(image.create());
    let s = image.disk.state();
    std::eprintln!("first open: {} reads + {} writes", s.reads, s.writes);
    assert!(
        s.reads + s.writes <= 500,
        "{} reads + {} writes",
        s.reads,
        s.writes
    );
}

#[test]
fn redb_refusal_text_matches_not_redb() {
    // Regression (t-133): the no_std `Corrupt` mapping matches redb 4.4.0's
    // message text; fail loudly if a redb upgrade changes it.
    let image = Image::new();
    {
        let backend = image.backend();
        redb::StorageBackend::set_len(&backend, 4096).unwrap();
        redb::StorageBackend::write(&backend, 0, &[0x5a; 4096]).unwrap();
        redb::StorageBackend::sync_data(&backend).unwrap();
    }
    let error = redb::Database::builder()
        .create_with_backend(image.backend())
        .err()
        .unwrap();
    assert!(
        std::format!("{error}").contains(crate::catalogue::NOT_REDB),
        "{error}"
    );
}
