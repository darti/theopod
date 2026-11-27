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
    let (image, mut cat) = small();
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
    // Regression (t-189): the fallback counts its victim out too.
    drop(cat);
    let (counts, sizes) = size_tables(&image);
    assert_eq!(counts, [(1, LIMIT)]);
    assert_eq!(sizes, [(LIMIT, 1)]);
}

// Regression (t-148): the default limit leaves the redb doubling headroom.
#[test]
fn the_default_limit_is_below_the_failure_point() {
    const { assert!(MAX_EPISODES < 8_600) };
}

/// Upsert episode `n` of `subscription`, published at `published`.
fn add_to(cat: &mut Catalogue, subscription: u64, n: u64, published: i64) -> UpsertOutcome {
    let guid = format!("s{subscription}-{n}");
    let url = format!("https://x/{subscription}/{n}.mp3");
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.upsert(
        SubscriptionId(subscription),
        meta(&guid, &url, Some(published)),
        &mut scratch,
    )
    .unwrap()
}

/// Publication dates of the episodes of `subscription`, by id.
fn dates(cat: &mut Catalogue, subscription: u64) -> Vec<i64> {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = cat
            .scan(after, &mut scratch, |e| {
                if e.subscription.0 == subscription {
                    out.push(e.metadata.published.unwrap());
                }
            })
            .unwrap();
        after = page.resume_after;
        if !page.has_more {
            return out;
        }
    }
}

/// Pairs read from a size table.
type Pairs = Vec<(u64, u64)>;

/// The stored `(subscription, count)` rows and `(count, subscription)` keys.
fn size_tables(image: &Image) -> (Pairs, Pairs) {
    use redb::{ReadableDatabase, ReadableTable};
    let db = redb::Database::builder()
        .create_with_backend(image.backend())
        .unwrap();
    let tx = db.begin_read().unwrap();
    let counts = tx
        .open_table(crate::tables::SUBSCRIPTION_EPISODES)
        .unwrap()
        .iter()
        .unwrap()
        .map(|e| {
            let (k, v) = e.unwrap();
            (k.value(), v.value())
        })
        .collect();
    let sizes = tx
        .open_table(crate::tables::SUBSCRIPTION_SIZES)
        .unwrap()
        .iter()
        .unwrap()
        .map(|e| e.unwrap().0.value())
        .collect();
    (counts, sizes)
}

/// Give episode `id` a unit record, which makes it unprunable.
fn give_unit(cat: &mut Catalogue, id: EpisodeId) {
    let tx = cat.write().unwrap();
    tx.open_table(crate::tables::UNIT_EXTENTS)
        .unwrap()
        .insert(id.0, &[0u8; 8][..])
        .unwrap();
    cat.done().unwrap();
}

// Regression (t-189): pruning takes the oldest prunable episode of the
// largest subscription, so a large feed cannot evict a small one's episodes
// even when they are older than all of its own.
#[test]
fn a_large_feed_cannot_evict_a_small_one() {
    let (_image, mut cat) = small();
    // The small feed's episodes are the oldest in the catalogue.
    for n in 0..3 {
        add_to(&mut cat, 2, n, 100 + n as i64);
    }
    for n in 0..3 * LIMIT {
        add_to(&mut cat, 1, n, 10_000 + n as i64);
    }
    assert_eq!(
        dates(&mut cat, 2),
        [100, 101, 102],
        "the small feed is intact"
    );
    let large = dates(&mut cat, 1);
    assert_eq!(large.len() as u64, LIMIT - 3);
    // Only the newest survive: the oldest went first.
    let first = 10_000 + 3 * LIMIT as i64 - large.len() as i64;
    assert_eq!(
        large,
        (first..first + large.len() as i64).collect::<Vec<_>>()
    );
    // A new small-feed episode at the cap takes the large feed's oldest.
    add_to(&mut cat, 2, 3, 50);
    assert_eq!(dates(&mut cat, 2), [100, 101, 102, 50]);
    assert_eq!(dates(&mut cat, 1), large[1..]);
}

