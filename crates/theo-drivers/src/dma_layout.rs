//! Layout arithmetic of DMA transfers read in place from a framebuffer.
//!
//! - [`descriptor_chunks`], [`DmaChunk`]: splits a transfer into descriptors.
//! - [`frame_payload`], [`FramePayloadError`]: tells whether a payload lies in
//!   the framebuffer and can be read there in place.
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

/// Reason a framebuffer payload cannot be read in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePayloadError {
    /// The payload is longer than one transfer.
    TooLong {
        /// Payload length in bytes.
        len: usize,
    },
    /// The payload does not start on an alignment boundary.
    Misaligned {
        /// Payload start, in bytes from the frame start.
        offset: usize,
    },
}

/// Tells whether `payload` lies in `frame` and can be read there in place.
///
/// Returns `Ok(false)` for an empty payload or one not wholly inside
/// `frame`: it is not a framebuffer payload. Addresses are byte addresses;
/// `frame` should start on an `align` boundary, so offsets and addresses
/// agree on alignment.
///
/// # Errors
///
/// Returns [`FramePayloadError::TooLong`] for a framebuffer payload longer than
/// `max_len`, else [`FramePayloadError::Misaligned`] if it does not start on a
/// multiple of `align`.
pub fn frame_payload(
    frame: Range<usize>,
    payload: Range<usize>,
    max_len: usize,
    align: usize,
) -> Result<bool, FramePayloadError> {
    if payload.is_empty() || payload.start < frame.start || payload.end > frame.end {
        return Ok(false);
    }
    if payload.len() > max_len {
        return Err(FramePayloadError::TooLong { len: payload.len() });
    }
    if !payload.start.is_multiple_of(align) {
        return Err(FramePayloadError::Misaligned {
            offset: payload.start - frame.start,
        });
    }
    Ok(true)
}
