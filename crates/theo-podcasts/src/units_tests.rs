use super::*;
use crate::{catalogue::*, test_support::Image};
use allocator_api2::alloc::Global;
use futures::executor::block_on;
use theo_storage::page_store::PageStore;

fn allocate<A: Allocator>(t: &mut UnitTable<A>, max: u32) -> Option<(u32, u32)> {
    let run = t.find_run(t.cursor(), max)?;
    t.claim(run);
    Some((run.start, run.count))
}

#[test]
fn table_allocates_next_fit_runs_and_counts() {
    let mut t = UnitTable::new_in(40_000, Global).unwrap();
    assert_eq!(t.pages(), 2);
    assert_eq!(allocate(&mut t, 1), Some((0, 1)));
    assert_eq!(allocate(&mut t, 3), Some((1, 3)));
    assert!(t.release(0));
    assert_eq!(allocate(&mut t, 1), Some((4, 1))); // next-fit, not first-fit
    assert_eq!(t.used(), 4);
    assert!(!t.release(0)); // double free refused
    assert_eq!(t.page_of(32_768), 1);
}

// Regression (t-121): a run stops at the first allocated unit and never
// wraps; the scan itself wraps once to the start.
#[test]
fn runs_stop_at_allocated_units_and_scan_wraps() {
    let mut t = UnitTable::new_in(20, Global).unwrap();
    t.claim(Extent { start: 5, count: 1 });
    t.cursor = 2;
    assert_eq!(allocate(&mut t, 10), Some((2, 3)));
    assert_eq!(allocate(&mut t, 100), Some((6, 14)));
    assert_eq!(allocate(&mut t, 100), Some((0, 2))); // wrapped
    assert_eq!(allocate(&mut t, 1), None);
    assert_eq!(t.free(), 0);
}

// Regression (t-121): the scan skips full bytes and does not overflow at
// the end of the largest table.
#[test]
fn scan_skips_full_bytes_up_to_the_last_unit() {
    let mut t = UnitTable::new_in(MAX_UNITS, Global).unwrap();
    t.claim(Extent {
        start: 0,
        count: MAX_UNITS - 1,
    });
    assert_eq!(t.free(), 1);
    assert_eq!(allocate(&mut t, 64), Some((MAX_UNITS - 1, 1)));
    assert_eq!(t.cursor(), 0);
    assert_eq!(allocate(&mut t, 1), None);
    assert!(t.release(MAX_UNITS - 1));
    assert!(!t.release(MAX_UNITS));
    assert!(!t.release(u32::MAX));
}

#[test]
fn table_is_full_after_all_units() {
    let mut t = UnitTable::new_in(3, Global).unwrap();
    assert_eq!(allocate(&mut t, 3), Some((0, 3)));
    assert_eq!(allocate(&mut t, 1), None);
    assert_eq!(t.free(), 0);
}

// Regression (t-121): extent records round trip and validate their length,
// extents and size.
#[test]
fn record_round_trips_and_rejects_invalid() {
    let mut buf = [0; 4096];
    let extents = [
        Extent { start: 7, count: 2 },
        Extent {
            start: 20,
            count: 1,
        },
    ];
    let n = encode_record(RecordState::Temp, 5, &extents, &mut buf).unwrap();
    assert_eq!(n, RECORD_HEADER + 16);
    let r = RecordView::decode(&buf[..n]).unwrap();
    assert_eq!(
        (r.state, r.len, r.extent_count(), r.total_units()),
        (RecordState::Temp, 5, 2, 3)
    );
    assert!(r.extents().eq(extents));
    assert_eq!(r.extent(2), None);
    assert!(RecordView::decode(&buf[..n - 1]).is_none());
    // `len` past the extents' units.
    let n = encode_record(
        RecordState::Published,
        3 * UNIT_BYTES + 1,
        &extents,
        &mut buf,
    )
    .unwrap();
    assert!(RecordView::decode(&buf[..n]).is_none());
    // An empty or overflowing extent.
    let n = encode_record(
        RecordState::Temp,
        0,
        &[Extent { start: 1, count: 0 }],
        &mut buf,
    )
    .unwrap();
    assert!(RecordView::decode(&buf[..n]).is_none());
    let n = encode_record(
        RecordState::Temp,
        0,
        &[Extent {
            start: u32::MAX,
            count: 2,
        }],
        &mut buf,
    )
    .unwrap();
    assert!(RecordView::decode(&buf[..n]).is_none());
    // Too many extents, or too small a buffer, is refused without panicking.
    let many = [Extent { start: 0, count: 1 }; MAX_EXTENTS + 1];
    assert_eq!(encode_record(RecordState::Temp, 0, &many, &mut buf), None);
    assert_eq!(
        encode_record(RecordState::Temp, 0, &many[..MAX_EXTENTS], &mut buf),
        Some(RECORD_HEADER + MAX_EXTENTS * 8)
    );
    assert_eq!(
        encode_record(RecordState::Temp, 0, &extents, &mut [0; 10]),
        None
    );
    assert_eq!(MAX_EXTENTS, 510);
}

