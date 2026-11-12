//! The scale target of ADR-0022: 50 subscriptions, 5,000 episodes, bounded
//! in sector commands (2 ms each on the board).
//!
//! Release only: redb walks every allocated page on open in debug builds,
//! and filling 5,000 episodes there is slow (`just test-catalogue-bounds`).
use crate::{catalogue::*, media::MediaStore, test_support::Image};
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

// Regression (t-133): a feed scan page plus its commit stay within the IO budget.
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
    eprintln!("commit: {} reads + {} writes", s.reads, s.writes);
    assert!(
        s.reads + s.writes <= REQUEST_COMMANDS,
        "commit: {} reads + {} writes",
        s.reads,
        s.writes
    );
}

// One synced episode is one commit (episode, identity and counter), within
// a podcast request's budget.
// Regression (t-133): an upsert commit stays within the IO budget.
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
        s.reads + s.writes <= REQUEST_COMMANDS,
        "upsert: {} reads + {} writes",
        s.reads,
        s.writes
    );
}

/// A podcast request runs under `PODCAST_BUDGET` (1 s) on the board: 500
/// sector commands at 2 ms each, catalogue and media sectors together.
/// User-approved 2026-11-12 (was 500 ms / 250): cold-cache redb commits
/// measured ~250 commands.
const REQUEST_COMMANDS: usize = 500;

/// Reopen the catalogue and scan every episode: redb's page cache then
/// holds none of the pages a commit needs (the worst case measured).
fn chill(image: &Image) -> Catalogue {
    let mut cat = image.reopen();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let mut after = None;
    loop {
        let page = cat.scan(after, &mut scratch, |_| ()).unwrap();
        after = page.resume_after;
        if !page.has_more {
            return cat;
        }
    }
}

/// Catalogue and media sector commands since the last reset.
fn commands(image: &Image, media: &Media) -> (usize, usize) {
    let s = image.disk.state();
    let d = media.device();
    (s.reads + d.reads, s.writes + d.writes)
}
fn reset(image: &Image, media: &mut Media) {
    image.disk.reset_counters();
    media.device_mut().reads = 0;
    media.device_mut().writes = 0;
}
fn within_budget(what: &str, (reads, writes): (usize, usize)) {
    eprintln!("{what}: {reads} reads + {writes} writes");
    assert!(
        reads + writes <= REQUEST_COMMANDS,
        "{what}: {reads} reads + {writes} writes"
    );
}

type Media =
    crate::unit_media::UnitMedia<crate::media_test_support::Disk, allocator_api2::alloc::Global>;
/// A unit media store of `units` 1 MiB units over a counting disk.
fn media(cat: &mut Catalogue, units: u32) -> Media {
    use crate::units::UNIT_SECTORS;
    let disk = crate::media_test_support::Disk::counting(units * UNIT_SECTORS);
    crate::unit_media::UnitMedia::open_in(cat, disk, units, allocator_api2::alloc::Global).unwrap()
}
/// Episode `i`'s job (the fill's metadata), queued for download.
fn job(cat: &mut Catalogue, id: EpisodeId) -> crate::download::DownloadJob {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let episode = cat.get(id, &mut scratch).unwrap().unwrap();
    crate::download::DownloadJob::from_episode(episode).unwrap()
}

/// Start, stream `len` bytes (known length) and return the session ready to
/// finish; each start step must fit a request.
fn started(
    image: &Image,
    cat: &mut Catalogue,
    media: &mut Media,
    id: EpisodeId,
    len: u64,
) -> crate::download::DownloadSession {
    use crate::{
        download::{DownloadSession, Start},
        media::AUDIO_CHUNK,
        reclaim::Reclaim,
    };
    let mut scratch = [0; MAX_RECORD_BYTES];
    let job = job(cat, id);
    let mut reclaim = Reclaim::new();
    let mut session = loop {
        reset(image, media);
        let start = futures::executor::block_on(DownloadSession::start(
            cat,
            media,
            &mut reclaim,
            job.clone(),
            &mut scratch,
        ))
        .unwrap();
        within_budget("download start step", commands(image, media));
        match start {
            Start::Ready(session) => break session,
            Start::Pending => (),
            _ => panic!("not started"),
        }
    };
    session.response(200, Some(len), None).unwrap();
    let mut left = len;
    while left > 0 {
        let n = left.min(AUDIO_CHUNK as u64) as usize;
        futures::executor::block_on(session.push(
            cat,
            media,
            &mut reclaim,
            &[7; AUDIO_CHUNK][..n],
            &mut scratch,
        ))
        .unwrap();
        left -= n as u64;
    }
    session
}

/// The fill's announced length; every episode is left `Queued`.
const AUDIO_LEN: u64 = 30_000_000;

