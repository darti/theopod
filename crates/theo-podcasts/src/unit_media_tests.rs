use super::*;
use crate::{
    catalogue::*,
    download::{DownloadJob, DownloadSession, Start},
    media_test_support::Disk,
    test_support::{HostFile, Image},
    units::{MAX_EXTENTS, UNIT_BYTES},
};
use allocator_api2::alloc::Global;
use core::cell::Cell;
use futures::executor::block_on;
use std::{collections::BTreeSet, vec::Vec};
use theo_storage::page_store::PageStore;

type Cat = Catalogue<PageStore<HostFile>>;

const SECTORS: u32 = UNIT_SECTORS;

/// A catalogue image without `fsync`: writes still reach the file for reopen,
/// and these tests issue thousands of commits.
fn image(pages: usize) -> Image {
    let image = Image::with_pages(pages);
    image.sync_to_disk.set(false);
    image
}

async fn open<D: SectorIo>(cat: &mut Cat, disk: D, units: u32) -> UnitMedia<D, Global> {
    UnitMedia::open_in(cat, disk, units, Global).await.unwrap()
}

async fn append_all<D: SectorIo>(
    cat: &mut Cat,
    media: &mut UnitMedia<D, Global>,
    id: EpisodeId,
    mut len: u64,
) {
    while len > 0 {
        let n = len.min(AUDIO_CHUNK as u64) as usize;
        media
            .append_temp(cat, id, &[7; AUDIO_CHUNK][..n])
            .await
            .unwrap();
        len -= n as u64;
    }
}

/// reset/append/publish `len` bytes for `id`, announcing `expected`.
async fn fill_as<D: SectorIo>(
    cat: &mut Cat,
    media: &mut UnitMedia<D, Global>,
    id: EpisodeId,
    len: u64,
    expected: Option<u64>,
) {
    media.reset_temp(cat, id, expected).await.unwrap();
    append_all(cat, media, id, len).await;
    media.publish_temp(cat, id).await.unwrap();
}

/// reset/append/publish `len` bytes for `id`, length unknown up front.
async fn fill<D: SectorIo>(
    cat: &mut Cat,
    media: &mut UnitMedia<D, Global>,
    id: EpisodeId,
    len: u64,
) {
    fill_as(cat, media, id, len, None).await;
}

async fn used(cat: &mut Cat, units: u32) -> u32 {
    let mut table = UnitTable::new_in(units, Global).unwrap();
    cat.load_units(&mut table).await.unwrap();
    table.used()
}

async fn space<D: SectorIo>(cat: &mut Cat, media: &mut UnitMedia<D, Global>) -> Space {
    MediaStore::space(media, cat).await.unwrap()
}

/// The persisted bitmap, the in-RAM bitmap and the union of every record's
/// extents are the same set, and no unit is owned twice.
async fn assert_bitmap_matches_records<D: SectorIo>(
    cat: &mut Cat,
    media: &mut UnitMedia<D, Global>,
) {
    let mut owned = BTreeSet::new();
    let mut after = None;
    let mut ids = [EpisodeId(0); FILE_PAGE];
    let mut buf = [0; 4096];
    loop {
        let n = media.list_ids(cat, after, &mut ids).await.unwrap();
        if n == 0 {
            break;
        }
        for &id in &ids[..n] {
            let record = cat.unit_record(id, &mut buf).await.unwrap().unwrap();
            for extent in record.extents() {
                for unit in extent.start..extent.end().unwrap() {
                    assert!(owned.insert(unit), "unit {unit} owned twice");
                }
            }
        }
        after = Some(ids[n - 1]);
    }
    let units = media.table.units();
    let mut loaded = UnitTable::new_in(units, Global).unwrap();
    cat.load_units(&mut loaded).await.unwrap();
    let persisted: BTreeSet<u32> = (0..units).filter(|&u| loaded.is_allocated(u)).collect();
    let ram: BTreeSet<u32> = (0..units)
        .filter(|&u| media.table.is_allocated(u))
        .collect();
    assert_eq!(persisted, owned);
    assert_eq!(ram, owned);
    assert_eq!(loaded.used() as usize, owned.len());
    assert_eq!(media.table.used() as usize, owned.len());
}

/// Catalogue reads, catalogue writes, catalogue commits, device reads and
/// device writes so far.
fn counters<D: SectorIo>(
    image: &Image,
    media: &UnitMedia<D, Global>,
    disk: (usize, usize),
) -> [usize; 5] {
    [
        image.reads.get(),
        image.writes.get(),
        media.commits,
        disk.0,
        disk.1,
    ]
}

