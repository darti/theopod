//! Panel power-up and power-down through a TCA9554 I/O expander.
//!
//! - [`PanelPower`]: which expander pins feed the panel rails/reset, the
//!   timing of the power-up pulse, and the power-down before deep sleep.
//!   Other pins on the expander, such as a touch reset, are left untouched.

use embedded_hal_async::{delay::DelayNs, i2c::I2c};

use crate::tca9554::Tca9554;

/// Panel rails/reset wired to expander pins, and their power-up timing.
///
/// The board values live with the rest of its wiring (`src/device/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelPower {
    /// Expander pins driving the panel rails and reset.
    pub pins: u8,
    /// Time the pins are held high before the reset pulse.
    pub settle_ms: u32,
    /// Length of the low reset pulse.
    pub reset_ms: u32,
    /// Wait after release before the panel accepts commands.
    pub power_up_ms: u32,
}

impl PanelPower {
    /// Pulses the panel pins low once, then leaves them high.
    ///
    /// The pins are latched high before becoming outputs, so the rails never
    /// glitch low outside the deliberate pulse. Other expander pins are kept.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access; the sequence stops.
    pub async fn power_up<I: I2c>(
        &self,
        expander: &mut Tca9554<I>,
        delay: &mut impl DelayNs,
    ) -> Result<(), I::Error> {
        expander.configure_outputs(self.pins, self.pins).await?;
        delay.delay_ms(self.settle_ms).await;
        expander.write_levels(self.pins, 0x00).await?;
        delay.delay_ms(self.reset_ms).await;
        expander.write_levels(self.pins, self.pins).await?;
        delay.delay_ms(self.power_up_ms).await;
        Ok(())
    }

    /// Drives the panel pins low, cutting the rails and holding reset.
    ///
    /// Call it after the controller's sleep-in, before deep sleep: the
    /// expander stays powered, so the pins stay low until the next
    /// [`Self::power_up`]. Levels are latched before the pins become
    /// outputs; other expander pins are kept.
    ///
    /// # Errors
    ///
    /// Returns the I2C error of the first failed access.
    pub async fn power_down<I: I2c>(&self, expander: &mut Tca9554<I>) -> Result<(), I::Error> {
        expander.configure_outputs(self.pins, 0x00).await
    }
}
