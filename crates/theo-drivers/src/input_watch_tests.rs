use crate::{InputTask, InputWatch};

// Regression (t-83): inactivity is valid; only missing bounded progress fails.
#[test]
fn initial_grace_and_exact_boundary() {
    let mut watch = InputWatch::new(3000);
    watch.enable(InputTask::Button, 100);
    assert_eq!(watch.check(3099), None);
    assert_eq!(watch.check(3100), Some(InputTask::Button));
}

#[test]
fn each_enabled_input_can_stall_while_others_progress() {
    for task in [InputTask::Button, InputTask::Touch, InputTask::Orientation] {
        let mut watch = InputWatch::new(3000);
        for other in [InputTask::Button, InputTask::Touch, InputTask::Orientation] {
            watch.enable(other, 0);
            if other != task {
                watch.progress(other, 2999);
            }
        }
        assert_eq!(watch.check(3000), Some(task));
    }
}

#[test]
fn absent_imu_and_disabled_entries_never_fail() {
    let mut watch = InputWatch::new(3000);
    assert_eq!(watch.check(10000), None);
    watch.enable(InputTask::Button, 10000);
    watch.enable(InputTask::Touch, 10000);
    assert_eq!(watch.check(12999), None);
}

#[test]
fn late_progress_rebase_and_stop_do_not_clear_latched_fault() {
    let mut watch = InputWatch::new(3000);
    watch.enable(InputTask::Touch, 0);
    assert_eq!(watch.check(3000), Some(InputTask::Touch));
    watch.progress(InputTask::Touch, 3001);
    watch.rebase(4000);
    watch.stop();
    assert_eq!(watch.check(4000), Some(InputTask::Touch));
}

#[test]
fn stop_suspends_checks_and_rebase_grants_only_one_grace_period() {
    let mut watch = InputWatch::new(3000);
    watch.enable(InputTask::Button, 0);
    watch.rebase(10000);
    assert_eq!(watch.check(12999), None);
    assert_eq!(watch.check(13000), Some(InputTask::Button));
    let mut watch = InputWatch::new(3000);
    watch.enable(InputTask::Button, 0);
    watch.stop();
    assert_eq!(watch.check(100000), None);
}
