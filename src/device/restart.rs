//! Restarts: why the chip started, and display reboots.
//!
//! - [`log_start_cause`]: logs the reset reason, or the deep-sleep wake
//!   cause (t-51).
//! - [`display_resets`], [`reset_for_display`], [`clear_display_resets`]:
//!   reboots for a broken display, counted across resets (t-44).
//!
//! A software reset (`CoreSw`) is a digital-core reset, which keeps RTC
//! fast RAM and so the reboot count; deep sleep powers that RAM down, which
//! invalidates the count (it then reads as zero).

use esp_hal::system::{reset_reason, wakeup_cause};
use theo_drivers::{decode_resets, encode_resets};

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

/// Reboots for a broken display in a row, kept across core resets.
///
/// In RTC fast RAM, which a digital-core reset (`CoreSw`) keeps. Its contents are
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
