//! Text format for dumping a framebuffer over a serial log.
//!
//! - [`write_frame`], [`WriteError`]: writes a big-endian RGB565 frame as
//!   text lines (firmware side, `no_std`).
//! - [`parse_frame`], [`ParseError`]: finds the last complete dump in a log
//!   (host side).
//! - [`Frame`]: a parsed frame, convertible to RGB8 with [`Frame::to_rgb8`].
//!
//! Format, one line each, so dumps survive other log lines around them:
//!
//! ```text
//! theo-frame-begin <width> <height>
//! theo-frame-row <y> <row bytes as lowercase hex>
//! theo-frame-end
//! ```

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod parse;
pub mod write;

#[cfg(test)]
mod parse_tests;
#[cfg(test)]
mod write_tests;

pub use parse::{Frame, ParseError, parse_frame};
pub use write::{WriteError, write_frame};

/// Starts a dump: `theo-frame-begin <width> <height>`.
const BEGIN: &str = "theo-frame-begin";
/// One row: `theo-frame-row <y> <hex>`.
const ROW: &str = "theo-frame-row";
/// Ends a dump.
const END: &str = "theo-frame-end";
