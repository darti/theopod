use super::*;
use crate::test_support::{Image, SECTORS};
use theo_storage::{
    redb_backend::{BackendError, RegionBackend},
    test_disk::TestDisk,
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

#[test]
fn region_without_redb_magic_is_corrupt() {
    // Regression (t-133): a non-empty region that is not a redb database is
    // a corrupt catalogue, not a device failure.
    let image = Image::new();
    {
        let backend = image.backend();
        redb::StorageBackend::set_len(&backend, 4096).unwrap();
        redb::StorageBackend::write(&backend, 0, &[0x5a; 4096]).unwrap();
        redb::StorageBackend::sync_data(&backend).unwrap();
    }
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Corrupt)
    ));
}
