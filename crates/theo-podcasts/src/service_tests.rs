use super::*;
#[test]
fn continuous_data_cannot_starve_logs_or_control() {
    // Regression (t-110): each bounded data unit gives logs/control an admission point.
    let mut scheduler = Scheduler::default();
    assert_eq!(scheduler.next(true, true, true), Some(Work::Control));
    assert_eq!(scheduler.next(false, true, true), Some(Work::Log));
    assert_eq!(scheduler.next(false, true, true), Some(Work::Data));
    assert_eq!(scheduler.next(false, true, true), Some(Work::Log));
    assert_eq!(scheduler.next(true, true, true), Some(Work::Control));
    assert_eq!(scheduler.next(false, true, true), Some(Work::Data));
    assert_eq!(scheduler.next(false, false, false), None);
}
#[test]
fn offline_boot_and_duplicate_sync_triggers_never_start_parallel_jobs() {
    // Regression (t-110): missing setup/network stays offline; reconnect coalesces work.
    let mut state = Admission::default();
    state.request_sync();
    state.ready(false, true, true);
    assert!(!state.begin(Job::Sync));
    state.ready(true, true, false);
    assert!(!state.begin(Job::Sync));
    state.ready(true, true, true);
    assert!(state.begin(Job::Sync));
    state.request_sync();
    state.request_sync();
    assert!(!state.begin(Job::Sync));
    assert!(!state.begin(Job::Download));
    state.finish(false);
    assert!(state.begin(Job::Sync));
    state.finish(false);
    assert!(!state.begin(Job::Sync));
    assert!(state.begin(Job::Download));
    assert!(!state.begin(Job::Download));
}
#[test]
fn capacity_wait_requires_relevant_change_and_sleep_or_storage_failure_cancels_work() {
    // Regression (t-110): full media cannot spin on timer/reconnect, or block shutdown.
    let mut state = Admission::default();
    state.ready(true, true, true);
    assert!(state.begin(Job::Download));
    state.finish(true);
    assert!(!state.begin(Job::Download));
    state.ready(true, true, false);
    state.ready(true, true, true);
    state.request_sync();
    assert!(state.begin(Job::Sync));
    state.finish(false);
    assert!(!state.begin(Job::Download));
    state.space_changed();
    assert!(state.begin(Job::Download));
    assert!(state.stop());
    assert!(!state.begin(Job::Download));
    state.ready(true, true, true);
    assert!(!state.begin(Job::Sync));
    let mut state = Admission::default();
    state.ready(true, true, true);
    assert!(state.begin(Job::Download));
    state.storage_failed();
    state.ready(true, true, true);
    state.request_sync();
    assert!(!state.begin(Job::Sync));
    assert!(!state.begin(Job::Download));
}
