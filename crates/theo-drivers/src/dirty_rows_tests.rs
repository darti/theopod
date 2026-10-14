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
