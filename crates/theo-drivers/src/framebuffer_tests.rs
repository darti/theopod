use speculoos::prelude::*;

use crate::{
    co5300::{Co5300, PanelGeometry},
    dirty_rows::DirtyRows,
    framebuffer::{FlushError, flush_bands, flush_rows},
    test_support::{
        AddressBus, BusFault, PanelCall, RecordingBus, RecordingPanel, Transfer, block_on,
        counting_frame,
    },
};

const WIDTH: u16 = 2;
const HEIGHT: u16 = 6;
const ROW_BYTES: usize = WIDTH as usize * 2;

/// The recording panel's frame: its bytes count up, so every row is
/// distinguishable.
fn frame() -> Vec<u8> {
    counting_frame(ROW_BYTES * usize::from(HEIGHT))
}

fn rows_bytes(frame: &[u8], rows: core::ops::Range<usize>) -> Vec<u8> {
    frame[rows.start * ROW_BYTES..rows.end * ROW_BYTES].to_vec()
}

#[test]
fn flush_rows_full_frame_opens_whole_panel_and_sends_frame() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);
    let frame = frame();

    let result = block_on(flush_rows(&mut panel, 0..HEIGHT));

    assert_that!(result).is_ok();
    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin {
            x: 0..WIDTH,
            y: 0..HEIGHT,
        },
        PanelCall::Pixels(frame),
    ]);
}

// Regression guard (t-8): the CO5300 ignores areas not aligned on 2x2 blocks, so
// dirty rows are widened to even boundaries and full width.
#[test]
fn flush_rows_odd_bounds_widen_to_even_rows() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);
    let frame = frame();

    block_on(flush_rows(&mut panel, 1..3)).unwrap();

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

    block_on(flush_rows(&mut panel, 4..5)).unwrap();

    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin {
            x: 0..WIDTH,
            y: 4..5,
        },
        PanelCall::Pixels(rows_bytes(&frame, 4..5)),
    ]);
}

// Regression guard (t-8): the widening exists for the CO5300's 2x2 rule, so check
// it against the real driver, not only the permissive recording panel.
#[test]
fn flush_rows_odd_dirty_rows_into_co5300_opens_aligned_area() {
    let geometry = PanelGeometry {
        width: WIDTH,
        height: HEIGHT,
        column_offset: 0,
    };
    let frame = frame();
    let mut panel =
        Co5300::new(RecordingBus::default().with_frame(frame.clone()), geometry).unwrap();

    let result = block_on(flush_rows(&mut panel, 1..3));

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

    let result = block_on(flush_rows(&mut panel, 0..2));

    assert_that!(result.err()).is_equal_to(Some(FlushError::Panel(BusFault)));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn flush_rows_empty_range_sends_nothing() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_rows(&mut panel, 3..3));

    assert_that!(result).is_ok();
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn flush_rows_beyond_panel_returns_rows_out_of_range() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_rows(&mut panel, 4..7));

    assert_that!(result.err()).is_equal_to(Some(FlushError::RowsOutOfRange { rows: 4..7 }));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
#[expect(clippy::reversed_empty_ranges, reason = "the invalid input under test")]
fn flush_rows_reversed_range_returns_rows_out_of_range() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_rows(&mut panel, 4..2));

    assert_that!(result.err()).is_equal_to(Some(FlushError::RowsOutOfRange { rows: 4..2 }));
}

#[test]
fn flush_rows_wrong_frame_size_returns_frame_size_mismatch() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT).with_frame(vec![0; 8]);

    let result = block_on(flush_rows(&mut panel, 0..2));

    assert_that!(result.err()).is_equal_to(Some(FlushError::FrameSizeMismatch {
        expected: 24,
        actual: 8,
    }));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn flush_rows_panel_failure_returns_panel_error() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT).failing(1);

    let result = block_on(flush_rows(&mut panel, 0..2));

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

/// Firmware panel and transfer size: keep in sync with `DISPLAY_GEOMETRY`
/// in `src/device/mod.rs` and `MAX_TRANSFER_LEN` in `src/device/esp_qspi.rs`.
/// The firmware DMA reads each pixel transfer from where it starts, so starts
/// must follow a fixed stride.
const PANEL: PanelGeometry = PanelGeometry {
    width: 368,
    height: 448,
    column_offset: 16,
};
const PANEL_TRANSFER: usize = 32_704;

#[test]
fn flush_rows_pixel_transfers_start_at_even_row_plus_whole_chunks() {
    let row_bytes = usize::from(PANEL.width) * 2;
    let frame_len = row_bytes * usize::from(PANEL.height);
    for rows in [0..448, 1..2, 43..47, 301..448, 447..448] {
        let bus = AddressBus::<PANEL_TRANSFER>::with_frame_len(frame_len);
        let mut panel = Co5300::new(bus, PANEL).unwrap();

        block_on(flush_rows(&mut panel, rows.clone())).unwrap();

        let band_start = usize::from(rows.start & !1) * row_bytes;
        let band_end = usize::from(rows.end.next_multiple_of(2)) * row_bytes;
        let transfers = panel.release().transfers;
        let expected: Vec<(usize, usize)> = (band_start..band_end)
            .step_by(PANEL_TRANSFER)
            .map(|offset| (offset, (band_end - offset).min(PANEL_TRANSFER)))
            .collect();
        assert_that!(transfers)
            .named(&format!("rows {rows:?}"))
            .is_equal_to(expected);
    }
}

#[test]
fn flush_bands_sends_each_band_as_its_own_area() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);
    let frame = frame();
    let mut rows = DirtyRows::<3>::new(HEIGHT);
    rows.add(4..6);
    rows.add(0..2);

    let result = block_on(flush_bands(&mut panel, &rows));

    assert_that!(result).is_ok();
    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin {
            x: 0..WIDTH,
            y: 0..2,
        },
        PanelCall::Pixels(rows_bytes(&frame, 0..2)),
        PanelCall::Begin {
            x: 0..WIDTH,
            y: 4..6,
        },
        PanelCall::Pixels(rows_bytes(&frame, 4..6)),
    ]);
}

#[test]
fn flush_bands_without_bands_sends_nothing() {
    let mut panel = RecordingPanel::new(WIDTH, HEIGHT);

    let result = block_on(flush_bands(&mut panel, &DirtyRows::<3>::new(HEIGHT)));

    assert_that!(result).is_ok();
    assert_that!(panel.calls).is_empty();
}

#[test]
fn flush_bands_failure_stops_at_the_failing_band() {
    const ROWS: u16 = 10;
    let mut panel = RecordingPanel::new(WIDTH, ROWS).failing(2);
    let mut rows = DirtyRows::<3>::new(ROWS);
    rows.add(0..2);
    rows.add(4..6);
    rows.add(8..10);

    let result = block_on(flush_bands(&mut panel, &rows));

    assert_that!(result).is_equal_to(Err(FlushError::Panel(BusFault)));
    // First band, then the failed begin of the second; the third is skipped.
    assert_that!(panel.attempts).is_equal_to(3);
}
