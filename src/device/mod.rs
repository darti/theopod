//! Device support for the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.
//!
//! - [`entry`]: firmware entry point attribute.
//! - [`Board`]: chip runtime and board peripherals, ready to use.
//! - [`Display`]: concrete display type of this board; implements
//!   [`theo_drivers::Panel`].
//! - [`Button`]: the BOOT push button, debounced.
//! - [`I2cBus`], [`I2cDevice`]: the async I2C bus shared by the TCA9554
//!   expander and the touch controller, and one device's handle on it.
//! - [`log_heap_usage`]: logs internal RAM and PSRAM heap usage.
//! - `dump_frame` (feature `frame-dump`): prints the framebuffer to the
//!   serial log.
//!
//! This is the only module that depends on esp-hal, the ESP runtime crates,
//! pin assignments and panel geometry. Porting to another board means
//! providing another `device` module with the same items, where `Display`
//! implements [`theo_drivers::Panel`].

mod esp_qspi;
#[cfg(feature = "psram-direct")]
mod psram_cache;

use core::fmt;

use esp_backtrace as _;

use allocator_api2::{boxed::Box, vec};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice as SharedI2cDevice;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::Delay;
use esp_alloc::ExternalMemory;
use esp_hal::{
    Async,
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
use theo_drivers::{Co5300, DebouncedButton, ExpanderPin, PanelGeometry, PanelPower, Tca9554};

pub use esp_hal::main as entry;

esp_bootloader_esp_idf::esp_app_desc!();

/// Internal RAM heap; the framebuffer lives in PSRAM instead.
const HEAP_SIZE: usize = 64 * 1024;
/// The ESP32-S3R8 module carries 8 MB of octal PSRAM; auto-detection is
/// less reliable than naming the mode.
const PSRAM_MODE: PsramMode = PsramMode::OctalSpi;

const TCA9554_ADDRESS: u8 = 0x20;
/// EXIO0 is the panel reset, EXIO1 its power enable (Waveshare
/// `board_variant.c`: `LCD_RST`, `DSI_PWR_EN`).
const PANEL_POWER: PanelPower = PanelPower {
    pins: ExpanderPin::P0.mask() | ExpanderPin::P1.mask(),
    settle_ms: 100,
    reset_ms: 300,
    power_up_ms: 300,
};
/// Touch controller reset, active low (Waveshare `TOUCH_RST`).
const TOUCH_RESET: ExpanderPin = ExpanderPin::P2;
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

/// I2C0 on GPIO15 (SDA) / GPIO14 (SCL), locked per transaction.
///
/// The TCA9554 expander (0x20) and the touch controller share it.
pub type I2cBus = Mutex<CriticalSectionRawMutex, I2c<'static, Async>>;

/// One device's handle on [`I2cBus`]; implements `embedded-hal-async` I2C.
///
/// Create one per driver with `I2cDevice::new(board.i2c)`. Drive the
/// expander from a single task: its read-modify-writes are not atomic
/// across handles.
pub type I2cDevice = SharedI2cDevice<'static, CriticalSectionRawMutex, I2c<'static, Async>>;

/// Board peripherals initialized and ready to use.
pub struct Board {
    /// Initialized and powered AMOLED panel.
    pub display: Display,
    /// BOOT push button.
    pub button: Button,
    /// Shared I2C bus, for the touch controller and the expander.
    #[expect(dead_code, reason = "MVP: t-19 the touch driver reads it")]
    pub i2c: &'static I2cBus,
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
            // Not `self.i2c`: the field stays unread until t-19 (see its `expect`).
            .field("i2c", &format_args!("I2cBus"))
            .field("frame_pixels", &self.frame.len())
            .finish()
    }
}

impl Board {
    /// Starts the chip, heaps, logger and scheduler, then brings up the
    /// display and allocates its framebuffer in PSRAM.
    ///
    /// The touch controller is left out of reset, for its driver to pulse.
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

        let i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
            .unwrap()
            .with_sda(peripherals.GPIO15)
            .with_scl(peripherals.GPIO14)
            .into_async();
        let i2c: &'static I2cBus = Box::leak(Box::new(Mutex::new(i2c)));
        let mut expander = Tca9554::new(I2cDevice::new(i2c), TCA9554_ADDRESS);
        PANEL_POWER
            .power_up(&mut expander, &mut Delay)
            .await
            .unwrap();
        // After the panel rails, as Waveshare's `board_variant.c` does.
        // `configure_output` latches the level before the direction, so the
        // touch reset never glitches low; t-19 pulses it.
        expander
            .configure_output(TOUCH_RESET, PinState::High)
            .await
            .unwrap();

        let pixels = usize::from(DISPLAY_GEOMETRY.width) * usize::from(DISPLAY_GEOMETRY.height);
        let frame = Box::leak(vec![in ExternalMemory; 0u16; pixels].into_boxed_slice());

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
        let bus = EspQspi::new(spi, cs, bounce);
        #[cfg(feature = "psram-direct")]
        let bus = bus.with_in_place(bytemuck::cast_slice(frame));
        let mut display = Co5300::new(bus, DISPLAY_GEOMETRY).unwrap();
        display.init(&mut Delay).await.unwrap();

        let boot = Input::new(
            peripherals.GPIO0,
            InputConfig::default().with_pull(Pull::Up),
        );
        let button = DebouncedButton::new(boot, Delay, PinState::Low, BUTTON_DEBOUNCE_MS);

        log_heap_usage();
        Self {
            display,
            button,
            i2c,
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
    // The DMA view (t-4): drop cached lines so the dump reads PSRAM itself.
    // Lines the write-back lost stay lost, on screen and in the framebuffer.
    #[cfg(feature = "psram-direct")]
    psram_cache::invalidate_lines(frame);
    log::info!("dumping {width} x {height} frame");
    if let Err(error) =
        theo_frame_dump::write_frame(&mut esp_println::Printer, width, height, frame)
    {
        log::error!("frame dump failed: {error}");
    }
}
