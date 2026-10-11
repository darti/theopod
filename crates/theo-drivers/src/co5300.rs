//! HAL-independent CO5300 AMOLED controller driver.
//!
//! - [`Co5300`]: initializes the panel and streams RGB565 pixels over any
//!   [`QspiBus`], with delays from any `embedded-hal-async` [`DelayNs`].
//! - [`PanelGeometry`]: visible size and RAM offset of the attached panel.
//! - [`Error`]: bus failures and rejected geometry, areas or pixel counts.
//!
//! The controller uses two opcodes: `0x02` writes a register with every phase
//! on SIO0 (1-1-1), and `0x32` writes pixels with only the data phase on
//! SIO0..3 (1-1-4). Drawing areas must start on an even coordinate and span
//! an even number of pixels on both axes; other areas are silently ignored by
//! the panel, so [`Co5300`] rejects them up front.

use core::{fmt, ops::Range};

use embedded_hal_async::delay::DelayNs;

use crate::{
    panel::Panel,
    qspi::{Lanes, QspiBus},
};

const OPCODE_WRITE_REGISTER: u8 = 0x02;
const OPCODE_WRITE_PIXELS: u8 = 0x32;

const CMD_SOFTWARE_RESET: u8 = 0x01;
const CMD_SLEEP_OUT: u8 = 0x11;
const CMD_INVERSION_OFF: u8 = 0x20;
const CMD_DISPLAY_ON: u8 = 0x29;
const CMD_COLUMN_ADDRESS: u8 = 0x2A;
const CMD_ROW_ADDRESS: u8 = 0x2B;
const CMD_MEMORY_WRITE: u8 = 0x2C;
const CMD_MEMORY_ACCESS_CONTROL: u8 = 0x36;
const CMD_PIXEL_FORMAT: u8 = 0x3A;
const CMD_MEMORY_WRITE_CONTINUE: u8 = 0x3C;
const CMD_BRIGHTNESS: u8 = 0x51;
const CMD_WRITE_CTRL_DISPLAY: u8 = 0x53;
const CMD_CONTRAST_ENHANCEMENT: u8 = 0x58;
const CMD_HBM_BRIGHTNESS: u8 = 0x63;
const CMD_SPI_MODE_CONTROL: u8 = 0xC4;
const CMD_PAGE_SELECT: u8 = 0xFE;

/// User command page.
const PAGE_USER: u8 = 0x00;
/// Enables SPI writes to display RAM.
const SPI_MODE_RAM_WRITE: u8 = 0x80;
/// RGB565, 16 bits per pixel.
const PIXEL_FORMAT_RGB565: u8 = 0x55;
/// Brightness control block on.
const CTRL_DISPLAY_BRIGHTNESS: u8 = 0x20;
const HBM_BRIGHTNESS_MAX: u8 = 0xFF;
const BRIGHTNESS: u8 = 0xD0;
const CONTRAST_ENHANCEMENT_OFF: u8 = 0x00;
/// Row/column order unchanged, RGB element order.
const MADCTL_RGB: u8 = 0x00;

const RESET_SETTLE_MS: u32 = 200;
const SLEEP_OUT_SETTLE_MS: u32 = 120;
const DISPLAY_ON_SETTLE_MS: u32 = 10;

/// Failure while talking to the CO5300.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// The bus transfer failed.
    Bus(E),
    /// The geometry is empty or odd-sized, has an odd offset, overflows u16
    /// column addresses, or exceeds `u32::MAX` bytes per frame.
    InvalidGeometry {
        /// The rejected geometry.
        geometry: PanelGeometry,
    },
    /// The area is empty, not aligned on 2x2 pixel blocks, or exceeds the panel.
    InvalidArea {
        /// Requested columns.
        x: Range<u16>,
        /// Requested rows.
        y: Range<u16>,
    },
    /// More pixel bytes than the open area holds, or an odd byte count.
    PixelOverflow {
        /// Bytes still expected by the open area.
        remaining: usize,
        /// Bytes submitted.
        submitted: usize,
    },
}

impl<E: fmt::Debug> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(error) => write!(f, "bus transfer failed: {error:?}"),
            Self::InvalidGeometry { geometry } => write!(f, "invalid panel geometry {geometry:?}"),
            Self::InvalidArea { x, y } => write!(f, "invalid area columns {x:?} rows {y:?}"),
            Self::PixelOverflow {
                remaining,
                submitted,
            } => write!(
                f,
                "pixel overflow: {submitted} bytes submitted, {remaining} expected"
            ),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for Error<E> {}

/// Visible size of the panel and its position in controller RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelGeometry {
    /// Visible width in pixels; must be even.
    pub width: u16,
    /// Visible height in pixels; must be even.
    pub height: u16,
    /// First visible RAM column; must be even.
    pub column_offset: u16,
}

impl PanelGeometry {
    fn is_valid(&self) -> bool {
        let even = |value: u16| value.is_multiple_of(2);
        even(self.width)
            && even(self.height)
            && even(self.column_offset)
            && self.width > 0
            && self.height > 0
            && self.column_offset.checked_add(self.width).is_some()
            && u32::from(self.width)
                .checked_mul(u32::from(self.height))
                .and_then(|pixels| pixels.checked_mul(2))
                .is_some()
    }
}

