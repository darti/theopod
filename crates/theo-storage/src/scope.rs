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

/// How the storage owner answers a failed request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyKind {
    /// Pass the request's error through: the caller sees a failed request.
    Error,
    /// Reply that SD logs are offline; the request itself completed.
    LogsOffline,
    /// Reply that podcasts are offline; the request itself completed.
    PodcastsOffline,
}

/// What the storage owner does after a request failed in `scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effect {
    /// Drop the log volume.
    pub drop_logs: bool,
    /// Drop the podcast catalogue and media store.
    pub drop_podcasts: bool,
    /// Poison the shared sector bus (and drop the unmounted card).
    pub disable_bus: bool,
    /// The reply to send.
    pub reply: ReplyKind,
}

/// The owner's state transition for a failure in `scope`: a bus failure
/// disables everything and fails the request; a scope failure drops only
/// that scope and completes the request with an offline reply.
pub fn after_failure(scope: Scope) -> Effect {
    match scope {
        Scope::Bus => Effect {
            drop_logs: true,
            drop_podcasts: true,
            disable_bus: true,
            reply: ReplyKind::Error,
        },
        Scope::Logs => Effect {
            drop_logs: true,
            drop_podcasts: false,
            disable_bus: false,
            reply: ReplyKind::LogsOffline,
        },
        Scope::Podcasts => Effect {
            drop_logs: false,
            drop_podcasts: true,
            disable_bus: false,
            reply: ReplyKind::PodcastsOffline,
        },
    }
}

/// The owner stops this long before a request's deadline, so its error reply
/// reaches the caller before the caller's own deadline drops its guard.
pub const OWNER_MARGIN_MS: u64 = 20;

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
