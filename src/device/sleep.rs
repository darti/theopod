//! Deep sleep, ended by a press of the BOOT button (ADR-0008).
//!
//! - [`DeepSleep`]: stops the chip until the BOOT button is pressed.
//! - `check` (feature `sleep-check`): counts deep sleeps across wakes (t-67).

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
        #[cfg(feature = "sleep-check")]
        check::count_sleep();
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

#[cfg(feature = "sleep-check")]
pub(super) mod check {
    //! Diagnostic for t-67: does a wake come from deep sleep, or a reset?
    //!
    //! Feature `sleep-check` (`just sleep-check`). STORE0 is an always-on
    //! RTC_CNTL register: deep sleep keeps it, and so do core and system
    //! resets (software, watchdog); the chip reset that espflash's monitor
    //! triggers on connect cleared it on the board (t-67). Each sleep
    //! increments a count there, tagged by [`MAGIC`], and each start logs it
    //! with the raw reset state and brown-out register. A count alone proves
    //! no wake: read it with the reset cause (`0x05` is a deep-sleep wake).
    //! Read the log with `just monitor-passive`, which never resets the chip.

    use super::LPWR;

    /// Upper half of STORE0 while it holds a sleep count.
    const MAGIC: u32 = 0x5EE9_0000;
    /// Lower half of STORE0: the count, which saturates at this value.
    const COUNT_MASK: u32 = 0xFFFF;

    /// The sleep count in a STORE0 value, or `None` if it holds none.
    fn decode(store: u32) -> Option<u32> {
        (store & !COUNT_MASK == MAGIC).then_some(store & COUNT_MASK)
    }

    /// Counts the sleep about to start; call it just before entering it.
    pub(in crate::device) fn count_sleep() {
        let store0 = LPWR::regs().store0();
        let count = decode(store0.read().data().bits()).map_or(1, |n| (n + 1).min(COUNT_MASK));
        // SAFETY: STORE0 is a 32-bit scratch register, so any value is
        // valid. esp-hal 1.2 uses STORE1-4 and the ROM's wake stub STORE6-7;
        // nothing in this firmware or esp-hal writes STORE0.
        store0.write(|w| unsafe { w.bits(MAGIC | count) });
    }

    /// Logs the sleep count, the raw reset state and the brown-out register.
    pub(in crate::device) fn log() {
        let regs = LPWR::regs();
        let store = regs.store0().read().data().bits();
        let reset = regs.reset_state().read();
        let brown_out = regs.brown_out().read();
        log::info!(
            "sleep check: sleeps since chip reset {:?}, store0 {store:#010x}, \
             reset cause pro {:#04x} app {:#04x} (reset_state {:#010x}), \
             brown_out {:#010x} (ena {} rst_ena {} rst_sel {} ana_rst_en {} det {})",
            decode(store),
            reset.reset_cause_procpu().bits(),
            reset.reset_cause_appcpu().bits(),
            reset.bits(),
            brown_out.bits(),
            brown_out.brown_out_ena().bit(),
            brown_out.brown_out_rst_ena().bit(),
            brown_out.brown_out_rst_sel().bit(),
            brown_out.brown_out_ana_rst_en().bit(),
            brown_out.det().bit(),
        );
    }
}
