//! HAL-independent TCA9554 8-bit I2C I/O expander driver.
//!
//! - [`Tca9554`]: configures pin directions, drives output levels and reads
//!   inputs over any `embedded-hal` [`I2c`] bus, per pin or by mask.
//! - [`Pin`]: one of the eight expander pins (P0..P7, EXIO0..7 on Waveshare
//!   schematics).
//!
//! Every change is a read-modify-write of the chip's registers, so pins
//! outside the mask keep their direction and level whoever set them.

use embedded_hal::{digital::PinState, i2c::I2c};

const REGISTER_INPUT: u8 = 0x00;
const REGISTER_OUTPUT: u8 = 0x01;
const REGISTER_CONFIG: u8 = 0x03;

/// One of the eight TCA9554 pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pin {
    /// P0 (EXIO0).
    P0,
    /// P1 (EXIO1).
    P1,
    /// P2 (EXIO2).
    P2,
    /// P3 (EXIO3).
    P3,
    /// P4 (EXIO4).
    P4,
    /// P5 (EXIO5).
    P5,
    /// P6 (EXIO6).
    P6,
    /// P7 (EXIO7).
    P7,
}

impl Pin {
    /// Register bit of this pin.
    #[must_use]
    pub const fn mask(self) -> u8 {
        1 << self as u8
    }
}

/// Output bits for `mask`: all set when `level` is high, all clear when low.
const fn level_bits(mask: u8, level: PinState) -> u8 {
    match level {
        PinState::High => mask,
        PinState::Low => 0,
    }
}

/// TCA9554 expander at a fixed 7-bit I2C address.
///
/// Pass `&mut bus` to borrow a shared bus, or the bus itself to own it.
#[derive(Debug)]
pub struct Tca9554<I> {
    i2c: I,
    address: u8,
}

impl<I: I2c> Tca9554<I> {
    /// Drives the expander at `address` (0x20..=0x27) over `i2c`.
    pub fn new(i2c: I, address: u8) -> Self {
        debug_assert!((0x20..=0x27).contains(&address), "not a TCA9554 address");
        Self { i2c, address }
    }

    /// Releases the underlying bus.
    pub fn release(self) -> I {
        self.i2c
    }

    /// Makes the pins in `mask` outputs at `levels`; other pins are kept.
    ///
    /// Levels are latched before the direction changes, so a pin never
    /// drives a stale level from the output register.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access.
    pub fn configure_outputs(&mut self, mask: u8, levels: u8) -> Result<(), I::Error> {
        self.write_levels(mask, levels)?;
        // A cleared config bit selects output mode.
        self.update(REGISTER_CONFIG, mask, 0)
    }

    /// Makes the pins in `mask` inputs; other pins are kept.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access.
    pub fn configure_inputs(&mut self, mask: u8) -> Result<(), I::Error> {
        self.update(REGISTER_CONFIG, mask, mask)
    }

    /// Sets the output levels of the pins in `mask` to the bits of `levels`.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access.
    pub fn write_levels(&mut self, mask: u8, levels: u8) -> Result<(), I::Error> {
        self.update(REGISTER_OUTPUT, mask, levels)
    }

    /// Reads the level of every pin, input or output.
    ///
    /// # Errors
    ///
    /// Returns the I2C error if the read fails.
    pub fn read_inputs(&mut self) -> Result<u8, I::Error> {
        self.read(REGISTER_INPUT)
    }

    /// Makes `pin` an output driving `level`.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access.
    pub fn configure_output(&mut self, pin: Pin, level: PinState) -> Result<(), I::Error> {
        self.configure_outputs(pin.mask(), level_bits(pin.mask(), level))
    }

    /// Makes `pin` an input.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access.
    pub fn configure_input(&mut self, pin: Pin) -> Result<(), I::Error> {
        self.configure_inputs(pin.mask())
    }

    /// Drives output `pin` to `level`.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access.
    pub fn set_level(&mut self, pin: Pin, level: PinState) -> Result<(), I::Error> {
        self.write_levels(pin.mask(), level_bits(pin.mask(), level))
    }

    /// Reads the level of `pin`.
    ///
    /// # Errors
    ///
    /// Returns the I2C error if the read fails.
    pub fn level(&mut self, pin: Pin) -> Result<PinState, I::Error> {
        let inputs = self.read_inputs()?;
        Ok(PinState::from(inputs & pin.mask() != 0))
    }

    fn read(&mut self, register: u8) -> Result<u8, I::Error> {
        let mut value = [0];
        self.i2c.write_read(self.address, &[register], &mut value)?;
        Ok(value[0])
    }

    /// Replaces the `mask` bits of `register` with those of `bits`.
    fn update(&mut self, register: u8, mask: u8, bits: u8) -> Result<(), I::Error> {
        let current = self.read(register)?;
        let value = (current & !mask) | (bits & mask);
        self.i2c.write(self.address, &[register, value])
    }
}
