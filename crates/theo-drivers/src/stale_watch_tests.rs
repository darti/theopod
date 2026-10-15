use speculoos::prelude::*;

use crate::stale_watch::{StaleWatch, WatchAction, decode_resets, encode_resets};

const LIMIT_MS: u64 = 10_000;
const HEALTHY_MS: u64 = 60_000;
const MAX: u32 = 3;

fn watch() -> StaleWatch {
    StaleWatch::new(LIMIT_MS, HEALTHY_MS, MAX)
}

#[test]
fn check_stale_below_the_limit_waits() {
    let mut watch = watch();
    let _ = watch.check(0, true, 0);

    assert_that!(watch.check(LIMIT_MS - 1, true, 0)).is_equal_to(WatchAction::None);
}

#[test]
fn check_stale_for_the_limit_reboots() {
    let mut watch = watch();
    let _ = watch.check(5_000, true, 0);

    assert_that!(watch.check(5_000 + LIMIT_MS, true, 2)).is_equal_to(WatchAction::Reboot);
}

#[test]
fn check_recovery_restarts_the_stale_timer() {
    let mut watch = watch();
    let _ = watch.check(0, true, 0);
    let _ = watch.check(9_000, false, 0);
    let _ = watch.check(9_500, true, 0);

    assert_that!(watch.check(9_500 + LIMIT_MS - 1, true, 0)).is_equal_to(WatchAction::None);
}

// Regression guard (t-44): a cause that survives reboots must not loop.
#[test]
fn check_after_max_resets_gives_up_once() {
    let mut watch = watch();
    let _ = watch.check(0, true, MAX);

    let first = watch.check(LIMIT_MS, true, MAX);
    let second = watch.check(LIMIT_MS + 1_000, true, MAX);

    assert_that!(first).is_equal_to(WatchAction::GiveUp);
    assert_that!(second).is_equal_to(WatchAction::None);
}

#[test]
fn check_gives_up_again_after_a_recovery() {
    let mut watch = watch();
    let _ = watch.check(0, true, MAX);
    let _ = watch.check(LIMIT_MS, true, MAX);
    let _ = watch.check(LIMIT_MS + 1, false, MAX);
    let _ = watch.check(LIMIT_MS + 2, true, MAX);

    assert_that!(watch.check(2 * LIMIT_MS + 2, true, MAX)).is_equal_to(WatchAction::GiveUp);
}

#[test]
fn check_healthy_for_the_period_clears_the_resets() {
    let mut watch = watch();
    let _ = watch.check(1_000, false, 2);

    assert_that!(watch.check(1_000 + HEALTHY_MS, false, 2)).is_equal_to(WatchAction::ClearResets);
}

#[test]
fn check_healthy_without_resets_clears_nothing() {
    let mut watch = watch();
    let _ = watch.check(0, false, 0);

    assert_that!(watch.check(HEALTHY_MS, false, 0)).is_equal_to(WatchAction::None);
}

// Regression (t-44 review): the count cleared after a minute of uptime,
// so a fault coming back after a minute rebooted the device forever.
#[test]
fn check_stale_period_restarts_the_healthy_period() {
    let mut watch = watch();
    let _ = watch.check(0, false, 2);
    let _ = watch.check(50_000, true, 2);
    let _ = watch.check(55_000, false, 2);

    assert_that!(watch.check(55_000 + HEALTHY_MS - 1, false, 2)).is_equal_to(WatchAction::None);
    assert_that!(watch.check(55_000 + HEALTHY_MS, false, 2)).is_equal_to(WatchAction::ClearResets);
}

#[test]
fn decode_resets_reads_back_what_was_encoded() {
    assert_that!(decode_resets(encode_resets(2))).is_equal_to(2);
}

#[test]
fn decode_resets_random_contents_read_as_zero() {
    assert_that!(decode_resets([0xDEAD_BEEF, 0x1234_5678])).is_equal_to(0);
}

#[test]
fn decode_resets_one_corrupted_word_reads_as_zero() {
    let [count, check] = encode_resets(2);

    assert_that!(decode_resets([count ^ 1, check])).is_equal_to(0);
}

#[test]
fn decode_resets_all_zero_reads_as_zero() {
    assert_that!(decode_resets([0, 0])).is_equal_to(0);
}
