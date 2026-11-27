use super::*;
use crate::{catalogue::*, test_support::Image};
use std::{format, vec::Vec};

const LIMIT: u64 = 20;

fn meta<'a>(guid: &'a str, url: &'a str, published: Option<i64>) -> EpisodeMetadata<'a> {
    EpisodeMetadata {
        guid: Some(guid),
        title: "t",
        enclosure: url,
        description: None,
        published,
        enclosure_bytes: None,
        duration_seconds: None,
    }
}

/// Upsert episode `n` published at `published`.
fn add(cat: &mut Catalogue, n: u64, published: Option<i64>) -> UpsertOutcome {
    let guid = format!("g{n}");
    let url = format!("https://x/{n}.mp3");
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.upsert(
        SubscriptionId(1),
        meta(&guid, &url, published),
        &mut scratch,
    )
    .unwrap()
}

fn ids(cat: &mut Catalogue) -> Vec<u64> {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = cat.scan(after, &mut scratch, |e| out.push(e.id.0)).unwrap();
        after = page.resume_after;
        if !page.has_more {
            return out;
        }
    }
}

fn small() -> (Image, Catalogue) {
    let image = Image::new();
    let mut cat = image.create();
    cat.limit = LIMIT;
    (image, cat)
}

// Regression (t-148): creating an episode at the limit deletes the oldest
// published one without audio, whatever its id.
#[test]
fn creation_at_the_limit_prunes_the_oldest_published() {
    let (_image, mut cat) = small();
    // Newest published first, as a feed is synced: id 1 is the newest.
    for n in 0..LIMIT {
        add(&mut cat, n, Some(1_000_000 - n as i64));
    }
    assert_eq!(ids(&mut cat).len() as u64, LIMIT);
    add(&mut cat, 1_000, Some(2_000_000));
    let ids = ids(&mut cat);
    assert_eq!(ids.len() as u64, LIMIT);
    // Id 20 (oldest date) is gone; id 1 (newest of the feed) is kept.
    assert!(!ids.contains(&20));
    assert!(ids.contains(&1));
}

// Regression (t-148): an episode with audio on the card or being fetched is
// never pruned, even when it is the oldest.
#[test]
fn episodes_with_audio_are_never_pruned() {
    let (_image, mut cat) = small();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for n in 0..LIMIT {
        add(&mut cat, n, Some(100 + n as i64));
    }
    // Ids 1 and 2 are the oldest: one downloading, one available.
    for id in [1, 2] {
        cat.transition_audio(EpisodeId(id), AudioState::Downloading, &mut scratch)
            .unwrap();
    }
    cat.transition_audio(EpisodeId(2), AudioState::Available, &mut scratch)
        .unwrap();
    add(&mut cat, 1_000, Some(500));
    let ids = ids(&mut cat);
    assert!(ids.contains(&1) && ids.contains(&2));
    assert!(!ids.contains(&3), "the oldest prunable one goes: {ids:?}");
}

// Regression (t-148): a pruned episode met again by a sync is known, not
// recreated, so a long history does not churn.
#[test]
fn a_pruned_episode_is_not_imported_again() {
    let (_image, mut cat) = small();
    for n in 0..=LIMIT {
        add(&mut cat, n, Some(100 + n as i64));
    }
    // Episode 0 (id 1) was pruned; syncing it again creates nothing.
    let again = add(&mut cat, 0, Some(100));
    assert_eq!(again.id, EpisodeId(1));
    assert!(!again.created && !again.queued);
    assert_eq!(ids(&mut cat).len() as u64, LIMIT);
    assert_eq!(ids(&mut cat).first(), Some(&2));
}

// Regression (t-148): continuous syncing far past the limit keeps the count
// at the limit and never reports `Full`.
#[test]
fn continuous_sync_stays_at_the_limit() {
    let (_image, mut cat) = small();
    for n in 0..LIMIT * 5 {
        add(&mut cat, n, Some(n as i64));
    }
    assert_eq!(ids(&mut cat).len() as u64, LIMIT);
}

