//! HAL-independent QMI8658 inertial sensor driver, accelerometer only.
//!
//! - [`Qmi8658`]: identifies and resets the sensor, enables its
//!   accelerometer, reads raw accelerations and powers the sensor down,
//!   over any `embedded-hal-async` [`I2c`] bus.
//! - [`Acceleration`]: one raw sample on the sensor's own axes.
//! - [`Error`]: bus failures, an unexpected chip, or a stuck reset.
//!
//! The gyroscope stays off: orientation only needs gravity. The
//! accelerometer runs in its low-power mode at 21 Hz; the range is ±2 g,
//! so 1 g reads as [`Acceleration::ONE_G`].

use core::fmt;

use embedded_hal_async::{delay::DelayNs, i2c::I2c};

const REGISTER_WHO_AM_I: u8 = 0x00;
const REGISTER_CTRL1: u8 = 0x02;
const REGISTER_CTRL2: u8 = 0x03;
const REGISTER_CTRL7: u8 = 0x08;
/// X, Y, Z low/high bytes (6 registers).
const REGISTER_ACCEL_X_LOW: u8 = 0x35;
const REGISTER_RESET_RESULT: u8 = 0x4D;
const REGISTER_RESET: u8 = 0x60;

const WHO_AM_I: u8 = 0x05;
const RESET_COMMAND: u8 = 0xB0;
const RESET_DONE: u8 = 0x80;
/// Address auto-increment, little-endian data.
const CTRL1_AUTO_INCREMENT: u8 = 0x40;
/// ±2 g full scale (bits 6:4 = 0), low-power mode at 21 Hz (bits 3:0 =
/// 0xD, SensorLib's `ACC_ODR_LOWPOWER_21Hz`). Low-power rates need the
/// gyroscope off; 21 Hz still gives each 10 Hz orientation poll a fresh
/// sample, and gravity needs no more precision (t-41).
const CTRL2_ACCEL_2G_LOW_POWER_21HZ: u8 = 0x0D;
/// Accelerometer enabled, gyroscope disabled.
const CTRL7_ACCEL_ONLY: u8 = 0x01;
/// Accelerometer and gyroscope disabled.
const CTRL7_SENSORS_OFF: u8 = 0x00;
/// CTRL1 `sensorDisable`: stops the internal 2 MHz oscillator. With both
/// sensors off this is the power-down mode (SensorLib `powerDown`).
const CTRL1_SENSOR_DISABLE: u8 = 0x01;

/// Reset-result poll period and count: a 50 ms budget for a reset that
/// takes at most 15 ms.
const RESET_POLL_MS: u32 = 5;
const RESET_POLLS: usize = 10;

/// One raw acceleration sample on the sensor's X, Y and Z axes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Acceleration {
    /// X axis, in 1/[`Self::ONE_G`] g.
    pub x: i16,
    /// Y axis, in 1/[`Self::ONE_G`] g.
    pub y: i16,
    /// Z axis, in 1/[`Self::ONE_G`] g.
    pub z: i16,
}

impl Acceleration {
    /// Raw reading of 1 g at the ±2 g range.
    pub const ONE_G: i16 = 16_384;
}

/// Failure while talking to the QMI8658.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// An I2C transfer failed.
    Bus(E),
    /// The chip at the address is not a QMI8658.
    WrongChip {
        /// `WHO_AM_I` register value read back.
        id: u8,
    },
    /// The soft reset did not report completion in time.
    ResetTimeout,
}

