//! [`QspiBus`] implementation for an esp-hal SPI master with DMA.
//!
//! - [`EspQspi`]: async half-duplex SPI master with a software chip select.
//! - [`EspQspiError`]: SPI, chip-select, payload or DMA underrun failure.
//! - [`MAX_TRANSFER_LEN`]: longest payload of one transfer.
//!
//! Framebuffer payloads are read in place from PSRAM through a
//! [`PsramTxBuf`] (64-byte bursts, burst-aligned descriptors); they must be
//! burst-aligned, and the hardware underrun flag is checked after each.
//! Other payloads go through esp-hal's slice API: the SPI FIFO below 64
//! bytes, else DMA in place from internal RAM or PSRAM (16-byte bursts, too
//! slow for long PSRAM payloads), or a copy for flash. See `docs/adr/0004`.
//!
//! Writes are cancel-safe: dropping one mid-transfer stops the transfer,
//! waits for the SPI to go idle, then releases chip select; for a
//! framebuffer payload it also keeps the SPI master.
//! Leaking one (`mem::forget`) is not: see [`EspQspi::new`].

use core::{fmt, ops::Range};

use embedded_hal::digital::OutputPin;
use esp_hal::{
    Async,
    spi::{
        Error as SpiError,
        master::{Address, Command, DataMode, SpiDma, SpiDmaTransfer},
    },
};
use theo_drivers::{ChipSelect, FramePayloadError, Lanes, QspiBus, Transient, frame_payload};

use super::psram_tx_buf::{self, PsramTxBuf};

/// Failure of an [`EspQspi`] transfer.
#[derive(Debug)]
pub enum EspQspiError<P> {
    /// The SPI peripheral failed.
    Spi(SpiError),
    /// The chip-select pin failed.
    ChipSelect(P),
    /// A framebuffer payload exceeds [`MAX_TRANSFER_LEN`]; nothing was sent.
    TooLong {
        /// Payload length in bytes.
        len: usize,
    },
    /// A framebuffer payload does not start burst-aligned; nothing was sent.
    Misaligned {
        /// Payload offset in the framebuffer, in bytes.
        offset: usize,
    },
    /// An earlier framebuffer transfer was leaked mid-flight with the SPI
    /// master; only a reboot recovers.
    Unavailable,
    /// The DMA fell behind the SPI during a framebuffer transfer.
    Underrun,
}

impl<P: fmt::Debug> fmt::Display for EspQspiError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spi(error) => write!(f, "spi transfer failed: {error:?}"),
            Self::ChipSelect(error) => write!(f, "chip select failed: {error:?}"),
            Self::TooLong { len } => write!(
                f,
                "framebuffer payload of {len} bytes exceeds {MAX_TRANSFER_LEN}"
            ),
            Self::Misaligned { offset } => write!(
                f,
                "framebuffer payload at offset {offset} is not {}-byte aligned",
                psram_tx_buf::BURST
            ),
            Self::Unavailable => write!(f, "spi master lost to a leaked transfer"),
            Self::Underrun => write!(f, "dma fell behind the spi"),
        }
    }
}

impl<P: fmt::Debug> core::error::Error for EspQspiError<P> {}

impl<P> Transient for EspQspiError<P> {
    /// Underruns, unknown SPI and chip-select failures may clear. Payloads
    /// the bus can never send, DMA setup errors (raised when a transfer
    /// starts, from its descriptors or buffer) and a lost SPI master
    /// cannot.
    fn is_transient(&self) -> bool {
        match self {
            Self::Spi(
                SpiError::DmaError(_)
                | SpiError::MaxDmaTransferSizeExceeded
                | SpiError::FifoSizeExeeded
                | SpiError::Unsupported,
            )
            | Self::TooLong { .. }
            | Self::Misaligned { .. }
            | Self::Unavailable => false,
            // `SpiError` is non-exhaustive: a variant added by an esp-hal
            // upgrade counts as transient until classified (ADR-0003).
            Self::Spi(_) | Self::ChipSelect(_) | Self::Underrun => true,
        }
    }
}

impl<P> From<FramePayloadError> for EspQspiError<P> {
    fn from(error: FramePayloadError) -> Self {
        match error {
            FramePayloadError::TooLong { len } => Self::TooLong { len },
            FramePayloadError::Misaligned { offset } => Self::Misaligned { offset },
        }
    }
}

/// SPI master and framebuffer descriptor chain, while no transfer runs.
type Idle<'d> = (SpiDma<'d, Async>, PsramTxBuf);

/// esp-hal 1.2.2's private SPI `MAX_DMA_SIZE` (`src/spi/master/dma.rs`):
/// re-check it on every esp-hal upgrade (ADR-0003).
const ESP_HAL_MAX_DMA_SIZE: usize = 32_736;

