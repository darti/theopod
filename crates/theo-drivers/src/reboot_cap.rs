//! When repeated watchdog reboots call for a stop instead.
//!
//! - [`RebootCap`], [`StartDecision`], [`StartAction`]: counts watchdog
//!   starts in a row and stops at a cap; forgets them after a healthy run.
//!
//! A hang or panic that comes back after every boot would otherwise reboot
//! the device every watchdog period, forever. The count lives in RAM that
//! a watchdog reset keeps (stored with [`encode_resets`](crate::encode_resets)).
//! Other starts neither count nor clear it: a software reset keeps it, and
//! a power-on or a deep sleep loses that RAM, so it reads as zero then.
//! "In a row" therefore means: without a healthy run in between (t-66).

/// What the device does at start-up, after [`RebootCap::on_start`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartAction {
    /// Start as usual.
    Run,
    /// Too many watchdog reboots in a row: park the board, then stop until
    /// the user starts it.
    Stop,
    /// The last stop did not reach its sleep (it hung, and the watchdog
    /// reset the chip): stop at once, without parking.
    StopWithoutParking,
}

/// The decision of [`RebootCap::on_start`], with the count to store.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartDecision {
    /// What to do now.
    pub action: StartAction,
    /// Watchdog reboots in a row, to store for the next start.
    pub reboots: u32,
}

/// Caps watchdog reboots in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RebootCap {
    max_reboots: u32,
    healthy_ms: u64,
}

impl RebootCap {
    /// Stops at the `max_reboots`-th watchdog start in a row.
    ///
    /// A run of `healthy_ms` forgets the count.
    ///
    /// # Panics
    ///
    /// Panics if `max_reboots` is zero (at compile time in a `const`).
    #[must_use]
    pub const fn new(max_reboots: u32, healthy_ms: u64) -> Self {
        assert!(max_reboots > 0, "the cap must allow at least one reboot");
        Self {
            max_reboots,
            healthy_ms,
        }
    }

    /// Decides at start-up, from `stored` reboots and whether this start
    /// is a watchdog reset.
    ///
    /// A stop keeps the count at the cap while the board parks: if parking
    /// hangs, the next watchdog start finds it there and stops without
    /// parking. The caller clears the count just before it sleeps.
    pub const fn on_start(&self, watchdog_reset: bool, stored: u32) -> StartDecision {
        if !watchdog_reset {
            return StartDecision {
                action: StartAction::Run,
                reboots: stored,
            };
        }
        let reboots = stored.saturating_add(1);
        let action = if stored >= self.max_reboots {
            StartAction::StopWithoutParking
        } else if reboots >= self.max_reboots {
            StartAction::Stop
        } else {
            StartAction::Run
        };
        StartDecision { action, reboots }
    }

    /// Whether to forget `reboots` once the device has run `uptime_ms`.
    #[must_use]
    pub const fn should_clear(&self, uptime_ms: u64, reboots: u32) -> bool {
        reboots > 0 && uptime_ms >= self.healthy_ms
    }
}
