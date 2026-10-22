//! Device support for the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.
//!
//! - [`entry`]: firmware entry point attribute.
//! - [`Board`]: chip runtime and board peripherals, ready to use.
//! - [`Display`]: concrete display type of this board, owning the PSRAM
//!   framebuffer; implements [`theo_drivers::Panel`].
//! - [`Button`]: the BOOT push button, debounced.
//! - [`DeepSleep`]: stops the chip until the BOOT button is pressed.
//! - [`Touch`]: the CST820 touch controller, with the expander that resets
//!   it; [`TouchError`], [`TouchResetError`]: its failures.
//! - [`Expander`]: the TCA9554, shared by its runtime users.
//! - [`Imu`], [`IMU_AXES`]: the QMI8658 accelerometer, and how its axes lie
//!   on the panel.
//! - [`I2cBus`], [`I2cDevice`]: the async I2C bus shared by the TCA9554
//!   expander and the touch controller, and one device's handle on it.
//! - [`log_heap_usage`]: logs internal RAM and PSRAM heap usage.
//! - [`Watchdog`], [`WATCHDOG_TIMEOUT_MS`]: the main watchdog the UI loop
//!   feeds; [`log_start_cause`]: logs the reset or wake cause.
//! - [`display_resets`], [`reset_for_display`], [`clear_display_resets`]:
//!   reboots for a broken display, counted across core resets.
//! - [`clear_watchdog_resets_if_healthy`]: forgets watchdog reboots in a
//!   row; `Board::init` counts them and stops at the cap.
//! - `dump_frame` (feature `frame-dump`): prints the framebuffer to the
//!   serial log.
//!
//! This is the only module that depends on esp-hal, the ESP runtime crates,
//! pin assignments and panel geometry. Porting to another board means
//! providing another `device` module with the same items, where `Display`
//! implements [`theo_drivers::Panel`].

mod esp_qspi;
mod expander;
mod i2c_bus;
mod psram_tx_buf;
mod restart;
mod sleep;

use core::fmt;

use esp_backtrace as _;

use allocator_api2::{boxed::Box, vec};
use embassy_sync::mutex::Mutex;
use embassy_time::{Delay, Instant};
use esp_alloc::ExternalMemory;
use esp_hal::{
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    peripherals::GPIO0,
    psram::{PsramConfig, PsramMode, SpiRamFreq},
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use static_cell::StaticCell;

pub use self::expander::{Expander, Touch};
#[expect(
    unused_imports,
    reason = "named in `Touch`'s signatures and the docs; callers (`input`) only infer them"
)]
pub use self::expander::{TouchError, TouchResetError};
pub use self::i2c_bus::{I2cBus, I2cDevice};
pub use self::restart::{
    WATCHDOG_TIMEOUT_MS, Watchdog, clear_display_resets, clear_watchdog_resets_if_healthy,
    display_resets, log_start_cause, reset_for_display,
};
pub use self::sleep::DeepSleep;
use self::{esp_qspi::EspQspi, psram_tx_buf::PsramTxBuf};
use embedded_hal::digital::PinState;
use theo_drivers::{
    Axis, AxisMap, Co5300, Cst820, DebouncedButton, ExpanderPin, PanelGeometry, PanelPower,
    PanelSize, Qmi8658, StartAction, Tca9554, Transient, check_pattern_with, flush_rows,
};

pub use esp_hal::main as entry;

esp_bootloader_esp_idf::esp_app_desc!();

/// Internal RAM heap; the framebuffer lives in PSRAM instead.
const HEAP_SIZE: usize = 64 * 1024;
/// The ESP32-S3R8 module carries 8 MB of octal PSRAM; auto-detection is
/// less reliable than naming the mode.
const PSRAM_MODE: PsramMode = PsramMode::OctalSpi;
/// Twice esp-hal's 40 MHz default: at 40 MHz the display DMA, sharing PSRAM
/// with the CPU caches, falls behind the SPI, even with 64-byte bursts and
/// aligned transfers (ADR-0004, t-27). `Board::init` pattern-tests the
/// framebuffer region, as esp-hal does no timing tuning at 80 MHz.
const PSRAM_FREQUENCY: SpiRamFreq = SpiRamFreq::Freq80m;

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
/// CST820 I2C address (Waveshare `board_variant.c`, probed as CST816).
const TOUCH_ADDRESS: u8 = 0x15;
/// QMI8658 I2C addresses, in probe order: Waveshare's Arduino examples use
/// 0x6B, its ESP-IDF example probes both.
const IMU_ADDRESSES: [u8; 2] = [0x6B, 0x6A];
/// Accelerometer axes on the panel. Not documented by Waveshare: measured
/// on the board (`just imu-calibrate`, and rotation logs checked against
/// what the screen showed). Sensor +X points to the panel's top edge, +Y to
/// its right edge and +Z into the screen.
pub const IMU_AXES: AxisMap = AxisMap {
    right: Axis::Y,
    down: Axis::NegX,
    out: Axis::NegZ,
};
// A typo here would compile and turn the view the wrong way.
const _: () = assert!(
    IMU_AXES.is_rotation(),
    "IMU_AXES must use each sensor axis once and not mirror"
);