// Regression (t-189): the largest subscription may have nothing prunable
// (all its audio on the card); the next largest gives way, oldest first.
#[test]
fn pruning_skips_a_largest_feed_without_prunable_episodes() {
    let (_image, mut cat) = small();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for n in 0..12 {
        let id = add_to(&mut cat, 1, n, 1_000 + n as i64).id;
        cat.transition_audio(id, AudioState::Downloading, &mut scratch)
            .unwrap();
    }
    for n in 0..8 {
        add_to(&mut cat, 2, n, 100 + n as i64);
    }
    add_to(&mut cat, 3, 0, 5_000);
    assert_eq!(dates(&mut cat, 1).len(), 12, "audio is never pruned");
    assert_eq!(dates(&mut cat, 2), (101..108).collect::<Vec<_>>());
    assert_eq!(dates(&mut cat, 3), [5_000]);
}

// Regression (t-189): one subscription's entries blocked by unit records
// cannot fill the whole window and push pruning to the unfair fallback.
#[test]
fn blocked_entries_of_the_largest_feed_do_not_starve_the_next() {
    let (_image, mut cat) = small();
    // Imported first, so the fallback would consider these lowest ids.
    for n in 0..16 {
        let id = add_to(&mut cat, 1, n, 1_000 + n as i64).id;
        give_unit(&mut cat, id);
    }
    // Feed 2's oldest-published episode is its last imported.
    for n in 0..4 {
        add_to(&mut cat, 2, n, 200 - n as i64);
    }
    add_to(&mut cat, 3, 0, 5_000);
    assert_eq!(dates(&mut cat, 1).len(), 16);
    // The fair path prunes published 197; the fallback would prune 200.
    assert_eq!(dates(&mut cat, 2), [200, 199, 198]);
}

// Regression (t-189): creation, pruning (down to zero) and state changes
// keep the per-subscription counts exact, each size key mirroring its row.
#[test]
fn subscription_counts_follow_creation_and_pruning() {
    let (image, mut cat) = small();
    let mut scratch = [0; MAX_RECORD_BYTES];
    add_to(&mut cat, 1, 0, 100);
    for n in 0..LIMIT - 1 {
        let id = add_to(&mut cat, 2, n, 1_000 + n as i64).id;
        cat.transition_audio(id, AudioState::Downloading, &mut scratch)
            .unwrap();
    }
    // Feed 2 has nothing prunable: feed 1 gives way, down to no episode.
    add_to(&mut cat, 3, 0, 5_000);
    assert!(dates(&mut cat, 1).is_empty());
    // Re-upserting a live or a pruned episode counts nothing.
    assert!(!add_to(&mut cat, 3, 0, 5_000).created);
    assert!(!add_to(&mut cat, 1, 0, 100).created);
    drop(cat);
    let (counts, sizes) = size_tables(&image);
    assert_eq!(counts, [(2, LIMIT - 1), (3, 1)]);
    assert_eq!(sizes, [(1, 3), (LIMIT - 1, 2)]);
}

// Regression (t-189): a lost count row reads as 0 and is not decremented
// below it, so pruning at the cap keeps working instead of failing.
#[test]
fn a_lost_count_row_does_not_block_pruning() {
    let (_image, mut cat) = small();
    for n in 0..LIMIT {
        add_to(&mut cat, 1, n, 100 + n as i64);
    }
    {
        let tx = cat.write().unwrap();
        let mut counts = tx.open_table(crate::tables::SUBSCRIPTION_EPISODES).unwrap();
        while counts.pop_first().unwrap().is_some() {}
    }
    cat.done().unwrap();
    add_to(&mut cat, 2, 0, 5_000);
    assert_eq!(dates(&mut cat, 1).len() as u64, LIMIT - 1);
}