fn delta(before: [usize; 5], after: [usize; 5]) -> [usize; 5] {
    core::array::from_fn(|i| after[i] - before[i])
}

// Regression (t-121): space and append cost must not grow with stored media.
// With 1, 16 and 64 stored episodes on a two-page bitmap, a known-length
// download costs one catalogue commit to reserve its units, then `space()`
// and `append_temp` inside them read, write and commit nothing in the
// catalogue, and touch the device identically.
#[test]
fn space_and_append_cost_is_independent_of_stored_media() {
    const UNITS: u32 = 40_000; // more than one 32,768-unit bitmap page
    let mut costs = Vec::new();
    for episodes in [1u64, 16, 64] {
        block_on(async {
            let image = image(256);
            let mut cat = image.create().await;
            let mut media = open(&mut cat, Disk::counting(UNITS * SECTORS), UNITS).await;
            assert_eq!(media.table.pages(), 2);
            for id in 1..=episodes {
                fill_as(&mut cat, &mut media, EpisodeId(id), 5000, Some(5000)).await;
            }
            let id = EpisodeId(1000);
            let commits = media.commits;
            media
                .reset_temp(&mut cat, id, Some(2 * UNIT_BYTES))
                .await
                .unwrap();
            assert_eq!(media.commits - commits, 1, "one reservation commit");
            let disk = |m: &UnitMedia<Disk, Global>| (m.device().reads, m.device().writes);
            let before = counters(&image, &media, disk(&media));
            space(&mut cat, &mut media).await;
            for _ in 0..2 {
                media
                    .append_temp(&mut cat, id, &[7; AUDIO_CHUNK])
                    .await
                    .unwrap();
            }
            space(&mut cat, &mut media).await;
            costs.push(delta(before, counters(&image, &media, disk(&media))));
        });
    }
    std::println!("(cat reads, cat writes, commits, dev reads, dev writes): {costs:?}");
    assert_eq!(costs[0], [0, 0, 0, 0, 2 * AUDIO_CHUNK / 512], "{costs:?}");
    assert!(costs.windows(2).all(|w| w[0] == w[1]), "{costs:?}");
}

// Regression (t-121): a known length reserves every unit in one commit;
// streaming into the reservation commits nothing.
#[test]
fn known_length_reserves_up_front() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(8 * SECTORS), 8).await;
        let id = EpisodeId(1);
        let len = 3 * UNIT_BYTES + 5;
        media.reset_temp(&mut cat, id, Some(len)).await.unwrap();
        assert_eq!(media.commits, 1);
        assert_eq!(used(&mut cat, 8).await, 4);
        // Reserved units still count as free for this download.
        assert_eq!(space(&mut cat, &mut media).await.free, 8 * UNIT_BYTES);
        append_all(&mut cat, &mut media, id, len).await;
        assert_eq!(media.commits, 1);
        media.publish_temp(&mut cat, id).await.unwrap();
        assert_eq!(media.commits, 2);
        assert_eq!(used(&mut cat, 8).await, 4);
        assert_bitmap_matches_records(&mut cat, &mut media).await;
    });
}

// Regression (t-121): an unknown length grows in doubling batches capped at
// 64 units, one commit per batch, merged into one extent.
#[test]
fn unknown_length_grows_in_doubling_batches() {
    block_on(async {
        let image = image(64);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(200 * SECTORS), 200).await;
        let id = EpisodeId(1);
        media.reset_temp(&mut cat, id, None).await.unwrap();
        assert_eq!(media.commits, 0);
        let mut owned = Vec::new();
        for _ in 0..(129 * UNIT_BYTES as usize / AUDIO_CHUNK) {
            let commits = media.commits;
            media
                .append_temp(&mut cat, id, &[1; AUDIO_CHUNK])
                .await
                .unwrap();
            if media.commits != commits {
                assert_eq!(media.commits - commits, 1);
                owned.push(media.temp.owned);
            }
        }
        assert_eq!(owned, [1, 2, 4, 8, 16, 32, 64, 128, 192]);
        assert_eq!(used(&mut cat, 200).await, 192);
        assert_eq!(media.temp.extents.len(), 1);
        // The unused tail of the batch counts as free.
        assert_eq!(space(&mut cat, &mut media).await.free, 71 * UNIT_BYTES);
    });
}

