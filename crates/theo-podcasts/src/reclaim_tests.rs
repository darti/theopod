use super::*;
fn candidate(id: u64, date: Option<i64>, bytes: u64) -> ReclaimCandidate {
    ReclaimCandidate {
        id: EpisodeId(id),
        read: true,
        audio: AudioState::Available,
        published: date,
        import_order: id,
        active: false,
        bytes,
    }
}
#[test]
fn only_actual_pressure_selects_oldest_eligible_audio() {
    // Regression (t-109): no periodic purge or deletion of unread/active audio.
    let old = candidate(1, Some(10), 50);
    let new = candidate(2, Some(20), 70);
    let mut unread = candidate(3, Some(1), 999);
    unread.read = false;
    let mut active = candidate(4, Some(2), 999);
    active.active = true;
    let mut downloading = candidate(5, Some(3), 999);
    downloading.audio = AudioState::Downloading;
    assert_eq!(
        choose_reclaim([new, unread, active, old, downloading], 0),
        None
    );
    assert_eq!(
        choose_reclaim([new, unread, active, old, downloading], 60),
        Some(old)
    );
    assert_eq!(
        choose_reclaim([new, unread, active, downloading], 10),
        Some(new)
    );
    assert_eq!(choose_reclaim([unread, active, downloading], 10), None);
    assert_eq!(
        choose_reclaim([candidate(7, None, 10), candidate(6, None, 10)], 1)
            .unwrap()
            .id,
        EpisodeId(6)
    );
    assert_eq!(
        choose_reclaim([candidate(7, Some(1), 10), candidate(6, Some(1), 10)], 1)
            .unwrap()
            .id,
        EpisodeId(6)
    );
}
#[test]
fn reclamation_stops_at_deficit_and_preserves_protected_files() {
    // Regression (t-109): durable unlink occurs only for pressure and never crosses protection.
    use crate::{
        catalogue::*,
        media::*,
        media_test_support::{HostMedia, available, make_space_all, root_for},
        test_support::Image,
    };
    futures::executor::block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let old = available(
            &mut cat,
            &mut media,
            "old",
            Some(1),
            true,
            &[1; 4096],
            &mut scratch,
        );
        let new = available(
            &mut cat,
            &mut media,
            "new",
            Some(2),
            true,
            &[2; 8192],
            &mut scratch,
        );
        let unread = available(
            &mut cat,
            &mut media,
            "unread",
            Some(0),
            false,
            &[3; 4096],
            &mut scratch,
        );
        let active = available(
            &mut cat,
            &mut media,
            "active",
            Some(0),
            true,
            &[4; 4096],
            &mut scratch,
        );
        media.active.insert(active.0);
        media.other_bytes = media.capacity - reserve(media.capacity) - 20480 - 4096;
        assert!(
            make_space_all(&mut cat, &mut media, 4096, &mut scratch)
                .await
                .unwrap()
        );
        assert_eq!(std::fs::read(media.path(old, false)).unwrap().len(), 4096);
        assert_eq!(
            cat.get(old, &mut scratch).unwrap().unwrap().state.audio,
            AudioState::Available
        );
        assert!(
            make_space_all(&mut cat, &mut media, 8192, &mut scratch)
                .await
                .unwrap()
        );
        assert!(!media.path(old, false).exists());
        assert!(media.path(new, false).exists());
        let e = cat.get(old, &mut scratch).unwrap().unwrap();
        assert!(e.state.read);
        assert_eq!(e.state.audio, AudioState::Reclaimed);
        assert!(
            make_space_all(&mut cat, &mut media, 16000, &mut scratch)
                .await
                .unwrap()
        );
        assert!(!media.path(new, false).exists());
        assert!(
            !make_space_all(&mut cat, &mut media, 32000, &mut scratch)
                .await
                .unwrap()
        );
        assert!(media.path(unread, false).exists());
        assert!(media.path(active, false).exists());
    });
}

// Regression (t-133): the eviction round spans requests of at most one
// commit each, and still evicts the policy's oldest choice when it lies
// pages after the cursor's start.
#[test]
fn each_reclaim_step_commits_at_most_once_and_keeps_the_policy_order() {
    use crate::{
        catalogue::*,
        media::*,
        media_test_support::{HostMedia, available, root_for},
        reclaim::{Reclaim, SpaceStep, make_space},
        test_support::Image,
    };
    futures::executor::block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        // More episodes than `RECLAIM_SCAN_PAGES` scan pages; the oldest
        // was imported last.
        let count = (RECLAIM_SCAN_PAGES * MAX_SCAN_RECORDS + 4) as i64;
        let mut ids = std::vec::Vec::new();
        for i in 0..count {
            let guid = std::format!("e{i}");
            ids.push(available(
                &mut cat,
                &mut media,
                &guid,
                Some(count - i),
                true,
                &[1; 16],
                &mut scratch,
            ));
        }
        let oldest = *ids.last().unwrap();
        media.other_bytes = media.capacity - reserve(media.capacity) - 16 * count as u64;
        let mut reclaim = Reclaim::new();
        let mut steps = 0;
        loop {
            let commits = cat.commits();
            let step = make_space(&mut cat, &mut media, &mut reclaim, 16, &mut scratch)
                .await
                .unwrap();
            assert!(cat.commits() - commits <= 1);
            steps += 1;
            if step == SpaceStep::Ready {
                break;
            }
            assert_eq!(step, SpaceStep::Pending);
        }
        // Two scan steps, the `Deleting` intent, then unlink and `Reclaimed`.
        assert_eq!(steps, 5);
        let state = cat.get(oldest, &mut scratch).unwrap().unwrap().state;
        assert_eq!(state.audio, AudioState::Reclaimed);
        assert!(!media.path(oldest, false).exists());
        for &id in &ids[..ids.len() - 1] {
            let state = cat.get(id, &mut scratch).unwrap().unwrap().state;
            assert_eq!(state.audio, AudioState::Available);
        }
    });
}