/// 368 x 448 AMOLED panel, first visible column 16 in CO5300 RAM. Keep in
/// sync with `PANEL` in `crates/theo-drivers/src/framebuffer_tests.rs`.
const DISPLAY_GEOMETRY: PanelGeometry = PanelGeometry {
    width: 368,
    height: 448,
    column_offset: 16,
};
// `flush_rows` starts transfers on even rows: two rows must be whole PSRAM
// bursts, or `EspQspi` refuses framebuffer transfers as misaligned.
const _: () = assert!(
    (2 * DISPLAY_GEOMETRY.width as usize * size_of::<u16>()).is_multiple_of(psram_tx_buf::BURST),
    "two panel rows must be a multiple of the PSRAM burst"
);
const DISPLAY_SPI_FREQUENCY_MHZ: u32 = 40;
/// Shorter transfers (register writes) go through the 64-byte SPI FIFO,
/// skipping the DMA setup cost.
const DISPLAY_DMA_MIN_TRANSFER_LEN: usize = 64;
/// Contact bounce of the BOOT button outlasts this.
const BUTTON_DEBOUNCE_MS: u32 = 20;

/// CO5300 panel on this board's QSPI bus.
pub type Display = Co5300<EspQspi<'static, Output<'static>>>;

/// BOOT button on GPIO0 (pulled up, low while pressed).
///
/// A press held at start-up, such as the one that woke the chip from
/// [`DeepSleep`], only reports its release.
pub type Button = DebouncedButton<Input<'static>, Delay>;

/// QMI8658 accelerometer, used for the screen orientation.
///
/// Power it down with [`Qmi8658::power_down`] before deep sleep.
pub type Imu = Qmi8658<I2cDevice>;

/// Board peripherals initialized and ready to use.
pub struct Board {
    /// Initialized and powered AMOLED panel.
    pub display: Display,
    /// BOOT push button, in sync with its level at start-up.
    pub button: Button,
    /// Deep sleep, woken by the BOOT button.
    pub deep_sleep: DeepSleep,
    /// Touch controller; reset and with its interrupts enabled unless its
    /// init failed (logged).
    pub touch: Touch,
    /// Accelerometer, started; `None` if no QMI8658 answered (logged).
    pub imu: Option<Imu>,
    /// Armed main watchdog: the UI loop must feed it.
    pub watchdog: Watchdog,
}

impl fmt::Debug for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Board")
            .field("display", &self.display)
            .field("button", &self.button)
            .field("deep_sleep", &self.deep_sleep)
            .field("touch", &self.touch)
            .field("imu", &self.imu.is_some())
            .field("watchdog", &self.watchdog)
            .finish()
    }
}

impl Board {
    /// Starts the chip, heaps, logger and scheduler, then brings up the
    /// display, its PSRAM framebuffer, the touch controller and the IMU.
    ///
    /// The expander then becomes the shared [`Expander`], so runtime
    /// read-modify-writes cannot race (ADR-0005).
    ///
    /// # Stops
    ///
    /// Never returns at the third watchdog reset since the UI loop last ran
    /// healthy: it parks the board and sleeps until BOOT instead
    /// (`stop_after_watchdog_resets`, ADR-0009).
    ///
    /// # Panics
    ///
    /// Panics if a bring-up step fails: the board cannot run without it.
    /// A touch controller or IMU that fails to start is only logged: the
    /// display and the BOOT button still work.
    pub async fn init() -> Self {
        esp_alloc::heap_allocator!(size: HEAP_SIZE);
        esp_println::logger::init_logger_from_env();
        let peripherals = esp_hal::init(esp_hal::Config::default());
        // Registered after the internal heap, so general allocations only
        // spill into PSRAM once internal RAM is full.
        let psram_config = PsramConfig {
            mode: PSRAM_MODE,
            ram_frequency: PSRAM_FREQUENCY,
            ..PsramConfig::default()
        };
        esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram, psram_config);
        let timg0 = TimerGroup::new(peripherals.TIMG0);
        // Armed as soon as it exists: the display bring-up waits on DMA
        // with no timeout of its own, and a panic from here on reboots.
        let mut watchdog = Watchdog::arm(timg0.wdt);
        // Counted at once, so a hang anywhere in the bring-up is capped; the
        // stop itself waits for the expander, below (t-66).
        let start = restart::count_watchdog_start();
        esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

