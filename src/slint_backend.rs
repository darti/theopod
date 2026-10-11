//! Slint platform and renderer glue for any [`Panel`].
//!
//! - [`TheoPlatform`]: minimal Slint platform backed by one software window.
//! - [`LineBuffer`]: line scratch memory reused across frames.
//! - [`render_frame`]: renders a whole frame and streams it to a [`Panel`].
//!
//! The streaming rules (line order, full-panel area, byte order) live in
//! [`FrameStreamer`], which is host-tested; this module only adapts Slint's
//! line renderer to it.

use alloc::{rc::Rc, vec::Vec};
use core::{fmt, ops::Range, time::Duration};

use embassy_time::Instant;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{LineBufferProvider, MinimalSoftwareWindow, Rgb565Pixel, SoftwareRenderer},
};
use theo_drivers::{FrameError, FrameStreamer, Panel};

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

/// Scratch memory for one rendered line, reused across frames.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pixels: Vec<Rgb565Pixel>,
}

/// Renders a full frame with `renderer` and streams it to `panel`.
///
/// The window must use `RepaintBufferType::NewBuffer` so every line is
/// delivered whole and in order. `buffer` grows to the panel width on first
/// use.
///
/// # Errors
///
/// Returns the [`FrameError`] that made the frame drop.
pub fn render_frame<P: Panel>(
    renderer: &SoftwareRenderer,
    panel: &mut P,
    buffer: &mut LineBuffer,
) -> Result<(), FrameError<P::Error>> {
    let width = usize::from(panel.size().0);
    buffer.pixels.resize(width, Rgb565Pixel(0));
    let mut streamer = FrameStreamer::new(panel, bytemuck::cast_slice_mut(&mut buffer.pixels))?;
    renderer.render_by_line(SlintLines {
        streamer: &mut streamer,
    });
    streamer.finish()
}

/// Adapts Slint's line callbacks to a [`FrameStreamer`].
struct SlintLines<'s, 'a, P: Panel> {
    streamer: &'s mut FrameStreamer<'a, P>,
}

impl<P: Panel> LineBufferProvider for SlintLines<'_, '_, P> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        self.streamer.stream_line(line, range, |pixels| {
            render_fn(bytemuck::cast_slice_mut(pixels));
        });
    }
}
