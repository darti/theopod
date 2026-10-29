//! Input task progress policy, independent of clocks, executors and HALs.

/// Input task whose bounded operation must keep completing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum InputTask {
    /// BOOT button sampling.
    Button,
    /// Touch controller reads and recovery.
    Touch,
    /// Accelerometer sampling (only when present).
    Orientation,
}

/// Tracks enabled inputs and latches the first missing progress until reboot.
#[derive(Debug)]
pub struct InputWatch {
    stale_ms: u64,
    last: [Option<u64>; 3],
    fault: Option<InputTask>,
    stopping: bool,
}

impl InputWatch {
    /// Creates an inactive watch; enable tasks before spawning them.
    pub const fn new(stale_ms: u64) -> Self {
        Self {
            stale_ms,
            last: [None; 3],
            fault: None,
            stopping: false,
        }
    }

    /// Enables a task with one full grace interval.
    pub fn enable(&mut self, task: InputTask, now_ms: u64) {
        self.last[task as usize] = Some(now_ms);
    }

    /// Records a completed bounded operation, including a returned error.
    pub fn progress(&mut self, task: InputTask, now_ms: u64) {
        if let Some(last) = &mut self.last[task as usize] {
            *last = now_ms;
        }
    }

    /// Suspends new checks for the bounded stop handshake and parked inputs.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// Rebases enabled inputs after a known executor-blocking diagnostic pause.
    /// An already latched failure remains latched.
    pub fn rebase(&mut self, now_ms: u64) {
        for last in self.last.iter_mut().flatten() {
            *last = now_ms;
        }
    }

    /// Returns the first stale task; the exact stale boundary is inclusive.
    pub fn check(&mut self, now_ms: u64) -> Option<InputTask> {
        if self.fault.is_none() && !self.stopping {
            self.fault = [InputTask::Button, InputTask::Touch, InputTask::Orientation]
                .into_iter()
                .find(|task| {
                    self.last[*task as usize]
                        .is_some_and(|last| now_ms.saturating_sub(last) >= self.stale_ms)
                });
        }
        self.fault
    }
}
