//! Chip select held for one bus transfer, released even if it is cancelled.
//!
//! - [`ChipSelect`]: drives an active-low chip select for as long as it
//!   lives.
//!
//! A [`crate::QspiBus`] implementation holds one across the transfer: if
//! the transfer future is dropped mid-way, chip select is still released,
//! so the next transfer does not run into a half-finished one.

use embedded_hal::digital::OutputPin;

/// Active-low chip select, asserted from [`ChipSelect::select`] until
/// [`ChipSelect::release`] or drop.
#[derive(Debug)]
pub struct ChipSelect<'a, P: OutputPin> {
    /// `None` once released, so drop does not drive the pin again.
    pin: Option<&'a mut P>,
}

impl<'a, P: OutputPin> ChipSelect<'a, P> {
    /// Drives `pin` low.
    ///
    /// # Errors
    ///
    /// Returns the pin error; the pin is then not driven again on drop.
    pub fn select(pin: &'a mut P) -> Result<Self, P::Error> {
        pin.set_low()?;
        Ok(Self { pin: Some(pin) })
    }

    /// Drives the pin high, reporting a failure that drop would ignore.
    ///
    /// # Errors
    ///
    /// Returns the pin error; the pin is not driven again.
    pub fn release(mut self) -> Result<(), P::Error> {
        match self.pin.take() {
            Some(pin) => pin.set_high(),
            None => Ok(()),
        }
    }
}

impl<P: OutputPin> Drop for ChipSelect<'_, P> {
    fn drop(&mut self) {
        if let Some(pin) = self.pin.take() {
            // Nothing can report the error of a cancelled transfer.
            let _ = pin.set_high();
        }
    }
}
