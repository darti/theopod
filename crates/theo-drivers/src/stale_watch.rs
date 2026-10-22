//! When a display that stays broken calls for a reboot.
//!
//! - [`StaleWatch`], [`WatchAction`]: watches how long the panel stays
//!   stale and healthy, with the reboots already made in a row.
//!
//! A display no flush can repair (a lost SPI master, a permanent error) is
//! recovered by a reboot, as at boot. After a few reboots in a row the
//! cause survives them, so the device keeps running instead of looping.
//! The count is forgotten only after the display stays healthy for a while
//! without interruption (ADR-0004, t-44).

/// What to do after a [`StaleWatch::check`].
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchAction {
    /// Nothing.
    None,
    /// Reboot, counting it.
    Reboot,
    /// The panel stays broken after the allowed reboots: log it, once.
    GiveUp,
    /// The display stayed healthy long enough: forget past reboots.
    ClearResets,
}

/// Watches the panel's health between frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleWatch {
    limit_ms: u64,
    healthy_ms: u64,
    max_resets: u32,
    /// When the panel last became stale, while it is.
    stale_since: Option<u64>,
    /// When the panel last became healthy, while it is.
    healthy_since: Option<u64>,
    /// Whether giving up was reported for the current stale period.
    gave_up: bool,
}

impl StaleWatch {
    /// Reboots after `limit_ms` stale, at most `max_resets` times in a row;
    /// forgets them after `healthy_ms` of uninterrupted health.
    #[must_use]
    pub const fn new(limit_ms: u64, healthy_ms: u64, max_resets: u32) -> Self {
        Self {
            limit_ms,
            healthy_ms,
            max_resets,
            stale_since: None,
            healthy_since: None,
            gave_up: false,
        }
    }

    /// Checks the panel at `now_ms`, with `resets` reboots made in a row.
    pub fn check(&mut self, now_ms: u64, stale: bool, resets: u32) -> WatchAction {
        if !stale {
            self.stale_since = None;
            self.gave_up = false;
            let since = *self.healthy_since.get_or_insert(now_ms);
            return if resets > 0 && now_ms.saturating_sub(since) >= self.healthy_ms {
                WatchAction::ClearResets
            } else {
                WatchAction::None
            };
        }
        self.healthy_since = None;
        let since = *self.stale_since.get_or_insert(now_ms);
        if now_ms.saturating_sub(since) < self.limit_ms {
            WatchAction::None
        } else if resets < self.max_resets {
            WatchAction::Reboot
        } else if core::mem::replace(&mut self.gave_up, true) {
            WatchAction::None
        } else {
            WatchAction::GiveUp
        }
    }
}
