//! Device support for the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.
//!
//! - [`entry`]: firmware entry point attribute.
//! - [`Board`]: chip runtime and board peripherals, ready to use.
//! - [`Display`]: concrete display type of this board; implements
//!   [`theo_drivers::Panel`].
//!
//! This is the only module that depends on esp-hal, the ESP runtime crates,
//! pin assignments and panel geometry. Porting to another board means
//! providing another `device` module with the same three items, where
//! `Display` implements [`theo_drivers::Panel`].

mod esp_qspi;

use esp_backtrace as _;

use embassy_time::Delay;
use embedded_hal::i2c::I2c as I2cBus;
use embedded_hal_async::delay::DelayNs;
use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};

use self::esp_qspi::EspQspi;
use theo_drivers::{Co5300, PanelGeometry, Tca9554};

pub use esp_hal::main as entry;

esp_bootloader_esp_idf::esp_app_desc!();

const HEAP_SIZE: usize = 64 * 1024;

const TCA9554_ADDRESS: u8 = 0x20;
/// EXIO0..2 drive the panel rails/reset.
const PANEL_POWER_PINS: u8 = 0x07;
/// Rails/reset held high before the reset pulse.
const PANEL_POWER_SETTLE_MS: u32 = 100;
/// Rails/reset held low.
const PANEL_RESET_PULSE_MS: u32 = 300;
/// Wait after release before talking to the panel.
const PANEL_POWER_UP_MS: u32 = 300;
/// 368 x 448 AMOLED panel, first visible column 16 in CO5300 RAM.
const DISPLAY_GEOMETRY: PanelGeometry = PanelGeometry {
    width: 368,
    height: 448,
    column_offset: 16,
};
const DISPLAY_SPI_FREQUENCY_MHZ: u32 = 40;

/// CO5300 panel on this board's QSPI bus.
pub type Display = Co5300<EspQspi<'static, Output<'static>>>;

/// Board peripherals initialized and ready to use.
#[derive(Debug)]
pub struct Board {
    /// Initialized and powered AMOLED panel.
    pub display: Display,
}

impl Board {
    /// Starts the chip, heap, logger and scheduler, then brings up the display.
    ///
    /// # Panics
    ///
    /// Panics if a bring-up step fails: the board cannot run without it.
    pub async fn init() -> Self {
        esp_alloc::heap_allocator!(size: HEAP_SIZE);
        esp_println::logger::init_logger_from_env();
        let peripherals = esp_hal::init(esp_hal::Config::default());
        let timg0 = TimerGroup::new(peripherals.TIMG0);
        esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

        let mut i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
            .unwrap()
            .with_sda(peripherals.GPIO15)
            .with_scl(peripherals.GPIO14);
        power_up_panel(&mut i2c, &mut Delay).await.unwrap();

        let cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
        let spi = Spi::new(
            peripherals.SPI2,
            SpiConfig::default()
                .with_frequency(Rate::from_mhz(DISPLAY_SPI_FREQUENCY_MHZ))
                .with_mode(Mode::_0),
        )
        .unwrap()
        .with_sck(peripherals.GPIO11)
        .with_sio0(peripherals.GPIO4)
        .with_sio1(peripherals.GPIO5)
        .with_sio2(peripherals.GPIO6)
        .with_sio3(peripherals.GPIO7);
        let mut display = Co5300::new(EspQspi::new(spi, cs), DISPLAY_GEOMETRY).unwrap();
        display.init(&mut Delay).await.unwrap();

        Self { display }
    }
}

/// Pulses the panel rails/reset low through the expander, then leaves them high.
async fn power_up_panel<I: I2cBus>(i2c: &mut I, delay: &mut impl DelayNs) -> Result<(), I::Error> {
    let mut expander = Tca9554::new(i2c, TCA9554_ADDRESS);
    // Latch high levels before switching to outputs so the pins never glitch low.
    expander.write_levels(PANEL_POWER_PINS)?;
    expander.set_outputs(PANEL_POWER_PINS)?;
    delay.delay_ms(PANEL_POWER_SETTLE_MS).await;
    expander.write_levels(0x00)?;
    delay.delay_ms(PANEL_RESET_PULSE_MS).await;
    expander.write_levels(PANEL_POWER_PINS)?;
    delay.delay_ms(PANEL_POWER_UP_MS).await;
    Ok(())
}
