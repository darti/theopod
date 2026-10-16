//! Deep sleep, ended by a press of the BOOT button (ADR-0008).
//!
//! - [`DeepSleep`]: stops the chip until the BOOT button is pressed.

use core::fmt;

use esp_hal::{
    gpio::{Event, WakeupConfig},
    peripherals::LPWR,
    rtc_cntl::sleep::{LowPower, RtcSleepConfig},
};

use super::Button;

/// Deep sleep of the ESP32-S3, ended by a press of the BOOT button.
///
/// GPIO0 is an RTC pad, so it can wake the chip while the digital domain
/// is off. Waking resets the chip: the firmware starts again from
/// [`Board::init`](super::Board::init).
pub struct DeepSleep {
    low_power: LowPower<'static>,
}

impl DeepSleep {
    /// Takes the low-power controller that enters the sleep.
    pub(super) fn new(lpwr: LPWR<'static>) -> Self {
        Self {
            low_power: LowPower::new(lpwr),
        }
    }

    /// Waits for `button` to be released, then sleeps until it is pressed.
    ///
    /// Waking restarts the firmware. Sleeping with the button down would
    /// wake the chip at once, hence the wait. Peripherals outside the chip
    /// keep their state: put the panel to sleep, then park
    /// [`Touch`](super::Touch) (panel rails, touch reset) and power the
    /// [`Imu`](super::Imu) down first.
    ///
    /// If GPIO0 cannot be armed as a wakeup source, which its RTC pad rules
    /// out, the chip restarts instead: the panel is already dark, and a
    /// halted chip would look dead.
    pub async fn until_pressed(mut self, mut button: Button) -> ! {
        while button.is_pressed() {
            match button.changed().await {
                Ok(_) => {}
                Err(never) => match never {},
            }
        }
        let (mut pin, _) = button.release();
        // The low-power path is the one that works with the digital domain
        // off; sleep keeps the pin's pull-up, so the pad stays high.
        if let Err(error) =
            pin.apply_wakeup_config(&WakeupConfig::default().with_low_power_path(true))
        {
            log::error!("BOOT button cannot wake the chip, restarting: {error:?}");
            esp_hal::system::software_reset();
        }
        log::info!("entering deep sleep until BOOT is pressed");
        // The wake level is the listen trigger. Interrupts stay off from
        // here: the async GPIO handler would stop listening on a press
        // and leave the sleep without a wakeup source.
        critical_section::with(|_| {
            pin.listen(Event::LowLevel);
            self.low_power.sleep_deep(RtcSleepConfig::deep())
        })
    }
}

impl fmt::Debug for DeepSleep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeepSleep").finish_non_exhaustive()
    }
}
