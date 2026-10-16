use speculoos::prelude::*;

use crate::{
    co5300::Error as Co5300Error,
    framebuffer::FlushError,
    retry::{FlushRetry, Retry, Transient, reset_due},
    test_support::BusFault,
};

const BASE_MS: u32 = 16;
const MAX_MS: u32 = 1000;

fn retry() -> FlushRetry {
    FlushRetry::new(BASE_MS, MAX_MS)
}

#[test]
fn new_retry_is_not_stale() {
    assert_that!(retry().is_stale()).is_false();
}

#[test]
fn failed_transient_logs_and_retries_after_base_delay() {
    let mut retry = retry();

    let decision = retry.failed(true);

    assert_that!(decision).is_equal_to(Retry {
        log: true,
        after_ms: Some(BASE_MS),
    });
    assert_that!(retry.is_stale()).is_true();
}

#[test]
fn failed_transient_repeatedly_backs_off_silently_up_to_the_cap() {
    let mut retry = retry();
    let _ = retry.failed(true);

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
fn failed_transient_long_streak_keeps_the_cap() {
    let mut retry = retry();
    for _ in 0..100 {
        let _ = retry.failed(true);
    }

    assert_that!(retry.failed(true).after_ms).is_equal_to(Some(MAX_MS));
}

// Regression (t-1, policy half): deterministic errors were resent every
// 16 ms forever, flooding the log and keeping the loop awake.
#[test]
fn failed_permanent_logs_once_and_stops_retrying() {
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
fn failed_other_kind_is_logged_again() {
    let mut retry = retry();
    let _ = retry.failed(true);

    let permanent = retry.failed(false);

    assert_that!(permanent.log).is_true();
}

#[test]
fn succeeded_after_failures_reports_them_and_clears_staleness() {
    let mut retry = retry();
    let _ = retry.failed(true);
    let _ = retry.failed(true);

    let recovered = retry.succeeded();

    assert_that!(recovered).is_equal_to(Some(2));
    assert_that!(retry.is_stale()).is_false();
    assert_that!(retry.failed(true)).is_equal_to(Retry {
        log: true,
        after_ms: Some(BASE_MS),
    });
}

#[test]
fn succeeded_without_failures_reports_nothing() {
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
fn co5300_frame_size_mismatch_is_permanent() {
    let error = Co5300Error::<BusFault>::FrameSizeMismatch {
        expected: 8,
        actual: 6,
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

#[test]
fn reset_due_before_the_first_threshold_is_false() {
    assert_that!((0..8).any(|failures| reset_due(failures, 8, 64))).is_false();
}

#[test]
fn reset_due_doubles_then_repeats_at_the_cap() {
    let due: Vec<u32> = (0..=200)
        .filter(|&failures| reset_due(failures, 8, 64))
        .collect();

    assert_that!(due).is_equal_to(vec![8, 16, 32, 64, 128, 192]);
}

#[test]
fn reset_due_far_into_a_streak_keeps_the_cap() {
    assert_that!(reset_due(64 * 1000, 8, 64)).is_true();
    assert_that!(reset_due(64 * 1000 + 1, 8, 64)).is_false();
}

// Regression (t-1 review): a permanent streak inflated the next transient
// delay, and alternating kinds were logged on every failure.
#[test]
fn failed_kind_change_restarts_the_backoff_and_logs_each_kind_once() {
    let mut retry = retry();
    let _ = retry.failed(true);
    let _ = retry.failed(true);

    let decisions: Vec<_> = [false, true, false, true]
        .into_iter()
        .map(|transient| retry.failed(transient))
        .collect();

    assert_that!(decisions).is_equal_to(vec![
        Retry {
            log: true,
            after_ms: None,
        },
        Retry {
            log: false,
            after_ms: Some(BASE_MS),
        },
        Retry {
            log: false,
            after_ms: None,
        },
        Retry {
            log: false,
            after_ms: Some(BASE_MS),
        },
    ]);
}

#[test]
fn succeeded_after_permanent_failure_reports_it() {
    let mut retry = retry();
    let _ = retry.failed(false);

    assert_that!(retry.succeeded()).is_equal_to(Some(1));
}

#[test]
fn failed_transient_with_cap_below_base_waits_the_cap() {
    let mut retry = FlushRetry::new(BASE_MS, BASE_MS);
    let _ = retry.failed(true);

    assert_that!(retry.failed(true).after_ms).is_equal_to(Some(BASE_MS));
}

#[test]
fn co5300_invalid_geometry_is_permanent() {
    let error = Co5300Error::<BusFault>::InvalidGeometry {
        geometry: crate::co5300::PanelGeometry {
            width: 1,
            height: 2,
            column_offset: 0,
        },
    };

    assert_that!(error.is_transient()).is_false();
}

#[test]
fn co5300_bus_fault_is_transient() {
    assert_that!(Co5300Error::Bus(BusFault).is_transient()).is_true();
}
