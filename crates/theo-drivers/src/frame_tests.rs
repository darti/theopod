use speculoos::prelude::*;

use crate::{
    frame::{FrameError, FrameStreamer},
    test_support::{BusFault, PanelCall, RecordingPanel},
};

/// Streams `rows` as lines `0..rows.len()`, each filling the whole width.
fn stream_rows(streamer: &mut FrameStreamer<'_, RecordingPanel>, rows: &[&[u16]]) {
    for (line, row) in rows.iter().enumerate() {
        streamer.stream_line(line, 0..row.len(), |pixels| pixels.copy_from_slice(row));
    }
}

#[test]
fn new_buffer_narrower_than_panel_returns_line_buffer_too_small() {
    let mut panel = RecordingPanel::new(4, 2);
    let mut buffer = [0u16; 3];

    let result = FrameStreamer::new(&mut panel, &mut buffer);

    assert_that!(result.err()).is_equal_to(Some(FrameError::LineBufferTooSmall {
        required: 4,
        available: 3,
    }));
}

// Regression: the CO5300 needs one 2x2-aligned area; the whole frame is
// opened once, then every row is streamed into it.
#[test]
fn finish_full_frame_opens_one_area_and_streams_rows_big_endian() {
    let mut panel = RecordingPanel::new(2, 2);
    let mut buffer = [0u16; 2];
    let mut streamer = FrameStreamer::new(&mut panel, &mut buffer).unwrap();

    stream_rows(&mut streamer, &[&[0xF800, 0x07E0], &[0x001F, 0x1234]]);
    let result = streamer.finish();

    assert_that!(result).is_ok();
    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin { x: 0..2, y: 0..2 },
        PanelCall::Pixels(vec![0xF8, 0x00, 0x07, 0xE0]),
        PanelCall::Pixels(vec![0x00, 0x1F, 0x12, 0x34]),
    ]);
}

#[test]
fn stream_line_out_of_order_returns_unexpected_line() {
    let mut panel = RecordingPanel::new(2, 2);
    let mut buffer = [0u16; 2];
    let mut streamer = FrameStreamer::new(&mut panel, &mut buffer).unwrap();

    streamer.stream_line(1, 0..2, |_| {});
    let result = streamer.finish();

    assert_that!(result.err()).is_equal_to(Some(FrameError::UnexpectedLine {
        line: 1,
        range: 0..2,
    }));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn stream_line_partial_range_returns_unexpected_line() {
    let mut panel = RecordingPanel::new(4, 2);
    let mut buffer = [0u16; 4];
    let mut streamer = FrameStreamer::new(&mut panel, &mut buffer).unwrap();

    streamer.stream_line(0, 1..3, |_| {});
    let result = streamer.finish();

    assert_that!(result.err()).is_equal_to(Some(FrameError::UnexpectedLine {
        line: 0,
        range: 1..3,
    }));
    assert_that!(panel.calls.is_empty()).is_true();
}

#[test]
fn finish_buffer_wider_than_panel_sends_panel_width_rows() {
    let mut panel = RecordingPanel::new(2, 2);
    let mut buffer = [0u16; 3];
    let mut streamer = FrameStreamer::new(&mut panel, &mut buffer).unwrap();

    stream_rows(&mut streamer, &[&[0x0102, 0x0304], &[0x0506, 0x0708]]);
    let result = streamer.finish();

    assert_that!(result).is_ok();
    assert_that!(panel.calls).is_equal_to(vec![
        PanelCall::Begin { x: 0..2, y: 0..2 },
        PanelCall::Pixels(vec![0x01, 0x02, 0x03, 0x04]),
        PanelCall::Pixels(vec![0x05, 0x06, 0x07, 0x08]),
    ]);
}

#[test]
fn finish_before_last_line_returns_incomplete() {
    let mut panel = RecordingPanel::new(2, 2);
    let mut buffer = [0u16; 2];
    let mut streamer = FrameStreamer::new(&mut panel, &mut buffer).unwrap();

    stream_rows(&mut streamer, &[&[0, 0]]);
    let result = streamer.finish();

    assert_that!(result.err()).is_equal_to(Some(FrameError::Incomplete { lines: 1 }));
}

#[test]
fn stream_line_after_panel_error_keeps_first_error_and_stops() {
    let mut panel = RecordingPanel::new(2, 2).failing(1);
    let mut buffer = [0u16; 2];
    let mut streamer = FrameStreamer::new(&mut panel, &mut buffer).unwrap();
    let mut renders = 0;

    for line in 0..2 {
        streamer.stream_line(line, 0..2, |_| renders += 1);
    }
    let result = streamer.finish();

    assert_that!(result.err()).is_equal_to(Some(FrameError::Panel(BusFault)));
    assert_that!(renders).is_equal_to(1);
    assert_that!(panel.calls).is_equal_to(vec![PanelCall::Begin { x: 0..2, y: 0..2 }]);
}

#[test]
fn stream_line_begin_area_failure_skips_rendering() {
    let mut panel = RecordingPanel::new(2, 2).failing(0);
    let mut buffer = [0u16; 2];
    let mut streamer = FrameStreamer::new(&mut panel, &mut buffer).unwrap();
    let mut rendered = false;

    streamer.stream_line(0, 0..2, |_| rendered = true);
    let result = streamer.finish();

    assert_that!(result.err()).is_equal_to(Some(FrameError::Panel(BusFault)));
    assert_that!(rendered).is_false();
}