// Regression (t-121): publishing trims the owned units past the length, in
// the publish commit itself.
#[test]
fn publish_trims_unused_units() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(8 * SECTORS), 8).await;
        let id = EpisodeId(1);
        media
            .reset_temp(&mut cat, id, Some(5 * UNIT_BYTES))
            .await
            .unwrap();
        append_all(&mut cat, &mut media, id, 2 * UNIT_BYTES + 1).await;
        assert_eq!(used(&mut cat, 8).await, 5);
        media.publish_temp(&mut cat, id).await.unwrap();
        assert_eq!(media.commits, 2);
        assert_eq!(used(&mut cat, 8).await, 3);
        let mut buf = [0; 4096];
        let record = cat.unit_record(id, &mut buf).await.unwrap().unwrap();
        assert_eq!((record.total_units(), record.len), (3, 2 * UNIT_BYTES + 1));
        assert_eq!(space(&mut cat, &mut media).await.free, 5 * UNIT_BYTES);
        assert_bitmap_matches_records(&mut cat, &mut media).await;
    });
}

// Regression (t-121): `publish` does not re-sync what `sync_temp` synced.
#[test]
fn publish_skips_a_redundant_sync() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(4 * SECTORS), 4).await;
        let id = EpisodeId(1);
        media.reset_temp(&mut cat, id, Some(100)).await.unwrap();
        append_all(&mut cat, &mut media, id, 100).await;
        assert_eq!(media.sync_temp(&mut cat, id).await.unwrap(), 100);
        let writes = media.device().writes;
        assert_eq!(writes, 1);
        media.publish_temp(&mut cat, id).await.unwrap();
        assert_eq!(media.device().writes, writes);
    });
}

// Regression (t-121): a record holds extents, so its size depends on
// fragmentation, not on the episode length.
#[test]
fn record_size_is_independent_of_episode_length() {
    block_on(async {
        let image = image(64);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(32 * SECTORS), 32).await;
        fill(&mut cat, &mut media, EpisodeId(1), 1).await;
        fill(&mut cat, &mut media, EpisodeId(2), 20 * UNIT_BYTES).await;
        fill_as(
            &mut cat,
            &mut media,
            EpisodeId(3),
            5 * UNIT_BYTES,
            Some(5 * UNIT_BYTES),
        )
        .await;
        let mut buf = [0; 4096];
        for (id, units) in [(1, 1), (2, 20), (3, 5)] {
            let record = cat
                .unit_record(EpisodeId(id), &mut buf)
                .await
                .unwrap()
                .unwrap();
            assert_eq!((record.extent_count(), record.total_units()), (1, units));
        }
        assert_bitmap_matches_records(&mut cat, &mut media).await;
    });
}

#[test]
fn bytes_round_trip_across_unit_and_sector_boundaries() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut disk = Disk::storing(4 * SECTORS);
        let id = EpisodeId(9);
        let mut expected = Vec::new();
        {
            let mut media = open(&mut cat, &mut disk, 4).await;
            media.reset_temp(&mut cat, id, None).await.unwrap();
            let mut sizes = [1000usize, 4096, 3, 511, 1, 512, 4095].iter().cycle();
            let mut next = 0u8;
            while (expected.len() as u64) < UNIT_BYTES + 5000 {
                let n = *sizes.next().unwrap();
                let chunk: Vec<u8> = (0..n)
                    .map(|_| {
                        next = next.wrapping_add(31);
                        next
                    })
                    .collect();
                media.append_temp(&mut cat, id, &chunk).await.unwrap();
                expected.extend_from_slice(&chunk);
            }
            assert_eq!(
                media.sync_temp(&mut cat, id).await.unwrap(),
                expected.len() as u64
            );
            media.publish_temp(&mut cat, id).await.unwrap();
            assert_eq!(
                media.audio_len(&mut cat, id).await.unwrap(),
                Some(expected.len() as u64)
            );
        }
        let mut buf = [0; 4096];
        let record = cat.unit_record(id, &mut buf).await.unwrap().unwrap();
        assert_eq!(record.state, RecordState::Published);
        assert_eq!(record.total_units(), 2);
        let mut actual = Vec::new();
        for extent in record.extents() {
            for s in extent.start * SECTORS..extent.end().unwrap() * SECTORS {
                let mut sector = [0; 512];
                disk.read_sector(s, &mut sector).unwrap();
                actual.extend_from_slice(&sector);
            }
        }
        assert!(actual[expected.len()..].iter().all(|&b| b == 0));
        actual.truncate(expected.len());
        assert!(actual == expected);
    });
}

