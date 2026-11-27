use super::*;
use crate::{catalogue::*, test_support::Image};
use std::{format, string::String as StdString};
const ROOT: &str = "https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b";
const ID: SubscriptionId = SubscriptionId(1);

fn items(numbers: impl Iterator<Item = u64>, guid: bool) -> StdString {
    numbers
        .map(|n| {
            let guid = if guid {
                format!("<guid>g{n}</guid>")
            } else {
                StdString::new()
            };
            format!("<item><title>t</title>{guid}<enclosure url='https://cdn.example/{n}'/></item>")
        })
        .collect()
}

fn feed(numbers: impl Iterator<Item = u64>) -> StdString {
    format!("<rss><channel>{}</channel></rss>", items(numbers, true))
}

/// A page that names a next page.
fn first_page(numbers: impl Iterator<Item = u64>) -> StdString {
    format!(
        "<rss xmlns:atom='http://www.w3.org/2005/Atom'><channel><atom:link rel='next' href='{ROOT}?page=1'/>{}</channel></rss>",
        items(numbers, true)
    )
}

/// Walk `pages` (200, ETag `etag`); returns the session after its last page.
fn walk_pages(
    cat: &mut Catalogue,
    pages: &[&str],
    prior: Option<Checkpoint>,
    etag: &str,
) -> SyncSession {
    let mut session = SyncSession::new(ID, ROOT, prior).unwrap();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for (n, body) in pages.iter().enumerate() {
        let validators = Validators {
            etag: Some(etag),
            last_modified: None,
        };
        session.response(200, validators).unwrap();
        let mut bytes = body.as_bytes();
        while !bytes.is_empty() {
            let used = session.push(cat, bytes, &mut scratch).unwrap();
            assert!(used > 0);
            bytes = &bytes[used..];
        }
        assert_eq!(session.end_page().unwrap(), n + 1 < pages.len());
    }
    session
}

fn walk(cat: &mut Catalogue, body: &str, prior: Option<Checkpoint>) -> SyncSession {
    walk_pages(cat, &[body], prior, "v1")
}

/// Sweep if the walk allows it, then save the checkpoint.
fn finish(cat: &mut Catalogue, session: &mut SyncSession) {
    if session.begin_sweep() {
        loop {
            let commits = cat.commits();
            let done = session.sweep(cat).unwrap();
            assert!(cat.commits() - commits <= 1);
            if done {
                break;
            }
        }
    }
    let commits = cat.commits();
    session.complete(cat, &mut [0; MAX_RECORD_BYTES]).unwrap();
    assert!(cat.commits() - commits <= 1);
}

fn checkpoint(cat: &mut Catalogue) -> Option<Checkpoint> {
    cat.sync_checkpoint(ID, &mut [0; MAX_RECORD_BYTES]).unwrap()
}

/// Upsert episode `n` again; whether it was created, i.e. no tombstone
/// held it back.
fn reimported(cat: &mut Catalogue, n: u64, guid: bool) -> bool {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let id = format!("g{n}");
    let url = format!("https://cdn.example/{n}");
    let meta = EpisodeMetadata {
        guid: guid.then_some(id.as_str()),
        title: "t",
        enclosure: &url,
        description: None,
        published: None,
        enclosure_bytes: None,
        duration_seconds: None,
    };
    cat.upsert(ID, meta, &mut scratch).unwrap().created
}

/// A catalogue holding `1..=n` of which all but the newest 3 are pruned.
fn pruned(image: &Image, n: u64, guid: bool) -> Catalogue {
    let mut cat = image.create();
    cat.set_limit(3);
    let body = format!("<rss><channel>{}</channel></rss>", items(1..=n, guid));
    let mut session = walk(&mut cat, &body, None);
    finish(&mut cat, &mut session);
    cat
}

// Regression (t-188): a complete sync forgets the tombstones of episodes
// that left the feed and keeps those the feed still lists.
#[test]
fn complete_sync_sweeps_tombstones_that_left_the_feed() {
    let image = Image::new();
    // 1..=20 imported; undated, so 1..=17 are pruned.
    let mut cat = pruned(&image, 20, true);
    let prior = checkpoint(&mut cat);
    // The feed now lists 11..=20: 1..=10 left it.
    let mut session = walk(&mut cat, &feed(11..=20), prior);
    finish(&mut cat, &mut session);
    assert!(!reimported(&mut cat, 15, true), "a listed tombstone stays");
    assert!(reimported(&mut cat, 5, true), "a left tombstone was swept");
}

// Regression (t-188): enclosure-only items are swept by their enclosure
// identity.
#[test]
fn sweep_handles_items_without_guid() {
    let image = Image::new();
    let mut cat = pruned(&image, 20, false);
    let prior = checkpoint(&mut cat);
    let body = format!("<rss><channel>{}</channel></rss>", items(11..=20, false));
    let mut session = walk(&mut cat, &body, prior);
    finish(&mut cat, &mut session);
    assert!(!reimported(&mut cat, 15, false), "a listed tombstone stays");
    assert!(reimported(&mut cat, 5, false), "a left tombstone was swept");
}

