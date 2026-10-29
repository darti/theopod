use core::{
    pin::pin,
    task::{Context, Poll, Waker},
};

use embedded_hal::digital::{ErrorKind, PinState};
use speculoos::prelude::*;

use crate::{
    button::{ButtonEvent, DebouncedButton},
    long_press::{LongPress, LongPressButton, LongPressEvent},
    test_support::{FakeClock, NoDelay, ScriptedPin, block_on, poll_once},
};

const HOLD_MS: u32 = 2000;
const DEBOUNCE_MS: u32 = 20;
const HIGH: bool = true;
const LOW: bool = false;

#[test]
fn new_has_no_deadline() {
    assert_that!(LongPress::new(HOLD_MS).deadline_ms()).is_none();
}

#[test]
fn changed_press_is_forwarded_and_sets_deadline() {
    let mut press = LongPress::new(HOLD_MS);

    let forwarded = press.changed(ButtonEvent::Pressed, 500);

    assert_that!(forwarded).is_equal_to(Some(ButtonEvent::Pressed));
    assert_that!(press.deadline_ms()).is_equal_to(Some(2500));
}

#[test]
fn changed_release_before_deadline_is_forwarded_and_clears_deadline() {
    let mut press = LongPress::new(HOLD_MS);
    let _ = press.changed(ButtonEvent::Pressed, 500);

    let forwarded = press.changed(ButtonEvent::Released, 900);

    assert_that!(forwarded).is_equal_to(Some(ButtonEvent::Released));
    assert_that!(press.deadline_ms()).is_none();
}

#[test]
fn changed_release_without_press_is_dropped() {
    // A button held at start-up (`DebouncedButton::sync_state`): the press
    // that woke the board must not click once it is released.
    let mut press = LongPress::new(HOLD_MS);

    let forwarded = press.changed(ButtonEvent::Released, 100);

    assert_that!(forwarded).is_none();
}

#[test]
fn held_clears_deadline() {
    let mut press = LongPress::new(HOLD_MS);
    let _ = press.changed(ButtonEvent::Pressed, 0);

    press.held();

    assert_that!(press.deadline_ms()).is_none();
}

#[test]
fn held_then_release_is_dropped() {
    let mut press = LongPress::new(HOLD_MS);
    let _ = press.changed(ButtonEvent::Pressed, 0);
    press.held();

    let forwarded = press.changed(ButtonEvent::Released, 3000);

    assert_that!(forwarded).is_none();
}

#[test]
fn held_then_new_press_sets_new_deadline() {
    let mut press = LongPress::new(HOLD_MS);
    let _ = press.changed(ButtonEvent::Pressed, 0);
    press.held();
    let _ = press.changed(ButtonEvent::Released, 3000);

    let forwarded = press.changed(ButtonEvent::Pressed, 4000);

    assert_that!(forwarded).is_equal_to(Some(ButtonEvent::Pressed));
    assert_that!(press.deadline_ms()).is_equal_to(Some(6000));
}

#[test]
fn changed_press_near_u64_max_saturates_deadline() {
    let mut press = LongPress::new(HOLD_MS);

    let _ = press.changed(ButtonEvent::Pressed, u64::MAX - 1);

    assert_that!(press.deadline_ms()).is_equal_to(Some(u64::MAX));
}

/// Active-low button (BOOT) replaying `samples`, then staying where it is;
/// the debounce takes no time, so presses land on the clock's reading.
fn debounced(samples: &[bool]) -> DebouncedButton<ScriptedPin, NoDelay> {
    let pin = ScriptedPin {
        pend_when_exhausted: true,
        ..ScriptedPin::new(samples)
    };
    DebouncedButton::new(pin, NoDelay, PinState::Low, DEBOUNCE_MS)
}

/// A [`LongPressButton`] timed by `clock`.
fn long_press_button(
    button: DebouncedButton<ScriptedPin, NoDelay>,
    clock: &FakeClock,
) -> LongPressButton<ScriptedPin, NoDelay, FakeClock, impl FnMut() -> u64> {
    let now = clock.clone();
    LongPressButton::new(button, HOLD_MS, clock.clone(), move || now.now_ms())
}

