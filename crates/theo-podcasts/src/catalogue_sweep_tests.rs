use super::*;
use crate::{catalogue::*, test_support::Image};
use std::{collections::BTreeSet, format};

fn meta<'a>(guid: &'a str, url: &'a str) -> EpisodeMetadata<'a> {
    EpisodeMetadata {
        guid: Some(guid),
        title: "t",
        enclosure: url,
        description: None,
        published: None,
        enclosure_bytes: None,
        duration_seconds: None,
    }
}

fn add(cat: &mut Catalogue, sub: u64, n: u64) -> UpsertOutcome {
    let guid = format!("g{n}");
    let url = format!("https://x/{n}.mp3");
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.upsert(SubscriptionId(sub), meta(&guid, &url), &mut scratch)
        .unwrap()
}

fn key(n: u64) -> IdentityKey {
    identity_key(meta(&format!("g{n}"), &format!("https://x/{n}.mp3"))).unwrap()
}

/// Run a whole sweep of `sub`; returns its steps.
fn sweep_all(cat: &mut Catalogue, sub: u64, seen: &BTreeSet<IdentityKey>) -> usize {
    let mut cursor = None;
    let mut steps = 0;
    loop {
        let commits = cat.commits();
        cursor = cat
            .sweep_tombstones(SubscriptionId(sub), cursor, |k| seen.contains(k))
            .unwrap();
        assert!(cat.commits() - commits <= 1, "one commit per step");
        steps += 1;
        if cursor.is_none() {
            return steps;
        }
    }
}

fn identities(cat: &Catalogue, sub: u64) -> usize {
    let tx = redb::ReadableDatabase::begin_read(&cat.db).unwrap();
    let table = tx.open_table(crate::tables::EPISODE_IDENTITY).unwrap();
    redb::ReadableTable::range(
        &table,
        (
            core::ops::Bound::Included((sub, 0, [0; 32])),
            core::ops::Bound::Included((sub, u8::MAX, [u8::MAX; 32])),
        ),
    )
    .unwrap()
    .count()
}

// Regression (t-188): a sweep forgets only the tombstones a sync did not
// meet; listed tombstones and present episodes keep their identity.
#[test]
fn sweep_forgets_unseen_tombstones_only() {
    let image = Image::new();
    let mut cat = image.create();
    cat.limit = 2;
    // Undated episodes prune in id order: 1 and 2 go.
    for n in 1..=4 {
        add(&mut cat, 1, n);
    }
    assert_eq!(identities(&cat, 1), 4);
    // The feed still lists 2 (pruned) and 4 (present), no longer 1 and 3.
    let seen = BTreeSet::from([key(2), key(4)]);
    sweep_all(&mut cat, 1, &seen);
    assert_eq!(identities(&cat, 1), 3, "only 1's tombstone goes");
    // 2 stays known: a resync does not import it again.
    assert!(!add(&mut cat, 1, 2).created);
    // 1 is forgotten: if the feed lists it again, it is a new episode.
    let again = add(&mut cat, 1, 1);
    assert!(again.created);
    assert!(again.id.0 > 4, "ids are never reused");
}

// Regression (t-188): a step examines at most MAX_SCAN_RECORDS identities
// and makes at most one commit; the cursor walks them all.
#[test]
fn sweep_is_bounded_and_resumes_from_its_cursor() {
    let image = Image::new();
    let mut cat = image.create();
    cat.limit = 1;
    for n in 1..=40 {
        add(&mut cat, 1, n);
    }
    assert_eq!(identities(&cat, 1), 40);
    let steps = sweep_all(&mut cat, 1, &BTreeSet::new());
    assert_eq!(steps, 40usize.div_ceil(MAX_SCAN_RECORDS));
    assert_eq!(identities(&cat, 1), 1, "only the present episode remains");
}

// Regression (t-188): a step with nothing to delete commits nothing, and a
// sweep never touches another subscription.
#[test]
fn sweep_without_deletions_commits_nothing_and_stays_in_its_subscription() {
    let image = Image::new();
    let mut cat = image.create();
    cat.limit = 2;
    for n in 1..=3 {
        add(&mut cat, 2, n);
    }
    add(&mut cat, 1, 10);
    let commits = cat.commits();
    // Subscription 1's one identity has its episode: nothing to delete.
    assert_eq!(sweep_all(&mut cat, 1, &BTreeSet::new()), 1);
    assert_eq!(cat.commits(), commits);
    assert_eq!(identities(&cat, 2), 3, "subscription 2 untouched");
    sweep_all(&mut cat, 2, &BTreeSet::new());
    assert_eq!(identities(&cat, 2), 1);
    assert_eq!(identities(&cat, 1), 1);
}

#[test]
fn sweep_refuses_subscription_zero() {
    let image = Image::new();
    let mut cat = image.create();
    assert_eq!(
        cat.sweep_tombstones(SubscriptionId(0), None, |_| false),
        Err(Error::InvalidMetadata)
    );
}

// Regression (t-188): when the last window is exactly full, one more step
// finds nothing and ends the sweep; no identity is skipped.
#[test]
fn sweep_ends_after_an_exactly_full_last_window() {
    let image = Image::new();
    let mut cat = image.create();
    cat.limit = 1;
    let n = 2 * MAX_SCAN_RECORDS as u64;
    for i in 1..=n {
        add(&mut cat, 1, i);
    }
    let steps = sweep_all(&mut cat, 1, &BTreeSet::new());
    assert_eq!(steps, 3);
    assert_eq!(identities(&cat, 1), 1);
}

// Regression (t-188): a sweep never runs inside a batch, whose uncommitted
// writes its read would miss.
#[test]
fn sweep_refuses_an_open_batch() {
    let image = Image::new();
    let mut cat = image.create();
    cat.begin_batch().unwrap();
    assert_eq!(
        cat.sweep_tombstones(SubscriptionId(1), None, |_| false)
            .err(),
        Some(Error::InvalidState)
    );
    cat.end_batch(false).unwrap();
}