// Regression (t-133): `DownloadEnd` publishes and marks the episode
// `Available` in one commit, within a request's IO budget at scale.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn download_end_is_one_commit_within_the_io_budget() {
    let image = fill();
    let mut cat = image.reopen();
    let mut media = media(&mut cat, 64);
    let mut scratch = [0; MAX_RECORD_BYTES];
    let id = EpisodeId(2_500);
    let session = started(&image, &mut cat, &mut media, id, AUDIO_LEN);
    reset(&image, &mut media);
    let commits = cat.commits();
    let outcome =
        futures::executor::block_on(session.finish(&mut cat, &mut media, true, &mut scratch))
            .unwrap();
    assert_eq!(outcome, crate::download::DownloadOutcome::Complete);
    assert_eq!(cat.commits() - commits, 1);
    within_budget("download end", commands(&image, &media));
    let episode = cat.get(id, &mut scratch).unwrap().unwrap();
    assert_eq!(episode.state.audio, AudioState::Available);
}

// Regression (t-133): recovering an interrupted download (both files, then
// `Missing` and `Queued`) is one commit, within a request's IO budget.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn recover_one_is_one_commit_within_the_io_budget() {
    let image = fill();
    let mut cat = image.reopen();
    let mut media = media(&mut cat, 64);
    let mut scratch = [0; MAX_RECORD_BYTES];
    let id = EpisodeId(2_500);
    let session = started(&image, &mut cat, &mut media, id, AUDIO_LEN);
    drop(session);
    reset(&image, &mut media);
    let commits = cat.commits();
    futures::executor::block_on(crate::recovery::recover_one(
        &mut cat,
        &mut media,
        id,
        &mut scratch,
    ))
    .unwrap();
    assert_eq!(cat.commits() - commits, 1);
    within_budget("recover one", commands(&image, &media));
    let episode = cat.get(id, &mut scratch).unwrap().unwrap();
    assert_eq!(episode.state.audio, AudioState::Queued);
    assert_eq!(media.table.used(), 0);
}

// Regression (t-133): with every episode read and `Available`, each
// `make_space` step (`Deleting` intent of the index's first entry, then
// unlink and `Reclaimed`) fits a request, makes at most one commit, and
// evicts the policy's choice; `cold` reopens the catalogue and scans every
// episode before each step.
fn make_space_steps(cold: bool) {
    use crate::{
        media::AUDIO_CHUNK,
        reclaim::{Reclaim, SpaceStep, make_space},
    };
    let image = fill();
    let mut cat = Some(image.reopen());
    let mut scratch = [0; MAX_RECORD_BYTES];
    // Every episode read and `Available`, 50 per commit.
    let c = cat.as_mut().unwrap();
    for first in (1..=EPISODES).step_by(50) {
        c.begin_batch().unwrap();
        for id in first..first + 50 {
            let id = EpisodeId(id);
            c.transition_audio(id, AudioState::Downloading, &mut scratch)
                .unwrap();
            c.finish_download(id, 1, &mut scratch).unwrap();
            c.mark_read(id, &mut scratch).unwrap();
        }
        c.end_batch(true).unwrap();
    }
    // 20 units, 16 reserved: published audio of the oldest (episode 1,
    // one unit) and of episode 2 (three units) leaves a 4 KiB deficit.
    let mut media = media(c, 20);
    for (id, units) in [(1, 1), (2, 3)] {
        let id = EpisodeId(id);
        let len = units * crate::unit_media::UNIT_BYTES;
        futures::executor::block_on(async {
            media.reset_temp(c, id, Some(len)).await.unwrap();
            for _ in 0..len / AUDIO_CHUNK as u64 {
                media.append_temp(c, id, &[1; AUDIO_CHUNK]).await.unwrap();
            }
            media.sync_temp(c, id).await.unwrap();
            media.publish_temp(c, id).await.unwrap();
        });
    }
    let mut reclaim = Reclaim::new();
    let mut steps = 0;
    loop {
        if cold {
            drop(cat.take());
            cat = Some(chill(&image));
        }
        let c = cat.as_mut().unwrap();
        reset(&image, &mut media);
        let commits = c.commits();
        let step = futures::executor::block_on(make_space(
            c,
            &mut media,
            &mut reclaim,
            AUDIO_CHUNK as u64,
            &mut scratch,
        ))
        .unwrap();
        assert!(c.commits() - commits <= 1);
        within_budget(
            &std::format!("make_space cold={cold} step {steps} ({step:?})"),
            commands(&image, &media),
        );
        steps += 1;
        match step {
            SpaceStep::Pending => (),
            SpaceStep::Ready => break,
            SpaceStep::Unavailable => panic!("unavailable"),
        }
    }
    assert_eq!(steps, 3);
    let c = cat.as_mut().unwrap();
    let state = c.get(EpisodeId(1), &mut scratch).unwrap().unwrap().state;
    assert_eq!(state.audio, AudioState::Reclaimed);
    let state = c.get(EpisodeId(2), &mut scratch).unwrap().unwrap().state;
    assert_eq!(state.audio, AudioState::Available);
}
// Regression (t-133): each make-space step (warm cache) stays within the IO budget.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn make_space_steps_stay_within_the_io_budget() {
    make_space_steps(false);
}
// Regression (t-133): each make-space step (cold cache) stays within the IO budget.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn make_space_steps_stay_within_the_io_budget_cold() {
    make_space_steps(true);
}

