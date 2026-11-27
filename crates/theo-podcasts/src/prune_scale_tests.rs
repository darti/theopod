//! ADR-0030 at scale: syncing past the redb failure point (~8,600 episodes)
//! never reports `Full`, and a pruning creation stays within the request
//! budget.
//!
//! Release only (`just test-catalogue-bounds`): filling thousands of episodes
//! in debug builds is slow.
use crate::{catalogue::*, test_support::Image};
use std::eprintln;

/// Past the ~8,600 episodes where the unpruned catalogue reported `Full`.
const SYNCED: u64 = 10_500;
/// A podcast request's budget (see `scale_tests`).
const REQUEST_COMMANDS: usize = 500;

fn add(cat: &mut Catalogue, i: u64, description: usize, long: bool) -> UpsertOutcome {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let guid = std::format!(
        "guid-{i}-{}",
        if long { "g".repeat(240) } else { "".into() }
    );
    let url = std::format!(
        "https://media.radiofrance-podcast.net/{i}{}.mp3",
        if long { "u".repeat(1_900) } else { "".into() }
    );
    let title = if long {
        "t".repeat(500)
    } else {
        "A title".into()
    };
    let metadata = EpisodeMetadata {
        guid: Some(&guid),
        title: &title,
        enclosure: &url,
        description: Some(&"d".repeat(description)),
        published: Some(1_700_000_000 + i as i64),
        enclosure_bytes: Some(30_000_000),
        duration_seconds: Some(3_000),
    };
    cat.upsert(SubscriptionId(1 + i % 50), metadata, &mut scratch)
        .unwrap()
}

fn count(cat: &mut Catalogue) -> u64 {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let (mut n, mut after) = (0, None);
    loop {
        let page = cat.scan(after, &mut scratch, |_| ()).unwrap();
        n += page.records as u64;
        after = page.resume_after;
        if !page.has_more {
            return n;
        }
    }
}

fn sync_past_the_failure_point(description: usize, long: bool) {
    let image = Image::new();
    let mut cat = image.create();
    for i in 0..SYNCED {
        add(&mut cat, i, description, long);
    }
    let kept = count(&mut cat);
    eprintln!("synced {SYNCED}, kept {kept}");
    assert_eq!(kept, MAX_EPISODES);
    // Reopen: the pruned state is durable, and syncing the same history
    // again creates nothing.
    drop(cat);
    let mut cat = image.reopen();
    assert_eq!(count(&mut cat), MAX_EPISODES);
    let again = add(&mut cat, 0, description, long);
    assert!(!again.created && !again.queued);
    assert_eq!(again.id, EpisodeId(1));
    assert_eq!(count(&mut cat), MAX_EPISODES);
}

// Regression (t-148): continuous syncing past ~10,000 episodes never
// reports `Full`, with ordinary records.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn prune_at_scale_never_reports_full() {
    sync_past_the_failure_point(900, false);
}

// Regression (t-148): the same with worst-case (about 4 KiB) records.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn prune_at_scale_never_reports_full_with_maximal_records() {
    sync_past_the_failure_point(1_024, true);
}

// Regression (t-148): a creation that also prunes stays within the budget.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn prune_at_scale_commit_stays_within_the_io_budget() {
    let image = Image::new();
    let mut cat = image.create();
    for i in 0..MAX_EPISODES + 50 {
        add(&mut cat, i, 900, false);
    }
    drop(cat);
    let mut cat = image.reopen();
    image.disk.reset_counters();
    add(&mut cat, MAX_EPISODES + 50, 900, false);
    let (reads, writes) = {
        let s = image.disk.state();
        (s.reads, s.writes)
    };
    eprintln!("prune: {reads} reads + {writes} writes");
    assert!(
        reads + writes <= REQUEST_COMMANDS,
        "prune: {reads} reads + {writes} writes"
    );
    assert_eq!(count(&mut cat), MAX_EPISODES);
}
