//! Frees an I2C bus that a slave keeps busy after a reset.
//!
//! - [`clear_i2c_bus`], [`BusClear`], [`BusClearError`], [`MAX_CLOCK_PULSES`]:
//!   the bus clear of the I2C specification.
//!
//! A reset of the master in the middle of a transfer leaves the slave where
//! it was: if it was driving a 0 on SDA, it keeps the bus, and every
//! transfer of the restarted master fails (I2C specification UM10204,
//! 3.1.16). Clocking SCL until the slave lets SDA go, then sending a STOP,
//! frees it (t-84).

use core::fmt;

use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
};

/// SCL pulses at most: enough for a slave in the middle of any byte.
pub const MAX_CLOCK_PULSES: u8 = 9;

/// Half an SCL period at 100 kHz; also covers the STOP's setup (4.0 µs)
/// and bus-free (4.7 µs) times.
const HALF_PERIOD_NS: u32 = 5_000;

/// Half periods a released SCL may stay low (clock stretching) before
/// [`BusClearError::ClockHeld`].
const STRETCH_POLLS: u32 = 20;

/// The state the bus was in, after [`clear_i2c_bus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusClear {
    /// Both lines were high: nothing to do.
    Idle,
    /// A slave held SDA low until `pulses` SCL pulses; a STOP freed the bus.
    Cleared {
        /// SCL pulses sent.
        pulses: u8,
    },
}

/// Failure of [`clear_i2c_bus`]; both lines are then released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusClearError<E> {
    /// A pin access failed.
    Pin(E),
    /// SCL stays low once released: a device holds the clock.
    ClockHeld,
    /// SDA stays low after [`MAX_CLOCK_PULSES`] pulses.
    Stuck,
}

impl<E: fmt::Debug> fmt::Display for BusClearError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pin(error) => write!(f, "I2C bus pin failed: {error:?}"),
            Self::ClockHeld => f.write_str("a device holds SCL low"),
            Self::Stuck => write!(f, "SDA still low after {MAX_CLOCK_PULSES} clock pulses"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for BusClearError<E> {}

/// Frees the bus: clocks SCL until SDA is released, then sends a STOP.
///
/// `scl` and `sda` must be the bus lines as open-drain outputs that read
/// back their level (with pull-ups), not yet handed to the I2C peripheral.
/// A slave sending a byte may put a 0 back on SDA at the STOP's falling
/// edge, so the lines are read again after the STOP, and clocking goes on
/// within the pulse budget while SDA is low. Waits for a released SCL to
/// rise (clock stretching), up to 100 µs. Blocking: about 120 µs at most
/// without stretching, a few µs on an idle bus. On any error both lines
/// are released.
///
/// # Errors
///
/// Returns [`BusClearError::ClockHeld`] or [`BusClearError::Stuck`] if a
/// line stays low, or the pin error.
pub fn clear_i2c_bus<P: InputPin + OutputPin>(
    scl: &mut P,
    sda: &mut P,
    delay: &mut impl DelayNs,
) -> Result<BusClear, BusClearError<P::Error>> {
    let result = clear(scl, sda, delay);
    if result.is_err() {
        // Best effort: the error being reported matters more.
        let _ = scl.set_high();
        let _ = sda.set_high();
    }
    result
}

fn clear<P: InputPin + OutputPin>(
    scl: &mut P,
    sda: &mut P,
    delay: &mut impl DelayNs,
) -> Result<BusClear, BusClearError<P::Error>> {
    scl.set_high().map_err(BusClearError::Pin)?;
    sda.set_high().map_err(BusClearError::Pin)?;
    delay.delay_ns(HALF_PERIOD_NS);
    wait_for_clock(scl, delay)?;
    if sda.is_high().map_err(BusClearError::Pin)? {
        return Ok(BusClear::Idle);
    }
    for pulses in 1..=MAX_CLOCK_PULSES {
        scl.set_low().map_err(BusClearError::Pin)?;
        delay.delay_ns(HALF_PERIOD_NS);
        scl.set_high().map_err(BusClearError::Pin)?;
        delay.delay_ns(HALF_PERIOD_NS);
        wait_for_clock(scl, delay)?;
        if sda.is_high().map_err(BusClearError::Pin)? {
            stop(scl, sda, delay).map_err(BusClearError::Pin)?;
            wait_for_clock(scl, delay)?;
            if sda.is_high().map_err(BusClearError::Pin)? {
                return Ok(BusClear::Cleared { pulses });
            }
        }
    }
    Err(BusClearError::Stuck)
}

/// Waits for the released SCL to read high, as a slave may stretch it.
fn wait_for_clock<P: InputPin>(
    scl: &mut P,
    delay: &mut impl DelayNs,
) -> Result<(), BusClearError<P::Error>> {
    for _ in 0..STRETCH_POLLS {
        if scl.is_high().map_err(BusClearError::Pin)? {
            return Ok(());
        }
        delay.delay_ns(HALF_PERIOD_NS);
    }
    Err(BusClearError::ClockHeld)
}

/// Sends a STOP: SDA rises while SCL is high.
fn stop<P: OutputPin>(scl: &mut P, sda: &mut P, delay: &mut impl DelayNs) -> Result<(), P::Error> {
    scl.set_low()?;
    delay.delay_ns(HALF_PERIOD_NS);
    sda.set_low()?;
    delay.delay_ns(HALF_PERIOD_NS);
    scl.set_high()?;
    delay.delay_ns(HALF_PERIOD_NS);
    sda.set_high()?;
    delay.delay_ns(HALF_PERIOD_NS);
    Ok(())
}
