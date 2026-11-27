use core::sync::atomic::Ordering;

use super::{Admit, HEAL_MS, HealLog, REPORT_MS, TxGate};

#[test]
fn admit_below_limit_admits() {
    let gate = TxGate::new();
    gate.sent();
    assert_eq!(gate.admit(2, 0), Admit::Send);
}

#[test]
fn admit_at_limit_waits_for_done() {
    let gate = TxGate::new();
    gate.sent();
    gate.sent();
    assert_eq!(gate.admit(2, 10), Admit::Hold);
    gate.done();
    assert_eq!(gate.admit(2, 11), Admit::Send);
}

// Regression (t-172): the Wi-Fi blob sometimes skips a tx-done callback. The
// in-flight count then stuck at the limit and blocked TX and RX until reset.
#[test]
fn admit_blocked_past_heal_time_resets_leaked_count() {
    let gate = TxGate::new();
    gate.sent();
    gate.sent();
    assert_eq!(gate.admit(2, 1_000), Admit::Hold);
    assert_eq!(gate.admit(2, 1_000 + HEAL_MS - 1), Admit::Hold);
    assert_eq!(gate.admit(2, 1_000 + HEAL_MS), Admit::Healed);
    assert_eq!(gate.inflight.load(Ordering::SeqCst), 0);
}

#[test]
fn admit_done_while_blocked_restarts_heal_time() {
    let gate = TxGate::new();
    for _ in 0..3 {
        gate.sent();
    }
    assert_eq!(gate.admit(2, 0), Admit::Hold);
    gate.done();
    assert_eq!(gate.admit(2, HEAL_MS), Admit::Hold);
    assert_eq!(gate.admit(2, 2 * HEAL_MS - 1), Admit::Hold);
    assert_eq!(gate.admit(2, 2 * HEAL_MS), Admit::Healed);
}

#[test]
fn admit_heal_time_survives_clock_wrap() {
    let gate = TxGate::new();
    gate.sent();
    gate.sent();
    let start = u32::MAX - 10;
    assert_eq!(gate.admit(2, start), Admit::Hold);
    assert_eq!(gate.admit(2, start.wrapping_add(HEAL_MS - 1)), Admit::Hold);
    assert_eq!(gate.admit(2, start.wrapping_add(HEAL_MS)), Admit::Healed);
}

#[test]
fn done_without_send_stays_at_zero() {
    let gate = TxGate::new();
    gate.done();
    assert_eq!(gate.inflight.load(Ordering::SeqCst), 0);
}

// Regression (t-172 review): a done callback racing `admit` (count read at
// the limit, then the callback lowers it and clears the hold, then `admit`
// stores the hold start) left a stale hold start. The next time the count
// reached the limit, the gate healed at once instead of waiting `HEAL_MS`.
#[test]
fn admit_below_limit_after_racing_done_clears_stale_hold() {
    let gate = TxGate::new();
    gate.sent();
    gate.sent();
    assert_eq!(gate.admit(2, 1), Admit::Hold);
    // The callback's decrement, with its clear lost to the race.
    gate.inflight.fetch_sub(1, Ordering::SeqCst);
    assert_eq!(gate.admit(2, 500), Admit::Send);
    gate.sent();
    assert_eq!(gate.admit(2, 600), Admit::Hold);
}

#[test]
fn admit_hold_started_at_zero_still_heals() {
    let gate = TxGate::new();
    gate.sent();
    gate.sent();
    assert_eq!(gate.admit(2, 0), Admit::Hold);
    assert_eq!(gate.admit(2, HEAL_MS), Admit::Hold);
    assert_eq!(gate.admit(2, HEAL_MS + 1), Admit::Healed);
}

#[test]
fn admit_after_heal_sends_and_times_the_next_hold_afresh() {
    let gate = TxGate::new();
    gate.sent();
    gate.sent();
    assert_eq!(gate.admit(2, 10), Admit::Hold);
    assert_eq!(gate.admit(2, 10 + HEAL_MS), Admit::Healed);
    assert_eq!(gate.admit(2, 10 + HEAL_MS), Admit::Send);
    gate.sent();
    gate.sent();
    assert_eq!(gate.admit(2, 1_000), Admit::Hold);
    assert_eq!(gate.admit(2, 1_000 + HEAL_MS - 1), Admit::Hold);
}

// Regression (t-179): the heal warning fired 54 times in 16 minutes of
// download and could flood the serial and SD logs.
#[test]
fn heal_log_reports_first_heal_at_once() {
    let log = HealLog::new();
    assert_eq!(log.record(5_000), Some(1));
}

// Regression (t-179): heals inside a window are counted, not logged.
#[test]
fn heal_log_aggregates_heals_within_the_window() {
    let log = HealLog::new();
    let t0 = 1_000;
    assert_eq!(log.record(t0), Some(1));
    for t in [t0 + 1, t0 + 1_000, t0 + REPORT_MS - 1] {
        assert_eq!(log.record(t), None);
    }
    // The next heal after the window reports every heal since the last report.
    assert_eq!(log.record(t0 + REPORT_MS), Some(4));
    assert_eq!(log.record(t0 + REPORT_MS + 1), None);
    assert_eq!(log.record(t0 + 2 * REPORT_MS), Some(2));
}

// Regression (t-179): the observed flood, 54 heals over 16 minutes.
#[test]
fn heal_log_flood_logs_about_one_line_per_minute() {
    let log = HealLog::new();
    let (mut lines, mut reported) = (0, 0);
    for i in 0..54u32 {
        if let Some(n) = log.record(1 + i * 960_000 / 54) {
            lines += 1;
            reported += n;
        }
    }
    assert!(lines <= 17, "{lines} lines");
    assert_eq!(reported + log.unreported(), 54);
}

// Regression (t-179): racing callers must not lose, double-count or wrap
// the pending count (review of t-179).
#[test]
fn heal_log_racing_callers_report_each_heal_once() {
    use std::{sync::Arc, thread, vec::Vec};

    let log = Arc::new(HealLog::new());
    let threads: Vec<_> = (0..8)
        .map(|k| {
            let log = Arc::clone(&log);
            thread::spawn(move || {
                (0..1_000u32)
                    .filter_map(|i| log.record(1 + (k + i * 8) * 97))
                    .map(u64::from)
                    .sum::<u64>()
            })
        })
        .collect();
    let reported: u64 = threads.into_iter().map(|t| t.join().expect("recorder thread panicked")).sum();
    assert_eq!(reported + u64::from(log.unreported()), 8_000);
}

#[test]
fn heal_log_survives_clock_wrap() {
    let log = HealLog::new();
    let start = u32::MAX - 10;
    assert_eq!(log.record(start), Some(1));
    assert_eq!(log.record(start.wrapping_add(REPORT_MS - 1)), None);
    assert_eq!(log.record(start.wrapping_add(REPORT_MS)), Some(2));
}

// The window start uses the `NOT_HELD` sentinel: a first heal at 0 must
// still open a window.
#[test]
fn heal_log_first_heal_at_time_zero_opens_a_window() {
    let log = HealLog::new();
    assert_eq!(log.record(0), Some(1));
    assert_eq!(log.record(10), None);
}
