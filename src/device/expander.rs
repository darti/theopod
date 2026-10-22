//! The shared TCA9554 expander and the touch controller it resets.
//!
//! - [`Expander`]: the TCA9554, shared by its runtime users (ADR-0005).
//! - [`Touch`]: the CST820 touch controller, with the expander that resets
//!   it; [`TouchError`], [`TouchResetError`]: its failures.
//!
//! The expander pins themselves (`TOUCH_RESET`, `PANEL_POWER`) stay with
//! the rest of the board wiring in the parent module.

use core::{convert::Infallible, fmt};

use embassy_embedded_hal::shared_bus::I2cDeviceError;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::Delay;
use esp_hal::{gpio::Input, i2c::master::Error as I2cError};
use theo_drivers::{Cst820, Cst820Error, Cst820InitError, Tca9554, TouchEvent};

use super::{I2cDevice, PANEL_POWER, TOUCH_RESET};

/// CST820 touch controller, reporting points in panel pixels.
///
/// Its interrupt is GPIO21 (Waveshare BSP `BSP_LCD_TOUCH_INT`, active low).
/// It reaches the shared [`Expander`] to reset the controller at run time,
/// and to cut the panel rails before deep sleep ([`Touch::park`]).
pub struct Touch {
    controller: Cst820<I2cDevice, Input<'static>, Delay>,
    expander: &'static Expander,
}

/// The TCA9554 expander, shared by its runtime users (ADR-0005).
///
/// EXIO0/1 hold the panel reset and power, EXIO2 the touch reset; today
/// only [`Touch`] uses it at run time (resets, and parking before deep
/// sleep). One `Tca9554` behind a mutex: a user locks it for a whole
/// change, so read-modify-writes cannot interleave, and the driver's
/// register copy stays the single view of the chip. Hand out
/// `&'static Expander`, never a second `Tca9554`.
pub type Expander = Mutex<CriticalSectionRawMutex, Tca9554<I2cDevice>>;

/// Failure of [`Touch::changed`].
pub type TouchError = Cst820Error<I2cDeviceError<I2cError>, Infallible>;

/// Failure of [`Touch::reset`].
pub type TouchResetError = Cst820InitError<I2cDeviceError<I2cError>, I2cDeviceError<I2cError>>;

/// Holds the touch controller in reset, then cuts the panel rails.
///
/// The order is safety-critical: the unpowered controller must not be left
/// connected to the shared I2C bus. Both operations are attempted.
pub async fn park_touch_and_panel(
    expander: &mut Tca9554<I2cDevice>,
) -> Result<(), I2cDeviceError<I2cError>> {
    let held = expander
        .configure_output(TOUCH_RESET, embedded_hal::digital::PinState::Low)
        .await;
    let cut = PANEL_POWER.power_down(expander).await;
    held.and(cut)
}

impl Touch {
    /// Pairs the controller with the shared expander that resets it.
    pub(super) fn new(
        controller: Cst820<I2cDevice, Input<'static>, Delay>,
        expander: &'static Expander,
    ) -> Self {
        Self {
            controller,
            expander,
        }
    }

    /// Waits for the next press, move, release or cancellation.
    ///
    /// # Errors
    ///
    /// Returns the controller's error; see [`Cst820::changed`].
    pub async fn changed(&mut self) -> Result<TouchEvent, TouchError> {
        self.controller.changed().await
    }

    /// Pulses the controller's reset and enables its interrupts again.
    ///
    /// A touch in progress is then reported as cancelled.
    ///
    /// # Errors
    ///
    /// Returns the expander or controller error; see [`Cst820::init`].
    pub async fn reset(&mut self) -> Result<(), TouchResetError> {
        // Held for the whole reset (about 210 ms): no other expander change
        // can slip between the pulse's writes.
        let mut expander = self.expander.lock().await;
        self.controller.init(&mut expander, TOUCH_RESET).await
    }

    /// Holds the controller in reset and cuts the panel rails, for deep
    /// sleep.
    ///
    /// Call it after the panel's sleep-in. Both steps are tried even if the
    /// first fails; the expander keeps its pins until the next boot powers
    /// the panel and resets the controller again.
    ///
    /// # Errors
    ///
    /// Returns the first expander error.
    pub async fn park(&mut self) -> Result<(), I2cDeviceError<I2cError>> {
        // Held across both steps, like a reset: no expander change slips in.
        let mut expander = self.expander.lock().await;
        park_touch_and_panel(&mut expander).await
    }
}

impl fmt::Debug for Touch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Cst820`'s and `Tca9554`'s derived `Debug` need `I2cDevice:
        // Debug`, which the shared-bus handle does not implement.
        f.debug_struct("Touch").finish_non_exhaustive()
    }
}
