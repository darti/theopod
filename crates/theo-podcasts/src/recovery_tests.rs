use super::*;
use crate::{
    media_test_support::{HostMedia, root_for},
    test_support::Image,
};
use futures::executor::block_on;
#[test]
fn unfinished_files_are_removed_by_local_id_and_read_state_survives() {
    // Regression (t-109): a reset must not publish a partial or requeue read audio.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = HostMedia::new(root_for(&image));
        let mut scratch = [0; MAX_RECORD_BYTES];
        let id = cat
            .upsert(
                SubscriptionId(1),
                EpisodeMetadata {
                    guid: Some("a"),
                    title: "a",
                    enclosure: "https://cdn.example/a",
                    description: None,
                    published: None,
                    enclosure_bytes: None,
                    duration_seconds: None,
                },
                &mut scratch,
            )
            .unwrap()
            .id;
        cat.transition_audio(id, AudioState::Downloading, &mut scratch)
            .unwrap();
        cat.mark_read(id, &mut scratch).unwrap();
        std::fs::write(media.path(id, true), b"partial").unwrap();
        std::fs::write(media.path(EpisodeId(999), true), b"orphan").unwrap();
        drop(cat);
        let mut cat = image.reopen();
        recover_all(&mut cat, &mut media, &mut scratch)
            .await
            .unwrap();
        assert!(!media.path(id, true).exists());
        assert!(!media.path(EpisodeId(999), true).exists());
        let episode = cat.get(id, &mut scratch).unwrap().unwrap();
        assert!(episode.state.read);
        assert_eq!(episode.state.audio, AudioState::Missing);
    });
}
#[test]
fn deleting_intent_survives_each_unlink_boundary_without_unread_loss() {
    // Regression (t-109): unlink failures preserve tombstones and cannot select unread audio.
    use crate::{media_test_support::available, reclaim::make_space};
    block_on(async {
        let mut cuts = std::collections::VecDeque::from([None]);
        while let Some(cut) = cuts.pop_front() {
            let image = Image::new();
            let mut cat = image.create();
            let mut media = HostMedia::new(root_for(&image));
            media.physical_sync = false;
            let mut scratch = [0; MAX_RECORD_BYTES];
            let old = available(
                &mut cat,
                &mut media,
                "read",
                Some(1),
                true,
                &[1; 4096],
                &mut scratch,
            );
            let unread = available(
                &mut cat,
                &mut media,
                "unread",
                Some(0),
                false,
                b"keep",
                &mut scratch,
            );
            media.other_bytes = media.capacity - reserve(media.capacity) - 4100 - 4096;
            media.fail_at = cut;
            let result = make_space(&mut cat, &mut media, 8192, &mut scratch).await;
            if cut.is_none() {
                assert!(result.unwrap());
                cuts.extend((0..media.steps).map(Some));
            } else {
                assert!(result.is_err());
            }
            drop(cat);
            let mut cat = image.reopen();
            media.reopen();
            recover_all(&mut cat, &mut media, &mut scratch)
                .await
                .unwrap();
            let old_state = cat.get(old, &mut scratch).unwrap().unwrap().state;
            assert!(old_state.read);
            match old_state.audio {
                AudioState::Available => assert!(media.path(old, false).exists()),
                AudioState::Reclaimed => assert!(!media.path(old, false).exists()),
                other => panic!("{other:?}"),
            }
            let episode = cat.get(unread, &mut scratch).unwrap().unwrap();
            assert!(!episode.state.read);
            assert_eq!(episode.state.audio, AudioState::Available);
            assert_eq!(std::fs::read(media.path(unread, false)).unwrap(), b"keep");
        }
    });
}
#[test]
fn missing_or_short_completed_file_is_not_available_and_active_files_are_protected() {
    // Regression (t-109): availability is checked against persisted actual length on reboot.
    use crate::media_test_support::available;
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let short = available(
            &mut cat,
            &mut media,
            "short",
            None,
            false,
            b"complete",
            &mut scratch,
        );
        let absent = available(
            &mut cat,
            &mut media,
            "absent",
            None,
            true,
            b"complete",
            &mut scratch,
        );
        let active = available(
            &mut cat,
            &mut media,
            "active",
            None,
            false,
            b"keep",
            &mut scratch,
        );
        std::fs::write(media.path(short, false), b"cut").unwrap();
        std::fs::remove_file(media.path(absent, false)).unwrap();
        media.active.insert(active.0);
        recover_all(&mut cat, &mut media, &mut scratch)
            .await
            .unwrap();
        assert_eq!(
            cat.get(short, &mut scratch).unwrap().unwrap().state.audio,
            AudioState::Queued
        );
        let e = cat.get(absent, &mut scratch).unwrap().unwrap();
        assert!(e.state.read);
        assert_eq!(e.state.audio, AudioState::Missing);
        assert_eq!(std::fs::read(media.path(active, false)).unwrap(), b"keep");
    });
}
#[test]
fn failure_after_unlink_retains_read_deletion_intent_until_reopen() {
    // Regression (t-109): durable intent survives unlink-to-database commit failure.
    use crate::{media_test_support::available, reclaim::make_space};
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let id = available(
            &mut cat,
            &mut media,
            "read",
            None,
            true,
            &[1; 4096],
            &mut scratch,
        );
        media.other_bytes = media.capacity - reserve(media.capacity) - 4096 - 4096;
        media.after_unlink = Some(image.disk.clone());
        assert!(
            make_space(&mut cat, &mut media, 8192, &mut scratch)
                .await
                .is_err()
        );
        assert!(!media.path(id, false).exists());
        drop(cat);
        image.disk.state().fail_writes = false;
        let mut cat = image.reopen();
        media.reopen();
        let e = cat.get(id, &mut scratch).unwrap().unwrap();
        assert!(e.state.read);
        assert_eq!(e.state.audio, AudioState::Deleting);
        recover_all(&mut cat, &mut media, &mut scratch)
            .await
            .unwrap();
        let e = cat.get(id, &mut scratch).unwrap().unwrap();
        assert!(e.state.read);
        assert_eq!(e.state.audio, AudioState::Reclaimed);
    });
}
#[test]
fn recovery_steps_leave_other_orphans_untouched_until_admitted() {
    // Regression (t-110): one admitted recovery unit cannot consume an arbitrary library before control/log admission.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = HostMedia::new(root_for(&image));
        let mut scratch = [0; MAX_RECORD_BYTES];
        for id in 1..=20 {
            std::fs::write(media.path(EpisodeId(id), true), b"partial").unwrap();
        }
        let mut recovery = Recovery::new();
        let mut completed = false;
        for _ in 0..100 {
            let before = std::fs::read_dir(&media.root).unwrap().count();
            completed = recovery
                .step(&mut cat, &mut media, &mut scratch)
                .await
                .unwrap();
            let after = std::fs::read_dir(&media.root).unwrap().count();
            assert!(before - after <= 1, "one unit recovered more than one file");
            if completed {
                break;
            }
        }
        assert!(completed);
        assert_eq!(std::fs::read_dir(&media.root).unwrap().count(), 0);
    });
}