impl<E: fmt::Debug> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(error) => write!(f, "IMU I2C transfer failed: {error:?}"),
            Self::WrongChip { id } => write!(f, "not a QMI8658: WHO_AM_I is {id:#04x}"),
            Self::ResetTimeout => write!(f, "IMU soft reset did not complete"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for Error<E> {}

/// QMI8658 at a fixed 7-bit I2C address (0x6A or 0x6B).
#[derive(Debug)]
pub struct Qmi8658<I> {
    i2c: I,
    address: u8,
}

impl<I: I2c> Qmi8658<I> {
    /// Drives the sensor at `address` over `i2c`.
    pub fn new(i2c: I, address: u8) -> Self {
        Self { i2c, address }
    }

    /// Releases the underlying bus.
    pub fn release(self) -> I {
        self.i2c
    }

    /// Checks the sensor's identity, soft-resets it and starts the
    /// accelerometer.
    ///
    /// The oscillator is started before the reset, so a sensor left powered
    /// down by [`Self::power_down`] (across a deep sleep) resets too.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongChip`] before writing anything, so probing an
    /// address that holds another chip leaves it untouched;
    /// [`Error::ResetTimeout`] before any configuration but the oscillator
    /// is written; or the I2C error of a failed access.
    pub async fn init(&mut self, delay: &mut impl DelayNs) -> Result<(), Error<I::Error>> {
        let id = self.read(REGISTER_WHO_AM_I).await?;
        if id != WHO_AM_I {
            return Err(Error::WrongChip { id });
        }
        // The reset may need the oscillator that `power_down` stopped.
        self.write(REGISTER_CTRL1, CTRL1_AUTO_INCREMENT).await?;
        self.write(REGISTER_RESET, RESET_COMMAND).await?;
        self.wait_for_reset(delay).await?;
        self.write(REGISTER_CTRL1, CTRL1_AUTO_INCREMENT).await?;
        self.write(REGISTER_CTRL2, CTRL2_ACCEL_2G_LOW_POWER_21HZ)
            .await?;
        self.write(REGISTER_CTRL7, CTRL7_ACCEL_ONLY).await
    }

    /// Reads the latest acceleration sample.
    ///
    /// # Errors
    ///
    /// Returns the I2C error if the read fails.
    pub async fn acceleration(&mut self) -> Result<Acceleration, Error<I::Error>> {
        let mut raw = [0; 6];
        self.i2c
            .write_read(self.address, &[REGISTER_ACCEL_X_LOW], &mut raw)
            .await
            .map_err(Error::Bus)?;
        let [x_low, x_high, y_low, y_high, z_low, z_high] = raw;
        Ok(Acceleration {
            x: i16::from_le_bytes([x_low, x_high]),
            y: i16::from_le_bytes([y_low, y_high]),
            z: i16::from_le_bytes([z_low, z_high]),
        })
    }

    /// Turns the sensors and the oscillator off, for deep sleep.
    ///
    /// The sensor then draws only its power-down current until the next
    /// [`Self::init`] restarts it. Both writes are tried even if the first
    /// fails. Auto-increment stays on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bus`] for the first failed write.
    pub async fn power_down(&mut self) -> Result<(), Error<I::Error>> {
        let sensors = self.write(REGISTER_CTRL7, CTRL7_SENSORS_OFF).await;
        let oscillator = self
            .write(REGISTER_CTRL1, CTRL1_AUTO_INCREMENT | CTRL1_SENSOR_DISABLE)
            .await;
        sensors.and(oscillator)
    }

    /// Polls the reset result until the soft reset reports completion.
    async fn wait_for_reset(&mut self, delay: &mut impl DelayNs) -> Result<(), Error<I::Error>> {
        for _ in 0..RESET_POLLS {
            delay.delay_ms(RESET_POLL_MS).await;
            if self.read(REGISTER_RESET_RESULT).await? == RESET_DONE {
                return Ok(());
            }
        }
        Err(Error::ResetTimeout)
    }

    async fn read(&mut self, register: u8) -> Result<u8, Error<I::Error>> {
        let mut value = [0];
        self.i2c
            .write_read(self.address, &[register], &mut value)
            .await
            .map_err(Error::Bus)?;
        Ok(value[0])
    }

    async fn write(&mut self, register: u8, value: u8) -> Result<(), Error<I::Error>> {
        self.i2c
            .write(self.address, &[register, value])
            .await
            .map_err(Error::Bus)
    }
}