// Regression (t-121): the bitmap equals the union of records after any history.
#[test]
fn invariant_holds_after_mixed_history() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        {
            let mut media = open(&mut cat, Disk::counting(16 * SECTORS), 16).await;
            fill(&mut cat, &mut media, EpisodeId(1), 2 * UNIT_BYTES).await;
            fill(&mut cat, &mut media, EpisodeId(2), 3 * UNIT_BYTES + 1).await;
            fill_as(&mut cat, &mut media, EpisodeId(3), 1, Some(4 * UNIT_BYTES)).await;
            assert_bitmap_matches_records(&mut cat, &mut media).await;
            media.unlink_audio(&mut cat, EpisodeId(2)).await.unwrap();
            media.unlink_audio(&mut cat, EpisodeId(2)).await.unwrap(); // idempotent
            media
                .reset_temp(&mut cat, EpisodeId(4), Some(2 * UNIT_BYTES))
                .await
                .unwrap();
            append_all(&mut cat, &mut media, EpisodeId(4), 10).await;
            assert_bitmap_matches_records(&mut cat, &mut media).await;
            // Another download frees the first temp.
            media
                .reset_temp(&mut cat, EpisodeId(5), None)
                .await
                .unwrap();
            append_all(&mut cat, &mut media, EpisodeId(5), UNIT_BYTES + 1).await;
            assert_bitmap_matches_records(&mut cat, &mut media).await;
        }
        drop(cat);
        let mut cat = image.reopen().await;
        let mut media = open(&mut cat, Disk::counting(16 * SECTORS), 16).await;
        let mut ids = [EpisodeId(0); FILE_PAGE];
        let n = media.list_ids(&mut cat, None, &mut ids).await.unwrap();
        assert_eq!(&ids[..n], &[EpisodeId(1), EpisodeId(3), EpisodeId(5)]);
        assert_bitmap_matches_records(&mut cat, &mut media).await;
        assert_eq!(media.table.used(), 2 + 1 + 2);
        assert_eq!(media.audio_len(&mut cat, EpisodeId(2)).await.unwrap(), None);
        media.unlink_temp(&mut cat, EpisodeId(5)).await.unwrap();
        assert_bitmap_matches_records(&mut cat, &mut media).await;
        assert_eq!(media.table.used(), 3);
    });
}

async fn queued(
    cat: &mut Cat,
    guid: &str,
    expected: Option<u64>,
    scratch: &mut [u8],
) -> DownloadJob {
    let id = cat
        .upsert(
            SubscriptionId(1),
            EpisodeMetadata {
                guid: Some(guid),
                title: guid,
                enclosure: "https://cdn.example/audio",
                description: None,
                published: Some(1),
                enclosure_bytes: expected,
                duration_seconds: None,
            },
            scratch,
        )
        .await
        .unwrap()
        .id;
    DownloadJob::from_episode(cat.get(id, scratch).await.unwrap().unwrap()).unwrap()
}

const CUT_UNITS: u32 = 32; // capacity above `reserve`'s 16 MiB minimum
const CUT_LEN: u64 = UNIT_BYTES + 3 * AUDIO_CHUNK as u64; // two units

