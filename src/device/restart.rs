//! Restarts: why the chip started, the watchdog, and display reboots.
//!
//! - [`log_start_cause`]: logs the reset reason, or the deep-sleep wake
//!   cause (t-51).
//! - [`WATCHDOG_TIMEOUT_MS`]: how long a UI loop pass may take at most.
//! - [`Watchdog`]: the main watchdog of timer group 0 (MWDT0), fed by the
//!   UI loop (t-57).
//! - [`display_resets`], [`reset_for_display`], [`clear_display_resets`]:
//!   reboots for a broken display, counted across resets (t-44).
//!
//! Every reset here is a digital-core reset (`CoreSw`, `CoreMwdt0`), which
//! keeps RTC fast RAM and so the reboot count; deep sleep powers that RAM
//! down, which invalidates the count (it then reads as zero).

use esp_hal::{
    peripherals::TIMG0,
    system::{reset_reason, wakeup_cause},
    time::Duration,
    timer::timg::{MwdtStage, Wdt},
};
use theo_drivers::{decode_resets, encode_resets};

/// How long the UI loop may go without feeding the [`Watchdog`], in ms.
///
/// A pass is an idle wait, a render and a flush; `src/main.rs` asserts its
/// bounded parts fit in half of it, leaving the rest for the render.
pub const WATCHDOG_TIMEOUT_MS: u64 = 5000;
const WATCHDOG_TIMEOUT: Duration = Duration::from_millis(WATCHDOG_TIMEOUT_MS);

/// Logs why the chip started: the deep-sleep wake cause, or the reset reason.
///
/// The UI loop calls it a few seconds after start-up: after a wake, the
/// USB serial port takes about a second to come back.
pub fn log_start_cause() {
    let wake = wakeup_cause();
    if !wake.is_empty() {
        log::info!("woke from deep sleep: {wake:?}");
    } else if let Some(reason) = reset_reason() {
        log::info!("started after reset {reason:?}");
    } else {
        log::info!("started after an unknown reset");
    }
}

/// Main watchdog of timer group 0, armed early in `Board::init`.
///
/// Unfed for [`WATCHDOG_TIMEOUT_MS`], it resets the digital core
/// (`CoreMwdt0`), as a software reset does: RTC fast RAM, and with it the
/// display reboot count, survives. Its clock stops in deep sleep. A panic
/// once it is armed therefore ends in a reset (esp-backtrace spins).
#[must_use = "an armed watchdog that nobody feeds resets the chip"]
pub struct Watchdog {
    wdt: Wdt<TIMG0<'static>>,
}

impl Watchdog {
    /// Arms `wdt` for [`WATCHDOG_TIMEOUT_MS`], from a fresh count.
    pub(super) fn arm(mut wdt: Wdt<TIMG0<'static>>) -> Self {
        wdt.set_timeout(MwdtStage::Stage0, WATCHDOG_TIMEOUT);
        wdt.enable();
        wdt.feed();
        Self { wdt }
    }

    /// Restarts the countdown.
    pub fn feed(&mut self) {
        self.wdt.feed();
    }

    /// Stops the watchdog for good, before the unbounded BOOT-release wait.
    pub fn disarm(mut self) {
        self.wdt.disable();
    }

    /// Runs `f` with the watchdog stopped, then rearms it.
    ///
    /// For a frame dump, which takes seconds on the serial port.
    #[cfg(feature = "frame-dump")]
    pub fn paused<R>(&mut self, f: impl FnOnce() -> R) -> R {
        self.wdt.disable();
        let result = f();
        self.wdt.enable();
        self.wdt.feed();
        result
    }
}

impl core::fmt::Debug for Watchdog {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Watchdog").finish_non_exhaustive()
    }
}

/// Checks on the board that the reboot count survives a watchdog reset.
///
/// Feature `watchdog-check`: a start with a zero count sets it to 1 and
/// hangs without feeding the watchdog; the start after the reset logs
/// `PASS` if it was a MWDT0 reset that kept the count, `FAIL` otherwise.
/// A nonzero count before any hang also fails, so the check never loops.
#[cfg(feature = "watchdog-check")]
pub(super) fn check_watchdog_reset() {
    use esp_hal::rtc_cntl::SocResetReason;

    let reason = reset_reason();
    let count = display_resets();
    if reason == Some(SocResetReason::CoreMwdt0) && count == 1 {
        log::info!("watchdog check: PASS, MWDT0 reset kept display reboot count 1");
    } else if count != 0 || reason == Some(SocResetReason::CoreMwdt0) {
        log::error!("watchdog check: FAIL, reset {reason:?} with display reboot count {count}");
        clear_display_resets();
    } else {
        set_display_resets(1);
        log::info!(
            "watchdog check: display reboot count 1, hanging until the watchdog \
             resets ({WATCHDOG_TIMEOUT_MS} ms)"
        );
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Reboots for a broken display in a row, kept across core resets.
///
/// In RTC fast RAM, which a digital-core reset (software or MWDT0) keeps. Its contents are
/// arbitrary after an early reset (esp-hal `persistent`) or a deep sleep,
/// which powers that RAM down (`DeepSleep`); the complement
/// ([`encode_resets`]) makes those read as zero, so a stop clears the count.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut DISPLAY_RESETS: [u32; 2] = [0; 2];

/// Reboots for a broken display since the display last stayed healthy.
///
/// Only the UI task may call this and the functions below: nothing else
/// touches the counter.
#[must_use]
pub fn display_resets() -> u32 {
    // SAFETY: a volatile read through a raw pointer (no reference to the
    // `static mut`); only the UI task touches it, and every bit pattern is
    // a valid `[u32; 2]`.
    decode_resets(unsafe { (&raw const DISPLAY_RESETS).read_volatile() })
}

fn set_display_resets(count: u32) {
    // SAFETY: as in `display_resets`; a reset during the write leaves a
    // pair that fails the check and reads as zero.
    unsafe { (&raw mut DISPLAY_RESETS).write_volatile(encode_resets(count)) };
}

/// Forgets past display reboots, once the display has stayed healthy.
pub fn clear_display_resets() {
    set_display_resets(0);
}

/// Reboots the device to recover a broken display, counting the reboot.
pub fn reset_for_display() -> ! {
    set_display_resets(display_resets().saturating_add(1));
    esp_hal::system::software_reset()
}