/// A feed of `count` short items with guids `prefix-0..`.
fn feed_items(prefix: &str, count: usize) -> std::string::String {
    (0..count)
        .map(|i| {
            std::format!(
                "<item><title>T</title><guid>{prefix}-{i}</guid><enclosure url='https://cdn.example/{prefix}-{i}'/></item>"
            )
        })
        .collect()
}
/// Push `body` through a session, every push within a request's budget;
/// returns the pushes and the worst push.
fn push_body(
    image: &Image,
    cat: &mut Option<Catalogue>,
    session: &mut crate::sync::SyncSession,
    body: &[u8],
    cold: bool,
) -> (usize, (usize, usize)) {
    let mut scratch = [0; MAX_RECORD_BYTES];
    let (mut pushes, mut worst) = (0, (0, 0));
    for chunk in body.chunks(crate::sync::MAX_CHUNK_BYTES) {
        let mut rest = chunk;
        while !rest.is_empty() {
            if cold {
                drop(cat.take());
                *cat = Some(chill(image));
            }
            let cat = cat.as_mut().unwrap();
            image.disk.reset_counters();
            let commits = cat.commits();
            let used = session.push(cat, rest, &mut scratch).unwrap();
            assert!(cat.commits() - commits <= 1);
            let s = image.disk.state();
            if s.reads + s.writes > worst.0 + worst.1 {
                worst = (s.reads, s.writes);
            }
            assert!(
                s.reads + s.writes <= REQUEST_COMMANDS,
                "push {pushes}: {} reads + {} writes",
                s.reads,
                s.writes
            );
            assert!(used > 0);
            rest = &rest[used..];
            pushes += 1;
        }
    }
    (pushes, worst)
}

// Regression (t-133): a `SyncChunk` of many new episodes commits one per
// request (the network task resends the rest), and a chunk of known
// episodes reads at most `MAX_CHUNK_ITEMS` of them, each request within the
// budget at scale; `cold` reopens the catalogue and scans every episode
// before each push.
fn sync_chunk_requests(cold: bool) {
    use crate::sync::{MAX_CHUNK_ITEMS, SyncSession, Validators};
    const ROOT: &str = "https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b";
    let image = fill();
    let mut cat = Some(image.reopen());
    let mut scratch = [0; MAX_RECORD_BYTES];
    let feed = SubscriptionId(1);
    let new = feed_items("new", 40);
    let body = std::format!("<rss><channel>{new}</channel></rss>");
    let mut session = SyncSession::new(feed, ROOT, None).unwrap();
    session.response(200, Validators::default()).unwrap();
    let (pushes, worst) = push_body(&image, &mut cat, &mut session, body.as_bytes(), cold);
    eprintln!(
        "sync new cold={cold}: {pushes} pushes, worst {} reads + {} writes",
        worst.0, worst.1
    );
    assert!(!session.end_page().unwrap());
    let c = cat.as_mut().unwrap();
    assert_eq!(session.complete(c, &mut scratch).unwrap().created, 40);
    // Known episodes, then new ones: `MAX_CHUNK_ITEMS - 1` reads-only
    // upserts before a committing one in the same request.
    let mixed = std::format!(
        "<rss><channel>{}{}</channel></rss>",
        feed_items("new", 40),
        feed_items("more", 40)
    );
    let mut session = SyncSession::new(feed, ROOT, None).unwrap();
    session.response(200, Validators::default()).unwrap();
    // Align the first new item after `MAX_CHUNK_ITEMS - 1` known ones.
    let skip = feed_items("new", 40 - (MAX_CHUNK_ITEMS - 1));
    let head = "<rss><channel>".len() + skip.len();
    push_body(
        &image,
        &mut cat,
        &mut session,
        &mixed.as_bytes()[..head],
        cold,
    );
    let (pushes, worst) = push_body(
        &image,
        &mut cat,
        &mut session,
        &mixed.as_bytes()[head..],
        cold,
    );
    eprintln!(
        "sync mixed cold={cold}: {pushes} pushes, worst {} reads + {} writes",
        worst.0, worst.1
    );
}
// Regression (t-133): sync-chunk requests (warm cache) stay within the IO budget.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn sync_chunk_requests_stay_within_the_io_budget() {
    sync_chunk_requests(false);
}
// Regression (t-133): sync-chunk requests (cold cache) stay within the IO budget.
#[test]
#[cfg_attr(debug_assertions, ignore = "sector bounds are release-only")]
fn sync_chunk_requests_stay_within_the_io_budget_cold() {
    sync_chunk_requests(true);
}
