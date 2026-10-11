//! Firmware side: writes a frame as text lines.
//!
//! - [`write_frame`]: writes one dump to any [`fmt::Write`].
//! - [`WriteError`]: why a dump was not written.

use core::fmt;

use crate::{BEGIN, END, ROW};

/// Bytes encoded per `write_str` call; bounds the stack buffer.
const HEX_CHUNK: usize = 64;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Reason a frame dump failed.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteError {
    /// The frame has no pixels; nothing written.
    EmptyFrame,
    /// `pixels` does not hold `width * height` RGB565 pixels; nothing written.
    SizeMismatch {
        /// Bytes of the frame.
        expected: usize,
        /// Bytes given.
        actual: usize,
    },
    /// The output failed mid-dump.
    Write(fmt::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => write!(f, "frame has no pixels"),
            Self::SizeMismatch { expected, actual } => {
                write!(f, "frame holds {actual} bytes, expected {expected}")
            }
            Self::Write(_) => write!(f, "frame dump output failed"),
        }
    }
}

impl core::error::Error for WriteError {}

impl From<fmt::Error> for WriteError {
    fn from(error: fmt::Error) -> Self {
        Self::Write(error)
    }
}

/// Writes `pixels`, a row-major big-endian RGB565 frame, as one dump.
///
/// # Errors
///
/// Returns [`WriteError::EmptyFrame`] or [`WriteError::SizeMismatch`]
/// before writing anything, or [`WriteError::Write`] if `out` fails.
pub fn write_frame(
    out: &mut impl fmt::Write,
    width: u16,
    height: u16,
    pixels: &[u8],
) -> Result<(), WriteError> {
    if width == 0 || height == 0 {
        return Err(WriteError::EmptyFrame);
    }
    let row_bytes = usize::from(width) * 2;
    let expected = row_bytes * usize::from(height);
    if pixels.len() != expected {
        return Err(WriteError::SizeMismatch {
            expected,
            actual: pixels.len(),
        });
    }

    writeln!(out, "{BEGIN} {width} {height}")?;
    for (y, row) in pixels.chunks(row_bytes).enumerate() {
        write!(out, "{ROW} {y} ")?;
        write_hex(out, row)?;
        out.write_char('\n')?;
    }
    writeln!(out, "{END}")?;
    Ok(())
}

fn write_hex(out: &mut impl fmt::Write, bytes: &[u8]) -> fmt::Result {
    let mut buffer = [0u8; HEX_CHUNK * 2];
    for chunk in bytes.chunks(HEX_CHUNK) {
        for (byte, digits) in chunk.iter().zip(buffer.as_chunks_mut::<2>().0) {
            digits[0] = HEX_DIGITS[usize::from(byte >> 4)];
            digits[1] = HEX_DIGITS[usize::from(byte & 0x0F)];
        }
        let hex = core::str::from_utf8(&buffer[..chunk.len() * 2]).map_err(|_| fmt::Error)?;
        out.write_str(hex)?;
    }
    Ok(())
}