        // I2C0: SCL on GPIO14, SDA on GPIO15 (Waveshare `pin_config.h`),
        // freed first (t-84).
        let i2c = i2c_bus::init(peripherals.I2C0, peripherals.GPIO14, peripherals.GPIO15);
        let mut expander = Tca9554::new(I2cDevice::new(i2c), TCA9554_ADDRESS);
        let deep_sleep = DeepSleep::new(peripherals.LPWR);
        // Before the panel bring-up, which a hang may be in.
        let park = match start {
            StartAction::Run => None,
            StartAction::Stop => Some(true),
            StartAction::StopWithoutParking => Some(false),
        };
        if let Some(park) = park {
            let boot = peripherals.GPIO0;
            stop_after_watchdog_resets(park, &mut expander, i2c, watchdog, deep_sleep, boot).await;
        }
        PANEL_POWER
            .power_up(&mut expander, &mut Delay)
            .await
            .unwrap();
        // After the panel rails, as Waveshare's `board_variant.c` does.
        let touch_interrupt = Input::new(
            peripherals.GPIO21,
            InputConfig::default().with_pull(Pull::Up),
        );
        let mut controller = Cst820::new(
            I2cDevice::new(i2c),
            TOUCH_ADDRESS,
            touch_interrupt,
            Delay,
            PanelSize {
                width: DISPLAY_GEOMETRY.width,
                height: DISPLAY_GEOMETRY.height,
            },
        );
        if let Err(error) = controller.init(&mut expander, TOUCH_RESET).await {
            log::error!("touch controller init failed: {error}");
        }
        static EXPANDER: StaticCell<Expander> = StaticCell::new();
        let touch = Touch::new(controller, EXPANDER.init(Mutex::new(expander)));
        let imu = start_imu(i2c).await;

        let pixels = usize::from(DISPLAY_GEOMETRY.width) * usize::from(DISPLAY_GEOMETRY.height);
        // Burst-aligned, so the display DMA reads it in whole bursts
        // (`PsramTxBuf`); the padding leaves room to align the start.
        let frame = {
            const PAD: usize = psram_tx_buf::BURST / size_of::<u16>();
            let raw = Box::leak(vec![in ExternalMemory; 0u16; pixels + PAD].into_boxed_slice());
            let skip = raw.as_ptr().align_offset(psram_tx_buf::BURST);
            assert!(skip < PAD, "framebuffer cannot be burst-aligned");
            &mut raw[skip..skip + pixels]
        };
        // PSRAM runs at 80 MHz without timing tuning (ADR-0004): fail here
        // rather than corrupt data silently (t-27). This region stands in
        // for all of PSRAM, the heap spill included.
        let started = Instant::now();
        // Whole 32-bit words: two pixels per access, the frame is 64-byte
        // aligned. Each pattern is pushed past the data cache before it is
        // checked, so the check reads PSRAM itself.
        let words: &mut [u32] = bytemuck::cast_slice_mut(frame);
        let sync = |words: &mut [u32]| {
            psram_tx_buf::write_back_and_invalidate(bytemuck::cast_slice(words));
        };
        if let Err(mismatch) = check_pattern_with(words, sync) {
            panic!("PSRAM self-test failed: {mismatch:?}");
        }
        log::info!(
            "PSRAM self-test passed in {} ms",
            started.elapsed().as_millis()
        );

        // Descriptors for framebuffer transfers: static, in internal RAM.
        let (_, descriptors) = esp_hal::dma_descriptors_chunk_size!(
            0,
            esp_qspi::MAX_TRANSFER_LEN,
            psram_tx_buf::CHUNK
        );
        let cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
        // SPI2: `EspQspi`'s underrun check reads SPI2's registers.
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
        // The display owns the framebuffer from here on: the renderer
        // borrows it through `Panel::frame_mut` (t-46).
        let frame: &'static mut [u8] = bytemuck::cast_slice_mut(frame);
        let bus = EspQspi::new(spi, cs, PsramTxBuf::new(descriptors), frame);
        let mut display = Co5300::new(bus, DISPLAY_GEOMETRY).unwrap();
        display.init(&mut Delay).await.unwrap();
        check_display(&mut display).await;

