//! Host-side fakes for driver tests.
//!
//! - [`RecordingBus`]: [`QspiBus`] that records every transfer.
//! - [`RecordingPanel`]: [`Panel`] that records every call.
//! - [`NoDelay`]: [`DelayNs`] that returns immediately.
//! - [`block_on`]: runs a future that never waits on I/O to completion.

use core::{
    future::Future,
    ops::Range,
    pin::pin,
    task::{Context, Poll, Waker},
};

use embedded_hal_async::delay::DelayNs;

use crate::{
    panel::Panel,
    qspi::{Lanes, QspiBus},
};

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

/// Error injected by [`RecordingBus::failing`].
#[derive(Debug, PartialEq, Eq)]
pub struct BusFault;

/// QSPI bus that records successful transfers and can fail exactly once.
#[derive(Debug, Default)]
pub struct RecordingBus {
    pub transfers: Vec<Transfer>,
    fail_at: Option<usize>,
}

impl RecordingBus {
    /// Creates a bus whose transfer attempt when `fail_at` transfers are
    /// recorded fails once; later attempts succeed.
    pub fn failing(fail_at: usize) -> Self {
        Self {
            transfers: Vec::new(),
            fail_at: Some(fail_at),
        }
    }
}

impl QspiBus for RecordingBus {
    type Error = BusFault;

    fn write(
        &mut self,
        opcode: u8,
        address: u32,
        lanes: Lanes,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        if self.fail_at == Some(self.transfers.len()) {
            self.fail_at = None;
            return Err(BusFault);
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
    width: u16,
    height: u16,
    fail_at: Option<usize>,
}

impl RecordingPanel {
    /// Creates a `width` by `height` panel that never fails.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            calls: Vec::new(),
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

    fn begin_area(&mut self, x: Range<u16>, y: Range<u16>) -> Result<(), Self::Error> {
        self.record(PanelCall::Begin { x, y })
    }

    fn write_pixels(&mut self, pixels: &[u8]) -> Result<(), Self::Error> {
        self.record(PanelCall::Pixels(pixels.to_vec()))
    }
}

/// Delay that completes immediately.
#[derive(Debug, Default)]
pub struct NoDelay;

impl DelayNs for NoDelay {
    async fn delay_ns(&mut self, _ns: u32) {}
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