/// Longest payload of one transfer, a multiple of [`psram_tx_buf::BURST`].
///
/// It is esp-hal's DMA limit rounded down to whole bursts (32,704 bytes), so
/// transfers of a burst-aligned framebuffer area all start burst-aligned.
/// Keep in sync with `PANEL_TRANSFER` in
/// `crates/theo-drivers/src/framebuffer_tests.rs`.
pub const MAX_TRANSFER_LEN: usize =
    ESP_HAL_MAX_DMA_SIZE / psram_tx_buf::BURST * psram_tx_buf::BURST;
// Host tests use this value: keep `crates/theo-drivers/src/dma_layout_tests.rs`
// in sync.
const _: () = assert!(MAX_TRANSFER_LEN == 32_704);

/// Async esp-hal DMA SPI master in half-duplex mode with a software chip
/// select.
#[derive(Debug)]
pub struct EspQspi<'d, CS> {
    /// SPI master and framebuffer descriptor chain; `None` while a
    /// framebuffer transfer runs, and forever after one is leaked.
    bus: Option<Idle<'d>>,
    cs: CS,
    /// Addresses of the framebuffer, sent through the [`PsramTxBuf`].
    frame: Range<usize>,
}

impl<'d, CS: OutputPin> EspQspi<'d, CS> {
    /// Wraps the SPI2 DMA master, whose SIO0..3 lines are already routed.
    ///
    /// Payloads inside `frame` (a burst-aligned PSRAM framebuffer) are read
    /// in place through `psram_tx`. `spi` must be SPI2: the underrun check
    /// reads SPI2's registers.
    ///
    /// # Panics
    ///
    /// Panics if `psram_tx` cannot chain [`MAX_TRANSFER_LEN`] bytes or if
    /// `frame` is not burst-aligned.
    ///
    /// # Safety
    ///
    /// `frame` must be one allocation in PSRAM (the descriptors write back
    /// the PSRAM cache and claim PSRAM accesses) that stays allocated for
    /// the rest of the program (leaked). It is a raw pointer because the
    /// renderer holds the framebuffer as `&'static mut` meanwhile: only its
    /// address range is kept, never dereferenced. Leaking a write future (`mem::forget`) skips the guard that stops the
    /// DMA, which then keeps reading the framebuffer after the write's
    /// borrow ended; this keeps that read inside live memory. The CPU may
    /// still change the pixels meanwhile: the panel then shows wrong pixels,
    /// as with esp-hal's own in-place DMA, and the bus is lost
    /// ([`EspQspiError::Unavailable`]).
    pub unsafe fn new(
        spi: SpiDma<'d, Async>,
        cs: CS,
        psram_tx: PsramTxBuf,
        frame: *const [u8],
    ) -> Self {
        assert!(
            psram_tx.capacity() >= MAX_TRANSFER_LEN,
            "PsramTxBuf shorter than MAX_TRANSFER_LEN"
        );
        let start = frame.cast::<u8>() as usize;
        assert!(
            start.is_multiple_of(psram_tx_buf::BURST),
            "framebuffer not burst-aligned"
        );
        Self {
            bus: Some((spi, psram_tx)),
            cs,
            frame: start..start + frame.len(),
        }
    }

    /// Sends `source` with esp-hal's slice API (FIFO, DMA in place, or copy).
    ///
    /// Cancel-safe: esp-hal stops the DMA when the future is dropped, but
    /// does not wait for the SPI to go idle; `write`'s [`IdleOnDrop`] does,
    /// before chip select rises.
    async fn send(
        bus: &mut Option<Idle<'d>>,
        mode: DataMode,
        command: Command,
        address: Address,
        source: &[u8],
    ) -> Result<(), EspQspiError<CS::Error>> {
        let (spi, _) = bus.as_mut().ok_or(EspQspiError::Unavailable)?;
        spi.half_duplex_write_async(mode, command, address, 0, source)
            .await
            .map_err(EspQspiError::Spi)
    }

    /// Sends a framebuffer `source` through the [`PsramTxBuf`].
    ///
    /// Cancel-safe: dropping the future stops the DMA and puts the SPI
    /// master back ([`InFlight`]).
    async fn send_frame(
        bus: &mut Option<Idle<'d>>,
        mode: DataMode,
        command: Command,
        address: Address,
        source: &[u8],
    ) -> Result<(), EspQspiError<CS::Error>> {
        let (master, mut buffer) = bus.take().ok_or(EspQspiError::Unavailable)?;
        // SAFETY: `source` is borrowed for this whole call, and the transfer
        // below is owned by `InFlight`, which stops the DMA before this call
        // returns or its future is dropped. Leaking the future skips that:
        // `EspQspi::new`'s contract keeps the frame, which holds `source`,
        // allocated, so the DMA then only reads stale pixels.
        unsafe { buffer.set_source(source) };
        match master.half_duplex_write_buffer(mode, command, address, 0, source.len(), buffer) {
            Ok(transfer) => {
                let mut in_flight = InFlight {
                    transfer: Some(transfer),
                    slot: bus,
                };
                in_flight.wait_for_done().await;
                Ok(())
            }
            // esp-hal halts the channel on a failed start (descriptor
            // error); the next transfer resets it.
            Err((error, master, mut buffer)) => {
                buffer.clear_source();
                *bus = Some((master, buffer));
                Err(EspQspiError::Spi(error))
            }
        }
    }
}

