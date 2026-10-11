use speculoos::prelude::*;

use crate::{
    Frame,
    parse::{ParseError, parse_frame},
    write::write_frame,
};

fn dump(width: u16, height: u16, pixels: &[u8]) -> String {
    let mut out = String::new();
    write_frame(&mut out, width, height, pixels).unwrap();
    out
}

#[test]
fn parse_frame_round_trips_written_frame() {
    let pixels: Vec<u8> = (0..24).collect();

    let frame = parse_frame(&dump(3, 4, &pixels));

    assert_that!(frame).is_ok().is_equal_to(Frame {
        width: 3,
        height: 4,
        pixels,
    });
}

#[test]
fn parse_frame_ignores_log_lines_prefixes_and_crlf() {
    let log = format!(
        "INFO - heap usage\r\n\x1b[0m{}garbage line\r\n",
        dump(1, 1, &[0x12, 0x34]).replace('\n', "\r\n")
    );

    let frame = parse_frame(&log).unwrap();

    assert_that!(frame.pixels).is_equal_to(vec![0x12, 0x34]);
}

#[test]
fn parse_frame_several_dumps_returns_last_complete_one() {
    let log = dump(1, 1, &[0, 1]) + &dump(1, 1, &[2, 3]) + "theo-frame-begin 1 1\n";

    let frame = parse_frame(&log).unwrap();

    assert_that!(frame.pixels).is_equal_to(vec![2, 3]);
}

#[test]
fn parse_frame_without_dump_returns_no_frame() {
    assert_that!(parse_frame("boot\nINFO hello\n").err()).is_equal_to(Some(ParseError::NoFrame));
}

#[test]
fn parse_frame_truncated_dump_returns_incomplete() {
    let log = "theo-frame-begin 1 2\ntheo-frame-row 0 0000\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::Incomplete { line: 1 }));
}

#[test]
fn parse_frame_missing_row_returns_missing_row() {
    let log = "theo-frame-begin 1 2\ntheo-frame-row 1 0000\ntheo-frame-end\n";

    assert_that!(parse_frame(log).err())
        .is_equal_to(Some(ParseError::MissingRow { line: 3, row: 0 }));
}

#[test]
fn parse_frame_short_row_returns_bad_row() {
    let log = "theo-frame-begin 2 1\ntheo-frame-row 0 0000\ntheo-frame-end\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadRow { line: 2 }));
}

#[test]
fn parse_frame_row_beyond_height_returns_bad_row() {
    let log = "theo-frame-begin 1 1\ntheo-frame-row 1 0000\ntheo-frame-end\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadRow { line: 2 }));
}

#[test]
fn parse_frame_non_hex_row_returns_bad_row() {
    let log = "theo-frame-begin 1 1\ntheo-frame-row 0 zz00\ntheo-frame-end\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadRow { line: 2 }));
}

#[test]
fn parse_frame_bad_header_returns_bad_header() {
    let log = "theo-frame-begin wide 1\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadHeader { line: 1 }));
}

#[test]
fn to_rgb8_expands_big_endian_rgb565() {
    let frame = Frame {
        width: 3,
        height: 1,
        pixels: vec![0xF8, 0x00, 0x07, 0xE0, 0xFF, 0xFF],
    };

    assert_that!(frame.to_rgb8()).is_equal_to(vec![255, 0, 0, 0, 255, 0, 255, 255, 255]);
}

#[test]
fn parse_frame_corrupted_later_dump_keeps_earlier_complete_one() {
    let log = dump(1, 1, &[4, 5]) + "theo-frame-begin 1 1\ntheo-frame-row 0 zz\ntheo-frame-end\n";

    let frame = parse_frame(&log).unwrap();

    assert_that!(frame.pixels).is_equal_to(vec![4, 5]);
}

// Regression: the header sized an allocation straight from the log, so one
// corrupted line could make the tool allocate gigabytes and abort.
#[test]
fn parse_frame_oversized_header_returns_bad_header() {
    let log = "theo-frame-begin 65535 65535\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadHeader { line: 1 }));
}

#[test]
fn parse_frame_zero_size_header_returns_bad_header() {
    let log = "theo-frame-begin 0 2\ntheo-frame-end\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadHeader { line: 1 }));
}

#[test]
fn parse_frame_signed_hex_pair_returns_bad_row() {
    let log = "theo-frame-begin 1 1\ntheo-frame-row 0 +f00\ntheo-frame-end\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadRow { line: 2 }));
}

#[test]
fn parse_frame_signed_row_index_returns_bad_row() {
    let log = "theo-frame-begin 1 1\ntheo-frame-row +0 0000\ntheo-frame-end\n";

    assert_that!(parse_frame(log).err()).is_equal_to(Some(ParseError::BadRow { line: 2 }));
}

#[test]
fn parse_frame_uppercase_hex_decodes() {
    let log = "theo-frame-begin 1 1\ntheo-frame-row 0 ABcd\ntheo-frame-end\n";

    assert_that!(parse_frame(log).map(|frame| frame.pixels))
        .is_ok()
        .is_equal_to(vec![0xAB, 0xCD]);
}
