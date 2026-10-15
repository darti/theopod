//! Slint platform and framebuffer rendering.
//!
//! - [`TheoPlatform`]: minimal Slint platform backed by one software window.
//! - [`FrameRenderer`]: renders into the display's framebuffer, in the
//!   panel's big-endian RGB565 byte order, and reports the dirty rows.
//!
//! Sending the dirty rows to a panel is [`theo_drivers::flush_bands`]' job,
//! which is host-tested; this module only adapts Slint's renderer.

use alloc::rc::Rc;
use core::{fmt, time::Duration};

use embassy_time::Instant;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, Rgb565BigEndianPixel, SoftwareRenderer},
};
use theo_drivers::DirtyRows;

/// Slint platform exposing a single software-rendered window.
pub struct TheoPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl TheoPlatform {
    /// Creates a platform that hands out `window` to every component.
    pub fn new(window: Rc<MinimalSoftwareWindow>) -> Self {
        Self { window }
    }
}

impl fmt::Debug for TheoPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TheoPlatform").finish_non_exhaustive()
    }
}

impl Platform for TheoPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_micros(Instant::now().as_micros())
    }
}

/// Most row bands flushed per frame.
///
/// Slint's dirty region holds at most three rectangles, so their rows form
/// at most three bands.
pub const DIRTY_BANDS: usize = 3;

/// Dirty rows of the framebuffer, as up to [`DIRTY_BANDS`] bands.
pub type FrameRows = DirtyRows<DIRTY_BANDS>;

/// Renders Slint into the display's full-panel framebuffer.
///
/// The display owns the framebuffer (`Panel::frame_mut`), so it persists
/// between frames. The window must use `RepaintBufferType::ReusedBuffer`:
/// Slint then only redraws what changed since the previous frame.
#[derive(Debug)]
pub struct FrameRenderer {
    width: u16,
    height: u16,
}

impl FrameRenderer {
    /// Renders for a `width` x `height` panel.
    pub fn new((width, height): (u16, u16)) -> Self {
        Self { width, height }
    }

    /// Redraws what changed into `frame` and returns the dirty rows.
    ///
    /// `frame` holds the panel's big-endian RGB565 bytes, reinterpreted as
    /// [`Rgb565BigEndianPixel`]s.
    ///
    /// # Panics
    ///
    /// Panics if `frame` is not 2-byte aligned or does not hold exactly
    /// `width * height` pixels.
    pub fn render(&self, renderer: &SoftwareRenderer, frame: &mut [u8]) -> FrameRows {
        let pixels: &mut [Rgb565BigEndianPixel] = bytemuck::cast_slice_mut(frame);
        assert_eq!(
            pixels.len(),
            usize::from(self.width) * usize::from(self.height),
            "framebuffer size does not match the panel"
        );
        let region = renderer.render(pixels, usize::from(self.width));
        let mut rows = FrameRows::new(self.height);
        for (origin, size) in region.iter() {
            let top = i64::from(origin.y);
            let bottom = top + i64::from(size.height);
            rows.add(self.row(top)..self.row(bottom));
        }
        rows
    }

    /// Every row of the frame.
    pub fn all_rows(&self) -> FrameRows {
        FrameRows::all(self.height)
    }

    /// Clamps a rendered row coordinate to the frame.
    fn row(&self, y: i64) -> u16 {
        u16::try_from(y.clamp(0, i64::from(self.height))).unwrap_or(self.height)
    }
}
