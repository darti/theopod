use speculoos::prelude::*;

use crate::write::{WriteError, write_frame};

#[test]
fn write_frame_emits_header_one_hex_row_per_line_and_footer() {
    let mut out = String::new();

    write_frame(
        &mut out,
        2,
        2,
        &[0xF8, 0x00, 0x07, 0xE0, 0x00, 0x1F, 0xAB, 0xCD],
    )
    .unwrap();

    assert_that!(out.as_str()).is_equal_to(
        "theo-frame-begin 2 2\n\
         theo-frame-row 0 f80007e0\n\
         theo-frame-row 1 001fabcd\n\
         theo-frame-end\n",
    );
}

#[test]
fn write_frame_long_row_stays_on_one_line() {
    let mut out = String::new();
    let pixels = vec![0x5A; 2 * 100];

    write_frame(&mut out, 100, 1, &pixels).unwrap();

    let row = out.lines().nth(1).unwrap();
    assert_that!(row.len()).is_equal_to("theo-frame-row 0 ".len() + 400);
}

#[test]
fn write_frame_wrong_pixel_count_returns_size_mismatch() {
    let mut out = String::new();

    let result = write_frame(&mut out, 2, 2, &[0; 6]);

    assert_that!(result.err()).is_equal_to(Some(WriteError::SizeMismatch {
        expected: 8,
        actual: 6,
    }));
    assert_that!(out.is_empty()).is_true();
}

// Regression: an empty frame was written but could never be parsed back.
#[test]
fn write_frame_zero_width_returns_empty_frame() {
    let mut out = String::new();

    let result = write_frame(&mut out, 0, 2, &[]);

    assert_that!(result.err()).is_equal_to(Some(WriteError::EmptyFrame));
    assert_that!(out.is_empty()).is_true();
}

#[test]
fn write_frame_multi_chunk_row_round_trips_exact_bytes() {
    let mut out = String::new();
    let pixels: Vec<u8> = (0..=255).chain(0..=255).collect();

    write_frame(&mut out, 256, 1, &pixels).unwrap();

    assert_that!(crate::parse_frame(&out).map(|frame| frame.pixels))
        .is_ok()
        .is_equal_to(pixels);
}
