//! Device support for the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.
//!
//! - [`entry`]: firmware entry point attribute.
//! - [`Board`]: chip runtime and board peripherals, ready to use.
//! - [`Display`]: concrete display type of this board, owning the PSRAM
//!   framebuffer; implements [`theo_drivers::Panel`].
//! - [`Button`]: the BOOT push button, debounced.
//! - [`Touch`]: the CST820 touch controller, with the expander that resets
//!   it.
//! - [`Expander`]: the TCA9554, shared by its runtime users.
//! - [`Imu`], [`IMU_AXES`]: the QMI8658 accelerometer, and how its axes lie
//!   on the panel.
//! - [`I2cBus`], [`I2cDevice`]: the async I2C bus shared by the TCA9554
//!   expander and the touch controller, and one device's handle on it.
//! - [`log_heap_usage`]: logs internal RAM and PSRAM heap usage.
//! - [`display_resets`], [`reset_for_display`], [`clear_display_resets`]:
//!   reboots for a broken display, counted across software resets.
//! - `dump_frame` (feature `frame-dump`): prints the framebuffer to the
//!   serial log.
//!
//! This is the only module that depends on esp-hal, the ESP runtime crates,
//! pin assignments and panel geometry. Porting to another board means
//! providing another `device` module with the same items, where `Display`
//! implements [`theo_drivers::Panel`].

mod esp_qspi;
mod psram_tx_buf;

use core::{convert::Infallible, fmt};

use esp_backtrace as _;

use allocator_api2::{boxed::Box, vec};
use embassy_embedded_hal::shared_bus::{I2cDeviceError, asynch::i2c::I2cDevice as SharedI2cDevice};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Delay, Instant};
use esp_alloc::ExternalMemory;
use esp_hal::{
    Async,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    i2c::master::{BusTimeout, Config as I2cConfig, Error as I2cError, I2c},
    psram::{PsramConfig, PsramMode, SpiRamFreq},
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use static_cell::StaticCell;

use self::{esp_qspi::EspQspi, psram_tx_buf::PsramTxBuf};
use embedded_hal::digital::PinState;
use theo_drivers::{
    Axis, AxisMap, Co5300, Cst820, Cst820Error, Cst820InitError, DebouncedButton, ExpanderPin,
    PanelGeometry, PanelPower, PanelSize, Qmi8658, Tca9554, TouchEvent, Transient,
    check_pattern_with, flush_rows,
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

/// I2C bus timeout: at least 10 ms of SCL unchanged at 100 kHz.
///
/// esp-hal rounds it up to a power of two of clock cycles (about 13 ms).
/// A timed-out transfer fails and esp-hal clocks the bus free. The ESP32-S3
/// default disables it: a device holding SCL low would hang every transfer
/// on the bus (t-20).
const I2C_BUS_TIMEOUT: BusTimeout = BusTimeout::BusCycles(1000);

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
pub type Button = DebouncedButton<Input<'static>, Delay>;

/// CST820 touch controller; interrupt on GPIO21 (Waveshare BSP
/// `BSP_LCD_TOUCH_INT`, active low), points in panel pixels.
///
/// It reaches the shared [`Expander`] to reset the controller at run time.
pub struct Touch {
    controller: Cst820<I2cDevice, Input<'static>, Delay>,
    expander: &'static Expander,
}

/// The TCA9554 expander (EXIO0/1 panel reset and power, EXIO2 touch reset),
/// shared by its runtime users (ADR-0005).
///
/// One `Tca9554` behind a mutex: a user locks it for a whole change, so
/// read-modify-writes cannot interleave, and the driver's register copy
/// stays the single view of the chip. Hand out `&'static Expander`, never a
/// second `Tca9554`.
pub type Expander = Mutex<CriticalSectionRawMutex, Tca9554<I2cDevice>>;

/// Failure of [`Touch::changed`].
pub type TouchError = Cst820Error<I2cDeviceError<I2cError>, Infallible>;

/// Failure of [`Touch::reset`].
pub type TouchResetError = Cst820InitError<I2cDeviceError<I2cError>, I2cDeviceError<I2cError>>;

impl Touch {
    /// Waits for the next press, move, release or cancellation.
    ///
    /// # Errors
    ///
    /// Returns the controller's error; see [`Cst820::changed`].
    pub async fn changed(&mut self) -> Result<TouchEvent, TouchError> {
        self.controller.changed().await
    }

    /// Pulses the controller's reset and enables its interrupts again.
    ///
    /// A touch in progress is then reported as cancelled.
    ///
    /// # Errors
    ///
    /// Returns the expander or controller error; see [`Cst820::init`].
    pub async fn reset(&mut self) -> Result<(), TouchResetError> {
        // Held for the whole reset (about 210 ms): no other expander change
        // can slip between the pulse's writes.
        let mut expander = self.expander.lock().await;
        self.controller.init(&mut expander, TOUCH_RESET).await
    }
}

impl fmt::Debug for Touch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Cst820`'s and `Tca9554`'s derived `Debug` need `I2cDevice:
        // Debug`, which the shared-bus handle does not implement.
        f.debug_struct("Touch").finish_non_exhaustive()
    }
}