/// Records the catalogue write count around every media call, so the cut
/// test can tear the first and last writes of every operation's commit.
struct Probe<'a, M> {
    inner: M,
    writes: &'a Cell<usize>,
    marks: BTreeSet<usize>,
}
impl<M> Probe<'_, M> {
    fn mark(&mut self) {
        self.marks.insert(self.writes.get());
    }
}
macro_rules! probed {
    ($self:ident, $call:expr) => {{
        $self.mark();
        let result = $call.await;
        $self.mark();
        result
    }};
}
impl<M: MediaStore<PageStore<HostFile>>> MediaStore<PageStore<HostFile>> for Probe<'_, M> {
    type Error = M::Error;
    fn active(&self, id: EpisodeId) -> bool {
        self.inner.active(id)
    }
    async fn space(&mut self, cat: &mut Cat) -> Result<Space, M::Error> {
        probed!(self, self.inner.space(cat))
    }
    async fn reset_temp(
        &mut self,
        cat: &mut Cat,
        id: EpisodeId,
        expected: Option<u64>,
    ) -> Result<(), M::Error> {
        probed!(self, self.inner.reset_temp(cat, id, expected))
    }
    async fn append_temp(
        &mut self,
        cat: &mut Cat,
        id: EpisodeId,
        b: &[u8],
    ) -> Result<(), M::Error> {
        probed!(self, self.inner.append_temp(cat, id, b))
    }
    async fn sync_temp(&mut self, cat: &mut Cat, id: EpisodeId) -> Result<u64, M::Error> {
        probed!(self, self.inner.sync_temp(cat, id))
    }
    async fn publish_temp(&mut self, cat: &mut Cat, id: EpisodeId) -> Result<(), M::Error> {
        probed!(self, self.inner.publish_temp(cat, id))
    }
    async fn unlink_temp(&mut self, cat: &mut Cat, id: EpisodeId) -> Result<(), M::Error> {
        probed!(self, self.inner.unlink_temp(cat, id))
    }
    async fn unlink_audio(&mut self, cat: &mut Cat, id: EpisodeId) -> Result<(), M::Error> {
        probed!(self, self.inner.unlink_audio(cat, id))
    }
    async fn audio_len(&mut self, cat: &mut Cat, id: EpisodeId) -> Result<Option<u64>, M::Error> {
        probed!(self, self.inner.audio_len(cat, id))
    }
    async fn list_ids(
        &mut self,
        cat: &mut Cat,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> Result<usize, M::Error> {
        probed!(self, self.inner.list_ids(cat, after, out))
    }
}

/// Download (reset, append into two units, sync, publish, `finish_download`),
/// then unlink the audio; stops at the first error.
async fn cut_history<M: MediaStore<PageStore<HostFile>>>(
    cat: &mut Cat,
    media: &mut M,
    job: DownloadJob,
    scratch: &mut [u8],
) -> Result<(), ()> {
    let id = job.id;
    let Start::Ready(mut session) = DownloadSession::start(cat, media, job, scratch)
        .await
        .map_err(|_| ())?
    else {
        return Err(());
    };
    session.response(200, Some(CUT_LEN), None).map_err(|_| ())?;
    let mut left = CUT_LEN;
    while left > 0 {
        let n = left.min(AUDIO_CHUNK as u64) as usize;
        session
            .push(cat, media, &[5; AUDIO_CHUNK][..n], scratch)
            .await
            .map_err(|_| ())?;
        left -= n as u64;
    }
    session
        .finish(cat, media, true, scratch)
        .await
        .map_err(|_| ())?;
    media.unlink_audio(cat, id).await.map_err(|_| ())
}

// Regression (t-121): a crash at any operation's catalogue commit never
// leaks a unit.
#[test]
fn cut_after_each_step_leaks_no_unit() {
    let mut scratch = [0; MAX_RECORD_BYTES];
    // An uncut history marks every operation boundary in catalogue writes.
    let (base, total, marks) = block_on(async {
        let image = image(64);
        let mut cat = image.create().await;
        let job = queued(&mut cat, "cut", Some(CUT_LEN), &mut scratch).await;
        let base = image.writes.get();
        let disk = Disk::counting(CUT_UNITS * SECTORS);
        let mut media = Probe {
            inner: open(&mut cat, disk, CUT_UNITS).await,
            writes: &image.writes,
            marks: BTreeSet::new(),
        };
        cut_history(&mut cat, &mut media, job, &mut scratch)
            .await
            .unwrap();
        assert_eq!(used(&mut cat, CUT_UNITS).await, 0);
        (base, image.writes.get(), media.marks)
    });
    // Tear the last two writes before and the first two after every boundary
    // (between media calls, catalogue operations run back to back), the
    // middle of every gap, and a stride through all writes.
    let mut cuts = BTreeSet::new();
    let marks: Vec<usize> = marks.into_iter().chain([base, total]).collect();
    for (i, &m) in marks.iter().enumerate() {
        cuts.extend([m.wrapping_sub(2), m.wrapping_sub(1), m, m + 1]);
        if let Some(&next) = marks.get(i + 1) {
            cuts.insert((m + next) / 2);
        }
    }
    cuts.extend((base..total).step_by(97));
    let cuts: Vec<usize> = cuts
        .into_iter()
        .filter(|c| (base..total).contains(c))
        .collect();
    assert!(cuts.len() > 20, "{cuts:?}");
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let kept: usize = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|t| {
                let cuts = &cuts;
                scope.spawn(move || {
                    cuts.iter()
                        .skip(t)
                        .step_by(threads)
                        .filter(|&&cut| cut_once(base, cut))
                        .count()
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).sum()
    });
    // Cuts after `finish_download` keep the Available episode's units.
    assert!(kept > 0 && kept < cuts.len(), "{kept}");
}

