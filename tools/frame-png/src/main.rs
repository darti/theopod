//! Converts the last framebuffer dump of a serial log into a PNG.
//!
//! Usage: `frame-png <log> <out.png>`; the log is the serial output of a
//! firmware built with the `frame-dump` feature, as recorded by
//! `just flash-dump` or `just monitor-log`.

use std::{env, error::Error, fs, io::BufWriter, process::ExitCode};

use theo_frame_dump::parse_frame;

#[cfg(test)]
mod main_tests;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let [log, out] = args.as_slice() else {
        eprintln!("usage: frame-png <log> <out.png>");
        return ExitCode::from(2);
    };
    match convert(log, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("frame-png: {error}");
            ExitCode::FAILURE
        }
    }
}

fn convert(log: &str, out: &str) -> Result<(), Box<dyn Error>> {
    // Serial logs may hold bytes that are not UTF-8 (boot noise).
    let bytes = fs::read(log).map_err(|error| format!("cannot read {log}: {error}"))?;
    let frame =
        parse_frame(&String::from_utf8_lossy(&bytes)).map_err(|error| format!("{log}: {error}"))?;

    let file = fs::File::create(out).map_err(|error| format!("cannot create {out}: {error}"))?;
    let file = BufWriter::new(file);
    let mut encoder = png::Encoder::new(file, u32::from(frame.width), u32::from(frame.height));
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&frame.to_rgb8())?;

    println!("{out}: {} x {} frame", frame.width, frame.height);
    Ok(())
}
