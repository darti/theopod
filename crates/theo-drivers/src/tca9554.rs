//! HAL-independent TCA9554 8-bit I2C I/O expander driver.
//!
//! - [`Tca9554`]: configures pin directions and drives output levels over any
//!   `embedded-hal` [`I2c`] bus.

use embedded_hal::i2c::I2c;

const REGISTER_OUTPUT: u8 = 0x01;
const REGISTER_CONFIG: u8 = 0x03;

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

    /// Makes the pins set in `mask` outputs and the others inputs.
    ///
    /// # Errors
    ///
    /// Returns the I2C error if the write fails.
    pub fn set_outputs(&mut self, mask: u8) -> Result<(), I::Error> {
        // A cleared config bit selects output mode.
        self.i2c.write(self.address, &[REGISTER_CONFIG, !mask])
    }

    /// Drives the output pins to the bit levels in `levels`.
    ///
    /// # Errors
    ///
    /// Returns the I2C error if the write fails.
    pub fn write_levels(&mut self, levels: u8) -> Result<(), I::Error> {
        self.i2c.write(self.address, &[REGISTER_OUTPUT, levels])
    }
}
