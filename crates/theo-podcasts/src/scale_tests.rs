//! The scale target of ADR-0022: 50 subscriptions, 5,000 episodes, bounded
//! in sector commands (2 ms each on the board).
//!
//! Release only: redb walks every allocated page on open in debug builds,
//! and filling 5,000 episodes there is slow (`just test-catalogue-bounds`).
use crate::{catalogue::*, test_support::Image};
use std::eprintln;

const FEEDS: u64 = 50;
const EPISODES: u64 = 5_000;

/// Upsert episode `i` (about 900-byte description) into feed `1 + i % 50`.
fn add(cat: &mut Catalogue, i: u64) {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let guid = std::format!("guid-{i}");
    let url = std::format!("https://media.radiofrance-podcast.net/{i}.mp3");
    let metadata = EpisodeMetadata {
        guid: Some(&guid),
        title: "A title of moderate length for a podcast episode",
        enclosure: &url,
        description: Some(&"d".repeat(900)),
        published: Some(1_700_000_000),
        enclosure_bytes: Some(30_000_000),
        duration_seconds: Some(3_000),
    };
    cat.upsert(SubscriptionId(1 + i % FEEDS), metadata, &mut scratch)
        .unwrap();
}

fn fill() -> Image {
    let image = Image::new();
    let mut cat = image.create();
    for i in 0..EPISODES {
        add(&mut cat, i);
    }
    image
}

// Regression (t-133): boot-time open stays O(1) at the scale target.
// Catalogue::open measured 80 reads (its table checks); the guard is 96
// (user-approved 2026-11-12). Raw redb open keeps its <= 64 guard in
// theo-storage.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn open_at_scale_reads_at_most_96_sectors() {
    let image = fill();
    image.crash();
    image.disk.reset_counters();
    drop(image.reopen());
    let reads = image.disk.state().reads;
    eprintln!("open: {reads} reads");
    assert!(reads <= 96, "{reads} reads");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn scan_page_and_commit_stay_within_the_io_budget() {
    let image = fill();
    let mut cat = image.reopen();
    let mut scratch = [0; MAX_RECORD_BYTES];
    image.disk.reset_counters();
    let page = cat
        .scan(Some(EpisodeId(2_500)), &mut scratch, |_| ())
        .unwrap();
    let scan = image.disk.state().reads;
    // ~1 KiB records: the 16 KiB byte limit ends the page before 16 records.
    assert!(page.records > 0 && page.has_more);
    eprintln!("scan: {scan} reads");
    assert!(scan <= 160, "scan: {scan} reads");
    image.disk.reset_counters();
    cat.mark_read(EpisodeId(2_500), &mut scratch).unwrap();
    let s = image.disk.state();
    // IO_BUDGET 500 ms at 2 ms per command.
    eprintln!("commit: {} reads + {} writes", s.reads, s.writes);
    assert!(
        s.reads + s.writes <= 250,
        "commit: {} reads + {} writes",
        s.reads,
        s.writes
    );
}

// One synced episode is one commit (episode, identity and counter); a podcast
// request runs under a 500 ms IO budget, 250 commands at 2 ms each.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn upsert_commit_stays_within_the_io_budget() {
    let image = fill();
    let mut cat = image.reopen();
    image.disk.reset_counters();
    add(&mut cat, EPISODES);
    let s = image.disk.state();
    eprintln!("upsert: {} reads + {} writes", s.reads, s.writes);
    assert!(
        s.reads + s.writes <= 250,
        "upsert: {} reads + {} writes",
        s.reads,
        s.writes
    );
}
