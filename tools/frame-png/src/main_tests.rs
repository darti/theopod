use std::{fs, path::PathBuf};

use speculoos::prelude::*;

use crate::convert;

/// Fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frame-png-{}-{name}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn convert_dump_writes_png_with_frame_size_and_pixels() {
    let dir = scratch("ok");
    let log = dir.join("frame.log");
    let out = dir.join("frame.png");
    fs::write(
        &log,
        "boot\ntheo-frame-begin 2 1\ntheo-frame-row 0 f80007e0\ntheo-frame-end\n",
    )
    .unwrap();

    convert(log.to_str().unwrap(), out.to_str().unwrap()).unwrap();

    let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(&out).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    assert_that!((reader.info().width, reader.info().height)).is_equal_to((2, 1));
    assert_that!(pixels).is_equal_to(vec![255, 0, 0, 0, 255, 0]);
}

#[test]
fn convert_missing_log_names_the_file() {
    let dir = scratch("missing");
    let log = dir.join("absent.log");

    let error = convert(log.to_str().unwrap(), dir.join("out.png").to_str().unwrap())
        .unwrap_err()
        .to_string();

    assert_that!(error.as_str()).starts_with("cannot read ");
    assert_that!(error.as_str()).contains("absent.log");
}

#[test]
fn convert_log_without_dump_reports_no_frame() {
    let dir = scratch("empty");
    let log = dir.join("frame.log");
    fs::write(&log, "boot only\n").unwrap();

    let error = convert(log.to_str().unwrap(), dir.join("out.png").to_str().unwrap())
        .unwrap_err()
        .to_string();

    assert_that!(error.as_str()).ends_with("no frame dump in the log");
}
