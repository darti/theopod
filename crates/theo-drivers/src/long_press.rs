//! Long-press detection on top of debounced button changes.
//!
//! - [`LongPress`]: tells which changes reach the UI, and when the press in
//!   progress becomes a long press.
//!
//! It only keeps time stamps: the caller races its wait for the next change
//! against [`LongPress::deadline_ms`] and reports a win with
//! [`LongPress::held`].

use crate::button::ButtonEvent;

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
