//! Panel abstraction consumed by renderers.
//!
//! - [`Panel`]: holds a framebuffer and streams ranges of it into
//!   rectangular areas.
//!
//! Renderers usually go through [`crate::flush_rows`] rather than calling
//! the trait directly.

use core::{fmt::Debug, ops::Range};

/// Display panel with its framebuffer, sent into rectangular areas.
///
/// Implement this trait to drive another display controller from the
/// renderer: draw into [`Panel::frame_mut`], open an area with
/// [`Panel::begin_area`], then send its pixels row by row with one or more
/// [`Panel::write_frame`] calls.
///
/// [`crate::flush_rows`] only opens full-width areas starting on an even
/// row with an even number of rows (except a last odd row on an odd-height
/// panel): implementations must accept those to be usable with it.
///
/// Cancel safety: dropping either future mid-way must leave the panel
/// usable once [`Panel::begin_area`] opens a new area; the interrupted
/// area then holds unknown pixels.
#[expect(
    async_fn_in_trait,
    reason = "single-threaded executor; implementations need no `Send` bound"
)]
pub trait Panel {
    /// Transport or validation failure.
    type Error: Debug;

    /// Visible size as `(width, height)` in pixels.
    fn size(&self) -> (u16, u16);

    /// Opens the `x` by `y` area for writing.
    ///
    /// Controllers may reject areas they cannot address (for example
    /// misaligned ones); the call must then leave the panel untouched.
    async fn begin_area(&mut self, x: Range<u16>, y: Range<u16>) -> Result<(), Self::Error>;

    /// The framebuffer: [`Panel::size`] of big-endian RGB565, row-major.
    fn frame(&self) -> &[u8];

    /// The framebuffer, for the renderer to draw into.
    fn frame_mut(&mut self) -> &mut [u8];

    /// Appends the framebuffer `bytes` to the open area.
    ///
    /// `bytes` may be longer than one bus transfer; the panel splits it.
    async fn write_frame(&mut self, bytes: Range<usize>) -> Result<(), Self::Error>;
}
