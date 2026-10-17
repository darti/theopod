//! Restarts: why the chip started, the watchdog, and display reboots.
//!
//! - [`log_start_cause`]: logs the reset reason, or the deep-sleep wake
//!   cause (t-51).
//! - [`WATCHDOG_TIMEOUT_MS`]: how long a UI loop pass may take at most.
//! - [`Watchdog`]: the main watchdog of timer group 0 (MWDT0), fed by the
//!   UI loop (t-57).
//! - [`display_resets`], [`reset_for_display`], [`clear_display_resets`]:
//!   reboots for a broken display, counted across resets (t-44).
//! - [`clear_watchdog_resets_if_healthy`]: forgets watchdog reboots in a
//!   row once the UI loop has run a while; `Board::init` counts them and
//!   stops at the cap (t-66).
//!
//! Every reset here is a digital-core reset (`CoreSw`, `CoreMwdt0`), which
//! keeps RTC fast RAM and so the reboot count; deep sleep powers that RAM
//! down, which invalidates the count (it then reads as zero).

use esp_hal::{
    peripherals::TIMG0,
    rtc_cntl::SocResetReason,
    system::{reset_reason, wakeup_cause},
    time::Duration,
    timer::timg::{MwdtStage, Wdt},
};
use theo_drivers::{RebootCap, StartAction, decode_resets, encode_resets};

/// How long the UI loop may go without feeding the [`Watchdog`], in ms.
///
/// A pass is a render, a flush, an idle wait and, on a stop, a sleep-in;
/// `src/main.rs` asserts its bounded parts fit in half of it.
pub const WATCHDOG_TIMEOUT_MS: u64 = 5000;
const WATCHDOG_TIMEOUT: Duration = Duration::from_millis(WATCHDOG_TIMEOUT_MS);

/// Stops at the third watchdog start in a row.
///
/// 60 s of UI loop forget them. A hang or panic that comes back after
/// every boot would otherwise reboot every [`WATCHDOG_TIMEOUT_MS`],
/// forever; one that comes back later than that is not capped (t-66).
const WATCHDOG_REBOOT_CAP: RebootCap = RebootCap::new(3, 60_000);

/// Logs why the chip started: the deep-sleep wake cause, or the reset reason.
///
/// The UI loop calls it once the USB serial port is back from a wake
/// (`START_CAUSE_LOG_AFTER`), or before a stop that comes earlier. Read a
/// wake with a passive monitor: `espflash monitor` resets the chip on
/// connect.
pub fn log_start_cause() {
    let wake = wakeup_cause();
    if !wake.is_empty() {
        log::info!("woke from deep sleep: {wake:?}");
    } else if let Some(reason) = reset_reason() {
        log::info!("started after reset {reason:?}");
    } else {
        log::info!("started after an unknown reset");
    }
    #[cfg(feature = "sleep-check")]
    super::sleep::check::log();
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

/// Hangs until the watchdog resets the chip, on every start.
///
/// Feature `watchdog-loop`: a hang that comes back at each boot, so the
/// third watchdog start in a row must stop into deep sleep (t-66).
#[cfg(feature = "watchdog-loop")]
pub(super) fn hang_until_watchdog() {
    log::info!(
        "watchdog loop: {} resets in a row so far, hanging until the watchdog resets \
         ({WATCHDOG_TIMEOUT_MS} ms)",
        watchdog_resets()
    );
    loop {
        core::hint::spin_loop();
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

/// Watchdog reboots in a row, kept across core resets like `DISPLAY_RESETS`.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut WATCHDOG_RESETS: [u32; 2] = [0; 2];

/// Watchdog reboots in a row since the UI loop last ran healthy.
///
/// Only `Board::init` and then the UI task use the functions below, one
/// after the other: nothing else touches the counter.
fn watchdog_resets() -> u32 {
    // SAFETY: a volatile read through a raw pointer (no reference to the
    // `static mut`); one user at a time, and every bit pattern is a valid
    // `[u32; 2]`.
    decode_resets(unsafe { (&raw const WATCHDOG_RESETS).read_volatile() })
}

fn set_watchdog_resets(count: u32) {
    // SAFETY: as in `watchdog_resets`; a reset during the write leaves a
    // pair that fails the check and reads as zero.
    unsafe { (&raw mut WATCHDOG_RESETS).write_volatile(encode_resets(count)) };
}

/// Forgets past watchdog reboots, just before the stop's deep sleep.
///
/// Deep sleep powers RTC fast RAM down, but a short sleep may not lose it.
pub(super) fn clear_watchdog_resets() {
    set_watchdog_resets(0);
}

/// Forgets past watchdog reboots once the UI loop has run `uptime_ms`.
///
/// The UI loop calls it on each pass: a loop that got this far is not a
/// hang that comes back at every boot.
pub fn clear_watchdog_resets_if_healthy(uptime_ms: u64) {
    if WATCHDOG_REBOOT_CAP.should_clear(uptime_ms, watchdog_resets()) {
        clear_watchdog_resets();
        log::info!("UI loop healthy: watchdog reboot count cleared");
    }
}

/// Counts this start if the watchdog caused it, and says whether to stop.
///
/// For `Board::init`, once, right after arming the watchdog: a hang later
/// in the bring-up is capped too. Only `CoreMwdt0`, the main watchdog's
/// reset (ADR-0009), counts.
pub(super) fn count_watchdog_start() -> StartAction {
    let watchdog = reset_reason() == Some(SocResetReason::CoreMwdt0);
    let start = WATCHDOG_REBOOT_CAP.on_start(watchdog, watchdog_resets());
    set_watchdog_resets(start.reboots);
    match start.action {
        StartAction::Run if watchdog => {
            log::warn!("watchdog reset: {} in a row", start.reboots);
        }
        StartAction::Run => {}
        StartAction::Stop => {
            log::error!("{} watchdog resets in a row: stopping", start.reboots);
        }
        StartAction::StopWithoutParking => {
            log::error!("the last stop did not finish: stopping without parking");
        }
    }
    start.action
}
