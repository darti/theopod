use super::tests::metadata;
use super::*;
use crate::test_support::Image;
use theo_storage::test_disk::Crash;

/// `EVICTABLE` holds exactly the read `Available` episodes, keyed in the
/// policy's order, and its first entry is `choose_reclaim`'s choice.
fn assert_index(cat: &mut Catalogue, context: &str) {
    use crate::reclaim::{ReclaimCandidate, choose_reclaim};
    let mut scratch = [0; MAX_RECORD_BYTES];
    let (mut expected, mut candidates) = (std::vec::Vec::new(), std::vec::Vec::new());
    let mut after = None;
    loop {
        let page = cat
            .scan(after, &mut scratch, |e| {
                if evictable(e.state) {
                    expected.push(evict_key(e.id, e.metadata.published));
                }
                candidates.push(ReclaimCandidate {
                    id: e.id,
                    read: e.state.read,
                    audio: e.state.audio,
                    published: e.metadata.published,
                    import_order: e.id.0,
                    active: false,
                    bytes: 0,
                });
            })
            .unwrap();
        after = page.resume_after;
        if !page.has_more {
            break;
        }
    }
    expected.sort();
    let actual: std::vec::Vec<_> = {
        let tx = redb::ReadableDatabase::begin_read(&cat.db).unwrap();
        let table = tx.open_table(crate::tables::EVICTABLE).unwrap();
        redb::ReadableTable::iter(&table)
            .unwrap()
            .map(|entry| entry.unwrap().0.value())
            .collect()
    };
    assert_eq!(actual, expected, "{context}");
    assert_eq!(
        cat.first_evictable(|_| false).unwrap(),
        choose_reclaim(candidates, 1).map(|c| c.id),
        "{context}"
    );
}

// Regression (t-133): the eviction index follows every state change
// (upsert with a new date, mark-read, each audio transition, download
// completion), so reclaim reads its first entry instead of scanning.
#[test]
fn eviction_index_follows_every_state_change() {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let dates = [None, Some(5), Some(1), Some(5), Some(-3)];
    let upsert = |cat: &mut Catalogue, k: u64, date: Option<i64>, scratch: &mut [u8]| {
        let guid = std::format!("g{k}");
        let mut m = metadata(Some(&guid), "t", "https://x/a");
        m.published = date;
        cat.upsert(SubscriptionId(1), m, scratch).map(|o| o.id)
    };
    let mut ids = std::vec::Vec::new();
    for k in 0..8 {
        ids.push(upsert(&mut cat, k, dates[k as usize % dates.len()], &mut scratch).unwrap());
    }
    let states = [
        AudioState::Missing,
        AudioState::Queued,
        AudioState::Downloading,
        AudioState::Available,
        AudioState::Deleting,
        AudioState::Reclaimed,
    ];
    let mut seed = 0x2545_f491_u64;
    let mut indexed = 0;
    for step in 0..600 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let r = seed >> 33;
        let k = (r % 8) as usize;
        let id = ids[k];
        let op = (r >> 3) % 5;
        let result = match op {
            0 => upsert(
                &mut cat,
                k as u64,
                dates[(r >> 6) as usize % dates.len()],
                &mut scratch,
            )
            .map(drop),
            1 => cat.mark_read(id, &mut scratch),
            2 => cat.finish_download(id, 1 + r % 100, &mut scratch),
            _ => cat.transition_audio(id, states[(r >> 6) as usize % states.len()], &mut scratch),
        };
        assert!(
            matches!(result, Ok(()) | Err(Error::InvalidState)),
            "step {step}: {result:?}"
        );
        assert_index(&mut cat, &std::format!("step {step} op {op}"));
        indexed += usize::from(cat.first_evictable(|_| false).unwrap().is_some());
    }
    // The sequence does reach evictable states.
    assert!(indexed > 30, "{indexed}");
}

// Regression (t-133): the index entry is written in the episode's own
// transaction: a cut at any write of the commit that makes an episode
// evictable reopens with both or neither.
#[test]
fn eviction_index_and_state_are_one_commit() {
    let probe = Image::new();
    let setup = |image: &Image| {
        let mut cat = image.create();
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut m = metadata(Some("a"), "a", "https://x/a");
        m.published = Some(7);
        let id = cat.upsert(SubscriptionId(1), m, &mut scratch).unwrap().id;
        cat.transition_audio(id, AudioState::Downloading, &mut scratch)
            .unwrap();
        cat.finish_download(id, 10, &mut scratch).unwrap();
        (cat, id)
    };
    let (mut cat, id) = setup(&probe);
    let start = probe.disk.state().writes;
    cat.mark_read(id, &mut [0; MAX_RECORD_BYTES]).unwrap();
    let total = probe.disk.state().writes - start;
    for cut in 0..total {
        for prefix in [0, 511] {
            for mode in [Crash::DropPending, Crash::KeepPending] {
                let context = std::format!("cut {cut}/{total} prefix {prefix} {mode:?}");
                let image = Image::new();
                let (mut cat, id) = setup(&image);
                let start = image.disk.state().writes;
                image.disk.state().cut = Some((start + cut, prefix));
                let _ = cat.mark_read(id, &mut [0; MAX_RECORD_BYTES]);
                drop(cat);
                image.disk.crash(mode);
                image.disk.state().cut = None;
                let mut cat = image.reopen();
                assert_index(&mut cat, &context);
            }
        }
    }
}
