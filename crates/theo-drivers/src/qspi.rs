//! Quad-SPI command bus abstraction for display controllers.
//!
//! - [`QspiBus`]: transport trait used by QSPI panel drivers.
//! - [`Lanes`]: number of data lines used by the data phase.
//!
//! `embedded-hal` has no Quad-SPI trait, so panel drivers depend on
//! [`QspiBus`] instead of a concrete HAL.

use core::fmt::Debug;

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
/// The trait is async so DMA implementations can yield while a framebuffer
/// is streamed out.
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
}