#[test]
fn next_short_press_forwards_press_then_release() {
    let clock = FakeClock::default();
    let mut button = long_press_button(debounced(&[LOW, HIGH]), &clock);

    let press = block_on(button.next());
    let release = block_on(button.next());

    assert_that!(press)
        .is_ok()
        .is_equal_to(LongPressEvent::Changed(ButtonEvent::Pressed));
    assert_that!(release)
        .is_ok()
        .is_equal_to(LongPressEvent::Changed(ButtonEvent::Released));
    assert_that!(clock.delays_ms()).is_empty();
}

#[test]
fn next_held_past_deadline_reports_held() {
    let clock = FakeClock::default();
    let mut button = long_press_button(debounced(&[LOW]), &clock);
    clock.advance(500);
    let _ = block_on(button.next());

    let held = block_on(button.next());

    assert_that!(held).is_ok().is_equal_to(LongPressEvent::Held);
    assert_that!(clock.delays_ms()).is_equal_to(vec![HOLD_MS]);
    assert_that!(clock.now_ms()).is_equal_to(500 + u64::from(HOLD_MS));
}

#[test]
fn next_after_held_waits_for_release_without_deadline() {
    let clock = FakeClock::default();
    let mut button = long_press_button(debounced(&[LOW]), &clock);
    let _ = block_on(button.next());
    let _ = block_on(button.next());

    let done = poll_once(button.next());

    // No second `Held`: the long press is over until the next press.
    assert_that!(done).is_false();
    assert_that!(clock.delays_ms()).is_equal_to(vec![HOLD_MS]);
}

#[test]
fn next_cancelled_while_held_keeps_the_deadline() {
    // Regression guard (t-52): `next` is cancel-safe API; a caller that
    // drops and restarts it must not restart the hold, or the stop slips.
    let clock = FakeClock::default();
    let mut button = long_press_button(debounced(&[LOW]), &clock);
    let _ = block_on(button.next());
    clock.pend_next_delay();
    assert_that!(poll_once(button.next())).is_false();
    clock.advance(500);

    let held = block_on(button.next());

    assert_that!(held).is_ok().is_equal_to(LongPressEvent::Held);
    assert_that!(clock.delays_ms()).is_equal_to(vec![HOLD_MS, HOLD_MS - 500]);
}

#[test]
fn next_cancelled_past_deadline_reports_held_at_once() {
    let clock = FakeClock::default();
    let mut button = long_press_button(debounced(&[LOW]), &clock);
    let _ = block_on(button.next());
    clock.pend_next_delay();
    assert_that!(poll_once(button.next())).is_false();
    clock.advance(u64::from(HOLD_MS) + 100);

    let held = block_on(button.next());

    assert_that!(held).is_ok().is_equal_to(LongPressEvent::Held);
    assert_that!(clock.delays_ms()).is_equal_to(vec![HOLD_MS, 0]);
}

#[test]
fn next_press_held_at_sync_has_no_deadline() {
    // The press that woke the chip neither clicks nor stops it (ADR-0008).
    let clock = FakeClock::default();
    let mut debounced = debounced(&[LOW]);
    assert_that!(debounced.sync_state()).is_ok().is_true();
    let mut button = long_press_button(debounced, &clock);

    let done = poll_once(button.next());

    assert_that!(done).is_false();
    assert_that!(clock.delays_ms()).is_empty();
}

#[test]
fn next_synced_press_release_is_dropped() {
    let clock = FakeClock::default();
    let mut debounced = debounced(&[LOW, HIGH]);
    assert_that!(debounced.sync_state()).is_ok().is_true();
    let mut button = long_press_button(debounced, &clock);

    let done = poll_once(button.next());

    // The release is debounced and dropped; the button then waits for the
    // next press.
    assert_that!(done).is_false();
    assert_that!(button.into_button().is_pressed()).is_false();
}

