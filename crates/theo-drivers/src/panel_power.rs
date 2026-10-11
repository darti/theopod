//! Panel power-up through a TCA9554 I/O expander.
//!
//! - [`PanelPower`]: which expander pins feed the panel rails/reset, and the
//!   timing of the power-up pulse.

use embedded_hal::i2c::I2c;
use embedded_hal_async::delay::DelayNs;

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
        expander.configure_outputs(self.pins, self.pins)?;
        delay.delay_ms(self.settle_ms).await;
        expander.write_levels(self.pins, 0x00)?;
        delay.delay_ms(self.reset_ms).await;
        expander.write_levels(self.pins, self.pins)?;
        delay.delay_ms(self.power_up_ms).await;
        Ok(())
    }
}
