use speculoos::prelude::*;

use crate::dirty_rows::DirtyRows;

const HEIGHT: u16 = 448;

/// Bands as `(start, end)` pairs.
fn bands<const N: usize>(rows: &DirtyRows<N>) -> Vec<(u16, u16)> {
    rows.bands()
        .iter()
        .map(|band| (band.start, band.end))
        .collect()
}

#[test]
fn new_has_no_bands() {
    let rows = DirtyRows::<3>::new(HEIGHT);

    assert_that!(rows.is_empty()).is_true();
    assert_that!(bands(&rows)).is_empty();
}

#[test]
fn all_covers_every_row() {
    let rows = DirtyRows::<3>::all(HEIGHT);

    assert_that!(rows.is_empty()).is_false();
    assert_that!(bands(&rows)).is_equal_to(vec![(0, HEIGHT)]);
}

// Regression guard (t-5): the CO5300 ignores areas not aligned on 2x2 blocks.
#[test]
fn add_widens_to_even_rows() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);

    rows.add(5..8);

    assert_that!(bands(&rows)).is_equal_to(vec![(4, 8)]);
}

#[test]
fn add_on_odd_height_keeps_the_last_row_alone() {
    let mut rows = DirtyRows::<3>::new(5);

    rows.add(4..5);

    assert_that!(bands(&rows)).is_equal_to(vec![(4, 5)]);
}

#[test]
fn add_clamps_rows_past_the_panel() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);

    rows.add(446..500);

    assert_that!(bands(&rows)).is_equal_to(vec![(446, HEIGHT)]);
}

#[test]
fn add_empty_or_reversed_rows_is_ignored() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);

    rows.add(10..10);
    #[expect(clippy::reversed_empty_ranges, reason = "the case under test")]
    rows.add(20..12);
    rows.add(HEIGHT..HEIGHT + 2);

    assert_that!(rows.is_empty()).is_true();
}

#[test]
fn add_disjoint_bands_keeps_them_sorted() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);

    rows.add(400..420);
    rows.add(0..20);

    assert_that!(bands(&rows)).is_equal_to(vec![(0, 20), (400, 420)]);
}

#[test]
fn add_overlapping_bands_merges_them() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);

    rows.add(10..30);
    rows.add(20..40);

    assert_that!(bands(&rows)).is_equal_to(vec![(10, 40)]);
}

#[test]
fn add_bands_touching_once_widened_merges_them() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);

    // 0..4 and 4..6 once widened: no 2-row block is sent twice.
    rows.add(1..3);
    rows.add(5..6);

    assert_that!(bands(&rows)).is_equal_to(vec![(0, 6)]);
}

#[test]
fn add_band_bridging_two_bands_merges_all_three() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);
    rows.add(0..10);
    rows.add(100..110);

    rows.add(8..102);

    assert_that!(bands(&rows)).is_equal_to(vec![(0, 110)]);
}

#[test]
fn add_past_capacity_merges_the_closest_bands() {
    let mut rows = DirtyRows::<2>::new(HEIGHT);
    rows.add(0..2);
    rows.add(100..102);

    // Gaps: 0..2 to 100..102 is 98 rows, 100..102 to 110..112 is 8.
    rows.add(110..112);

    assert_that!(bands(&rows)).is_equal_to(vec![(0, 2), (100, 112)]);
}

#[test]
fn add_past_capacity_merges_a_closest_pair_before_the_new_band() {
    let mut rows = DirtyRows::<3>::new(HEIGHT);
    rows.add(0..2);
    rows.add(10..12);
    rows.add(100..102);

    // Gaps: 8, 88, then 98 to the new band.
    rows.add(200..202);

    assert_that!(bands(&rows)).is_equal_to(vec![(0, 12), (100, 102), (200, 202)]);
}

#[test]
fn add_past_capacity_band_sorting_first_merges_the_closest_older_pair() {
    let mut rows = DirtyRows::<2>::new(HEIGHT);
    rows.add(100..102);
    rows.add(110..112);

    rows.add(0..2);

    assert_that!(bands(&rows)).is_equal_to(vec![(0, 2), (100, 112)]);
}

#[test]
fn add_with_capacity_one_keeps_the_bounding_band() {
    let mut rows = DirtyRows::<1>::new(HEIGHT);
    rows.add(10..12);

    rows.add(0..2);

    assert_that!(bands(&rows)).is_equal_to(vec![(0, 12)]);
}

#[test]
fn add_on_largest_height_clamps_without_overflow() {
    let mut rows = DirtyRows::<3>::new(u16::MAX);

    rows.add(65533..65535);

    assert_that!(bands(&rows)).is_equal_to(vec![(65532, u16::MAX)]);
}

/// Checks `rows` against `dirty`, the rows added so far.
fn assert_invariants<const N: usize>(rows: &DirtyRows<N>, height: u16, dirty: &[bool]) {
    let bands = rows.bands();
    assert!(bands.len() <= N, "{bands:?}: more than {N} bands");
    for band in bands {
        assert!(
            band.start < band.end && band.end <= height,
            "{bands:?}: bad band"
        );
        assert!(band.start % 2 == 0, "{bands:?}: odd start");
        assert!(
            band.end % 2 == 0 || band.end == height,
            "{bands:?}: odd end"
        );
    }
    for pair in bands.windows(2) {
        assert!(
            pair[0].end < pair[1].start,
            "{bands:?}: not sorted and apart"
        );
    }
    for (row, &is_dirty) in (0..height).zip(dirty) {
        let covered = bands.iter().any(|band| band.contains(&row));
        assert!(!is_dirty || covered, "{bands:?}: row {row} lost");
    }
}

#[test]
fn add_any_two_or_three_ranges_keeps_the_invariants() {
    const H: u16 = 9;
    let ranges: Vec<_> = (0..=H)
        .flat_map(|start| (start + 1..=H + 1).map(move |end| start..end))
        .collect();
    for first in &ranges {
        for second in &ranges {
            for third in [None, Some(0..1), Some(7..9)] {
                let mut rows = DirtyRows::<2>::new(H);
                let mut dirty = [false; H as usize];
                for range in [Some(first.clone()), Some(second.clone()), third.clone()]
                    .into_iter()
                    .flatten()
                {
                    rows.add(range.clone());
                    for row in range.start..range.end.min(H) {
                        dirty[usize::from(row)] = true;
                    }
                    assert_invariants(&rows, H, &dirty);
                }
            }
        }
    }
}
