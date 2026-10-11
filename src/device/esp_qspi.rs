//! [`QspiBus`] implementation for an esp-hal SPI master.
//!
//! - [`EspQspi`]: half-duplex SPI master with a software chip select.
//! - [`EspQspiError`]: SPI or chip-select failure.

use core::fmt;

use embedded_hal::digital::OutputPin;
use esp_hal::{
    Blocking,
    spi::{
        Error as SpiError,
        master::{Address, Command, DataMode, Spi},
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
}

impl<P: fmt::Debug> fmt::Display for EspQspiError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spi(error) => write!(f, "spi transfer failed: {error:?}"),
            Self::ChipSelect(error) => write!(f, "chip select failed: {error:?}"),
        }
    }
}

impl<P: fmt::Debug> core::error::Error for EspQspiError<P> {}

/// esp-hal SPI master in half-duplex mode with a software chip select.
#[derive(Debug)]
pub struct EspQspi<'d, CS> {
    spi: Spi<'d, Blocking>,
    cs: CS,
}

impl<'d, CS: OutputPin> EspQspi<'d, CS> {
    /// Wraps an SPI master whose SIO0..3 lines are already routed.
    pub fn new(spi: Spi<'d, Blocking>, cs: CS) -> Self {
        Self { spi, cs }
    }
}

impl<CS: OutputPin> QspiBus for EspQspi<'_, CS> {
    type Error = EspQspiError<CS::Error>;

    fn write(
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
        self.cs.set_low().map_err(EspQspiError::ChipSelect)?;
        let transferred = self.spi.half_duplex_write(
            data_mode,
            Command::_8Bit(u16::from(opcode), DataMode::Single),
            Address::_24Bit(address, DataMode::Single),
            0,
            data,
        );
        // Always release chip select, but report the SPI failure first.
        let released = self.cs.set_high();
        transferred.map_err(EspQspiError::Spi)?;
        released.map_err(EspQspiError::ChipSelect)
    }
}
