//! Host side: finds and decodes frame dumps in a serial log.
//!
//! - [`parse_frame`]: decodes the last complete dump of a log.
//! - [`Frame`]: decoded frame.
//! - [`ParseError`]: why no frame could be decoded.

use alloc::{vec, vec::Vec};
use core::fmt;

use crate::{BEGIN, END, ROW};

/// Largest frame accepted, in pixels: well above any panel, small enough
/// that a corrupted header cannot exhaust memory (32 MB of pixels).
const MAX_PIXELS: usize = 4096 * 4096;

/// Frame decoded from a dump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Row-major big-endian RGB565 pixels.
    pub pixels: Vec<u8>,
}

impl Frame {
    /// Expands the pixels to 8-bit RGB triplets.
    pub fn to_rgb8(&self) -> Vec<u8> {
        self.pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&pixel| {
                let value = u16::from_be_bytes(pixel);
                let expand5 = |bits: u16| ((bits << 3) | (bits >> 2)) as u8;
                let green = (value >> 5) & 0x3F;
                [
                    expand5(value >> 11),
                    ((green << 2) | (green >> 4)) as u8,
                    expand5(value & 0x1F),
                ]
            })
            .collect()
    }
}

/// Reason no frame was decoded; `line` numbers start at 1.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The log holds no dump.
    NoFrame,
    /// The dump starting at `line` has no end marker.
    Incomplete {
        /// Line of the begin marker.
        line: usize,
    },
    /// The begin marker at `line` has no valid size (empty, malformed, or
    /// above 4096 x 4096 pixels).
    BadHeader {
        /// Line of the begin marker.
        line: usize,
    },
    /// The row at `line` is malformed, out of range or of the wrong length.
    BadRow {
        /// Line of the row.
        line: usize,
    },
    /// The dump ending at `line` lacks `row`.
    MissingRow {
        /// Line of the end marker.
        line: usize,
        /// First missing row.
        row: u16,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFrame => write!(f, "no frame dump in the log"),
            Self::Incomplete { line } => write!(f, "dump starting at line {line} never ends"),
            Self::BadHeader { line } => write!(f, "invalid dump header at line {line}"),
            Self::BadRow { line } => write!(f, "invalid row at line {line}"),
            Self::MissingRow { line, row } => {
                write!(f, "dump ending at line {line} lacks row {row}")
            }
        }
    }
}

impl core::error::Error for ParseError {}

/// Dump being decoded.
struct Partial {
    begin_line: usize,
    frame: Frame,
    received: Vec<bool>,
}

/// Decodes the last complete dump in `log`.
///
/// Markers may follow any prefix on their line (log decorations), and
/// lines may end with `\r\n`. A later dump that is cut short or corrupted
/// does not hide an earlier complete one.
///
/// # Errors
///
/// Returns [`ParseError::NoFrame`] if the log holds no dump, otherwise the
/// error of the last dump when none is complete and valid.
pub fn parse_frame(log: &str) -> Result<Frame, ParseError> {
    let mut found = None;
    let mut last = ParseError::NoFrame;
    let mut partial: Option<Partial> = None;
    for (index, line) in log.lines().enumerate() {
        let line_number = index + 1;
        let line = line.trim_end_matches('\r');
        if let Some(rest) = after_marker(line, BEGIN) {
            partial = None;
            match parse_header(rest) {
                Some((width, height)) => {
                    partial = Some(Partial {
                        begin_line: line_number,
                        frame: Frame {
                            width,
                            height,
                            pixels: vec![0; usize::from(width) * usize::from(height) * 2],
                        },
                        received: vec![false; usize::from(height)],
                    });
                }
                None => last = ParseError::BadHeader { line: line_number },
            }
        } else if let Some(rest) = after_marker(line, ROW) {
            if let Some(dump) = partial.as_mut()
                && parse_row(dump, rest).is_none()
            {
                last = ParseError::BadRow { line: line_number };
                partial = None;
            }
        } else if after_marker(line, END).is_some()
            && let Some(dump) = partial.take()
        {
            match dump.received.iter().position(|&row| !row) {
                Some(row) => {
                    last = ParseError::MissingRow {
                        line: line_number,
                        // Fits: rows are indexed by a u16 height.
                        row: u16::try_from(row).unwrap_or(u16::MAX),
                    };
                }
                None => found = Some(dump.frame),
            }
        }
    }
    match (found, partial) {
        (Some(frame), _) => Ok(frame),
        (None, Some(dump)) => Err(ParseError::Incomplete {
            line: dump.begin_line,
        }),
        (None, None) => Err(last),
    }
}

/// Text after `marker` and one separator, if the line holds the marker.
fn after_marker<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &line[line.find(marker)? + marker.len()..];
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn parse_header(rest: &str) -> Option<(u16, u16)> {
    let mut fields = rest.split_whitespace();
    let width = parse_decimal(fields.next()?)?;
    let height = parse_decimal(fields.next()?)?;
    let pixels = usize::from(width) * usize::from(height);
    let valid = fields.next().is_none() && pixels > 0 && pixels <= MAX_PIXELS;
    valid.then_some((width, height))
}

/// Parses digits only: `str::parse` would also accept a leading `+`.
fn parse_decimal<T: core::str::FromStr>(text: &str) -> Option<T> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Value of one hex digit; `None` for anything else, including `+`.
fn hex_digit(byte: u8) -> Option<u8> {
    // Fits: a hex digit is below 16.
    char::from(byte).to_digit(16).map(|digit| digit as u8)
}

/// Stores the row in `dump`; `None` if it is malformed.
fn parse_row(dump: &mut Partial, rest: &str) -> Option<()> {
    let (y, hex) = rest.split_once(' ')?;
    let y: usize = parse_decimal(y)?;
    let row_bytes = usize::from(dump.frame.width) * 2;
    if y >= usize::from(dump.frame.height) || hex.len() != row_bytes * 2 {
        return None;
    }
    let row = &mut dump.frame.pixels[y * row_bytes..(y + 1) * row_bytes];
    for (byte, &[high, low]) in row.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        *byte = (hex_digit(high)? << 4) | hex_digit(low)?;
    }
    dump.received[y] = true;
    Some(())
}
