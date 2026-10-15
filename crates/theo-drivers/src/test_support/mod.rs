//! Host-side fakes for driver tests.
//!
//! - [`RecordingBus`]: [`QspiBus`] that records every transfer.
//! - [`AddressBus`]: [`QspiBus`] that records where pixel payloads start.
//! - [`RecordingPanel`]: [`Panel`] that records every call.
//! - [`FakeTca9554`]: async I2C bus modelling a TCA9554's registers; can
//!   fail reads or writes and suspend once per transaction.
//! - [`FakeCst820`]: async I2C bus replaying scripted CST820 touch reports
//!   and recording register writes.
//! - [`FakeQmi8658`]: async I2C bus modelling a QMI8658's registers.
//! - [`NoDelay`]: [`DelayNs`] that returns immediately.
//! - [`RecordingDelay`]: [`DelayNs`] that records millisecond delays and can
//!   stay pending once, to cancel a future mid-delay.
//! - [`block_on`]: runs a future that never waits on I/O to completion.

use core::{
    future::Future,
    ops::Range,
    pin::pin,
    task::{Context, Poll, Waker},
};

use embedded_hal_async::delay::DelayNs;

use crate::{
    co5300::OPCODE_WRITE_PIXELS,
    panel::Panel,
    qspi::{Lanes, QspiBus},
    retry::Transient,
};

mod cst820;
mod qmi8658;
mod tca9554;

pub use cst820::FakeCst820;
pub use qmi8658::FakeQmi8658;
pub use tca9554::{FakeTca9554, TCA_CONFIG, TCA_OUTPUT};

/// One recorded QSPI transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub opcode: u8,
    pub address: u32,
    pub lanes: Lanes,
    pub data: Vec<u8>,
}

impl Transfer {
    /// Builds the transfer expected for a CO5300 register write.
    pub fn register(command: u8, data: &[u8]) -> Self {
        Self {
            opcode: 0x02,
            address: u32::from(command) << 8,
            lanes: Lanes::Single,
            data: data.to_vec(),
        }
    }

    /// Builds the transfer expected for a CO5300 pixel write.
    pub fn pixels(data: &[u8]) -> Self {
        Self {
            opcode: 0x32,
            address: 0x3C00,
            lanes: Lanes::Quad,
            data: data.to_vec(),
        }
    }
}

/// Error injected by [`RecordingBus::failing`]; transient.
#[derive(Debug, PartialEq, Eq)]
pub struct BusFault;

impl Transient for BusFault {
    fn is_transient(&self) -> bool {
        true
    }
}

/// QSPI bus that records successful transfers and can fail exactly once,
/// or stay pending once, to cancel a transfer mid-way.
///
/// It accepts at most 16 bytes per transfer and panics on longer payloads,
/// so every test checks that drivers respect `MAX_TRANSFER_LEN`.
#[derive(Debug, Default)]
pub struct RecordingBus {
    pub transfers: Vec<Transfer>,
    fail_at: Option<usize>,
    pend_at: Option<usize>,
}

impl RecordingBus {
    /// Creates a bus whose transfer attempt when `fail_at` transfers are
    /// recorded fails once; later attempts succeed.
    pub fn failing(fail_at: usize) -> Self {
        Self {
            fail_at: Some(fail_at),
            ..Self::default()
        }
    }

    /// Creates a bus whose transfer attempt when `pend_at` transfers are
    /// recorded returns `Pending` once before completing.
    pub fn pending(pend_at: usize) -> Self {
        Self {
            pend_at: Some(pend_at),
            ..Self::default()
        }
    }
}

impl QspiBus for RecordingBus {
    type Error = BusFault;

    const MAX_TRANSFER_LEN: usize = 16;