/// Replays the history with catalogue write `cut` torn, reboots, recovers and
/// checks no unit leaked. Returns whether a published record was kept.
fn cut_once(base: usize, cut: usize) -> bool {
    let mut scratch = [0; MAX_RECORD_BYTES];
    block_on(async {
        let image = image(64);
        let mut cat = image.create().await;
        let job = queued(&mut cat, "cut", Some(CUT_LEN), &mut scratch).await;
        assert_eq!(image.writes.get(), base);
        image.cut.set(Some(cut));
        let mut disk = Disk::counting(CUT_UNITS * SECTORS);
        {
            let mut media = open(&mut cat, &mut disk, CUT_UNITS).await;
            let history = cut_history(&mut cat, &mut media, job, &mut scratch).await;
            assert!(history.is_err(), "cut {cut}");
        }
        image.cut.set(None);
        drop(cat);
        let mut cat = image.reopen().await;
        let mut media = open(&mut cat, &mut disk, CUT_UNITS).await;
        crate::recovery::recover_all(&mut cat, &mut media, &mut scratch)
            .await
            .unwrap();
        let used = media.table.used();
        assert_eq!(used + media.table.free(), CUT_UNITS);
        assert_bitmap_matches_records(&mut cat, &mut media).await;
        let mut ids = [EpisodeId(0); FILE_PAGE];
        if used == 0 {
            assert_eq!(media.list_ids(&mut cat, None, &mut ids).await.unwrap(), 0);
            return false;
        }
        // Only a published record of an Available episode may stay.
        assert_eq!(media.list_ids(&mut cat, None, &mut ids).await.unwrap(), 1);
        let audio = cat
            .get(ids[0], &mut scratch)
            .await
            .unwrap()
            .unwrap()
            .state
            .audio;
        assert_eq!(audio, AudioState::Available, "cut {cut}");
        let mut buf = [0; 4096];
        let record = cat.unit_record(ids[0], &mut buf).await.unwrap().unwrap();
        assert_eq!(record.state, RecordState::Published);
        assert_eq!(record.total_units(), used, "cut {cut}");
        true
    })
}

// Regression (t-121): a full card is `Full` before any change, and the
// store stays enabled.
#[test]
fn full_card_reports_full_and_download_waits() {
    block_on(async {
        // A 2-unit store: the third unit cannot be allocated.
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(2 * SECTORS), 2).await;
        let id = EpisodeId(1);
        media.reset_temp(&mut cat, id, None).await.unwrap();
        append_all(&mut cat, &mut media, id, 2 * UNIT_BYTES).await;
        let (commits, writes) = (media.commits, media.device().writes);
        assert!(matches!(
            media.append_temp(&mut cat, id, &[1]).await,
            Err(UnitError::Full)
        ));
        assert_eq!((media.commits, media.device().writes), (commits, writes));
        assert_eq!(media.temp.len, 2 * UNIT_BYTES);
        assert_eq!(space(&mut cat, &mut media).await.free, 0);
        // A reservation larger than the card is `Full` without freeing the temp.
        assert!(matches!(
            media
                .reset_temp(&mut cat, EpisodeId(2), Some(3 * UNIT_BYTES))
                .await,
            Err(UnitError::Full)
        ));
        assert_eq!((media.commits, media.temp.id), (commits, Some(id)));
        assert_eq!(used(&mut cat, 2).await, 2);
        // Still enabled: the temp publishes.
        media.publish_temp(&mut cat, id).await.unwrap();
        assert_bitmap_matches_records(&mut cat, &mut media).await;
    });
    block_on(async {
        // Above the 16 MiB reserve: an unreclaimable published record leaves
        // too little room for a 3 MiB episode, so the download waits.
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(20 * SECTORS), 20).await;
        fill(&mut cat, &mut media, EpisodeId(1000), 2 * UNIT_BYTES).await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let job = queued(&mut cat, "big", Some(3 * UNIT_BYTES), &mut scratch).await;
        assert!(matches!(
            DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
                .await
                .unwrap(),
            Start::WaitingForSpace
        ));
    });
}

