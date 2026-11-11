use super::*;
use crate::{
    catalogue::*,
    download::{DownloadJob, DownloadSession, Start},
    test_support::{HostFile, Image},
    units::{MAX_RECORD_UNITS, UNIT_BYTES},
};
use allocator_api2::alloc::Global;
use core::cell::Cell;
use futures::executor::block_on;
use littlefs_rust::Error;
use std::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use theo_storage::page_store::PageStore;

type Cat = Catalogue<PageStore<HostFile>>;

/// Raw media device: bounds-checked, counting, storing only when `keep`.
struct Disk {
    sectors: BTreeMap<u32, [u8; 512]>,
    keep: bool,
    reads: usize,
    writes: usize,
    limit: u32,
}
impl Disk {
    fn counting(limit: u32) -> Self {
        Self {
            sectors: BTreeMap::new(),
            keep: false,
            reads: 0,
            writes: 0,
            limit,
        }
    }
    fn storing(limit: u32) -> Self {
        Self {
            keep: true,
            ..Self::counting(limit)
        }
    }
}
impl SectorIo for Disk {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        if sector >= self.limit {
            return Err(Error::Io);
        }
        self.reads += 1;
        *out = self.sectors.get(&sector).copied().unwrap_or([0; 512]);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        if sector >= self.limit {
            return Err(Error::Io);
        }
        self.writes += 1;
        if self.keep {
            self.sectors.insert(sector, *data);
        }
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn check(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
// Lets a test keep the disk across a simulated reboot.
impl SectorIo for &mut Disk {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        (**self).read_sector(sector, out)
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        (**self).write_sector(sector, data)
    }
    fn sync(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn check(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

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

/// reset/append/publish `len` bytes for `id`.
async fn fill<D: SectorIo>(
    cat: &mut Cat,
    media: &mut UnitMedia<D, Global>,
    id: EpisodeId,
    len: u64,
) {
    media.reset_temp(cat, id).await.unwrap();
    append_all(cat, media, id, len).await;
    media.publish_temp(cat, id).await.unwrap();
}

async fn used(cat: &mut Cat, units: u32) -> u32 {
    let mut table = UnitTable::new_in(units, Global).unwrap();
    cat.load_units(&mut table).await.unwrap();
    table.used()
}

// Regression (t-121): space and append cost must not grow with stored media.
// The cost is (catalogue commits, device reads, device writes) over space,
// reset and two appends. Raw EKV sector writes are printed but not compared:
// they follow EKV's merge schedule, i.e. commit history, not stored bytes.
#[test]
fn space_and_append_cost_is_independent_of_stored_media() {
    let mut costs = Vec::new();
    let mut raw = Vec::new();
    for stored_mib in [1u64, 4, 64] {
        block_on(async {
            let image = image(256);
            let mut cat = image.create().await;
            let mut media = open(&mut cat, Disk::counting(200 * SECTORS), 200).await;
            fill(&mut cat, &mut media, EpisodeId(1), stored_mib << 20).await;
            let (db_writes, reads, writes) = (
                image.writes.get(),
                media.device().reads,
                media.device().writes,
            );
            let mut commits = 0;
            let mut last = db_writes;
            let mut step = |commits: &mut usize| {
                let now = image.writes.get();
                *commits += usize::from(now != last);
                last = now;
            };
            MediaStore::space(&mut media, &mut cat).await.unwrap();
            step(&mut commits);
            media.reset_temp(&mut cat, EpisodeId(2)).await.unwrap();
            step(&mut commits);
            media
                .append_temp(&mut cat, EpisodeId(2), &[7; 4096])
                .await
                .unwrap(); // first unit
            step(&mut commits);
            media
                .append_temp(&mut cat, EpisodeId(2), &[7; 4096])
                .await
                .unwrap(); // same unit
            step(&mut commits);
            costs.push((
                commits,
                media.device().reads - reads,
                media.device().writes - writes,
            ));
            raw.push(image.writes.get() - db_writes);
        });
    }
    std::println!("(catalogue commits, device reads, device writes): {costs:?}");
    std::println!("raw catalogue sector writes: {raw:?}");
    assert_eq!(costs[0], (1, 0, 2 * AUDIO_CHUNK / 512), "{costs:?}");
    assert!(costs.windows(2).all(|w| w[0] == w[1]), "{costs:?}");
}

// Regression (t-121): one catalogue commit per unit, none between unit boundaries.
#[test]
fn one_catalogue_commit_per_unit() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(8 * SECTORS), 8).await;
        let id = EpisodeId(1);
        media.reset_temp(&mut cat, id).await.unwrap();
        let mut committing = 0;
        for _ in 0..(3 * UNIT_BYTES as usize / AUDIO_CHUNK) {
            let before = image.writes.get();
            media
                .append_temp(&mut cat, id, &[1; AUDIO_CHUNK])
                .await
                .unwrap();
            if image.writes.get() != before {
                committing += 1;
            }
        }
        assert_eq!(committing, 3);
        assert_eq!(used(&mut cat, 8).await, 3);
        assert_eq!(
            MediaStore::space(&mut media, &mut cat).await.unwrap().free,
            5 * UNIT_BYTES
        );
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
            media.reset_temp(&mut cat, id).await.unwrap();
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
        assert_eq!(record.count(), 2);
        let mut actual = Vec::new();
        for i in 0..record.count() {
            for s in 0..SECTORS {
                let mut sector = [0; 512];
                disk.read_sector(record.unit(i) * SECTORS + s, &mut sector)
                    .unwrap();
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
            fill(&mut cat, &mut media, EpisodeId(3), 1).await;
            media.unlink_audio(&mut cat, EpisodeId(2)).await.unwrap();
            media.unlink_audio(&mut cat, EpisodeId(2)).await.unwrap(); // idempotent
        }
        drop(cat);
        let mut cat = image.reopen().await;
        let mut media = open(&mut cat, Disk::counting(16 * SECTORS), 16).await;
        let mut ids = [EpisodeId(0); FILE_PAGE];
        let n = media.list_ids(&mut cat, None, &mut ids).await.unwrap();
        assert_eq!(&ids[..n], &[EpisodeId(1), EpisodeId(3)]);
        let mut buf = [0; 4096];
        let mut sum = 0;
        for id in [EpisodeId(1), EpisodeId(3)] {
            sum += cat
                .unit_record(id, &mut buf)
                .await
                .unwrap()
                .unwrap()
                .count() as u32;
        }
        assert_eq!(sum, 3);
        assert_eq!(media.table.used(), sum);
        assert_eq!(media.table.free() + media.table.used(), 16);
        assert_eq!(media.audio_len(&mut cat, EpisodeId(2)).await.unwrap(), None);
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
    async fn reset_temp(&mut self, cat: &mut Cat, id: EpisodeId) -> Result<(), M::Error> {
        probed!(self, self.inner.reset_temp(cat, id))
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
        if used == 0 {
            return false;
        }
        // Only a published record of an Available episode may stay.
        let mut ids = [EpisodeId(0); FILE_PAGE];
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
        assert_eq!(record.count() as u32, used, "cut {cut}");
        true
    })
}

#[test]
fn full_card_reports_full_and_download_waits() {
    block_on(async {
        // A 2-unit store: the third unit cannot be allocated.
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(2 * SECTORS), 2).await;
        let id = EpisodeId(1);
        media.reset_temp(&mut cat, id).await.unwrap();
        append_all(&mut cat, &mut media, id, 2 * UNIT_BYTES).await;
        assert!(matches!(
            media.append_temp(&mut cat, id, &[1]).await,
            Err(UnitError::Full)
        ));
        assert!(matches!(
            MediaStore::space(&mut media, &mut cat).await,
            Err(UnitError::Disabled)
        ));
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

#[test]
fn oversize_episode_is_refused() {
    block_on(async {
        let units = MAX_RECORD_UNITS as u32 + 1;
        let image = image(64);
        let mut cat = image.create().await;
        let id = EpisodeId(1);
        {
            let mut media = open(&mut cat, Disk::counting(units * SECTORS), units).await;
            media.reset_temp(&mut cat, id).await.unwrap();
            append_all(
                &mut cat,
                &mut media,
                id,
                MAX_RECORD_UNITS as u64 * UNIT_BYTES,
            )
            .await;
            assert!(matches!(
                media.append_temp(&mut cat, id, &[1]).await,
                Err(UnitError::Invalid)
            ));
            assert_eq!(media.table.used(), MAX_RECORD_UNITS as u32);
        }
        // The refusal disabled the store; a reopened one frees the temp.
        let mut media = open(&mut cat, Disk::counting(units * SECTORS), units).await;
        assert_eq!(media.table.used(), MAX_RECORD_UNITS as u32);
        media.unlink_temp(&mut cat, id).await.unwrap();
        assert_eq!(media.table.free(), units);
        assert_eq!(used(&mut cat, units).await, 0);
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
    });
}

#[test]
fn reset_of_published_id_is_exists() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(4 * SECTORS), 4).await;
        fill(&mut cat, &mut media, EpisodeId(1), 10).await;
        assert!(matches!(
            media.reset_temp(&mut cat, EpisodeId(1)).await,
            Err(UnitError::Exists)
        ));
        assert_eq!(used(&mut cat, 4).await, 1);
    });
}

#[test]
fn reset_frees_a_previous_temp() {
    block_on(async {
        let image = image(32);
        let mut cat = image.create().await;
        let mut media = open(&mut cat, Disk::counting(4 * SECTORS), 4).await;
        media.reset_temp(&mut cat, EpisodeId(1)).await.unwrap();
        append_all(&mut cat, &mut media, EpisodeId(1), UNIT_BYTES + 1).await;
        media.reset_temp(&mut cat, EpisodeId(2)).await.unwrap(); // other id
        assert_eq!(used(&mut cat, 4).await, 0);
        append_all(&mut cat, &mut media, EpisodeId(2), 1).await;
        media.reset_temp(&mut cat, EpisodeId(2)).await.unwrap(); // same id
        assert_eq!(used(&mut cat, 4).await, 0);
        assert_eq!(
            MediaStore::space(&mut media, &mut cat).await.unwrap().free,
            4 * UNIT_BYTES
        );
    });
}
