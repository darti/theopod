use speculoos::prelude::*;

use crate::{
    co5300::{Co5300, PanelGeometry},
    framebuffer::{FlushError, flush_rows},
    test_support::{BusFault, PanelCall, RecordingBus, RecordingPanel, Transfer, block_on},
};

const WIDTH: u16 = 2;
const HEIGHT: u16 = 6;
const ROW_BYTES: usize = WIDTH as usize * 2;

/// Frame whose bytes count up, so every row is distinguishable.
fn frame() -> Vec<u8> {
    (0..ROW_BYTES * usize::from(HEIGHT))
        .map(|byte| u8::try_from(byte).unwrap())
        .collect()
}

fn rows_bytes(frame: &[u8], rows: core::ops::Range<usize>) -> Vec<u8> {
    frame[rows.start * ROW_BYTES..rows.end * ROW_BYTES].to_vec()
}

#[test]
fn flush_rows_full_frame_opens_whole_panel_and_sends_frame() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);
    let frame = frame();

    let result = block_on(flush_rows(&mut panel, &frame, 0..HEIGHT));

    assert_that!(result).is_ok();
    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin {
            x: 0..WIDTH,
            y: 0..HEIGHT,
        },
        PanelCall::Pixels(frame),
    ]);
}

// Regression guard: the CO5300 ignores areas not aligned on 2x2 blocks, so
// dirty rows are widened to even boundaries and full width.
#[test]
fn flush_rows_odd_bounds_widen_to_even_rows() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);
    let frame = frame();

    block_on(flush_rows(&mut panel, &frame, 1..3)).unwrap();

    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin {
            x: 0..WIDTH,
            y: 0..4,
        },
        PanelCall::Pixels(rows_bytes(&frame, 0..4)),
    ]);
}

#[test]
fn flush_rows_odd_panel_height_clamps_widened_end() {
    let mut panel = RecordingPanel::new(WIDTH, 5);
    let frame = frame()[..ROW_BYTES * 5].to_vec();

    block_on(flush_rows(&mut panel, &frame, 4..5)).unwrap();

    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin {
            x: 0..WIDTH,
            y: 4..5,
        },
        PanelCall::Pixels(rows_bytes(&frame, 4..5)),
    ]);
}

// Regression guard: the widening exists for the CO5300's 2x2 rule, so check
// it against the real driver, not only the permissive recording panel.
#[test]
fn flush_rows_odd_dirty_rows_into_co5300_opens_aligned_area() {
    let geometry = PanelGeometry {
        width: WIDTH,
        height: HEIGHT,
        column_offset: 0,
    };
    let mut panel = Co5300::new(RecordingBus::default(), geometry).unwrap();
    let frame = frame();

    let result = block_on(flush_rows(&mut panel, &frame, 1..3));

    assert_that!(result).is_ok();
    assert_that!(panel.release().transfers).is_equal_to(vec![
        Transfer::register(0x2A, &[0, 0, 0, 1]),
        Transfer::register(0x2B, &[0, 0, 0, 3]),
        Transfer::register(0x2C, &[]),
        Transfer::pixels(&rows_bytes(&frame, 0..4)),
    ]);
}

#[test]
fn flush_rows_begin_area_failure_returns_panel_error_without_pixels() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT).failing(0);

    let result = block_on(flush_rows(&mut panel, &frame(), 0..2));

    assert_that!(result.err()).is_equal_to(Some(FlushError::Panel(BusFault)));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn flush_rows_empty_range_sends_nothing() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_rows(&mut panel, &frame(), 3..3));

    assert_that!(result).is_ok();
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn flush_rows_beyond_panel_returns_rows_out_of_range() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_rows(&mut panel, &frame(), 4..7));

    assert_that!(result.err()).is_equal_to(Some(FlushError::RowsOutOfRange { rows: 4..7 }));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
#[expect(clippy::reversed_empty_ranges, reason = "the invalid input under test")]
fn flush_rows_reversed_range_returns_rows_out_of_range() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_rows(&mut panel, &frame(), 4..2));

    assert_that!(result.err()).is_equal_to(Some(FlushError::RowsOutOfRange { rows: 4..2 }));
}

#[test]
fn flush_rows_wrong_frame_size_returns_frame_size_mismatch() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_rows(&mut panel, &[0; 8], 0..2));

    assert_that!(result.err()).is_equal_to(Some(FlushError::FrameSizeMismatch {
        expected: 24,
        actual: 8,
    }));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn flush_rows_panel_failure_returns_panel_error() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT).failing(1);

    let result = block_on(flush_rows(&mut panel, &frame(), 0..2));

    assert_that!(result.err()).is_equal_to(Some(FlushError::Panel(BusFault)));
}

#[test]
fn flush_error_display_describes_size_mismatch() {
    let message = FlushError::<BusFault>::FrameSizeMismatch {
        expected: 24,
        actual: 8,
    }
    .to_string();

    assert_that!(message.as_str()).is_equal_to("frame holds 8 bytes, panel needs 24");
}