        let boot = Input::new(
            peripherals.GPIO0,
            InputConfig::default().with_pull(Pull::Up),
        );
        let mut button = DebouncedButton::new(boot, Delay, PinState::Low, BUTTON_DEBOUNCE_MS);
        // Still held after the press that woke the chip: report only its
        // release, so it neither clicks nor counts as a long press.
        match button.sync_state() {
            Ok(true) => log::info!("BOOT held at start-up, waiting for its release"),
            Ok(false) => {}
            Err(never) => match never {},
        }

        // Bring-up took well under the timeout; the UI loop's first pass
        // feeds it next, after the Slint setup in `main`.
        watchdog.feed();
        #[cfg(feature = "watchdog-check")]
        restart::check_watchdog_reset();
        #[cfg(feature = "watchdog-loop")]
        restart::hang_until_watchdog();

        log_heap_usage();
        Self {
            display,
            button,
            deep_sleep,
            touch,
            imu,
            watchdog,
        }
    }
}

/// Clears the panel and checks a full-length transfer goes through.
///
/// Sends the display's zeroed framebuffer, whose first transfer is
/// [`esp_qspi::MAX_TRANSFER_LEN`] long.
/// `MAX_TRANSFER_LEN` mirrors a private esp-hal limit (ADR-0003): if an
/// upgrade lowers it, every framebuffer flush would fail for good, so this
/// fails at boot instead.
///
/// # Panics
///
/// Panics on an error that cannot clear on retry; transient ones are
/// logged, and the first frame resends every row anyway.
async fn check_display(display: &mut Display) {
    const _: () = assert!(
        DISPLAY_GEOMETRY.width as usize * DISPLAY_GEOMETRY.height as usize * size_of::<u16>()
            >= esp_qspi::MAX_TRANSFER_LEN,
        "a full frame must include a MAX_TRANSFER_LEN transfer"
    );
    let rows = 0..DISPLAY_GEOMETRY.height;
    if let Err(error) = flush_rows(display, rows).await {
        if error.is_transient() {
            log::warn!("display check failed, will retry: {error}");
        } else {
            panic!("display cannot send a full transfer: {error}");
        }
    }
}

/// Parks the board if `park`, then sleeps until BOOT, after watchdog reboots.
///
/// Runs early in [`Board::init`], before the panel bring-up. A watchdog
/// reset keeps the expander's pins, so the panel rails may still be on.
/// Parking follows a normal stop (ADR-0008) without the panel's sleep-in,
/// as QSPI is not up yet: the IMU powers down (written at the first
/// address that acknowledges, without the probe `start_imu` does), the
/// touch controller is held in reset, and the rails are cut last. Each
/// step is best effort and bounded by the I2C timeout, under the armed
/// watchdog: if parking hangs anyway, the next start skips it (`park`
/// false), and the rails may then stay on through the sleep. The count is
/// cleared and the watchdog disarmed just before the sleep; a press of
/// BOOT starts the device afresh.
async fn stop_after_watchdog_resets(
    park: bool,
    expander: &mut Tca9554<I2cDevice>,
    i2c: &'static I2cBus,
    watchdog: Watchdog,
    deep_sleep: DeepSleep,
    boot: GPIO0<'static>,
) -> ! {
    if park {
        let mut imu_down = false;
        for address in IMU_ADDRESSES {
            let mut imu = Qmi8658::new(I2cDevice::new(i2c), address);
            if imu.power_down().await.is_ok() {
                imu_down = true;
                break;
            }
        }
        if !imu_down {
            log::error!("IMU power-down failed before the stop");
        }
        // No `Cst820` yet: what its `hold_in_reset` does.
        if let Err(error) = expander::park_touch_and_panel(expander).await {
            log::error!("panel power-down failed before the stop: {error:?}");
        }
    }
    restart::clear_watchdog_resets();
    watchdog.disarm();
    let boot = Input::new(boot, InputConfig::default().with_pull(Pull::Up));
    deep_sleep.enter(boot)
}

/// Starts the QMI8658 at the first of [`IMU_ADDRESSES`] that answers.
async fn start_imu(i2c: &'static I2cBus) -> Option<Imu> {
    for address in IMU_ADDRESSES {
        let mut imu = Qmi8658::new(I2cDevice::new(i2c), address);
        match imu.init(&mut Delay).await {
            Ok(()) => {
                log::info!("IMU found at {address:#04x}");
                return Some(imu);
            }
            Err(error) => log::info!("no IMU at {address:#04x}: {error}"),
        }
    }
    log::error!("IMU not found: the screen will not rotate");
    None
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
