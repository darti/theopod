//! Panel abstraction consumed by renderers.
//!
//! - [`Panel`]: streams big-endian RGB565 pixels into rectangular areas.

use core::{fmt::Debug, ops::Range};

/// Display panel that accepts pixel streams into rectangular areas.
///
/// Implement this trait to drive another display controller from the
/// renderer: open an area with [`Panel::begin_area`], then send its pixels
/// row by row with one or more [`Panel::write_pixels`] calls.
pub trait Panel {
    /// Transport or validation failure.
    type Error: Debug;

    /// Visible size as `(width, height)` in pixels.
    fn size(&self) -> (u16, u16);

    /// Opens the `x` by `y` area for writing.
    ///
    /// Controllers may reject areas they cannot address (for example
    /// misaligned ones); the call must then leave the panel untouched.
    fn begin_area(&mut self, x: Range<u16>, y: Range<u16>) -> Result<(), Self::Error>;

    /// Appends big-endian RGB565 `pixels` to the open area.
    fn write_pixels(&mut self, pixels: &[u8]) -> Result<(), Self::Error>;
}
