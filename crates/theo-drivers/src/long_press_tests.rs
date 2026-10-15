use speculoos::prelude::*;

use crate::{button::ButtonEvent, long_press::LongPress};

const HOLD_MS: u32 = 2000;

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
