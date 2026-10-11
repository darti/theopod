//! Full-frame streaming from a line renderer to a [`Panel`].
//!
//! - [`FrameStreamer`]: validates line order, opens one full-panel area and
//!   streams each rendered line into it as big-endian RGB565.
//! - [`FrameError`]: why a frame was dropped.
//!
//! Opening the whole panel once per frame keeps areas aligned for
//! controllers such as the CO5300 (2x2 blocks). The renderer must therefore
//! deliver every line, full width, from top to bottom.

use core::{fmt, ops::Range};

use crate::panel::Panel;

/// Reason a frame was dropped.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError<E> {
    /// The panel rejected the area or a pixel write.
    Panel(E),
    /// The line buffer is narrower than the panel.
    LineBufferTooSmall {
        /// Panel width in pixels.
        required: usize,
        /// Line buffer length in pixels.
        available: usize,
    },
    /// A line arrived out of order or not full-width.
    UnexpectedLine {
        /// Line index delivered by the renderer.
        line: usize,
        /// Column range delivered by the renderer.
        range: Range<usize>,
    },
    /// The frame ended before the last line.
    Incomplete {
        /// Lines streamed before the frame ended.
        lines: usize,
    },
}

impl<E: fmt::Debug> fmt::Display for FrameError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panel(error) => write!(f, "panel write failed: {error:?}"),
            Self::LineBufferTooSmall {
                required,
                available,
            } => write!(
                f,
                "line buffer holds {available} pixels, panel needs {required}"
            ),
            Self::UnexpectedLine { line, range } => {
                write!(f, "unexpected line {line} with columns {range:?}")
            }
            Self::Incomplete { lines } => write!(f, "frame ended after {lines} lines"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for FrameError<E> {}

/// Streams one frame, line by line, into a full-panel area.
///
/// After the first error every later line is skipped without rendering;
/// [`FrameStreamer::finish`] reports that error.
#[derive(Debug)]
pub struct FrameStreamer<'a, P: Panel> {
    panel: &'a mut P,
    line: &'a mut [u16],
    width: u16,
    height: u16,
    next_line: usize,
    error: Option<FrameError<P::Error>>,
}

impl<'a, P: Panel> FrameStreamer<'a, P> {
    /// Starts a frame on `panel`, rendering each line into `line_buffer`.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::LineBufferTooSmall`] if `line_buffer` is
    /// narrower than the panel.
    pub fn new(panel: &'a mut P, line_buffer: &'a mut [u16]) -> Result<Self, FrameError<P::Error>> {
        let (width, height) = panel.size();
        let required = usize::from(width);
        if line_buffer.len() < required {
            return Err(FrameError::LineBufferTooSmall {
                required,
                available: line_buffer.len(),
            });
        }
        Ok(Self {
            panel,
            line: &mut line_buffer[..required],
            width,
            height,
            next_line: 0,
            error: None,
        })
    }

    /// Renders line `line` over columns `range` with `render`, then sends it.
    ///
    /// `render` receives native-endian RGB565 pixels.
    pub fn stream_line(
        &mut self,
        line: usize,
        range: Range<usize>,
        render: impl FnOnce(&mut [u16]),
    ) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.try_stream_line(line, range, render) {
            self.error = Some(error);
        }
    }

    /// Ends the frame.
    ///
    /// # Errors
    ///
    /// Returns the first error met while streaming, or
    /// [`FrameError::Incomplete`] if lines are missing.
    pub fn finish(self) -> Result<(), FrameError<P::Error>> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.next_line != usize::from(self.height) {
            return Err(FrameError::Incomplete {
                lines: self.next_line,
            });
        }
        Ok(())
    }

    fn try_stream_line(
        &mut self,
        line: usize,
        range: Range<usize>,
        render: impl FnOnce(&mut [u16]),
    ) -> Result<(), FrameError<P::Error>> {
        if line != self.next_line || range != (0..usize::from(self.width)) {
            return Err(FrameError::UnexpectedLine { line, range });
        }
        if line == 0 {
            self.panel
                .begin_area(0..self.width, 0..self.height)
                .map_err(FrameError::Panel)?;
        }

        render(self.line);
        // The panel expects big-endian RGB565; swap in place, then view as bytes.
        for pixel in self.line.iter_mut() {
            *pixel = pixel.to_be();
        }
        self.panel
            .write_pixels(bytemuck::cast_slice(self.line))
            .map_err(FrameError::Panel)?;
        self.next_line += 1;
        Ok(())
    }
}
