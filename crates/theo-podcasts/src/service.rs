//! Host-testable admission and fairness for the firmware's background services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    Control,
    Log,
    Data,
}
/// Invoke between bounded storage work units, never around an entire library
/// traversal. Control has priority; continuously pending logs/data alternate.
#[derive(Default)]
pub struct Scheduler {
    prefer_data: bool,
}
impl Scheduler {
    pub fn next(&mut self, control: bool, logs: bool, data: bool) -> Option<Work> {
        if control {
            return Some(Work::Control);
        }
        if logs && (!data || !self.prefer_data) {
            self.prefer_data = true;
            return Some(Work::Log);
        }
        if data {
            self.prefer_data = false;
            return Some(Work::Data);
        }
        None
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    Sync,
    Download,
}
/// One network job, coalesced sync triggers and a capacity gate independent of
/// connectivity. Failure/stop are permanent for this boot's owner instance.
#[derive(Default)]
pub struct Admission {
    configured: bool,
    storage: bool,
    network: bool,
    failed: bool,
    stopped: bool,
    active: Option<Job>,
    pending_sync: bool,
    waiting: bool,
}
impl Admission {
    pub fn ready(&mut self, configured: bool, storage: bool, network: bool) {
        self.configured = configured;
        self.storage = storage;
        self.network = network;
    }
    pub fn request_sync(&mut self) {
        self.pending_sync = true;
    }
    pub fn begin(&mut self, job: Job) -> bool {
        if !self.configured
            || !self.storage
            || !self.network
            || self.failed
            || self.stopped
            || self.active.is_some()
        {
            return false;
        }
        match job {
            Job::Sync => {
                if !self.pending_sync {
                    return false;
                }
                self.pending_sync = false;
            }
            Job::Download => {
                if self.waiting || self.pending_sync {
                    return false;
                }
            }
        }
        self.active = Some(job);
        true
    }
    pub fn finish(&mut self, waiting_for_space: bool) {
        if self.active == Some(Job::Download) {
            self.waiting = waiting_for_space;
        }
        self.active = None;
    }
    /// Mark-read, released active lease, increased free space or explicit retry.
    /// Ordinary ticks/reconnects deliberately do not call this method.
    pub fn space_changed(&mut self) {
        self.waiting = false;
    }
    /// Caller cancels any active socket/future before bounded final flush.
    pub fn stop(&mut self) -> bool {
        self.stopped = true;
        self.active.take().is_some()
    }
    pub fn storage_failed(&mut self) {
        self.failed = true;
        self.active = None;
    }
}
#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
