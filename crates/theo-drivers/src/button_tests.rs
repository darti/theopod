use embedded_hal::digital::{ErrorKind, PinState};
use speculoos::prelude::*;

use crate::{
    button::{ButtonEvent, DebouncedButton},
    test_support::{RecordingDelay, ScriptedPin, block_on, poll_once},
};

const DEBOUNCE_MS: u32 = 20;
const HIGH: bool = true;
const LOW: bool = false;

/// Active-low button (pressed pulls the pin low), like the BOOT button.
fn button(samples: &[bool]) -> DebouncedButton<ScriptedPin, RecordingDelay> {
    DebouncedButton::new(
        ScriptedPin::new(samples),
        RecordingDelay::default(),
        PinState::Low,
        DEBOUNCE_MS,
    )
}

#[test]
fn new_always_starts_released() {
    assert_that!(button(&[]).is_pressed()).is_false();
}

#[test]
fn changed_released_waits_for_active_level_then_reports_pressed() {
    let mut button = button(&[LOW]);

    let event = block_on(button.changed());

    assert_that!(event)
        .is_ok()
        .is_equal_to(ButtonEvent::Pressed);
    assert_that!(button.is_pressed()).is_true();
    let (pin, delay) = button.release();
    assert_that!(pin.waits).is_equal_to(vec![PinState::Low]);
    assert_that!(delay.delays_ms).is_equal_to(vec![DEBOUNCE_MS]);
}

#[test]
fn changed_pressed_waits_for_inactive_level_then_reports_released() {
    let mut button = button(&[LOW, HIGH]);
    block_on(button.changed()).unwrap();

    let event = block_on(button.changed());

    assert_that!(event)
        .is_ok()
        .is_equal_to(ButtonEvent::Released);
    assert_that!(button.release().0.waits).is_equal_to(vec![PinState::Low, PinState::High]);
}

#[test]
fn changed_bounce_shorter_than_debounce_is_ignored() {
    // Back high after the debounce delay, then a real press.
    let mut button = button(&[HIGH, LOW]);

    let event = block_on(button.changed());

    assert_that!(event)
        .is_ok()
        .is_equal_to(ButtonEvent::Pressed);
    let (pin, delay) = button.release();
    assert_that!(pin.waits).is_equal_to(vec![PinState::Low, PinState::Low]);
    assert_that!(delay.delays_ms).is_equal_to(vec![DEBOUNCE_MS, DEBOUNCE_MS]);
}

#[test]
fn changed_release_bounce_is_ignored() {
    let mut button = button(&[LOW, LOW, HIGH]);
    block_on(button.changed()).unwrap();

    let event = block_on(button.changed());

    assert_that!(event)
        .is_ok()
        .is_equal_to(ButtonEvent::Released);
}

#[test]
fn changed_active_high_button_reports_high_as_pressed() {
    let pin = ScriptedPin::new(&[HIGH]);
    let mut button = DebouncedButton::new(pin, RecordingDelay::default(), PinState::High, 5);

    let event = block_on(button.changed());

    assert_that!(event)
        .is_ok()
        .is_equal_to(ButtonEvent::Pressed);
    assert_that!(button.release().0.waits).is_equal_to(vec![PinState::High]);
}

// Regression guard (t-8): cancelling `changed` mid-debounce must not change the
// state; the next call debounces again and reports the change once.
#[test]
fn changed_cancelled_mid_debounce_keeps_state_and_reports_next_call() {
    let delay = RecordingDelay {
        pend_next: true,
        ..RecordingDelay::default()
    };
    let pin = ScriptedPin::new(&[LOW]);
    let mut button = DebouncedButton::new(pin, delay, PinState::Low, DEBOUNCE_MS);

    let completed = poll_once(button.changed());
    let pressed_after_cancel = button.is_pressed();
    let event = block_on(button.changed());

    assert_that!(completed).is_false();
    assert_that!(pressed_after_cancel).is_false();
    assert_that!(event)
        .is_ok()
        .is_equal_to(ButtonEvent::Pressed);
}

