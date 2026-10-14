use speculoos::prelude::*;

use crate::{
    co5300::Error as Co5300Error,
    framebuffer::FlushError,
    retry::{FlushRetry, Retry, Transient},
    test_support::BusFault,
};

const BASE_MS: u32 = 16;
const MAX_MS: u32 = 1000;

fn retry() -> FlushRetry {
    FlushRetry::new(BASE_MS, MAX_MS)
}

#[test]
fn new_is_not_stale() {
    assert_that!(retry().is_stale()).is_false();
}

#[test]
fn transient_failure_logs_and_retries_after_base_delay() {
    let mut retry = retry();

    let decision = retry.failed(true);

    assert_that!(decision).is_equal_to(Retry {
        log: true,
        after_ms: Some(BASE_MS),
    });
    assert_that!(retry.is_stale()).is_true();
}

#[test]
fn repeated_transient_failures_back_off_silently_up_to_the_cap() {
    let mut retry = retry();
    retry.failed(true);

    let delays: Vec<_> = (0..8).map(|_| retry.failed(true)).collect();

    assert_that!(delays.iter().any(|decision| decision.log)).is_false();
    let delays: Vec<_> = delays.iter().map(|decision| decision.after_ms).collect();
    assert_that!(delays).is_equal_to(vec![
        Some(32),
        Some(64),
        Some(128),
        Some(256),
        Some(512),
        Some(MAX_MS),
        Some(MAX_MS),
        Some(MAX_MS),
    ]);
}

#[test]
fn long_transient_streak_keeps_the_cap() {
    let mut retry = retry();
    for _ in 0..100 {
        retry.failed(true);
    }

    assert_that!(retry.failed(true).after_ms).is_equal_to(Some(MAX_MS));
}

// Regression (t-1): deterministic errors were resent every 16 ms forever,
// flooding the log and keeping the loop awake.
#[test]
fn permanent_failure_logs_once_and_stops_retrying() {
    let mut retry = retry();

    let first = retry.failed(false);
    let second = retry.failed(false);

    assert_that!(first).is_equal_to(Retry {
        log: true,
        after_ms: None,
    });
    assert_that!(second).is_equal_to(Retry {
        log: false,
        after_ms: None,
    });
    assert_that!(retry.is_stale()).is_true();
}

#[test]
fn failure_of_another_kind_is_logged_again() {
    let mut retry = retry();
    retry.failed(true);

    let permanent = retry.failed(false);

    assert_that!(permanent.log).is_true();
}

#[test]
fn success_after_failures_reports_them_and_clears_staleness() {
    let mut retry = retry();
    retry.failed(true);
    retry.failed(true);

    let recovered = retry.succeeded();

    assert_that!(recovered).is_equal_to(Some(2));
    assert_that!(retry.is_stale()).is_false();
    assert_that!(retry.failed(true)).is_equal_to(Retry {
        log: true,
        after_ms: Some(BASE_MS),
    });
}

#[test]
fn success_without_failures_reports_nothing() {
    assert_that!(retry().succeeded()).is_none();
}

#[test]
fn flush_error_panel_bus_fault_is_transient() {
    let error = FlushError::Panel(Co5300Error::Bus(BusFault));

    assert_that!(error.is_transient()).is_true();
}

#[test]
fn flush_error_rows_out_of_range_is_permanent() {
    let error = FlushError::<Co5300Error<BusFault>>::RowsOutOfRange { rows: 0..500 };

    assert_that!(error.is_transient()).is_false();
}

#[test]
fn flush_error_frame_size_mismatch_is_permanent() {
    let error = FlushError::<Co5300Error<BusFault>>::FrameSizeMismatch {
        expected: 4,
        actual: 2,
    };

    assert_that!(error.is_transient()).is_false();
}

#[test]
fn co5300_invalid_area_is_permanent() {
    let error = Co5300Error::<BusFault>::InvalidArea { x: 0..1, y: 0..2 };

    assert_that!(error.is_transient()).is_false();
}

#[test]
fn co5300_pixel_overflow_is_permanent() {
    let error = Co5300Error::<BusFault>::PixelOverflow {
        remaining: 0,
        submitted: 2,
    };

    assert_that!(error.is_transient()).is_false();
}
