//! Fake millisecond clock whose delays move time forward.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use embedded_hal_async::delay::DelayNs;

use super::YieldOnce;

/// Clock shared by its clones; each delay advances it by its length.
///
/// Delays are recorded. With [`FakeClock::pend_next_delay`], the next delay
/// returns `Pending` once before it starts, so a test can poll a future into
/// the delay, drop it, and move time on with [`FakeClock::advance`].
#[derive(Clone, Debug, Default)]
pub struct FakeClock(Rc<ClockState>);

#[derive(Debug, Default)]
struct ClockState {
    now_ms: Cell<u64>,
    delays_ms: RefCell<Vec<u32>>,
    pend_next: Cell<bool>,
}

impl FakeClock {
    /// Milliseconds since the clock was created.
    pub fn now_ms(&self) -> u64 {
        self.0.now_ms.get()
    }

    /// Moves time forward by `ms`.
    pub fn advance(&self, ms: u64) {
        self.0.now_ms.set(self.now_ms().saturating_add(ms));
    }

    /// Every delay requested so far, in milliseconds.
    pub fn delays_ms(&self) -> Vec<u32> {
        self.0.delays_ms.borrow().clone()
    }

    /// Makes the next delay return `Pending` once before it starts.
    pub fn pend_next_delay(&self) {
        self.0.pend_next.set(true);
    }
}

impl DelayNs for FakeClock {
    async fn delay_ns(&mut self, ns: u32) {
        self.delay_ms(ns.div_ceil(1_000_000)).await;
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.0.delays_ms.borrow_mut().push(ms);
        if self.0.pend_next.take() {
            YieldOnce(false).await;
        }
        self.advance(u64::from(ms));
    }
}
