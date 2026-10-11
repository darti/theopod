//! Device support for the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.
//!
//! - [`entry`]: firmware entry point attribute.
//! - [`Board`]: chip runtime and board peripherals, ready to use.
//! - [`Display`]: concrete display type of this board; implements
//!   [`theo_drivers::Panel`].
//! - [`Button`]: the BOOT push button, debounced.
//! - [`log_heap_usage`]: logs internal RAM and PSRAM heap usage.
//! - `dump_frame` (feature `frame-dump`): prints the framebuffer to the
//!   serial log.
//!
//! This is the only module that depends on esp-hal, the ESP runtime crates,
//! pin assignments and panel geometry. Porting to another board means
//! providing another `device` module with the same items, where `Display`
//! implements [`theo_drivers::Panel`].

mod esp_qspi;

use core::fmt;

use esp_backtrace as _;

use allocator_api2::{boxed::Box, vec};
use embassy_time::Delay;
use esp_alloc::ExternalMemory;
use esp_hal::{
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    i2c::master::{Config as I2cConfig, I2c},
    psram::{PsramConfig, PsramMode},
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};

use self::esp_qspi::EspQspi;
use embedded_hal::digital::PinState;
use theo_drivers::{Co5300, DebouncedButton, PanelGeometry, PanelPower, Tca9554};

pub use esp_hal::main as entry;

esp_bootloader_esp_idf::esp_app_desc!();

/// Internal RAM heap; the framebuffer lives in PSRAM instead.
const HEAP_SIZE: usize = 64 * 1024;
/// The ESP32-S3R8 module carries 8 MB of octal PSRAM; auto-detection is
/// less reliable than naming the mode.
const PSRAM_MODE: PsramMode = PsramMode::OctalSpi;

const TCA9554_ADDRESS: u8 = 0x20;
/// EXIO0..2 drive the panel rails/reset.
const PANEL_POWER: PanelPower = PanelPower {
    pins: 0x07,
    settle_ms: 100,
    reset_ms: 300,
    power_up_ms: 300,
};
/// 368 x 448 AMOLED panel, first visible column 16 in CO5300 RAM.
const DISPLAY_GEOMETRY: PanelGeometry = PanelGeometry {
    width: 368,
    height: 448,
    column_offset: 16,
};
const DISPLAY_SPI_FREQUENCY_MHZ: u32 = 40;
/// Shorter transfers (register writes) go through the 64-byte SPI FIFO,
/// skipping the DMA setup cost.
const DISPLAY_DMA_MIN_TRANSFER_LEN: usize = 64;
/// Contact bounce of the BOOT button outlasts this.
const BUTTON_DEBOUNCE_MS: u32 = 20;

/// CO5300 panel on this board's QSPI bus.
pub type Display = Co5300<EspQspi<'static, Output<'static>>>;

/// BOOT button on GPIO0 (pulled up, low while pressed).
pub type Button = DebouncedButton<Input<'static>, Delay>;

/// Board peripherals initialized and ready to use.
pub struct Board {
    /// Initialized and powered AMOLED panel.
    pub display: Display,
    /// BOOT push button.
    pub button: Button,
    /// Zeroed framebuffer in PSRAM: one pixel per display pixel, row-major,
    /// sized from [`Display`]'s geometry. Each `u16` slot holds big-endian
    /// RGB565 bytes (the panel's order), not a native-endian value.
    pub frame: &'static mut [u16],
}

impl fmt::Debug for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Board")
            .field("display", &self.display)
            .field("button", &self.button)
            .field("frame_pixels", &self.frame.len())
            .finish()
    }
}

impl Board {
    /// Starts the chip, heaps, logger and scheduler, then brings up the
    /// display and allocates its framebuffer in PSRAM.
    ///
    /// # Panics
    ///
    /// Panics if a bring-up step fails: the board cannot run without it.
    pub async fn init() -> Self {
        esp_alloc::heap_allocator!(size: HEAP_SIZE);
        esp_println::logger::init_logger_from_env();
        let peripherals = esp_hal::init(esp_hal::Config::default());
        // Registered after the internal heap, so general allocations only
        // spill into PSRAM once internal RAM is full.
        let psram_config = PsramConfig {
            mode: PSRAM_MODE,
            ..PsramConfig::default()
        };
        esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram, psram_config);
        let timg0 = TimerGroup::new(peripherals.TIMG0);
        esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

        let mut i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
            .unwrap()
            .with_sda(peripherals.GPIO15)
            .with_scl(peripherals.GPIO14);
        let mut expander = Tca9554::new(&mut i2c, TCA9554_ADDRESS);
        PANEL_POWER
            .power_up(&mut expander, &mut Delay)
            .await
            .unwrap();

        // DMA source for the display (see `EspQspi`): a static buffer in
        // internal RAM, outside the heap. No receive buffer is needed.
        let (_, _, bounce, _) = esp_hal::dma_buffers!(0, esp_qspi::MAX_TRANSFER_LEN);
        let cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
        let spi = Spi::new(
            peripherals.SPI2,
            SpiConfig::default()
                .with_frequency(Rate::from_mhz(DISPLAY_SPI_FREQUENCY_MHZ))
                .with_mode(Mode::_0)
                .with_min_async_transfer_size(DISPLAY_DMA_MIN_TRANSFER_LEN),
        )
        .unwrap()
        .with_sck(peripherals.GPIO11)
        .with_sio0(peripherals.GPIO4)
        .with_sio1(peripherals.GPIO5)
        .with_sio2(peripherals.GPIO6)
        .with_sio3(peripherals.GPIO7)
        .with_dma(peripherals.DMA_CH0)
        .into_async();
        let mut display = Co5300::new(EspQspi::new(spi, cs, bounce), DISPLAY_GEOMETRY).unwrap();
        display.init(&mut Delay).await.unwrap();

        let boot = Input::new(
            peripherals.GPIO0,
            InputConfig::default().with_pull(Pull::Up),
        );
        let button = DebouncedButton::new(boot, Delay, PinState::Low, BUTTON_DEBOUNCE_MS);

        let pixels = usize::from(DISPLAY_GEOMETRY.width) * usize::from(DISPLAY_GEOMETRY.height);
        let frame = Box::leak(vec![in ExternalMemory; 0u16; pixels].into_boxed_slice());

        log_heap_usage();
        Self {
            display,
            button,
            frame,
        }
    }
}

/// Logs used and free bytes of every heap region (internal RAM and PSRAM).
pub fn log_heap_usage() {
    log::info!("heap usage:\n{}", esp_alloc::HEAP.stats());
}

/// Prints `frame` (big-endian RGB565, `width` x `height`) to the serial log.
#[cfg(feature = "frame-dump")]
pub fn dump_frame((width, height): (u16, u16), frame: &[u8]) {
    log::info!("dumping {width} x {height} frame");
    if let Err(error) =
        theo_frame_dump::write_frame(&mut esp_println::Printer, width, height, frame)
    {
        log::error!("frame dump failed: {error}");
    }
}