/// Framebuffer transfer in flight, with the slot its SPI master returns to.
///
/// Dropping it, after completion or mid-transfer, stops the DMA and puts
/// the master and the cleared buffer back in the slot: a cancelled write
/// leaves the bus usable instead of losing the master.
struct InFlight<'a, 'd> {
    transfer: Option<SpiDmaTransfer<'d, Async, PsramTxBuf>>,
    slot: &'a mut Option<Idle<'d>>,
}

impl InFlight<'_, '_> {
    /// Waits for the transfer to complete.
    async fn wait_for_done(&mut self) {
        if let Some(transfer) = &mut self.transfer {
            transfer.wait_for_done().await;
        }
    }
}

impl Drop for InFlight<'_, '_> {
    fn drop(&mut self) {
        if let Some(mut transfer) = self.transfer.take() {
            // No-op once done; otherwise stops the DMA, which `wait` then
            // waits for (as `SpiDmaTransfer::drop` does).
            transfer.cancel();
            let (master, mut buffer) = transfer.wait();
            // esp-hal returns the buffer without `from_view`: forget the
            // source here, so the borrow of it cannot outlive the transfer.
            buffer.clear_source();
            *self.slot = Some((master, buffer));
        }
    }
}

impl<CS: OutputPin> QspiBus for EspQspi<'_, CS> {
    type Error = EspQspiError<CS::Error>;

    const MAX_TRANSFER_LEN: usize = MAX_TRANSFER_LEN;

    /// Cancel-safe: dropping the future mid-transfer releases chip select
    /// and keeps the SPI master; the panel then holds a partial transfer.
    async fn write(
        &mut self,
        opcode: u8,
        address: u32,
        lanes: Lanes,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        let mode = match lanes {
            Lanes::Single => DataMode::Single,
            Lanes::Quad => DataMode::Quad,
        };
        let command = Command::_8Bit(u16::from(opcode), DataMode::Single);
        let address = Address::_24Bit(address, DataMode::Single);
        let span = data.as_ptr_range();
        let frame_payload = frame_payload(
            self.frame.clone(),
            span.start as usize..span.end as usize,
            MAX_TRANSFER_LEN,
            psram_tx_buf::BURST,
        )?;
        if frame_payload {
            // esp-hal also clears it when the transfer starts; this keeps the
            // check correct without relying on that.
            underrun::clear();
        }
        let selected = ChipSelect::select(&mut self.cs).map_err(EspQspiError::ChipSelect)?;
        // Dropped in reverse order on cancellation: the transfer future
        // (stops the DMA), then `_idle` (waits for the SPI to finish its
        // last byte), then `selected` (raises chip select).
        let _idle = IdleOnDrop;
        let transferred = if frame_payload {
            Self::send_frame(&mut self.bus, mode, command, address, data).await
        } else {
            Self::send(&mut self.bus, mode, command, address, data).await
        };
        // Always release chip select. Errors are reported in this order:
        // SPI, then underrun, then chip select.
        let released = selected.release();
        transferred?;
        if frame_payload && underrun::happened() {
            return Err(EspQspiError::Underrun);
        }
        released.map_err(EspQspiError::ChipSelect)
    }
}

/// Waits for the display SPI (SPI2) to go idle when dropped (t-47).
///
/// A cancelled esp-hal slice transfer aborts the SPI without waiting for
/// it: chip select must not rise while the last byte is still clocked out.
struct IdleOnDrop;

impl Drop for IdleOnDrop {
    fn drop(&mut self) {
        // One aborted byte takes well under a microsecond at 40 MHz; the
        // bound only keeps a stuck peripheral from hanging the executor.
        for _ in 0..10_000 {
            if !spi_busy() {
                return;
            }
            core::hint::spin_loop();
        }
    }
}

/// Tells whether SPI2 is still running a transaction (esp-hal's `busy`).
fn spi_busy() -> bool {
    esp_hal::peripherals::SPI2::regs()
        .cmd()
        .read()
        .usr()
        .bit_is_set()
}

/// "DMA TX slower than SPI" flag of the display SPI (SPI2), latched by the
/// hardware: the SPI ran out of data mid-transfer. It writes only this
/// write-1-to-clear bit, so the flags the SPI2 `SpiDma` uses are untouched.
mod underrun {
    use esp_hal::peripherals::SPI2;

    /// Clears the flag before a transfer.
    pub fn clear() {
        SPI2::regs()
            .dma_int_clr()
            .write(|w| w.dma_outfifo_empty_err().clear_bit_by_one());
    }

    /// Tells whether the SPI ran out of DMA data since [`clear`].
    pub fn happened() -> bool {
        SPI2::regs()
            .dma_int_raw()
            .read()
            .dma_outfifo_empty_err()
            .bit_is_set()
    }
}
