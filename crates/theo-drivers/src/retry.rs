//! When to resend the frame after failed panel flushes.
//!
//! - [`Transient`]: tells an error that may clear on retry from one that
//!   cannot.
//! - [`FlushRetry`], [`Retry`]: tracks a failure streak and decides when
//!   to resend every row and what to log.
//!
//! The framebuffer stays the source of truth: after any failure the panel
//! is stale, and the next flush sends every row. Transient failures (bus
//! glitches, DMA underruns) are retried with an exponential backoff;
//! permanent ones (a logic error, a payload the bus can never send) are not
//! retried by themselves, only resent with the next redraw. Each kind of
//! failure is logged once per streak (ADR-0004).

use crate::{co5300, framebuffer::FlushError};

/// Error that may, or may not, clear by itself on retry.
///
/// Implement this for a bus or panel error so [`FlushRetry`] retries
/// transient failures and stops on permanent ones.
pub trait Transient {
    /// Tells whether retrying the same operation may succeed.
    fn is_transient(&self) -> bool;
}

impl<E: Transient> Transient for FlushError<E> {
    fn is_transient(&self) -> bool {
        match self {
            Self::Panel(error) => error.is_transient(),
            Self::FrameSizeMismatch { .. } | Self::RowsOutOfRange { .. } => false,
        }
    }
}

impl<E: Transient> Transient for co5300::Error<E> {
    fn is_transient(&self) -> bool {
        match self {
            Self::Bus(error) => error.is_transient(),
            Self::InvalidGeometry { .. }
            | Self::InvalidArea { .. }
            | Self::PixelOverflow { .. } => false,
        }
    }
}

/// What to do after a failed flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    /// Log this failure: the first of its streak, or of a new kind.
    pub log: bool,
    /// Resend every row after this many milliseconds; `None` to wait for
    /// the next redraw.
    pub after_ms: Option<u32>,
}

/// Failure streak of panel flushes, and the retry policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlushRetry {
    base_ms: u32,
    max_ms: u32,
    /// Consecutive failed flushes.
    failures: u32,
    /// Whether the last failure was transient.
    last_transient: bool,
}

impl FlushRetry {
    /// Retries after `base_ms`, doubling per failure up to `max_ms`.
    #[must_use]
    pub const fn new(base_ms: u32, max_ms: u32) -> Self {
        Self {
            base_ms,
            max_ms,
            failures: 0,
            last_transient: false,
        }
    }

    /// Tells whether the panel misses rows: the next flush sends them all.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.failures > 0
    }

    /// Records a successful flush; returns the failures it ends, if any.
    ///
    /// Only a flush of every row ends a streak: while [`Self::is_stale`],
    /// send them all.
    pub fn succeeded(&mut self) -> Option<u32> {
        let failures = core::mem::take(&mut self.failures);
        (failures > 0).then_some(failures)
    }

    /// Records a failed flush and says when to resend every row.
    pub fn failed(&mut self, transient: bool) -> Retry {
        let log = self.failures == 0 || transient != self.last_transient;
        self.failures = self.failures.saturating_add(1);
        self.last_transient = transient;
        let after_ms = transient.then(|| {
            let doublings = (self.failures - 1).min(31);
            self.base_ms
                .checked_shl(doublings)
                .filter(|&delay| delay >> doublings == self.base_ms)
                .map_or(self.max_ms, |delay| delay.min(self.max_ms))
        });
        Retry { log, after_ms }
    }
}