// Regression (t-188): the seen set spans every page of a walk, so an
// episode listed only on the first page keeps its tombstone.
#[test]
fn sweep_keeps_tombstones_listed_on_an_earlier_page() {
    let image = Image::new();
    let mut cat = pruned(&image, 20, true);
    let prior = checkpoint(&mut cat);
    let pages = [first_page(1..=5), feed(11..=20)];
    let mut session = walk_pages(&mut cat, &[&pages[0], &pages[1]], prior, "v2");
    finish(&mut cat, &mut session);
    assert!(!reimported(&mut cat, 3, true), "page 1 tombstone stays");
    assert!(
        reimported(&mut cat, 8, true),
        "unlisted tombstone was swept"
    );
}

// Regression (t-188): a 304 walked nothing, so it must not sweep.
#[test]
fn not_modified_sync_does_not_sweep() {
    let image = Image::new();
    let mut cat = pruned(&image, 10, true);
    let prior = checkpoint(&mut cat);
    let mut session = SyncSession::new(ID, ROOT, prior).unwrap();
    session.response(304, Validators::default()).unwrap();
    assert!(!session.end_page().unwrap());
    assert!(!session.begin_sweep());
    assert!(matches!(session.sweep(&mut cat), Err(SyncError::Protocol)));
    let mut scratch = [0; MAX_RECORD_BYTES];
    session.complete(&mut cat, &mut scratch).unwrap();
    assert!(matches!(
        session.complete(&mut cat, &mut scratch),
        Err(SyncError::Incomplete)
    ));
    assert!(!reimported(&mut cat, 1, true), "the tombstone stays");
}

// Regression (t-188): an empty 200 vouches for nothing, so it must not
// wipe every tombstone.
#[test]
fn empty_feed_does_not_sweep() {
    let image = Image::new();
    let mut cat = pruned(&image, 10, true);
    let prior = checkpoint(&mut cat);
    let mut session = walk(&mut cat, &feed(0..0), prior);
    assert!(!session.begin_sweep());
    finish(&mut cat, &mut session);
    assert!(!reimported(&mut cat, 1, true), "the tombstone stays");
}

// Regression (t-188): a sweep only starts after the last page, and asking
// for a step before then does not spend the session.
#[test]
fn sweep_waits_for_the_last_page() {
    let image = Image::new();
    let mut cat = pruned(&image, 10, true);
    let prior = checkpoint(&mut cat);
    let mut session = SyncSession::new(ID, ROOT, prior).unwrap();
    assert!(!session.begin_sweep());
    assert!(matches!(session.sweep(&mut cat), Err(SyncError::Protocol)));
    let prior = checkpoint(&mut cat);
    let mut session = walk(&mut cat, &feed(5..=10), prior);
    assert!(matches!(session.sweep(&mut cat), Err(SyncError::Protocol)));
    assert!(session.begin_sweep());
}

// Regression (t-188): the sweep runs before the checkpoint, so a sync
// abandoned mid-sweep keeps the old checkpoint and the next one (no 304)
// sweeps again.
#[test]
fn interrupted_sweep_keeps_the_old_checkpoint() {
    let image = Image::new();
    // 40 identities, 37 tombstones: several sweep steps.
    let mut cat = pruned(&image, 40, true);
    let old = checkpoint(&mut cat);
    let body = feed(31..=40);
    let mut session = walk_pages(&mut cat, &[&body], old.clone(), "v2");
    assert!(session.begin_sweep());
    assert!(!session.sweep(&mut cat).unwrap());
    drop(session);
    assert_eq!(checkpoint(&mut cat), old, "no checkpoint yet");
    let mut session = walk_pages(&mut cat, &[&body], old, "v2");
    finish(&mut cat, &mut session);
    assert_eq!(checkpoint(&mut cat).unwrap().etag.as_str(), "v2");
    assert!(reimported(&mut cat, 30, true), "the sweep completed");
    assert!(!reimported(&mut cat, 35, true));
}

// Regression (t-188): a sweep error spends the session; nothing is saved.
#[test]
fn sweep_error_spends_the_session() {
    let image = Image::new();
    let mut cat = pruned(&image, 10, true);
    let old = checkpoint(&mut cat);
    let mut session = walk_pages(&mut cat, &[&feed(5..=10)], old.clone(), "v2");
    assert!(session.begin_sweep());
    cat.begin_batch().unwrap();
    assert!(session.sweep(&mut cat).is_err());
    cat.end_batch(false).unwrap();
    assert!(!session.sweeping());
    assert!(matches!(session.sweep(&mut cat), Err(SyncError::Protocol)));
    let mut scratch = [0; MAX_RECORD_BYTES];
    assert!(session.complete(&mut cat, &mut scratch).is_err());
    assert_eq!(checkpoint(&mut cat), old);
}
