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

// Regression (t-121): a podcast (domain) overrun drops only podcasts; logs
// stay mounted and the request completes as podcasts-offline.
#[test]
fn domain_overrun_leaves_logs_usable() {
    let effect = after_failure(failure_scope(false, RequestKind::Podcast));
    assert_eq!(
        effect,
        Effect {
            drop_logs: false,
            drop_podcasts: true,
            disable_bus: false,
            reply: ReplyKind::PodcastsOffline,
        }
    );
}

// Regression (t-121): a log failure drops only logs; podcasts stay mounted.
#[test]
fn log_failure_leaves_podcasts_usable() {
    let effect = after_failure(failure_scope(false, RequestKind::Log));
    assert_eq!(
        effect,
        Effect {
            drop_logs: true,
            drop_podcasts: false,
            disable_bus: false,
            reply: ReplyKind::LogsOffline,
        }
    );
}

// Regression (t-121): a bus failure (or any failure once the bus is
// poisoned) disables everything and fails the request.
#[test]
fn bus_failure_disables_everything() {
    for kind in [RequestKind::Mount, RequestKind::Log, RequestKind::Podcast] {
        let effect = after_failure(failure_scope(true, kind));
        assert_eq!(
            effect,
            Effect {
                drop_logs: true,
                drop_podcasts: true,
                disable_bus: true,
                reply: ReplyKind::Error,
            }
        );
    }
    assert_eq!(
        after_failure(failure_scope(false, RequestKind::Mount)).reply,
        ReplyKind::Error
    );
}
