//! HAL-independent CST820 capacitive touch controller driver.
//!
//! - [`Cst820`]: resets the controller through a TCA9554 pin, then reports
//!   touches from its interrupt pin and I2C registers.
//! - [`TouchEvent`]: what changed, as [`TouchPoint`]s in panel pixels.
//! - [`Error`], [`InitError`]: bus or pin failures, rejected touch reports,
//!   and reset failures.
//!
//! The controller pulses its interrupt line low when a finger lands, then
//! periodically while it stays. [`Cst820::changed`] waits for that edge
//! while idle and polls the registers while touched: the lift is seen by
//! the next poll, without relying on an interrupt for it. A failed read
//! mid-touch cancels the touch rather than releasing it, so the UI never
//! takes a glitch for a tap. The CST820 shares the CST816 register set
//! (Waveshare drives it as a CST816).

use core::fmt;

use embedded_hal::digital::PinState;
use embedded_hal_async::{delay::DelayNs, digital::Wait, i2c::I2c};

use crate::{
    geometry::{PanelSize, TouchPoint},
    tca9554::{Pin, Tca9554},
};

/// Finger count, then X and Y high/low bytes (5 registers).
pub(crate) const REGISTER_FINGERS: u8 = 0x02;
const REGISTER_IRQ_CONTROL: u8 = 0xFA;
/// Pulse periodically while touched. The driver polls during a touch, but
/// these pulses re-arm it when an edge is missed while it was not waiting
/// (e.g. while its caller was busy between two `changed` calls).
const IRQ_EN_TOUCH: u8 = 0x40;
/// Pulse when the touch state changes.
const IRQ_EN_CHANGE: u8 = 0x20;
/// Coordinates are 12-bit: the top bits of the X high byte carry the touch
/// event, those of the Y high byte the touch id.
const COORDINATE_HIGH_MASK: u8 = 0x0F;

/// Low reset pulse length (Waveshare `Arduino_CST816x`).
const RESET_PULSE_MS: u32 = 10;
/// Wait after reset before the controller answers on I2C.
const BOOT_MS: u32 = 200;
/// Register poll period while a finger is down.
pub(crate) const POLL_MS: u32 = 10;

/// Change of the touch state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchEvent {
    /// A finger landed at this point.
    Pressed(TouchPoint),
    /// The finger moved to this point.
    Moved(TouchPoint),
    /// The finger lifted; this is its last reported point.
    Released(TouchPoint),
    /// The touch was abandoned after a failed read, at its last point: no
    /// tap or click should result from it.
    Cancelled(TouchPoint),
}

/// Failure while reading touches.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<I, P> {
    /// An I2C transfer to the controller failed.
    Bus(I),
    /// Waiting on the interrupt pin failed.
    Pin(P),
    /// The finger count is neither 0 nor 1, or the point is off the panel.
    InvalidReport {
        /// Reported finger count.
        fingers: u8,
        /// Reported column.
        x: u16,
        /// Reported row.
        y: u16,
    },
}

impl<I: fmt::Debug, P: fmt::Debug> fmt::Display for Error<I, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(error) => write!(f, "touch I2C transfer failed: {error:?}"),
            Self::Pin(error) => write!(f, "touch interrupt pin failed: {error:?}"),
            Self::InvalidReport { fingers, x, y } => {
                write!(f, "invalid touch report: {fingers} fingers at ({x}, {y})")
            }
        }
    }
}

impl<I: fmt::Debug, P: fmt::Debug> core::error::Error for Error<I, P> {}

/// Failure while resetting and configuring the controller.
#[derive(Debug, PartialEq, Eq)]
pub enum InitError<R, I> {
    /// Driving the reset pin on the expander failed.
    Reset(R),
    /// Configuring the controller over I2C failed.
    Bus(I),
}

impl<R: fmt::Debug, I: fmt::Debug> fmt::Display for InitError<R, I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reset(error) => write!(f, "touch reset through the expander failed: {error:?}"),
            Self::Bus(error) => write!(f, "touch controller setup failed: {error:?}"),
        }
    }
}

impl<R: fmt::Debug, I: fmt::Debug> core::error::Error for InitError<R, I> {}

/// CST820 on an I2C bus, with its active-low interrupt pin.
#[derive(Debug)]
pub struct Cst820<I, P, D> {
    i2c: I,
    address: u8,
    interrupt: P,
    delay: D,
    /// Reported points must lie on this panel.
    size: PanelSize,
    /// Last reported point while a finger is down.
    touched: Option<TouchPoint>,
    /// Touch abandoned by a failed read, reported by the next call.
    cancelled: Option<TouchPoint>,
}

