//! When to resend the frame after failed panel flushes.
//!
//! - [`Transient`]: tells an error that may clear on retry from one that
//!   cannot.
//! - [`FlushRetry`], [`Retry`]: tracks a failure streak and decides when
//!   to resend every row and what to log.
//! - [`reset_due`]: when a failure streak calls for a device reset.
//!
//! The framebuffer stays the source of truth: after any failure the panel
//! is stale, and the next flush sends every row. Transient failures (bus
//! glitches, DMA underruns) are resent after an exponential backoff;
//! permanent ones (a logic error, a payload the bus can never send) are
//! not resent by themselves. The caller holds flushes while the panel is
//! stale until the resend is due, so redraws cannot bypass the backoff
//! (ADR-0004). Each kind of failure is logged once per streak.

/// Error that may, or may not, clear by itself on retry.
///
/// Implement this trait to let [`FlushRetry`] retry a bus or panel error's
/// transient failures and stop on its permanent ones.
pub trait Transient {
    /// Tells whether retrying the same operation may succeed.
    fn is_transient(&self) -> bool;
}

/// What to do after a failed flush.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    /// Log this failure: the first of its kind in the streak.
    pub log: bool,
    /// Resend every row after this many milliseconds; `None` for a
    /// permanent failure.
    pub after_ms: Option<u32>,
}

/// Failure streak of panel flushes, and the retry policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlushRetry {
    base_ms: u32,
    max_ms: u32,
    /// Consecutive failed flushes.
    failures: u32,
    /// Consecutive failures of the last failure's kind.
    same_kind: u32,
    /// Whether the last failure was transient.
    last_transient: bool,
    /// Kinds already logged in this streak.
    logged_transient: bool,
    logged_permanent: bool,
}

impl FlushRetry {
    /// Retries after `base_ms`, doubling per failure up to `max_ms`.
    ///
    /// # Panics
    ///
    /// Panics unless `0 < base_ms <= max_ms`.
    #[must_use]
    pub const fn new(base_ms: u32, max_ms: u32) -> Self {
        assert!(
            base_ms > 0 && base_ms <= max_ms,
            "retry delays need 0 < base <= max"
        );
        Self {
            base_ms,
            max_ms,
            failures: 0,
            same_kind: 0,
            last_transient: false,
            logged_transient: false,
            logged_permanent: false,
        }
    }

    /// Tells whether the panel misses rows: the next flush sends them all.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.failures > 0
    }

    /// Longest resend delay, also used after a redraw while stale.
    #[must_use]
    pub const fn max_ms(&self) -> u32 {
        self.max_ms
    }

    /// Records a successful flush; returns the failures it ends, if any.
    ///
    /// Only a flush of every row ends a streak: while [`Self::is_stale`],
    /// send them all.
    pub fn succeeded(&mut self) -> Option<u32> {
        let failures = self.failures;
        *self = Self::new(self.base_ms, self.max_ms);
        (failures > 0).then_some(failures)
    }

    /// Records a failed flush and says when to resend every row.
    ///
    /// A change of kind restarts the backoff.
    pub fn failed(&mut self, transient: bool) -> Retry {
        if self.failures == 0 || transient != self.last_transient {
            self.same_kind = 0;
        }
        self.failures = self.failures.saturating_add(1);
        self.same_kind = self.same_kind.saturating_add(1);
        self.last_transient = transient;
        let logged = if transient {
            &mut self.logged_transient
        } else {
            &mut self.logged_permanent
        };
        let log = !core::mem::replace(logged, true);
        let after_ms = transient.then(|| {
            let doublings = (self.same_kind - 1).min(31);
            self.base_ms
                .checked_shl(doublings)
                .filter(|&delay| delay >> doublings == self.base_ms)
                .map_or(self.max_ms, |delay| delay.min(self.max_ms))
        });
        Retry { log, after_ms }
    }
}

/// Tells whether a streak of `failures` is due for a device reset.
///
/// Resets fall at `first` failures, then each doubling up to
/// `max_interval`, then every `max_interval` failures: a wedged device is
/// reset quickly, and a dead one neither floods the log nor waits hours
/// between attempts. `first` and `max_interval` must be powers of two,
/// `first <= max_interval`.
#[must_use]
pub const fn reset_due(failures: u32, first: u32, max_interval: u32) -> bool {
    failures >= first
        && if failures <= max_interval {
            failures.is_power_of_two()
        } else {
            failures.is_multiple_of(max_interval)
        }
}
