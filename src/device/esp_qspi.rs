//! [`QspiBus`] implementation for an esp-hal SPI master with DMA.
//!
//! - [`EspQspi`]: async half-duplex SPI master with a software chip select.
//! - [`EspQspiError`]: SPI, chip-select or payload-length failure.
//! - [`MAX_TRANSFER_LEN`]: longest payload, and size of the bounce buffer.
//!
//! Every payload is first copied into a bounce buffer in internal RAM, and
//! the DMA only ever reads that buffer. Sending a PSRAM framebuffer directly
//! showed stale data on the panel (a band at the top of the first frame)
//! even though esp-hal writes the cache back before each transfer; the copy
//! sidesteps that, and also covers payloads that live in flash. See
//! `docs/adr/0004`.

use core::fmt;

use embedded_hal::digital::OutputPin;
use esp_hal::{
    Async,
    spi::{
        Error as SpiError,
        master::{Address, Command, DataMode, SpiDma},
    },
};
use theo_drivers::{Lanes, QspiBus};

/// Failure of an [`EspQspi`] transfer.
#[derive(Debug)]
pub enum EspQspiError<P> {
    /// The SPI peripheral failed.
    Spi(SpiError),
    /// The chip-select pin failed.
    ChipSelect(P),
    /// The payload is longer than the bounce buffer; nothing was sent.
    TooLong {
        /// Payload length in bytes.
        len: usize,
        /// Bounce buffer length in bytes.
        capacity: usize,
    },
}

impl<P: fmt::Debug> fmt::Display for EspQspiError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spi(error) => write!(f, "spi transfer failed: {error:?}"),
            Self::ChipSelect(error) => write!(f, "chip select failed: {error:?}"),
            Self::TooLong { len, capacity } => {
                write!(
                    f,
                    "payload of {len} bytes exceeds the {capacity}-byte bounce buffer"
                )
            }
        }
    }
}

impl<P: fmt::Debug> core::error::Error for EspQspiError<P> {}

/// Longest payload of one transfer: esp-hal's `MAX_DMA_SIZE` for the SPI
/// master (`src/spi/master/dma.rs`, esp-hal 1.2.2), since the command and
/// address phases cannot span two DMA transfers. The constant is private
/// in esp-hal: re-check it on every esp-hal upgrade (ADR-0003).
pub const MAX_TRANSFER_LEN: usize = 32_736;

/// Async esp-hal DMA SPI master in half-duplex mode with a software chip
/// select.
#[derive(Debug)]
pub struct EspQspi<'d, CS> {
    spi: SpiDma<'d, Async>,
    cs: CS,
    /// Internal-RAM copy of the payload being sent.
    bounce: &'static mut [u8],
}

impl<'d, CS: OutputPin> EspQspi<'d, CS> {
    /// Wraps a DMA SPI master whose SIO0..3 lines are already routed.
    ///
    /// `bounce` must be in internal RAM (DMA-readable); longer payloads
    /// than it holds are refused.
    ///
    /// # Panics
    ///
    /// Panics if `bounce` holds fewer than [`MAX_TRANSFER_LEN`] bytes: every
    /// full-size transfer would fail.
    pub fn new(spi: SpiDma<'d, Async>, cs: CS, bounce: &'static mut [u8]) -> Self {
        assert!(
            bounce.len() >= MAX_TRANSFER_LEN,
            "bounce buffer smaller than MAX_TRANSFER_LEN"
        );
        Self { spi, cs, bounce }
    }
}

impl<CS: OutputPin> QspiBus for EspQspi<'_, CS> {
    type Error = EspQspiError<CS::Error>;

    const MAX_TRANSFER_LEN: usize = MAX_TRANSFER_LEN;

    /// Not cancel-safe: dropping the future mid-transfer leaves chip
    /// select asserted.
    async fn write(
        &mut self,
        opcode: u8,
        address: u32,
        lanes: Lanes,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        let data_mode = match lanes {
            Lanes::Single => DataMode::Single,
            Lanes::Quad => DataMode::Quad,
        };
        let capacity = self.bounce.len();
        let bounce = self
            .bounce
            .get_mut(..data.len())
            .ok_or(EspQspiError::TooLong {
                len: data.len(),
                capacity,
            })?;
        bounce.copy_from_slice(data);
        self.cs.set_low().map_err(EspQspiError::ChipSelect)?;
        let transferred = self
            .spi
            .half_duplex_write_async(
                data_mode,
                Command::_8Bit(u16::from(opcode), DataMode::Single),
                Address::_24Bit(address, DataMode::Single),
                0,
                bounce,
            )
            .await;
        // Always release chip select, but report the SPI failure first.
        let released = self.cs.set_high();
        transferred.map_err(EspQspiError::Spi)?;
        released.map_err(EspQspiError::ChipSelect)
    }
}
