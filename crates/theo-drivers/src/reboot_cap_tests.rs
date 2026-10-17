use speculoos::prelude::*;

use crate::reboot_cap::{RebootCap, StartAction, StartDecision};

const MAX: u32 = 3;
const HEALTHY_MS: u64 = 60_000;

fn cap() -> RebootCap {
    RebootCap::new(MAX, HEALTHY_MS)
}

fn decision(action: StartAction, reboots: u32) -> StartDecision {
    StartDecision { action, reboots }
}

#[test]
fn on_start_power_on_runs_with_no_count() {
    assert_that!(cap().on_start(false, 0)).is_equal_to(decision(StartAction::Run, 0));
}

#[test]
fn on_start_watchdog_reset_counts_it() {
    assert_that!(cap().on_start(true, 0)).is_equal_to(decision(StartAction::Run, 1));
}

#[test]
fn on_start_other_reset_keeps_the_count() {
    assert_that!(cap().on_start(false, 2)).is_equal_to(decision(StartAction::Run, 2));
}

#[test]
fn on_start_other_reset_above_the_cap_runs_and_keeps_it() {
    assert_that!(cap().on_start(false, MAX + 5)).is_equal_to(decision(StartAction::Run, MAX + 5));
}

#[test]
fn on_start_reaching_the_cap_stops_and_keeps_the_count() {
    assert_that!(cap().on_start(true, MAX - 1)).is_equal_to(decision(StartAction::Stop, MAX));
}

#[test]
fn on_start_after_an_unfinished_stop_stops_without_parking() {
    assert_that!(cap().on_start(true, MAX))
        .is_equal_to(decision(StartAction::StopWithoutParking, MAX + 1));
}

#[test]
fn on_start_count_at_the_top_stops_without_overflow() {
    assert_that!(cap().on_start(true, u32::MAX))
        .is_equal_to(decision(StartAction::StopWithoutParking, u32::MAX));
}

#[test]
fn on_start_cap_of_one_stops_at_the_first_watchdog_reset() {
    let cap = RebootCap::new(1, HEALTHY_MS);

    assert_that!(cap.on_start(true, 0)).is_equal_to(decision(StartAction::Stop, 1));
}

#[test]
#[should_panic(expected = "at least one reboot")]
fn new_cap_of_zero_panics() {
    let _ = RebootCap::new(0, HEALTHY_MS);
}

// Regression (t-66): a hang that repeats after every boot rebooted every
// 5 s forever; the cap-th watchdog start in a row must stop instead, and
// a stop that hangs in turn must not start the count over.
#[test]
fn on_start_repeated_watchdog_resets_stop_at_the_cap() {
    let cap = cap();
    let mut reboots = 0;
    let mut actions = Vec::new();
    for _ in 0..=MAX {
        let start = cap.on_start(true, reboots);
        reboots = start.reboots;
        actions.push(start.action);
    }

    let mut expected = vec![StartAction::Run; MAX as usize - 1];
    expected.extend([StartAction::Stop, StartAction::StopWithoutParking]);
    assert_that!(actions).is_equal_to(expected);
}

#[test]
fn should_clear_before_the_healthy_time_keeps_the_count() {
    assert_that!(cap().should_clear(HEALTHY_MS - 1, 2)).is_false();
}

#[test]
fn should_clear_after_the_healthy_time_forgets_the_count() {
    assert_that!(cap().should_clear(HEALTHY_MS, 2)).is_true();
}

#[test]
fn should_clear_with_no_count_does_nothing() {
    assert_that!(cap().should_clear(HEALTHY_MS, 0)).is_false();
}
