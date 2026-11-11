use super::*;

// Regression (t-121): a poisoned bus disables every scope, whatever the request.
#[test]
fn poisoned_bus_wins_over_the_request_scope() {
    for kind in [RequestKind::Mount, RequestKind::Log, RequestKind::Podcast] {
        assert_eq!(failure_scope(true, kind), Scope::Bus);
    }
}

// Regression (t-121): a failed mount still disables the bus (descriptors are bus scope).
#[test]
fn failed_mount_disables_the_bus() {
    assert_eq!(failure_scope(false, RequestKind::Mount), Scope::Bus);
}

// Regression (t-121): a failed log request leaves podcasts running.
#[test]
fn failed_log_request_disables_only_logs() {
    assert_eq!(failure_scope(false, RequestKind::Log), Scope::Logs);
}

// Regression (t-121): a failed podcast request leaves SD logging running.
#[test]
fn failed_podcast_request_disables_only_podcasts() {
    assert_eq!(failure_scope(false, RequestKind::Podcast), Scope::Podcasts);
}