#[test]
fn changed_pin_failure_returns_pin_error_and_keeps_pressed_state() {
    let pin = ScriptedPin {
        fail_when_exhausted: true,
        ..ScriptedPin::new(&[LOW])
    };
    let mut button = DebouncedButton::new(pin, RecordingDelay::default(), PinState::Low, 5);
    block_on(button.changed()).unwrap();

    let result = block_on(button.changed());

    assert_that!(result.err()).is_equal_to(Some(ErrorKind::Other));
    assert_that!(button.is_pressed()).is_true();
}

#[test]
fn sync_state_low_pin_reports_held_button_only_when_released() {
    let mut button = button(&[LOW, HIGH]);

    let held = button.sync_state();
    let event = block_on(button.changed());

    assert_that!(held).is_ok().is_true();
    assert_that!(event)
        .is_ok()
        .is_equal_to(ButtonEvent::Released);
    assert_that!(button.release().0.waits).is_equal_to(vec![PinState::High]);
}

#[test]
fn sync_state_high_pin_after_held_reports_released() {
    let mut button = button(&[LOW, HIGH]);
    button.sync_state().unwrap();

    let held = button.sync_state();

    assert_that!(held).is_ok().is_false();
    assert_that!(button.is_pressed()).is_false();
}

#[test]
fn sync_state_pin_failure_returns_pin_error_and_keeps_state() {
    let pin = ScriptedPin {
        fail_when_exhausted: true,
        ..ScriptedPin::new(&[LOW])
    };
    let mut button = DebouncedButton::new(pin, RecordingDelay::default(), PinState::Low, 5);
    button.sync_state().unwrap();

    let result = button.sync_state();

    assert_that!(result.err()).is_equal_to(Some(ErrorKind::Other));
    assert_that!(button.is_pressed()).is_true();
}

// Regression (t-83): progress follows completed debounce samples.
#[test]
fn changed_progress_reports_sample_and_error() {
    let mut button = button(&[HIGH, LOW]);
    let mut checkpoints = 0;
    assert_eq!(
        block_on(button.changed_with_progress(|| checkpoints += 1)),
        Ok(ButtonEvent::Pressed)
    );
    assert_eq!(checkpoints, 2);
    let pin = ScriptedPin {
        fail_when_exhausted: true,
        ..ScriptedPin::new(&[])
    };
    let mut button =
        DebouncedButton::new(pin, RecordingDelay::default(), PinState::Low, DEBOUNCE_MS);
    assert!(block_on(button.changed_with_progress(|| checkpoints += 1)).is_err());
    assert_eq!(checkpoints, 3);
}

// Regression (t-83): idle timer samples GPIO before reporting progress.
#[test]
fn changed_progress_idle_samples_level_then_preserves_debounce() {
    let pin = ScriptedPin {
        pend_wait: Some(0),
        ..ScriptedPin::new(&[HIGH, LOW])
    };
    let mut button = DebouncedButton::new(pin, RecordingDelay::default(), PinState::Low, 400);
    let mut checkpoints = 0;
    assert_eq!(
        block_on(button.changed_with_progress(|| checkpoints += 1)),
        Ok(ButtonEvent::Pressed)
    );
    assert_eq!(checkpoints, 2);
    assert_eq!(button.release().1.delays_ms, vec![250, 400]);
}

// Regression (t-83): even a debounce longer than the heartbeat is uninterrupted.
#[test]
fn changed_progress_does_not_interrupt_debounce() {
    let mut button = DebouncedButton::new(
        ScriptedPin::new(&[LOW]),
        RecordingDelay::default(),
        PinState::Low,
        400,
    );
    let mut checkpoints = 0;
    assert_eq!(
        block_on(button.changed_with_progress(|| checkpoints += 1)),
        Ok(ButtonEvent::Pressed)
    );
    assert_eq!(checkpoints, 1);
    assert_eq!(button.release().1.delays_ms, vec![400]);
}

// Regression (t-83): polling an unfinished debounce is not progress.
#[test]
fn changed_progress_pending_debounce_does_not_checkpoint() {
    let delay = RecordingDelay {
        pend_next: true,
        ..RecordingDelay::default()
    };
    let mut button = DebouncedButton::new(ScriptedPin::new(&[LOW]), delay, PinState::Low, 400);
    let mut checkpoints = 0;
    assert!(!poll_once(
        button.changed_with_progress(|| checkpoints += 1)
    ));
    assert_eq!(checkpoints, 0);
    assert!(!button.is_pressed());
}
