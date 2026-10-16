//! Long-press detection on top of debounced button changes.
//!
//! - [`LongPress`]: tells which changes reach the UI, and when the press in
//!   progress becomes a long press. It only keeps time stamps.
//! - [`LongPressButton`], [`LongPressEvent`]: a [`DebouncedButton`] raced
//!   against the [`LongPress`] deadline.

use core::{fmt, future::poll_fn, pin::pin, task::Poll};

use embedded_hal::digital::InputPin;
use embedded_hal_async::{delay::DelayNs, digital::Wait};

use crate::button::{ButtonEvent, DebouncedButton};

/// Splits a button's presses into short ones, forwarded, and long ones.
///
/// Every press is forwarded at once, so the UI reacts without waiting. Its
/// release is forwarded only if the press was forwarded and did not become
/// a long press: a long press, or a press already held when the button was
/// synchronized (see [`crate::DebouncedButton::sync_state`]), never ends in
/// a click, provided the UI acts on release (ADR-0008): the press of a
/// long press has already been forwarded, and its release never is.
///
/// Not `Copy`: a copy would track the same press separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LongPress {
    hold_ms: u32,
    /// When the forwarded press in progress started.
    pressed_at_ms: Option<u64>,
}

impl LongPress {
    /// Detects presses held for `hold_ms` milliseconds.
    pub const fn new(hold_ms: u32) -> Self {
        Self {
            hold_ms,
            pressed_at_ms: None,
        }
    }

    /// Records a debounced change at `now_ms`; returns it if the UI gets it.
    #[must_use = "a change that is not forwarded never reaches the UI"]
    pub fn changed(&mut self, event: ButtonEvent, now_ms: u64) -> Option<ButtonEvent> {
        match event {
            ButtonEvent::Pressed => {
                self.pressed_at_ms = Some(now_ms);
                Some(event)
            }
            ButtonEvent::Released => self.pressed_at_ms.take().map(|_| event),
        }
    }

    /// When the press in progress becomes a long press, if one is in progress.
    pub const fn deadline_ms(&self) -> Option<u64> {
        match self.pressed_at_ms {
            Some(at) => Some(at.saturating_add(self.hold_ms as u64)),
            None => None,
        }
    }

    /// Records that the deadline passed with the button still down.
    ///
    /// The press is over for the UI: its release is not forwarded.
    pub fn held(&mut self) {
        self.pressed_at_ms = None;
    }
}

/// What [`LongPressButton::next`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LongPressEvent {
    /// A press or release for the UI.
    Changed(ButtonEvent),
    /// The press in progress was held until its deadline.
    Held,
}

/// Debounced button that also reports presses held past a deadline.
///
/// `timer` waits out the deadline and `now_ms` reads the clock it runs on;
/// deadlines are absolute, so cancelling [`Self::next`] and calling it again
/// does not restart the hold.
pub struct LongPressButton<P, D, T, C> {
    button: DebouncedButton<P, D>,
    press: LongPress,
    timer: T,
    now_ms: C,
}

impl<P, D, T, C> LongPressButton<P, D, T, C>
where
    P: InputPin + Wait,
    D: DelayNs,
    T: DelayNs,
    C: FnMut() -> u64,
{
    /// Reports presses of `button` held for `hold_ms` as [`LongPressEvent::Held`].
    ///
    /// A press already held when `button` was synchronized
    /// ([`DebouncedButton::sync_state`]) has no deadline, and its release is
    /// dropped.
    pub fn new(button: DebouncedButton<P, D>, hold_ms: u32, timer: T, now_ms: C) -> Self {
        Self {
            button,
            press: LongPress::new(hold_ms),
            timer,
            now_ms,
        }
    }

    /// Hands back the debounced button, in its current state.
    #[must_use = "dropping the button drops its pin"]
    pub fn into_button(self) -> DebouncedButton<P, D> {
        self.button
    }

    /// Waits for the next forwarded change or long press.
    ///
    /// Cancel-safe: the debounced state and the deadline are kept. A release
    /// still in its debounce at the deadline loses, so a press can stop up
    /// to one debounce delay short of `hold_ms`.
    ///
    /// # Errors
    ///
    /// Returns the pin error; the debounced state and the deadline are kept
    /// (both live in the button and in [`LongPress`], which a failed read
    /// leaves alone).
    pub async fn next(&mut self) -> Result<LongPressEvent, P::Error> {
        loop {
            let changed = match self.press.deadline_ms() {
                Some(at) => {
                    let left = at.saturating_sub((self.now_ms)());
                    let left = u32::try_from(left).unwrap_or(u32::MAX);
                    let mut changed = pin!(self.button.changed());
                    let mut deadline = pin!(self.timer.delay_ms(left));
                    // The change is polled first: one that is ready wins a
                    // tie with the deadline. Both waits are cancel-safe.
                    let changed = poll_fn(|context| {
                        if let Poll::Ready(changed) = changed.as_mut().poll(context) {
                            return Poll::Ready(Some(changed));
                        }
                        deadline.as_mut().poll(context).map(|()| None)
                    })
                    .await;
                    match changed {
                        Some(changed) => changed,
                        None => {
                            self.press.held();
                            return Ok(LongPressEvent::Held);
                        }
                    }
                }
                None => self.button.changed().await,
            };
            if let Some(event) = self.press.changed(changed?, (self.now_ms)()) {
                return Ok(LongPressEvent::Changed(event));
            }
        }
    }
}

impl<P, D, T, C> fmt::Debug for LongPressButton<P, D, T, C>
where
    P: fmt::Debug,
    D: fmt::Debug,
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LongPressButton")
            .field("button", &self.button)
            .field("press", &self.press)
            .field("timer", &self.timer)
            .finish_non_exhaustive()
    }
}
