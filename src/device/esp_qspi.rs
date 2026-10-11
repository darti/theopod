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

use core::{fmt, ops::Range};

use embedded_hal::digital::OutputPin;
use esp_hal::{
    Async,
    spi::{
        Error as SpiError,
        master::{Address, Command, DataMode, SpiDma},
    },
};
use theo_drivers::{Lanes, QspiBus};

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
    /// An earlier transfer was cancelled mid-flight and took the SPI master.
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
            Self::Unavailable => write!(f, "spi master lost to a cancelled transfer"),
            Self::Underrun => write!(f, "dma fell behind the spi"),
        }
    }
}

impl<P: fmt::Debug> core::error::Error for EspQspiError<P> {}

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

/// Async esp-hal DMA SPI master in half-duplex mode with a software chip
/// select.
#[derive(Debug)]
pub struct EspQspi<'d, CS> {
    /// SPI master and framebuffer descriptor chain; `None` only after a
    /// cancelled framebuffer transfer, which consumes both.
    bus: Option<(SpiDma<'d, Async>, PsramTxBuf)>,
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
    pub fn new(spi: SpiDma<'d, Async>, cs: CS, psram_tx: PsramTxBuf, frame: &[u8]) -> Self {
        assert!(
            psram_tx.capacity() >= MAX_TRANSFER_LEN,
            "PsramTxBuf shorter than MAX_TRANSFER_LEN"
        );
        let frame = frame.as_ptr_range();
        assert!(
            (frame.start as usize).is_multiple_of(psram_tx_buf::BURST),
            "framebuffer not burst-aligned"
        );
        Self {
            bus: Some((spi, psram_tx)),
            cs,
            frame: frame.start as usize..frame.end as usize,
        }
    }

    /// Tells whether `data` lies in the framebuffer, checking it can be sent.
    ///
    /// # Errors
    ///
    /// Returns [`EspQspiError::TooLong`] or [`EspQspiError::Misaligned`] for
    /// a framebuffer payload the [`PsramTxBuf`] cannot send.
    fn is_frame_payload(&self, data: &[u8]) -> Result<bool, EspQspiError<CS::Error>> {
        let span = data.as_ptr_range();
        let (start, end) = (span.start as usize, span.end as usize);
        if data.is_empty() || start < self.frame.start || end > self.frame.end {
            return Ok(false);
        }
        if data.len() > MAX_TRANSFER_LEN {
            return Err(EspQspiError::TooLong { len: data.len() });
        }
        if !start.is_multiple_of(psram_tx_buf::BURST) {
            return Err(EspQspiError::Misaligned {
                offset: start - self.frame.start,
            });
        }
        Ok(true)
    }

    /// Sends `source` with esp-hal's slice API (FIFO, DMA in place, or copy).
    async fn send(
        bus: &mut Option<(SpiDma<'d, Async>, PsramTxBuf)>,
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
    /// Cancelling the future stops the DMA but loses the SPI master: later
    /// transfers fail with [`EspQspiError::Unavailable`].
    async fn send_frame(
        bus: &mut Option<(SpiDma<'d, Async>, PsramTxBuf)>,
        mode: DataMode,
        command: Command,
        address: Address,
        source: &[u8],
    ) -> Result<(), EspQspiError<CS::Error>> {
        let (master, mut buffer) = bus.take().ok_or(EspQspiError::Unavailable)?;
        // SAFETY: `source` is borrowed for this whole call, and the transfer
        // below is either waited on or dropped here; dropping it cancels the
        // DMA and waits for it to stop (`SpiDmaTransfer::drop`). Leaking the
        // `write` future (`mem::forget`) is the accepted gap: the DMA only
        // reads, so the worst case is wrong pixels on the panel.
        unsafe { buffer.set_source(source) };
        let (result, master, mut buffer) = match master.half_duplex_write_buffer(
            mode,
            command,
            address,
            0,
            source.len(),
            buffer,
        ) {
            Ok(mut transfer) => {
                transfer.wait_for_done().await;
                let (master, buffer) = transfer.wait();
                (Ok(()), master, buffer)
            }
            // A failed start leaves no transfer running: the next one
            // resets the channel.
            Err((error, master, buffer)) => (Err(EspQspiError::Spi(error)), master, buffer),
        };
        // esp-hal returns the buffer without `from_view`: forget the source
        // here, so the borrow of `source` cannot outlive this call.
        buffer.clear_source();
        *bus = Some((master, buffer));
        result
    }
}

impl<CS: OutputPin> QspiBus for EspQspi<'_, CS> {
    type Error = EspQspiError<CS::Error>;

    const MAX_TRANSFER_LEN: usize = MAX_TRANSFER_LEN;

    /// Not cancel-safe: dropping the future mid-transfer leaves chip
    /// select asserted, and mid-framebuffer-transfer loses the SPI master.
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
        let frame_payload = self.is_frame_payload(data)?;
        if frame_payload {
            // esp-hal also clears it when the transfer starts; this keeps the
            // check correct without relying on that.
            underrun::clear();
        }
        self.cs.set_low().map_err(EspQspiError::ChipSelect)?;
        let transferred = if frame_payload {
            Self::send_frame(&mut self.bus, mode, command, address, data).await
        } else {
            Self::send(&mut self.bus, mode, command, address, data).await
        };
        // Always release chip select. Errors are reported in this order:
        // SPI, then underrun, then chip select.
        let released = self.cs.set_high();
        transferred?;
        if frame_payload && underrun::happened() {
            return Err(EspQspiError::Underrun);
        }
        released.map_err(EspQspiError::ChipSelect)
    }
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
