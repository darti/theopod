//! Layout arithmetic of DMA transfers read in place from a framebuffer.
//!
//! - [`descriptor_chunks`], [`DmaChunk`]: splits a transfer into descriptors.
//! - [`frame_payload`], [`FramePayloadError`]: checks a framebuffer range
//!   can be read in place in one transfer.
//!
//! The firmware's PSRAM DMA path (`src/device/psram_tx_buf.rs`,
//! `esp_qspi.rs`) builds on these, so its arithmetic is tested on the host
//! (ADR-0004). Addresses are plain `usize` values: nothing here touches
//! memory.

use core::ops::Range;

/// One descriptor of a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaChunk {
    /// Start, in bytes from the start of the transfer.
    pub offset: usize,
    /// Length in bytes; never zero.
    pub len: usize,
    /// Whether this is the transfer's last descriptor.
    pub last: bool,
}

/// Splits a transfer of `len` bytes into descriptors of `chunk` bytes.
///
/// All descriptors but the last hold exactly `chunk` bytes, so each starts
/// on a multiple of `chunk`. An empty transfer has no descriptors.
///
/// # Panics
///
/// Panics if `chunk` is zero.
pub fn descriptor_chunks(len: usize, chunk: usize) -> impl Iterator<Item = DmaChunk> {
    assert!(chunk > 0, "descriptor chunk size must not be zero");
    let count = len.div_ceil(chunk);
    (0..count).map(move |index| {
        let offset = index * chunk;
        DmaChunk {
            offset,
            len: (len - offset).min(chunk),
            last: index + 1 == count,
        }
    })
}

/// Reason a framebuffer range cannot be read in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FramePayloadError {
    /// The range is empty, reversed or extends past the frame.
    OutOfFrame {
        /// Requested bytes.
        bytes: Range<usize>,
    },
    /// The range is longer than one transfer.
    TooLong {
        /// Range length in bytes.
        len: usize,
    },
    /// The range does not start on an alignment boundary.
    Misaligned {
        /// Range start, in bytes from the frame start.
        offset: usize,
    },
}

/// Checks that `bytes` of a `frame_len`-byte frame can be read in place.
///
/// The frame must start on an `align` boundary, so offsets and addresses
/// agree on alignment.
///
/// # Errors
///
/// Returns [`FramePayloadError::OutOfFrame`] for an empty range or one not
/// wholly inside the frame, else [`FramePayloadError::TooLong`] for one
/// longer than `max_len`, else [`FramePayloadError::Misaligned`] if it does
/// not start on a multiple of `align`.
pub fn frame_payload(
    frame_len: usize,
    bytes: Range<usize>,
    max_len: usize,
    align: usize,
) -> Result<(), FramePayloadError> {
    if bytes.is_empty() || bytes.end > frame_len {
        return Err(FramePayloadError::OutOfFrame { bytes });
    }
    if bytes.len() > max_len {
        return Err(FramePayloadError::TooLong { len: bytes.len() });
    }
    if !bytes.start.is_multiple_of(align) {
        return Err(FramePayloadError::Misaligned {
            offset: bytes.start,
        });
    }
    Ok(())
}