/// QMI8658 accelerometer, used for the screen orientation.
pub type Imu = Qmi8658<I2cDevice>;

/// I2C0 on GPIO15 (SDA) / GPIO14 (SCL), locked per transaction.
///
/// The TCA9554 expander (0x20), the touch controller and the IMU share it.
pub type I2cBus = Mutex<CriticalSectionRawMutex, I2c<'static, Async>>;

/// One device's handle on [`I2cBus`]; implements `embedded-hal-async` I2C.
///
/// `Board::init` creates one per driver from the static bus. The expander
/// has a single handle, shared as [`Expander`]: its read-modify-writes are
/// not atomic across handles (ADR-0005).
pub type I2cDevice = SharedI2cDevice<'static, CriticalSectionRawMutex, I2c<'static, Async>>;

/// Board peripherals initialized and ready to use.
pub struct Board {
    /// Initialized and powered AMOLED panel.
    pub display: Display,
    /// BOOT push button.
    pub button: Button,
    /// Touch controller; reset and with its interrupts enabled unless its
    /// init failed (logged).
    pub touch: Touch,
    /// Accelerometer, started; `None` if no QMI8658 answered (logged).
    pub imu: Option<Imu>,
}

impl fmt::Debug for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Board")
            .field("display", &self.display)
            .field("button", &self.button)
            .field("touch", &self.touch)
            .field("imu", &self.imu.is_some())
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
        esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

        let i2c = I2c::new(
            peripherals.I2C0,
            I2cConfig::default().with_timeout(I2C_BUS_TIMEOUT),
        )
        .unwrap()
        .with_sda(peripherals.GPIO15)
        .with_scl(peripherals.GPIO14)
        .into_async();
        // In `.bss`: internal RAM, never PSRAM, and initialized only once.
        static I2C_BUS: StaticCell<I2cBus> = StaticCell::new();
        let i2c: &'static I2cBus = I2C_BUS.init(Mutex::new(i2c));
        let mut expander = Tca9554::new(I2cDevice::new(i2c), TCA9554_ADDRESS);
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
        let touch = Touch {
            controller,
            expander: EXPANDER.init(Mutex::new(expander)),
        };
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
        let button = DebouncedButton::new(boot, Delay, PinState::Low, BUTTON_DEBOUNCE_MS);

        log_heap_usage();
        Self {
            display,
            button,
            touch,
            imu,
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

/// Reboots for a broken display in a row, kept across software resets.
///
/// In RTC fast RAM, which a software reset keeps. Stored with its
/// complement: RAM that was never written (after power-on) reads as zero.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut DISPLAY_RESETS: [u32; 2] = [0; 2];

/// Reboots for a broken display since the display last stayed healthy.
pub fn display_resets() -> u32 {
    // SAFETY: plain read of a `static` of plain integers; only the UI task
    // touches it, and every bit pattern is valid.
    let [count, check] = unsafe { core::ptr::addr_of!(DISPLAY_RESETS).read_volatile() };
    if check == !count { count } else { 0 }
}

fn set_display_resets(count: u32) {
    // SAFETY: as in `display_resets`; a reset during the write leaves a
    // pair that fails the check and reads as zero.
    unsafe { core::ptr::addr_of_mut!(DISPLAY_RESETS).write_volatile([count, !count]) };
}

/// Forgets past display reboots, once the display has stayed healthy.
pub fn clear_display_resets() {
    set_display_resets(0);
}

/// Reboots the device to recover a broken display, counting the reboot.
pub fn reset_for_display() -> ! {
    set_display_resets(display_resets().saturating_add(1));
    esp_hal::system::software_reset()
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
