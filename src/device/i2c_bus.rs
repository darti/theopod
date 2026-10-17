//! The async I2C bus shared by the expander, the touch controller and the IMU.
//!
//! - [`I2cBus`], [`I2cDevice`]: the bus, locked per transaction, and one
//!   device's handle on it (ADR-0005).
//! - `init`: frees the bus, then starts the controller on it (t-84).

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice as SharedI2cDevice;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use esp_hal::{
    Async,
    delay::Delay,
    gpio::{DriveMode, Flex, InputConfig, OutputConfig, Pin, Pull},
    i2c::master::{BusTimeout, Config, I2c},
    peripherals::{GPIO14, GPIO15, I2C0},
};
use static_cell::StaticCell;
use theo_drivers::{BusClear, clear_i2c_bus};

/// I2C bus timeout: at least 10 ms of SCL unchanged at 100 kHz.
///
/// esp-hal rounds it up to a power of two of clock cycles (about 13 ms).
/// A timed-out transfer fails and esp-hal clocks the bus free. The ESP32-S3
/// default disables it: a device holding SCL low would hang every transfer
/// on the bus (t-20).
const TIMEOUT: BusTimeout = BusTimeout::BusCycles(1000);

/// I2C0 on GPIO15 (SDA) / GPIO14 (SCL), locked per transaction.
///
/// The TCA9554 expander (0x20), the touch controller and the IMU share it.
pub type I2cBus = Mutex<CriticalSectionRawMutex, I2c<'static, Async>>;

/// One device's handle on [`I2cBus`]; implements `embedded-hal-async` I2C.
///
/// `Board::init` creates one per driver from the static bus. The expander
/// has a single handle, shared as `Expander`: its read-modify-writes are
/// not atomic across handles (ADR-0005).
pub type I2cDevice = SharedI2cDevice<'static, CriticalSectionRawMutex, I2c<'static, Async>>;

/// Frees the bus on `scl` and `sda`, then starts I2C0 on them.
///
/// A reset of the chip mid-transfer (a watchdog reset, ADR-0009) can leave
/// a device holding SDA; the early stop parks the board over this bus.
/// The pin types fix the wiring, so the lines cannot be swapped.
///
/// # Panics
///
/// Panics if esp-hal rejects the bus configuration, or if called twice.
pub(super) fn init(
    i2c0: I2C0<'static>,
    mut scl: GPIO14<'static>,
    mut sda: GPIO15<'static>,
) -> &'static I2cBus {
    free(scl.reborrow(), sda.reborrow());
    let i2c = I2c::new(i2c0, Config::default().with_timeout(TIMEOUT))
        .unwrap()
        .with_sda(sda)
        .with_scl(scl)
        .into_async();
    // In `.bss`: internal RAM, never PSRAM, and initialized only once.
    static I2C_BUS: StaticCell<I2cBus> = StaticCell::new();
    I2C_BUS.init(Mutex::new(i2c))
}

/// Frees the bus on `scl` and `sda` before the controller takes them.
///
/// The lines are open-drain with pull-ups, as on the bus; the level is set
/// high before the output is enabled, so taking them drives nothing. A
/// device the last reset left mid-transfer lets SDA go after a few clock
/// pulses (`clear_i2c_bus`). Best effort: a line that stays low is
/// logged, and the transfers that follow fail within the bus timeout,
/// after which esp-hal clears the bus in hardware.
fn free(scl: impl Pin, sda: impl Pin) {
    let mut scl = open_drain(scl);
    let mut sda = open_drain(sda);
    match clear_i2c_bus(&mut scl, &mut sda, &mut Delay::new()) {
        Ok(BusClear::Idle) => {}
        Ok(BusClear::Cleared { pulses }) => {
            log::warn!("I2C bus held by a device, freed after {pulses} clock pulses");
        }
        Err(error) => log::error!("I2C bus cannot be freed: {error}"),
    }
}

/// `pin` as an open-drain line that reads back its level, released high.
fn open_drain<'d>(pin: impl Pin + 'd) -> Flex<'d> {
    let mut line = Flex::new(pin);
    line.set_high();
    line.apply_output_config(
        &OutputConfig::default()
            .with_drive_mode(DriveMode::OpenDrain)
            .with_pull(Pull::Up),
    );
    line.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
    line.set_input_enable(true);
    line.set_output_enable(true);
    line
}