    async fn write(
        &mut self,
        opcode: u8,
        address: u32,
        lanes: Lanes,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        assert!(
            data.len() <= Self::MAX_TRANSFER_LEN,
            "{} bytes exceed MAX_TRANSFER_LEN",
            data.len()
        );
        if self.fail_at == Some(self.transfers.len()) {
            self.fail_at = None;
            return Err(BusFault);
        }
        if self.pend_at == Some(self.transfers.len()) {
            self.pend_at = None;
            YieldOnce(false).await;
        }
        self.transfers.push(Transfer {
            opcode,
            address,
            lanes,
            data: data.to_vec(),
        });
        Ok(())
    }
}

/// QSPI bus that records the address of every pixel payload.
///
/// `MAX` sets `MAX_TRANSFER_LEN`, so tests can use a real transfer size
/// without copying the payloads.
#[derive(Debug, Default)]
pub struct AddressBus<const MAX: usize> {
    /// Start address and length of each pixel payload, in order.
    pub pixels: Vec<(usize, usize)>,
}

impl<const MAX: usize> QspiBus for AddressBus<MAX> {
    type Error = BusFault;

    const MAX_TRANSFER_LEN: usize = MAX;

    async fn write(
        &mut self,
        opcode: u8,
        _address: u32,
        _lanes: Lanes,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        assert!(
            data.len() <= MAX,
            "{} bytes exceed MAX_TRANSFER_LEN",
            data.len()
        );
        if opcode == OPCODE_WRITE_PIXELS {
            self.pixels.push((data.as_ptr() as usize, data.len()));
        }
        Ok(())
    }
}

/// Call recorded by [`RecordingPanel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelCall {
    Begin { x: Range<u16>, y: Range<u16> },
    Pixels(Vec<u8>),
}

/// Panel that records calls and fails every call from a given index on.
#[derive(Debug)]
pub struct RecordingPanel {
    pub calls: Vec<PanelCall>,
    /// Calls made, failed ones included.
    pub attempts: usize,
    width: u16,
    height: u16,
    fail_at: Option<usize>,
}

impl RecordingPanel {
    /// Creates a `width` by `height` panel that never fails.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            calls: Vec::new(),
            attempts: 0,
            width,
            height,
            fail_at: None,
        }
    }

    /// Makes every call fail once `fail_at` calls are recorded.
    pub fn failing(mut self, fail_at: usize) -> Self {
        self.fail_at = Some(fail_at);
        self
    }

    fn record(&mut self, call: PanelCall) -> Result<(), BusFault> {
        self.attempts += 1;
        if self.fail_at == Some(self.calls.len()) {
            return Err(BusFault);
        }
        self.calls.push(call);
        Ok(())
    }
}

impl Panel for RecordingPanel {
    type Error = BusFault;

    fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    async fn begin_area(&mut self, x: Range<u16>, y: Range<u16>) -> Result<(), Self::Error> {
        self.record(PanelCall::Begin { x, y })
    }

    async fn write_pixels(&mut self, pixels: &[u8]) -> Result<(), Self::Error> {
        self.record(PanelCall::Pixels(pixels.to_vec()))
    }
}

/// Delay that completes immediately.
#[derive(Debug, Default)]
pub struct NoDelay;

impl DelayNs for NoDelay {
    async fn delay_ns(&mut self, _ns: u32) {}
}

/// Delay that records every `delay_ms` request and returns immediately.
///
/// With `pend_next` set, the next delay returns `Pending` once first, so a
/// test can poll a future into the delay and then drop it.
#[derive(Debug, Default)]
pub struct RecordingDelay {
    pub delays_ms: Vec<u32>,
    pub pend_next: bool,
}

impl DelayNs for RecordingDelay {
    async fn delay_ns(&mut self, ns: u32) {
        self.delay_ms(ns / 1_000_000).await;
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.delays_ms.push(ms);
        if core::mem::take(&mut self.pend_next) {
            YieldOnce(false).await;
        }
    }
}

/// Future that returns `Pending` on its first poll only.
pub(crate) struct YieldOnce(pub(crate) bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: core::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

/// Polls `future` once; `true` if it completed.
pub fn poll_once<F: Future>(future: F) -> bool {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut context).is_ready()
}

/// Polls `future` until it completes; it must never return `Pending` forever.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}
