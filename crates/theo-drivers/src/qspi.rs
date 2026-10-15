//! Quad-SPI command bus abstraction for display controllers.
//!
//! - [`QspiBus`]: transport trait used by QSPI panel drivers; it holds the
//!   panel's framebuffer, which it sends from in place.
//! - [`Lanes`]: number of data lines used by the data phase.
//!
//! `embedded-hal` has no Quad-SPI trait, so panel drivers depend on
//! [`QspiBus`] instead of a concrete HAL.

use core::{fmt::Debug, ops::Range};

/// Number of data lines used by the data phase of a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lanes {
    /// Data on SIO0 only.
    Single,
    /// Data on SIO0..3.
    Quad,
}

/// Transport for QSPI panels using an 8-bit opcode and 24-bit address frame.
///
/// Implement this trait to run a QSPI panel driver on another HAL. The
/// opcode and address phases are always sent on a single line; only the data
/// phase uses `lanes`. Chip select must stay asserted for the whole transfer.
///
/// The bus owns the panel's framebuffer: a DMA implementation reads it in
/// place, so it must know it is the frame (ADR-0004). The renderer draws
/// into [`QspiBus::frame_mut`] between transfers, and drivers send byte
/// ranges of it with [`QspiBus::write_frame`].
///
/// The trait is async so DMA implementations can yield while a framebuffer
/// is streamed out. Implementations should be cancel-safe: dropping a
/// [`QspiBus::write`] future mid-transfer must release chip select (see
/// [`crate::ChipSelect`]) and leave the bus usable, the device then holding
/// a partial transfer.
#[expect(
    async_fn_in_trait,
    reason = "single-threaded executor; implementations need no `Send` bound"
)]
pub trait QspiBus {
    /// Transport failure.
    type Error: Debug;

    /// Longest `data` accepted by one [`QspiBus::write`]; at least 2 bytes.
    ///
    /// Drivers split longer payloads into several transfers.
    const MAX_TRANSFER_LEN: usize;

    /// Sends `opcode`, the 24-bit `address`, then `data` on `lanes`.
    async fn write(
        &mut self,
        opcode: u8,
        address: u32,
        lanes: Lanes,
        data: &[u8],
    ) -> Result<(), Self::Error>;

    /// The framebuffer the bus sends from, as raw bytes.
    ///
    /// Its size and pixel format are the panel driver's contract.
    fn frame(&self) -> &[u8];

    /// The framebuffer, to draw into between transfers.
    fn frame_mut(&mut self) -> &mut [u8];

    /// Sends `opcode`, the 24-bit `address`, then the framebuffer `bytes`
    /// on `lanes`.
    ///
    /// `bytes` is at most [`QspiBus::MAX_TRANSFER_LEN`] long; the bus
    /// rejects ranges outside the frame, and may reject others it cannot
    /// send in place (for example misaligned ones).
    async fn write_frame(
        &mut self,
        opcode: u8,
        address: u32,
        lanes: Lanes,
        bytes: Range<usize>,
    ) -> Result<(), Self::Error>;
}
