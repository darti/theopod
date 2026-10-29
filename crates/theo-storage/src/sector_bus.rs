//! A unique synchronous-filesystem client's owned-sector mailbox to its async
//! device service. Cancellation poisons the session instead of reusing replies.

use crate::littlefs::RequestGuard;
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use littlefs_rust::Error;

// Deliberately inline: bounded mailboxes avoid an allocation for every sector.
#[allow(clippy::large_enum_variant)]
pub enum Operation {
    Read(u32),
    Write(u32, [u8; 512]),
    Sync,
}
pub struct Request {
    pub operation: Operation,
    pub deadline_ticks: u64,
}

pub struct SectorBus {
    taken: AtomicBool,
    poison: AtomicBool,
    requests: Channel<CriticalSectionRawMutex, Request, 1>,
    response: Signal<CriticalSectionRawMutex, Result<[u8; 512], Error>>,
}

impl SectorBus {
    pub const fn new() -> Self {
        Self {
            taken: AtomicBool::new(false),
            poison: AtomicBool::new(false),
            requests: Channel::new(),
            response: Signal::new(),
        }
    }

    /// One client for this boot/session. Its exclusive borrow serializes RPCs.
    pub fn take_client(&self) -> Option<SectorClient<'_>> {
        self.taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(SectorClient { bus: self })
    }
    pub fn poison_flag(&self) -> &AtomicBool {
        &self.poison
    }
    pub fn disable(&self) {
        self.poison.store(true, Ordering::Release);
    }
    pub fn is_disabled(&self) -> bool {
        self.poison.load(Ordering::Acquire)
    }
    pub async fn receive(&self) -> Request {
        self.requests.receive().await
    }
    pub fn respond(&self, result: Result<[u8; 512], Error>) {
        if result.is_err() {
            self.disable();
        }
        self.response.signal(result);
    }
}
impl Default for SectorBus {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SectorClient<'a> {
    bus: &'a SectorBus,
}
impl SectorClient<'_> {
    /// For a dedicated synchronous core only. Poll the supplied clock instead
    /// of registering embassy_time::Timer with block_on's non-Embassy waker.
    /// Dropping the timed-out exchange still poisons the session.
    pub fn exchange_blocking(
        &mut self,
        request: Request,
        mut expired: impl FnMut() -> bool,
    ) -> Result<[u8; 512], Error> {
        use core::{
            future::{Future, poll_fn},
            pin::pin,
            task::Poll,
        };
        let result = {
            let mut exchange = pin!(self.exchange(request));
            embassy_futures::block_on(poll_fn(|cx| {
                if expired() {
                    Poll::Ready(Err(Error::Io))
                } else {
                    exchange.as_mut().poll(cx)
                }
            }))
        };
        if result.is_err() {
            self.bus.disable();
        }
        result
    }

    pub async fn exchange(&mut self, request: Request) -> Result<[u8; 512], Error> {
        let guard = RequestGuard::begin(self.bus.poison_flag())?;
        self.bus.requests.send(request).await;
        let bytes = self.bus.response.wait().await?;
        guard.complete()?;
        Ok(bytes)
    }
}
impl Drop for SectorClient<'_> {
    fn drop(&mut self) {
        self.bus.disable();
    }
}

#[cfg(test)]
#[path = "sector_bus_tests.rs"]
mod sector_bus_tests;