// Regression (t-148): when nothing is prunable, creation still succeeds.
#[test]
fn creation_goes_over_when_nothing_is_prunable() {
    let (_image, mut cat) = small();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for n in 0..LIMIT {
        let o = add(&mut cat, n, Some(n as i64));
        cat.transition_audio(o.id, AudioState::Downloading, &mut scratch)
            .unwrap();
    }
    let extra = add(&mut cat, 1_000, Some(0));
    assert!(extra.created);
    assert_eq!(ids(&mut cat).len() as u64, LIMIT + 1);
}

// Regression (t-148): an episode downloaded then reclaimed is prunable again.
#[test]
fn reclaimed_episodes_become_prunable() {
    let (_image, mut cat) = small();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for n in 0..LIMIT {
        add(&mut cat, n, Some(100 + n as i64));
    }
    let id = EpisodeId(1);
    for step in [AudioState::Downloading, AudioState::Available] {
        cat.transition_audio(id, step, &mut scratch).unwrap();
    }
    cat.mark_read(id, &mut scratch).unwrap();
    for step in [AudioState::Deleting, AudioState::Reclaimed] {
        cat.transition_audio(id, step, &mut scratch).unwrap();
    }
    add(&mut cat, 1_000, Some(500));
    assert!(!ids(&mut cat).contains(&1));
}

// Regression (t-148): undated episodes are pruned after every dated one.
#[test]
fn undated_episodes_are_pruned_last() {
    let (_image, mut cat) = small();
    add(&mut cat, 0, None);
    for n in 1..LIMIT {
        add(&mut cat, n, Some(100 + n as i64));
    }
    add(&mut cat, 1_000, Some(500));
    let ids = ids(&mut cat);
    assert!(ids.contains(&1), "undated kept: {ids:?}");
    assert!(!ids.contains(&2), "oldest dated pruned: {ids:?}");
}

// Regression (t-148): a creation that cannot be encoded deletes nothing.
#[test]
fn a_failed_creation_prunes_nothing() {
    let (_image, mut cat) = small();
    for n in 0..LIMIT {
        add(&mut cat, n, Some(100 + n as i64));
    }
    let mut scratch = [0; MAX_RECORD_BYTES];
    let huge = "t".repeat(MAX_TITLE_BYTES + 1);
    let result = cat.upsert(
        SubscriptionId(1),
        EpisodeMetadata {
            title: &huge,
            ..meta("big", "https://x/big.mp3", Some(1))
        },
        &mut scratch,
    );
    assert!(result.is_err());
    assert_eq!(ids(&mut cat).len() as u64, LIMIT);
    assert!(ids(&mut cat).contains(&1));
}

// Regression (t-148): the prune and the tombstone survive a reopen.
#[test]
fn pruning_survives_a_reopen() {
    let (image, mut cat) = small();
    for n in 0..=LIMIT {
        add(&mut cat, n, Some(100 + n as i64));
    }
    drop(cat);
    let mut cat = image.reopen();
    cat.limit = LIMIT;
    let again = add(&mut cat, 0, Some(100));
    assert!(!again.created);
    assert_eq!(again.id, EpisodeId(1));
    assert_eq!(ids(&mut cat).len() as u64, LIMIT);
}

// Regression (t-148): episodes missing from the index (written before it
// existed) are still pruned, oldest imported first.
#[test]
fn unindexed_episodes_are_pruned_by_the_fallback() {
    let (_image, mut cat) = small();
    for n in 0..LIMIT {
        add(&mut cat, n, Some(100 + n as i64));
    }
    {
        let tx = cat.write().unwrap();
        let mut table = tx.open_table(PRUNABLE).unwrap();
        while table.pop_first().unwrap().is_some() {}
    }
    cat.done().unwrap();
    add(&mut cat, 1_000, Some(500));
    let ids = ids(&mut cat);
    assert_eq!(ids.len() as u64, LIMIT);
    assert!(!ids.contains(&1), "{ids:?}");
}

// Regression (t-148): the default limit leaves the redb doubling headroom.
#[test]
fn the_default_limit_is_below_the_failure_point() {
    const { assert!(MAX_EPISODES < 8_600) };
}
