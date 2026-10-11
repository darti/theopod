//! Slint platform and framebuffer rendering.
//!
//! - [`TheoPlatform`]: minimal Slint platform backed by one software window.
//! - [`FrameBuffer`]: full-panel framebuffer Slint renders into, in the
//!   panel's big-endian RGB565 byte order.
//!
//! Sending the dirty rows to a panel is [`theo_drivers::flush_rows`]' job,
//! which is host-tested; this module only adapts Slint's renderer.

use alloc::rc::Rc;
use core::{fmt, ops::Range, time::Duration};

use embassy_time::Instant;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, Rgb565BigEndianPixel, SoftwareRenderer},
};

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

/// Full-panel framebuffer kept between frames.
///
/// The window must use `RepaintBufferType::ReusedBuffer`: Slint then only
/// redraws what changed since the previous frame.
pub struct FrameBuffer {
    pixels: &'static mut [Rgb565BigEndianPixel],
    width: u16,
    height: u16,
}

impl FrameBuffer {
    /// Wraps `pixels`, a row-major `width` x `height` RGB565 buffer.
    ///
    /// The slots are reinterpreted as [`Rgb565BigEndianPixel`]: they hold
    /// the panel's byte order, not native-endian values.
    ///
    /// # Panics
    ///
    /// Panics if `pixels` does not hold exactly `width * height` pixels.
    pub fn new(pixels: &'static mut [u16], (width, height): (u16, u16)) -> Self {
        assert_eq!(
            pixels.len(),
            usize::from(width) * usize::from(height),
            "framebuffer size does not match the panel"
        );
        Self {
            pixels: bytemuck::cast_slice_mut(pixels),
            width,
            height,
        }
    }

    /// Redraws what changed with `renderer` and returns the dirty rows.
    pub fn render(&mut self, renderer: &SoftwareRenderer) -> Range<u16> {
        let region = renderer.render(self.pixels, usize::from(self.width));
        let top = i64::from(region.bounding_box_origin().y);
        let bottom = top + i64::from(region.bounding_box_size().height);
        self.row(top)..self.row(bottom)
    }

    /// Every row of the frame.
    pub fn all_rows(&self) -> Range<u16> {
        0..self.height
    }

    /// The frame as big-endian RGB565 bytes, ready for the panel.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(self.pixels)
    }

    /// Clamps a rendered row coordinate to the frame.
    fn row(&self, y: i64) -> u16 {
        u16::try_from(y.clamp(0, i64::from(self.height))).unwrap_or(self.height)
    }
}

impl fmt::Debug for FrameBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameBuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}
