use super::*;
use crate::{catalogue::*, test_support::Image};
use allocator_api2::alloc::Global;
use futures::executor::block_on;
use theo_storage::page_store::PageStore;

#[test]
fn table_allocates_next_fit_and_counts() {
    let mut t = UnitTable::new_in(40_000, Global).unwrap();
    assert_eq!(t.pages(), 2);
    assert_eq!(t.allocate(), Some(0));
    assert_eq!(t.allocate(), Some(1));
    assert!(t.release(0));
    assert_eq!(t.allocate(), Some(2)); // next-fit, not first-fit
    assert_eq!(t.used(), 2);
    assert!(!t.release(0)); // double free refused
    assert_eq!(t.page_of(32_768), 1);
}

#[test]
fn table_is_full_after_all_units() {
    let mut t = UnitTable::new_in(3, Global).unwrap();
    for _ in 0..3 {
        t.allocate().unwrap();
    }
    assert_eq!(t.allocate(), None);
    assert_eq!(t.free(), 0);
}

#[test]
fn record_round_trips_and_rejects_oversize() {
    let mut buf = [0; 4096];
    let n = encode_record(RecordState::Temp, 5, &[7, 9], &mut buf);
    let r = RecordView::decode(&buf[..n]).unwrap();
    assert_eq!(
        (r.state, r.len, r.count(), r.unit(1)),
        (RecordState::Temp, 5, 2, 9)
    );
    assert!(RecordView::decode(&buf[..n - 1]).is_none());
    assert_eq!(RECORD_HEADER + MAX_RECORD_UNITS * 4, 4093);
}

// Regression (t-121): bitmap, counter and records change in one transaction.
#[test]
fn committed_units_reload_with_matching_counter() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut t = UnitTable::new_in(100, Global).unwrap();
        cat.load_units(&mut t).await.unwrap();
        let unit = t.allocate().unwrap();
        let mut rec = [0; 4096];
        let n = encode_record(RecordState::Temp, 0, &[unit], &mut rec);
        cat.commit_units(&t, &[t.page_of(unit)], EpisodeId(3), Some(&rec[..n]))
            .await
            .unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let mut again = UnitTable::new_in(100, Global).unwrap();
        cat.load_units(&mut again).await.unwrap();
        assert_eq!(again.used(), 1);
        let mut buf = [0; 4096];
        assert_eq!(
            cat.unit_record(EpisodeId(3), &mut buf)
                .await
                .unwrap()
                .unwrap()
                .unit(0),
            unit
        );
        let mut ids = [EpisodeId(0); crate::media::FILE_PAGE];
        assert_eq!(cat.unit_ids(None, &mut ids, &mut buf).await.unwrap(), 1);
        assert_eq!(ids[0], EpisodeId(3));
        assert_eq!(
            cat.unit_ids(Some(EpisodeId(3)), &mut ids, &mut buf)
                .await
                .unwrap(),
            0
        );
        // Deleting the record in a later commit removes it.
        cat.commit_units(&again, &[], EpisodeId(3), None)
            .await
            .unwrap();
        assert!(
            cat.unit_record(EpisodeId(3), &mut buf)
                .await
                .unwrap()
                .is_none()
        );
    });
}

// Regression (t-121): a counter that disagrees with the bitmap is corruption, not a format.
#[test]
fn counter_mismatch_is_corrupt() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut t = UnitTable::new_in(100, Global).unwrap();
        t.allocate().unwrap(); // bitmap in RAM says 1 ...
        let page = t.page(0).to_vec();
        let mut tx = cat.db.write_transaction().await;
        tx.write(&[b'b', 0, 0], &page).await.unwrap(); // ... counter stays 0
        tx.commit().await.unwrap();
        let mut fresh = UnitTable::new_in(100, Global).unwrap();
        assert!(matches!(
            cat.load_units(&mut fresh).await,
            Err(Error::Corrupt)
        ));
    });
}

// Regression (t-121): a bit set past the last unit is corruption.
#[test]
fn bit_beyond_units_is_corrupt() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut page = [0u8; 4096];
        page[25] = 1; // unit 200 >= 100
        let mut tx = cat.db.write_transaction().await;
        tx.write(&[b'b', 0, 0], &page).await.unwrap();
        tx.write(UNITS_KEY, &1u32.to_le_bytes()).await.unwrap();
        tx.commit().await.unwrap();
        let mut fresh = UnitTable::new_in(100, Global).unwrap();
        assert!(matches!(
            cat.load_units(&mut fresh).await,
            Err(Error::Corrupt)
        ));
    });
}

// Regression (t-121): schema 1 catalogues (no unit accounting) are refused, never migrated.
#[test]
fn schema_one_is_rejected_on_open() {
    block_on(async {
        let image = Image::new();
        let cat = image.create().await;
        let mut tx = cat.db.write_transaction().await;
        tx.write(SCHEMA_KEY, &1u32.to_le_bytes()).await.unwrap();
        tx.commit().await.unwrap();
        drop(cat);
        let result = Catalogue::open(PageStore::open(image.file(), 32).await.unwrap()).await;
        assert!(matches!(result, Err(Error::Schema)));
    });
}
