use core::sync::atomic::Ordering;

use super::{Admit, HEAL_MS, TxGate};

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
