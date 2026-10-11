//! Sending dirty rows of a full-panel framebuffer to a [`Panel`].
//!
//! - [`flush_rows`]: sends a band of rows of a big-endian RGB565 frame.
//! - [`FlushError`]: why a flush was refused or failed.
//!
//! Rows are sent full width, so the band is one contiguous slice of the
//! frame and goes out as a single area. The band is widened to even rows to
//! keep areas aligned for controllers such as the CO5300 (2x2 blocks); on an
//! odd-height panel the last row can only be sent as an odd band, which
//! such controllers reject.

use core::{fmt, ops::Range};

use crate::panel::Panel;

/// Reason rows could not be flushed.
#[derive(Debug, PartialEq, Eq)]
pub enum FlushError<E> {
    /// The panel rejected the area or a pixel write.
    Panel(E),
    /// The frame does not hold exactly one full panel of RGB565 pixels.
    FrameSizeMismatch {
        /// Bytes of a full panel.
        expected: usize,
        /// Bytes in the frame.
        actual: usize,
    },
    /// The rows are reversed or extend past the panel.
    RowsOutOfRange {
        /// Requested rows.
        rows: Range<u16>,
    },
}

impl<E: fmt::Debug> fmt::Display for FlushError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panel(error) => write!(f, "panel write failed: {error:?}"),
            Self::FrameSizeMismatch { expected, actual } => {
                write!(f, "frame holds {actual} bytes, panel needs {expected}")
            }
            Self::RowsOutOfRange { rows } => write!(f, "rows {rows:?} outside the panel"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for FlushError<E> {}

/// Sends `rows` of `frame`, a full panel of big-endian RGB565 pixels.
///
/// Empty `rows` send nothing. Otherwise the rows are widened to even
/// boundaries (clamped to the panel) and sent full width as one area.
///
/// # Errors
///
/// Returns [`FlushError::FrameSizeMismatch`] or
/// [`FlushError::RowsOutOfRange`] before any I/O, or [`FlushError::Panel`]
/// if the panel fails mid-way; the panel then shows a partial update.
pub async fn flush_rows<P: Panel>(
    panel: &mut P,
    frame: &[u8],
    rows: Range<u16>,
) -> Result<(), FlushError<P::Error>> {
    let (width, height) = panel.size();
    let row_bytes = usize::from(width) * 2;
    let expected = row_bytes * usize::from(height);
    if frame.len() != expected {
        return Err(FlushError::FrameSizeMismatch {
            expected,
            actual: frame.len(),
        });
    }
    if rows.start > rows.end || rows.end > height {
        return Err(FlushError::RowsOutOfRange { rows });
    }
    if rows.is_empty() {
        return Ok(());
    }

    let start = rows.start & !1;
    // Rounding up may pass the panel (odd height) or u16: clamp to the panel.
    let end = u16::try_from(usize::from(rows.end).next_multiple_of(2))
        .map_or(height, |end| end.min(height));
    panel
        .begin_area(0..width, start..end)
        .await
        .map_err(FlushError::Panel)?;
    let band = &frame[usize::from(start) * row_bytes..usize::from(end) * row_bytes];
    panel.write_pixels(band).await.map_err(FlushError::Panel)
}