#[test]
fn next_pin_failure_returns_pin_error() {
    let clock = FakeClock::default();
    let pin = ScriptedPin {
        fail_when_exhausted: true,
        ..ScriptedPin::new(&[LOW])
    };
    let mut button = long_press_button(
        DebouncedButton::new(pin, NoDelay, PinState::Low, DEBOUNCE_MS),
        &clock,
    );
    let _ = block_on(button.next());

    let failed = block_on(button.next());

    assert_that!(failed).is_err().is_equal_to(ErrorKind::Other);
    assert_that!(clock.delays_ms()).is_empty();
    assert_that!(button.into_button().is_pressed()).is_true();
}

#[test]
fn next_release_during_hold_wins_and_stops_the_timer() {
    let clock = FakeClock::default();
    let pin = ScriptedPin {
        pend_wait: Some(1),
        ..ScriptedPin::new(&[LOW, HIGH])
    };
    let mut button = long_press_button(
        DebouncedButton::new(pin, NoDelay, PinState::Low, DEBOUNCE_MS),
        &clock,
    );
    let _ = block_on(button.next());
    clock.pend_next_delay();
    let mut next = pin!(button.next());
    let mut context = Context::from_waker(Waker::noop());
    assert_that!(next.as_mut().poll(&mut context).is_pending()).is_true();
    clock.advance(700);

    let released = next.as_mut().poll(&mut context);

    assert_that!(released).is_equal_to(Poll::Ready(Ok(LongPressEvent::Changed(
        ButtonEvent::Released,
    ))));
    assert_that!(clock.delays_ms()).is_equal_to(vec![HOLD_MS]);
    assert_that!(clock.now_ms()).is_equal_to(700);
}

#[test]
fn next_release_ready_at_deadline_forwards_release() {
    // A change that is ready wins a tie with the deadline: a click, not a
    // stop.
    let clock = FakeClock::default();
    let mut button = long_press_button(debounced(&[LOW, HIGH]), &clock);
    let _ = block_on(button.next());
    clock.advance(u64::from(HOLD_MS));

    let released = block_on(button.next());

    assert_that!(released)
        .is_ok()
        .is_equal_to(LongPressEvent::Changed(ButtonEvent::Released));
    assert_that!(clock.delays_ms()).is_empty();
}

#[test]
fn next_after_held_drops_release_and_times_next_press_afresh() {
    let clock = FakeClock::default();
    let pin = ScriptedPin {
        pend_when_exhausted: true,
        pend_wait: Some(1),
        ..ScriptedPin::new(&[LOW, HIGH, LOW])
    };
    let mut button = long_press_button(
        DebouncedButton::new(pin, NoDelay, PinState::Low, DEBOUNCE_MS),
        &clock,
    );
    let _ = block_on(button.next());
    let held = block_on(button.next());

    // The long press's release is dropped: the next event is a new press.
    let pressed = block_on(button.next());
    clock.advance(300);
    let held_again = block_on(button.next());

    assert_that!(held).is_ok().is_equal_to(LongPressEvent::Held);
    assert_that!(pressed)
        .is_ok()
        .is_equal_to(LongPressEvent::Changed(ButtonEvent::Pressed));
    assert_that!(held_again)
        .is_ok()
        .is_equal_to(LongPressEvent::Held);
    // The second hold runs from the second press, at 2000 ms.
    assert_that!(clock.delays_ms()).is_equal_to(vec![HOLD_MS, HOLD_MS - 300]);
}

// Regression (t-83): progress preserves release-first ordering at the deadline.
#[test]
fn next_progress_release_at_deadline_wins() {
    let clock = FakeClock::default();
    let mut button = long_press_button(debounced(&[LOW, HIGH]), &clock);
    let mut checkpoints = 0;
    assert_eq!(
        block_on(button.next_with_progress(|| checkpoints += 1)),
        Ok(LongPressEvent::Changed(ButtonEvent::Pressed))
    );
    clock.advance(u64::from(HOLD_MS));
    assert_eq!(
        block_on(button.next_with_progress(|| checkpoints += 1)),
        Ok(LongPressEvent::Changed(ButtonEvent::Released))
    );
    assert_eq!(checkpoints, 2);
    assert!(clock.delays_ms().is_empty());
}
