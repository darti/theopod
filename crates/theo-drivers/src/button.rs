//! Debounced push button on any `embedded-hal-async` input pin.
//!
//! - [`DebouncedButton`]: waits for a debounced press or release.
//! - [`ButtonEvent`]: what changed.

use embedded_hal::digital::{InputPin, PinState};
use embedded_hal_async::{delay::DelayNs, digital::Wait};

/// Debounced change of a button's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonEvent {
    /// The button went down.
    Pressed,
    /// The button went up.
    Released,
}

/// Push button whose changes are confirmed after a debounce delay.
///
/// The button starts released; one held at start-up is reported by the
/// first [`DebouncedButton::changed`] call, unless
/// [`DebouncedButton::sync_state`] took it as the starting state.
#[derive(Debug)]
pub struct DebouncedButton<P, D> {
    pin: P,
    delay: D,
    /// Pin level while the button is held down.
    active: PinState,
    debounce_ms: u32,
    pressed: bool,
}

impl<P: InputPin + Wait, D: DelayNs> DebouncedButton<P, D> {
    /// Watches `pin`, which reads `active` while the button is held down.
    pub fn new(pin: P, delay: D, active: PinState, debounce_ms: u32) -> Self {
        Self {
            pin,
            delay,
            active,
            debounce_ms,
            pressed: false,
        }
    }

    /// Releases the pin and the delay.
    pub fn release(self) -> (P, D) {
        (self.pin, self.delay)
    }

    /// Debounced state as of the last reported change.
    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// Takes the pin's current level as the state, without reporting it.
    ///
    /// Returns whether the button is held. A button held now, such as the
    /// press that woke the chip, then only reports its release. The level
    /// is read once, not debounced: call it while the button is steady.
    ///
    /// # Errors
    ///
    /// Returns the pin error; the state is kept.
    pub fn sync_state(&mut self) -> Result<bool, P::Error> {
        self.pressed = self.read_pressed()?;
        Ok(self.pressed)
    }

    /// Waits for the next debounced press or release.
    ///
    /// Waits for the opposite level, which returns at once if the pin is
    /// already there, so no change is missed between calls. Then confirms
    /// the level after the debounce delay; a bounce back restarts the wait.
    ///
    /// Cancelling keeps the debounced state but restarts the debounce on the
    /// next call: callers that may cancel more often than the debounce delay
    /// (e.g. in a `select` against a frame timer) never see an event. Run
    /// the button in its own task instead.
    ///
    /// # Errors
    ///
    /// Returns the pin error; the debounced state is kept.
    pub async fn changed(&mut self) -> Result<ButtonEvent, P::Error> {
        loop {
            match self.level_when(!self.pressed) {
                PinState::High => self.pin.wait_for_high().await?,
                PinState::Low => self.pin.wait_for_low().await?,
            }
            self.delay.delay_ms(self.debounce_ms).await;
            let pressed = self.read_pressed()?;
            if pressed != self.pressed {
                self.pressed = pressed;
                return Ok(if pressed {
                    ButtonEvent::Pressed
                } else {
                    ButtonEvent::Released
                });
            }
        }
    }

    /// Pin level while the button is `pressed` (or released).
    fn level_when(&self, pressed: bool) -> PinState {
        if pressed { self.active } else { !self.active }
    }

    fn read_pressed(&mut self) -> Result<bool, P::Error> {
        let high = self.pin.is_high()?;
        Ok(PinState::from(high) == self.active)
    }
}