/// CO5300 panel on a QSPI bus.
#[derive(Debug)]
pub struct Co5300<B> {
    bus: B,
    geometry: PanelGeometry,
    /// Pixel bytes still expected by the open area.
    remaining: usize,
}

impl<B: QspiBus> Co5300<B> {
    /// Wraps the bus connected to a panel with the given `geometry`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGeometry`] if `geometry` cannot be addressed.
    pub fn new(bus: B, geometry: PanelGeometry) -> Result<Self, Error<B::Error>> {
        if !geometry.is_valid() {
            return Err(Error::InvalidGeometry { geometry });
        }
        Ok(Self {
            bus,
            geometry,
            remaining: 0,
        })
    }

    /// Releases the underlying bus.
    pub fn release(self) -> B {
        self.bus
    }

    /// Runs the Waveshare V2 power-up sequence and turns the panel on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bus`] on the first failed register write.
    pub async fn init(&mut self, delay: &mut impl DelayNs) -> Result<(), Error<B::Error>> {
        self.write_register(CMD_SOFTWARE_RESET, &[])?;
        delay.delay_ms(RESET_SETTLE_MS).await;
        self.write_register(CMD_SLEEP_OUT, &[])?;
        delay.delay_ms(SLEEP_OUT_SETTLE_MS).await;
        self.write_register(CMD_PAGE_SELECT, &[PAGE_USER])?;
        self.write_register(CMD_SPI_MODE_CONTROL, &[SPI_MODE_RAM_WRITE])?;
        self.write_register(CMD_PIXEL_FORMAT, &[PIXEL_FORMAT_RGB565])?;
        self.write_register(CMD_WRITE_CTRL_DISPLAY, &[CTRL_DISPLAY_BRIGHTNESS])?;
        self.write_register(CMD_HBM_BRIGHTNESS, &[HBM_BRIGHTNESS_MAX])?;
        self.write_register(CMD_DISPLAY_ON, &[])?;
        self.write_register(CMD_BRIGHTNESS, &[BRIGHTNESS])?;
        self.write_register(CMD_CONTRAST_ENHANCEMENT, &[CONTRAST_ENHANCEMENT_OFF])?;
        delay.delay_ms(DISPLAY_ON_SETTLE_MS).await;
        self.write_register(CMD_MEMORY_ACCESS_CONTROL, &[MADCTL_RGB])?;
        self.write_register(CMD_INVERSION_OFF, &[])
    }

    fn write_register(&mut self, command: u8, data: &[u8]) -> Result<(), Error<B::Error>> {
        self.transfer(OPCODE_WRITE_REGISTER, command, Lanes::Single, data)
    }

    /// Commands travel in the middle byte of the 24-bit address phase.
    fn transfer(
        &mut self,
        opcode: u8,
        command: u8,
        lanes: Lanes,
        data: &[u8],
    ) -> Result<(), Error<B::Error>> {
        self.bus
            .write(opcode, u32::from(command) << 8, lanes, data)
            .map_err(Error::Bus)
    }
}

impl<B: QspiBus> Panel for Co5300<B> {
    type Error = Error<B::Error>;

    fn size(&self) -> (u16, u16) {
        (self.geometry.width, self.geometry.height)
    }

    /// Opens the area; both ranges must start even and have an even length.
    fn begin_area(&mut self, x: Range<u16>, y: Range<u16>) -> Result<(), Self::Error> {
        let aligned = |range: &Range<u16>, limit: u16| {
            !range.is_empty()
                && range.start.is_multiple_of(2)
                && range.len().is_multiple_of(2)
                && range.end <= limit
        };
        if !aligned(&x, self.geometry.width) || !aligned(&y, self.geometry.height) {
            return Err(Error::InvalidArea { x, y });
        }

        // Cannot overflow: `x.end <= width` and `width + column_offset` fits in u16.
        let x_start = x.start + self.geometry.column_offset;
        let x_end = x.end - 1 + self.geometry.column_offset;
        let [xs_high, xs_low] = x_start.to_be_bytes();
        let [xe_high, xe_low] = x_end.to_be_bytes();
        let [ys_high, ys_low] = y.start.to_be_bytes();
        let [ye_high, ye_low] = (y.end - 1).to_be_bytes();
        self.remaining = 0;
        self.write_register(CMD_COLUMN_ADDRESS, &[xs_high, xs_low, xe_high, xe_low])?;
        self.write_register(CMD_ROW_ADDRESS, &[ys_high, ys_low, ye_high, ye_low])?;
        self.write_register(CMD_MEMORY_WRITE, &[])?;
        // Fits: bounded by width * height * 2, which `PanelGeometry` keeps within u32.
        self.remaining = x.len() * y.len() * 2;
        Ok(())
    }

    fn write_pixels(&mut self, pixels: &[u8]) -> Result<(), Self::Error> {
        if pixels.len() > self.remaining || !pixels.len().is_multiple_of(2) {
            return Err(Error::PixelOverflow {
                remaining: self.remaining,
                submitted: pixels.len(),
            });
        }
        self.transfer(
            OPCODE_WRITE_PIXELS,
            CMD_MEMORY_WRITE_CONTINUE,
            Lanes::Quad,
            pixels,
        )?;
        self.remaining -= pixels.len();
        Ok(())
    }
}