/// A store whose odd units are owned by two published records, so every
/// free unit is its own extent.
async fn fragmented(cat: &mut Cat, units: u32) -> UnitMedia<Disk, Global> {
    let mut table = UnitTable::new_in(units, Global).unwrap();
    cat.load_units(&mut table).await.unwrap();
    let odd: Vec<Extent> = (0..units / 2)
        .map(|i| Extent {
            start: 2 * i + 1,
            count: 1,
        })
        .collect();
    for unit in &odd {
        table.claim(*unit);
    }
    let mut rec = [0; 4096];
    for (id, half) in odd.chunks(odd.len().div_ceil(2)).enumerate() {
        let n = encode_record(RecordState::Published, 1, half, &mut rec).unwrap();
        cat.commit_units(&table, &[0], EpisodeId(1000 + id as u64), Some(&rec[..n]))
            .await
            .unwrap();
    }
    open(cat, Disk::counting(units * SECTORS), units).await
}

// Regression (t-121): an episode needing more than `MAX_EXTENTS` extents is
// refused before any change, and the store stays enabled.
#[test]
fn extent_overflow_is_refused_and_store_stays_enabled() {
    block_on(async {
        let units = 2 * (MAX_EXTENTS as u32 + 40);
        let image = image(64);
        let mut cat = image.create().await;
        let mut media = fragmented(&mut cat, units).await;
        let free = media.table.free();
        assert!(free > MAX_EXTENTS as u32);
        assert_bitmap_matches_records(&mut cat, &mut media).await;
        // Reserving one unit more than the extents can hold.
        let id = EpisodeId(1);
        assert!(matches!(
            media
                .reset_temp(&mut cat, id, Some((MAX_EXTENTS as u64 + 1) * UNIT_BYTES))
                .await,
            Err(UnitError::Invalid)
        ));
        assert_eq!((media.commits, media.table.free()), (0, free));
        // Exactly `MAX_EXTENTS` extents fit, in one commit.
        media
            .reset_temp(&mut cat, id, Some(MAX_EXTENTS as u64 * UNIT_BYTES))
            .await
            .unwrap();
        assert_eq!(media.commits, 1);
        assert_eq!(media.temp.extents.len(), MAX_EXTENTS);
        // Growing past them: the reserved units are skipped rather than
        // streamed, as only the temp's length decides when to grow.
        media.temp.len = MAX_EXTENTS as u64 * UNIT_BYTES;
        assert!(matches!(
            media.append_temp(&mut cat, id, &[1]).await,
            Err(UnitError::Invalid)
        ));
        assert_eq!(media.commits, 1);
        assert_eq!(media.table.free(), free - MAX_EXTENTS as u32);
        assert_eq!(
            space(&mut cat, &mut media).await.capacity,
            u64::from(units) * UNIT_BYTES
        );
        media.unlink_temp(&mut cat, id).await.unwrap();
        assert_eq!(media.table.free(), free);
        assert_bitmap_matches_records(&mut cat, &mut media).await;
    });
}

// Regression (t-121): input errors leave the store enabled and unchanged.
#[test]
fn invalid_input_leaves_the_store_enabled() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(4 * SECTORS), 4).await;
        let id = EpisodeId(1);
        assert!(matches!(
            media.reset_temp(&mut cat, EpisodeId(0), None).await,
            Err(UnitError::Invalid)
        ));
        media.reset_temp(&mut cat, id, None).await.unwrap();
        for (other, bytes) in [
            (id, &[][..]),
            (id, &[0; AUDIO_CHUNK + 1][..]),
            (EpisodeId(2), &[1][..]),
        ] {
            assert!(matches!(
                media.append_temp(&mut cat, other, bytes).await,
                Err(UnitError::Invalid)
            ));
        }
        assert!(matches!(
            media.publish_temp(&mut cat, id).await,
            Err(UnitError::Invalid)
        ));
        assert_eq!((media.commits, media.table.used()), (0, 0));
        fill(&mut cat, &mut media, id, 10).await;
    });
}

// Regression (t-121): a store larger than u32 sectors can address is refused.
#[test]
fn oversize_region_is_refused() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        assert!(matches!(
            UnitMedia::open_in(&mut cat, Disk::counting(0), MAX_UNITS + 1, Global).await,
            Err(UnitError::Invalid)
        ));
    });
}

#[test]
fn publish_refuses_existing_audio() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(4 * SECTORS), 4).await;
        fill(&mut cat, &mut media, EpisodeId(1), 10).await;
        assert!(matches!(
            media.publish_temp(&mut cat, EpisodeId(1)).await,
            Err(UnitError::Exists)
        ));
        assert_eq!(used(&mut cat, 4).await, 1);
        assert_eq!(space(&mut cat, &mut media).await.free, 3 * UNIT_BYTES);
    });
}

