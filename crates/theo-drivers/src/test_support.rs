//! Host-side fakes for driver tests.
//!
//! - [`RecordingBus`]: [`QspiBus`] that records every transfer.
//! - [`RecordingPanel`]: [`Panel`] that records every call.
//! - [`FakeTca9554`]: async I2C bus modelling a TCA9554's registers; can
//!   fail reads or writes and suspend once per transaction.
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

use embedded_hal_async::{
    delay::DelayNs,
    i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation},
};

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
///
/// It accepts at most 16 bytes per transfer and panics on longer payloads,
/// so every test checks that drivers respect `MAX_TRANSFER_LEN`.
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
struct YieldOnce(bool);

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

/// TCA9554 register indices, as seen by [`FakeTca9554`].
pub const TCA_INPUT: u8 = 0x00;
pub const TCA_OUTPUT: u8 = 0x01;
pub const TCA_CONFIG: u8 = 0x03;

/// Async I2C bus emulating one TCA9554: register pointer, reads and writes.
///
/// `writes` records every register write as `(register, value)`; `inputs`
/// is what the input port reads back. A bus at another address NACKs.
#[derive(Debug)]
pub struct FakeTca9554 {
    pub address: u8,
    pub registers: [u8; 4],
    pub inputs: u8,
    pub writes: Vec<(u8, u8)>,
    pointer: u8,
    fail_at: Option<usize>,
    fail_writes_only: bool,
    yielding: bool,
}

impl FakeTca9554 {
    /// Creates an expander at `address` with power-on register values.
    pub fn new(address: u8) -> Self {
        Self {
            address,
            // Input, output (all high), polarity (none), config (all inputs).
            registers: [0x00, 0xFF, 0x00, 0xFF],
            inputs: 0x00,
            writes: Vec::new(),
            pointer: 0,
            fail_at: None,
            fail_writes_only: false,
            yielding: false,
        }
    }

    /// Makes every transaction fail once `fail_at` writes are recorded.
    pub fn failing(mut self, fail_at: usize) -> Self {
        self.fail_at = Some(fail_at);
        self
    }

    /// Like [`Self::failing`], but register reads keep succeeding.
    pub fn failing_writes(mut self, fail_at: usize) -> Self {
        self.fail_writes_only = true;
        self.failing(fail_at)
    }

    /// Makes every transaction return `Pending` once before running.
    pub fn yielding(mut self) -> Self {
        self.yielding = true;
        self
    }

    /// Pins driven low after each recorded write, replayed from `initial`.
    ///
    /// A pin is driven low when it is an output (config bit clear) whose
    /// output bit is clear.
    pub fn driven_low_history(&self, initial: [u8; 4]) -> Vec<u8> {
        let mut registers = initial;
        self.writes
            .iter()
            .map(|&(register, value)| {
                registers[usize::from(register)] = value;
                !registers[usize::from(TCA_CONFIG)] & !registers[usize::from(TCA_OUTPUT)]
            })
            .collect()
    }
}

impl ErrorType for FakeTca9554 {
    type Error = ErrorKind;
}

impl I2c for FakeTca9554 {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        if self.yielding {
            YieldOnce(false).await;
        }
        // A write carries data after the register pointer.
        let writes_data = operations
            .iter()
            .any(|operation| matches!(operation, Operation::Write(bytes) if bytes.len() > 1));
        if self.fail_at == Some(self.writes.len()) && (writes_data || !self.fail_writes_only) {
            return Err(ErrorKind::Bus);
        }
        if address != self.address {
            return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address));
        }
        for operation in operations {
            match operation {
                Operation::Write(bytes) => {
                    let Some((&pointer, data)) = bytes.split_first() else {
                        continue;
                    };
                    if pointer > TCA_CONFIG {
                        return Err(ErrorKind::Other);
                    }
                    self.pointer = pointer;
                    // The TCA9554 does not auto-increment: extra bytes rewrite it.
                    for &value in data {
                        self.registers[usize::from(pointer)] = value;
                        self.writes.push((pointer, value));
                    }
                }
                Operation::Read(buffer) => {
                    let value = if self.pointer == TCA_INPUT {
                        self.inputs
                    } else {
                        self.registers[usize::from(self.pointer)]
                    };
                    buffer.fill(value);
                }
            }
        }
        Ok(())
    }
}