// Regression (t-121): bitmap, counter and records change in one transaction.
#[test]
fn committed_units_reload_with_matching_counter() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut t = UnitTable::new_in(100, Global).unwrap();
        cat.load_units(&mut t).await.unwrap();
        let (unit, _) = allocate(&mut t, 1).unwrap();
        let mut rec = [0; 4096];
        let n = encode_record(
            RecordState::Temp,
            0,
            &[Extent {
                start: unit,
                count: 1,
            }],
            &mut rec,
        )
        .unwrap();
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
                .extent(0),
            Some(Extent {
                start: unit,
                count: 1
            })
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

// Regression (t-121): a store of more than one bitmap page persists units
// allocated on both sides of the page boundary.
#[test]
fn units_across_the_page_boundary_reload() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut t = UnitTable::new_in(40_000, Global).unwrap();
        cat.load_units(&mut t).await.unwrap();
        t.cursor = UNITS_PER_PAGE - 8;
        let run = t.find_run(t.cursor(), 16).unwrap();
        assert_eq!(
            run,
            Extent {
                start: UNITS_PER_PAGE - 8,
                count: 16
            }
        );
        t.claim(run);
        let mut rec = [0; 4096];
        let n = encode_record(RecordState::Temp, 0, &[run], &mut rec).unwrap();
        cat.commit_units(&t, &[0, 1], EpisodeId(4), Some(&rec[..n]))
            .await
            .unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let mut again = UnitTable::new_in(40_000, Global).unwrap();
        cat.load_units(&mut again).await.unwrap();
        assert_eq!(again.used(), 16);
        let allocated: std::vec::Vec<u32> =
            (0..40_000).filter(|&u| again.is_allocated(u)).collect();
        assert_eq!(
            allocated,
            (UNITS_PER_PAGE - 8..UNITS_PER_PAGE + 8).collect::<std::vec::Vec<_>>()
        );
        assert_eq!(again.page_of(allocated[0]), 0);
        assert_eq!(again.page_of(allocated[15]), 1);
    });
}

// Regression (t-121): `commit_units` refuses unordered or out-of-range dirty
// pages before writing anything.
#[test]
fn unordered_dirty_pages_are_corrupt() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let t = UnitTable::new_in(40_000, Global).unwrap();
        let writes = image.writes.get();
        for dirty in [&[1u16, 0][..], &[0, 0], &[2]] {
            assert!(matches!(
                cat.commit_units(&t, dirty, EpisodeId(1), None).await,
                Err(Error::Corrupt)
            ));
        }
        assert_eq!(image.writes.get(), writes);
        cat.commit_units(&t, &[0, 1], EpisodeId(1), None)
            .await
            .unwrap();
    });
}

// Regression (t-121): a counter that disagrees with the bitmap is corruption, not a format.
#[test]
fn counter_mismatch_is_corrupt() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut t = UnitTable::new_in(100, Global).unwrap();
        allocate(&mut t, 1).unwrap(); // bitmap in RAM says 1 ...
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
