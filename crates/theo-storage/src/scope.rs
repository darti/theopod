//! Which storage scope a failed request disables (t-121, ADR-0020).
//!
//! Bus-level faults (card or HAL errors, an aborted in-flight sector RPC,
//! caller-side cancellation) poison the shared sector bus and disable every
//! scope until reboot. Any other failure, such as a LittleFS or EKV error or
//! the owner's soft deadline passing between sector RPCs, disables only the
//! scope of the request that hit it.

/// What a failed request disables until reboot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The shared sector bus: logs and podcasts together.
    Bus,
    /// The log LittleFS volume only.
    Logs,
    /// The podcast catalogue and media store only.
    Podcasts,
}

/// The scope a storage owner request runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    /// Descriptor discovery and the mounts of both scopes.
    Mount,
    /// Log append, tail and rotation checks.
    Log,
    /// Podcast domain requests.
    Podcast,
}

/// A poisoned bus wins; otherwise a failure stays in its request's scope.
pub fn failure_scope(bus_poisoned: bool, kind: RequestKind) -> Scope {
    match (bus_poisoned, kind) {
        (true, _) | (false, RequestKind::Mount) => Scope::Bus,
        (false, RequestKind::Log) => Scope::Logs,
        (false, RequestKind::Podcast) => Scope::Podcasts,
    }
}

/// The owner stops this long before a request's deadline, so its error reply
/// reaches the caller before the caller's own deadline drops its guard.
pub const OWNER_MARGIN_MS: u64 = 20;

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