#[test]
fn reset_of_published_id_is_exists() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(4 * SECTORS), 4).await;
        fill(&mut cat, &mut media, EpisodeId(1), 10).await;
        media
            .reset_temp(&mut cat, EpisodeId(2), None)
            .await
            .unwrap();
        append_all(&mut cat, &mut media, EpisodeId(2), 1).await;
        let commits = media.commits;
        assert!(matches!(
            media.reset_temp(&mut cat, EpisodeId(1), None).await,
            Err(UnitError::Exists)
        ));
        // Nothing changed, not even the other download's temp.
        assert_eq!(
            (media.commits, media.temp.id),
            (commits, Some(EpisodeId(2)))
        );
        assert_eq!(used(&mut cat, 4).await, 2);
        assert_eq!(
            space(&mut cat, &mut media).await.free,
            2 * UNIT_BYTES + UNIT_BYTES - 1
        );
    });
}

#[test]
fn reset_frees_a_previous_temp() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(4 * SECTORS), 4).await;
        media
            .reset_temp(&mut cat, EpisodeId(1), None)
            .await
            .unwrap();
        append_all(&mut cat, &mut media, EpisodeId(1), UNIT_BYTES + 1).await;
        media
            .reset_temp(&mut cat, EpisodeId(2), None)
            .await
            .unwrap(); // other id
        assert_eq!(used(&mut cat, 4).await, 0);
        append_all(&mut cat, &mut media, EpisodeId(2), 1).await;
        media
            .reset_temp(&mut cat, EpisodeId(2), None)
            .await
            .unwrap(); // same id
        assert_eq!(used(&mut cat, 4).await, 0);
        assert_eq!(space(&mut cat, &mut media).await.free, 4 * UNIT_BYTES);
        // Freed units count toward a reservation.
        append_all(&mut cat, &mut media, EpisodeId(2), 1).await;
        media
            .reset_temp(&mut cat, EpisodeId(3), Some(4 * UNIT_BYTES))
            .await
            .unwrap();
        assert_eq!(used(&mut cat, 4).await, 4);
        assert_bitmap_matches_records(&mut cat, &mut media).await;
    });
}

// Regression (t-121): only errors that may leave RAM inconsistent are fatal.
#[test]
fn only_inconsistent_state_errors_are_fatal() {
    type M = UnitMedia<Disk, Global>;
    type E = UnitError<theo_storage::page_store::PageError<std::io::Error>>;
    let fatal = |e: &E| <M as MediaStore<PageStore<HostFile>>>::is_fatal(e);
    for e in [UnitError::Invalid, UnitError::Exists, UnitError::Full] {
        assert!(!fatal(&e), "{e:?}");
    }
    for e in [
        UnitError::Catalogue(Error::Corrupt),
        UnitError::Device(littlefs_rust::Error::Io),
        UnitError::Alloc,
        UnitError::Disabled,
    ] {
        assert!(fatal(&e), "{e:?}");
    }
}

// Regression (t-121): the storage owner future runs on core1's 48 KiB stack
// before it is pinned in PSRAM. A 4 KiB extent plan held across an await was
// copied into every enclosing future and overflowed that stack at boot, so no
// unit-store future may hold extent-sized state across an await.
#[test]
fn store_futures_hold_no_extent_sized_state() {
    // Below one extent plan (510 x 8 bytes), so holding one fails the test.
    const MAX_FUTURE: usize = 4096;
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(8 * SECTORS), 8).await;
        let id = EpisodeId(1);
        let reset = size_of_val(&media.reset_temp(&mut cat, id, Some(1)));
        let append = size_of_val(&media.append_temp(&mut cat, id, &[0]));
        let publish = size_of_val(&media.publish_temp(&mut cat, id));
        let unlink = size_of_val(&media.unlink_temp(&mut cat, id));
        let unlink_audio = size_of_val(&media.unlink_audio(&mut cat, id));
        let sizes = [
            ("reset_temp", reset),
            ("append_temp", append),
            ("publish_temp", publish),
            ("unlink_temp", unlink),
            ("unlink_audio", unlink_audio),
        ];
        for (name, size) in sizes {
            assert!(size <= MAX_FUTURE, "{name} future is {size} bytes");
        }
    });
}