impl<I: I2c, P: Wait, D: DelayNs> Cst820<I, P, D> {
    /// Drives the controller at `address` over a panel of `size`.
    pub fn new(i2c: I, address: u8, interrupt: P, delay: D, size: PanelSize) -> Self {
        Self {
            i2c,
            address,
            interrupt,
            delay,
            size,
            touched: None,
            cancelled: None,
        }
    }

    /// Releases the bus, the interrupt pin and the delay.
    pub fn release(self) -> (I, P, D) {
        (self.i2c, self.interrupt, self.delay)
    }

    /// Pulses `reset` low on `expander`, then enables touch interrupts.
    ///
    /// Call it before [`Self::changed`]: after an MCU-only reset the
    /// expander keeps the reset pin high, so the controller is otherwise in
    /// whatever state the previous firmware left it. `reset` ends as an
    /// output driven high; other expander pins are kept. Any touch in
    /// progress is forgotten.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Reset`] if driving the pin fails, and the
    /// controller is then not accessed; [`InitError::Bus`] if configuring
    /// the controller fails.
    pub async fn init<J: I2c>(
        &mut self,
        expander: &mut Tca9554<J>,
        reset: Pin,
    ) -> Result<(), InitError<J::Error, I::Error>> {
        expander
            .configure_output(reset, PinState::Low)
            .await
            .map_err(InitError::Reset)?;
        self.delay.delay_ms(RESET_PULSE_MS).await;
        expander
            .set_level(reset, PinState::High)
            .await
            .map_err(InitError::Reset)?;
        self.delay.delay_ms(BOOT_MS).await;
        self.touched = None;
        self.cancelled = None;
        self.i2c
            .write(
                self.address,
                &[REGISTER_IRQ_CONTROL, IRQ_EN_TOUCH | IRQ_EN_CHANGE],
            )
            .await
            .map_err(InitError::Bus)
    }

    /// Waits for the next press, move, release or cancellation.
    ///
    /// While idle, waits for an interrupt edge; while touched, polls every
    /// 10 ms and skips reports where the finger did not move.
    ///
    /// Cancelling it after an interrupt edge but before the read that
    /// follows loses that edge: the press is then only seen at the
    /// controller's next periodic pulse. Run it in its own task.
    ///
    /// # Errors
    ///
    /// Returns the pin or bus error, or [`Error::InvalidReport`]. A touch in
    /// progress is then abandoned: the next call returns
    /// [`TouchEvent::Cancelled`] at its last point, and the next touch
    /// starts with a press.
    pub async fn changed(&mut self) -> Result<TouchEvent, Error<I::Error, P::Error>> {
        if let Some(point) = self.cancelled.take() {
            return Ok(TouchEvent::Cancelled(point));
        }
        loop {
            if self.touched.is_some() {
                self.delay.delay_ms(POLL_MS).await;
            } else {
                self.interrupt
                    .wait_for_falling_edge()
                    .await
                    .map_err(Error::Pin)?;
            }
            let point = match self.read_point().await {
                Ok(point) => point,
                Err(error) => {
                    self.cancelled = self.touched.take();
                    return Err(error);
                }
            };
            match (self.touched, point) {
                (None, Some(point)) => {
                    self.touched = Some(point);
                    return Ok(TouchEvent::Pressed(point));
                }
                (Some(last), Some(point)) if point != last => {
                    self.touched = Some(point);
                    return Ok(TouchEvent::Moved(point));
                }
                (Some(last), None) => {
                    self.touched = None;
                    return Ok(TouchEvent::Released(last));
                }
                // An unmoved finger, or an interrupt without one.
                (Some(_), Some(_)) | (None, None) => {}
            }
        }
    }

    /// Reads the current point, `None` when no finger is down.
    async fn read_point(&mut self) -> Result<Option<TouchPoint>, Error<I::Error, P::Error>> {
        let mut report = [0; 5];
        self.i2c
            .write_read(self.address, &[REGISTER_FINGERS], &mut report)
            .await
            .map_err(Error::Bus)?;
        let [fingers, x_high, x_low, y_high, y_low] = report;
        let x = u16::from_be_bytes([x_high & COORDINATE_HIGH_MASK, x_low]);
        let y = u16::from_be_bytes([y_high & COORDINATE_HIGH_MASK, y_low]);
        match fingers {
            0 => Ok(None),
            1 if x < self.size.width && y < self.size.height => Ok(Some(TouchPoint { x, y })),
            _ => Err(Error::InvalidReport { fingers, x, y }),
        }
    }
}
