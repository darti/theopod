use super::*;
use crate::{
    media_test_support::{HostMedia, root_for},
    test_support::{HostFile, Image},
};
use futures::executor::block_on;
use theo_storage::page_store::PageStore;
pub(super) async fn queued(
    cat: &mut Catalogue<PageStore<HostFile>>,
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
#[test]
fn durable_publication_is_required_before_available() {
    // Regression (t-109): streamed bytes are not playable until sync+publish+DB commit.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut media = HostMedia::new(root_for(&image));
        let mut scratch = [0; MAX_RECORD_BYTES];
        let job = queued(&mut cat, "a", Some(3), &mut scratch).await;
        let id = job.id;
        let Start::Ready(mut session) =
            DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
                .await
                .unwrap()
        else {
            panic!("not started")
        };
        session.response(200, Some(3), None).unwrap();
        assert_eq!(
            session
                .push(&mut cat, &mut media, b"abc", &mut scratch)
                .await
                .unwrap(),
            ChunkOutcome::Written
        );
        assert!(!media.path(id, false).exists());
        assert_eq!(
            cat.get(id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Downloading
        );
        assert_eq!(
            session
                .finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::Complete
        );
        drop(cat);
        let mut cat = image.reopen().await;
        assert_eq!(std::fs::read(media.path(id, false)).unwrap(), b"abc");
        assert_eq!(cat.completed_audio_len(id).await.unwrap(), Some(3));
        assert_eq!(
            cat.get(id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Available
        );
    });
}
#[test]
fn short_or_failed_bodies_and_partial_http_never_publish() {
    // Regression (t-109): restart-only transfer rejects short bodies,206 and range metadata.
    block_on(async {
        let image = Image::new();
        image.sync_to_disk.set(false);
        let mut cat = image.create().await;
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        for (i, status, length, range) in [
            (0, 206, Some(3), Some("bytes 0-2/3")),
            (1, 200, Some(3), Some("bytes 1-2/3")),
            (2, 200, Some(4), None),
            (3, 500, None, None),
        ] {
            let job = queued(&mut cat, &std::format!("bad{i}"), Some(3), &mut scratch).await;
            let id = job.id;
            let Start::Ready(mut s) =
                DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
                    .await
                    .unwrap()
            else {
                panic!()
            };
            assert_eq!(
                s.response(status, length, range),
                Err(DownloadOutcome::RetryLater)
            );
            assert_eq!(
                s.finish(&mut cat, &mut media, true, &mut scratch)
                    .await
                    .unwrap(),
                DownloadOutcome::RetryLater
            );
            assert!(!media.path(id, false).exists());
        }
        for (guid, complete) in [("short", true), ("connection-failed", false)] {
            let job = queued(&mut cat, guid, Some(4), &mut scratch).await;
            let id = job.id;
            let Start::Ready(mut s) =
                DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
                    .await
                    .unwrap()
            else {
                panic!()
            };
            s.response(200, Some(4), None).unwrap();
            s.push(&mut cat, &mut media, b"abc", &mut scratch)
                .await
                .unwrap();
            assert_eq!(
                s.finish(&mut cat, &mut media, complete, &mut scratch)
                    .await
                    .unwrap(),
                DownloadOutcome::RetryLater
            );
            assert!(!media.path(id, false).exists());
            assert_ne!(
                cat.get(id, &mut scratch)
                    .await
                    .unwrap()
                    .unwrap()
                    .state
                    .audio,
                AudioState::Available
            );
        }
    });
}
#[test]
fn unknown_length_never_spends_reserve_or_publishes_while_waiting() {
    // Regression (t-109): unknown-length chunks cannot consume metadata/log reserve.
    block_on(async {
        let image = Image::new();
        image.sync_to_disk.set(false);
        let mut cat = image.create().await;
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let job = queued(&mut cat, "unknown", None, &mut scratch).await;
        let id = job.id;
        media.other_bytes = media.capacity - reserve(media.capacity) - AUDIO_CHUNK as u64;
        let Start::Ready(mut s) = DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
            .await
            .unwrap()
        else {
            panic!()
        };
        s.response(200, None, None).unwrap();
        s.push(&mut cat, &mut media, &[1; AUDIO_CHUNK], &mut scratch)
            .await
            .unwrap();
        assert_eq!(
            s.push(&mut cat, &mut media, b"x", &mut scratch)
                .await
                .unwrap(),
            ChunkOutcome::WaitingForSpace
        );
        assert_eq!(media.space().await.unwrap().free, reserve(media.capacity));
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::RetryLater
        );
        assert!(!media.path(id, false).exists());
    });
}
#[test]
fn every_media_boundary_recovers_without_false_available() {
    // Regression (t-109): reset/error before or after file publication cannot expose partial audio.
    block_on(async {
        let mut cuts = std::collections::VecDeque::from([None]);
        while let Some(cut) = cuts.pop_front() {
            let image = Image::new();
            image.sync_to_disk.set(false);
            let mut cat = image.create().await;
            let mut media = HostMedia::new(root_for(&image));
            media.physical_sync = false;
            let mut scratch = [0; MAX_RECORD_BYTES];
            let job = queued(&mut cat, "a", Some(3), &mut scratch).await;
            let id = job.id;
            media.fail_at = cut;
            let result = async {
                let Start::Ready(mut s) =
                    DownloadSession::start(&mut cat, &mut media, job, &mut scratch).await?
                else {
                    panic!("must fit")
                };
                s.response(200, Some(3), None).unwrap();
                s.push(&mut cat, &mut media, b"abc", &mut scratch).await?;
                s.finish(&mut cat, &mut media, true, &mut scratch).await
            }
            .await;
            if cut.is_none() {
                assert_eq!(result.unwrap(), DownloadOutcome::Complete);
                cuts.extend((0..media.steps).map(Some));
            } else {
                assert!(result.is_err(), "cut {cut:?}");
                assert!(!media.enabled);
            }
            drop(cat);
            let mut cat = image.reopen().await;
            media.reopen();
            crate::recovery::recover_all(&mut cat, &mut media, &mut scratch)
                .await
                .unwrap();
            let episode = cat.get(id, &mut scratch).await.unwrap().unwrap();
            if episode.state.audio == AudioState::Available {
                assert_eq!(std::fs::read(media.path(id, false)).unwrap(), b"abc");
                assert_eq!(cat.completed_audio_len(id).await.unwrap(), Some(3));
            } else {
                assert_eq!(episode.state.audio, AudioState::Queued);
                assert!(!media.path(id, false).exists());
            }
            assert!(!media.path(id, true).exists());
        }
    });
}
#[test]
fn restart_discards_old_partial_and_capacity_wait_can_retry_same_chunk() {
    // Regression (t-109):200 always restarts offset0; pressure waits consume no input.
    block_on(async {
        let image = Image::new();
        image.sync_to_disk.set(false);
        let mut cat = image.create().await;
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let job = queued(&mut cat, "a", None, &mut scratch).await;
        let id = job.id;
        std::fs::write(media.path(id, true), b"stale representation").unwrap();
        let Start::Ready(mut s) = DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
            .await
            .unwrap()
        else {
            panic!()
        };
        s.response(200, None, None).unwrap();
        media.other_bytes = media.capacity - reserve(media.capacity) - AUDIO_CHUNK as u64;
        assert_eq!(
            s.push(&mut cat, &mut media, &[1; AUDIO_CHUNK], &mut scratch)
                .await
                .unwrap(),
            ChunkOutcome::Written
        );
        assert_eq!(
            s.push(&mut cat, &mut media, b"x", &mut scratch)
                .await
                .unwrap(),
            ChunkOutcome::WaitingForSpace
        );
        media.other_bytes -= AUDIO_CHUNK as u64;
        assert_eq!(
            s.push(&mut cat, &mut media, b"x", &mut scratch)
                .await
                .unwrap(),
            ChunkOutcome::Written
        );
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::Complete
        );
        let bytes = std::fs::read(media.path(id, false)).unwrap();
        assert_eq!(bytes.len(), 4097);
        assert_eq!(bytes[4096], b'x');
        assert!(bytes[..4096].iter().all(|&b| b == 1));
    });
}
#[test]
fn publication_database_failure_and_append_cancellation_recover_safely() {
    // Regression (t-109): filesystem/DB disagreement and cancelled writes never expose partials.
    block_on(async {
        for cancelled in [false, true] {
            let image = Image::new();
            image.sync_to_disk.set(false);
            let mut cat = image.create().await;
            let mut media = HostMedia::new(root_for(&image));
            media.physical_sync = false;
            let mut scratch = [0; MAX_RECORD_BYTES];
            let job = queued(&mut cat, "a", Some(3), &mut scratch).await;
            let id = job.id;
            let Start::Ready(mut session) =
                DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
                    .await
                    .unwrap()
            else {
                panic!()
            };
            session.response(200, Some(3), None).unwrap();
            if cancelled {
                media.hang_append = true;
                {
                    let operation = session.push(&mut cat, &mut media, b"abc", &mut scratch);
                    futures::pin_mut!(operation);
                    assert!(futures::poll!(operation.as_mut()).is_pending());
                }
                assert!(!media.enabled);
                assert_eq!(
                    session
                        .finish(&mut cat, &mut media, true, &mut scratch)
                        .await
                        .unwrap(),
                    DownloadOutcome::RetryLater
                );
            } else {
                session
                    .push(&mut cat, &mut media, b"abc", &mut scratch)
                    .await
                    .unwrap();
                media.after_publish = Some(image.fail.clone());
                assert!(
                    session
                        .finish(&mut cat, &mut media, true, &mut scratch)
                        .await
                        .is_err()
                );
                assert!(matches!(
                    cat.get(id, &mut scratch).await,
                    Err(Error::Disabled)
                ));
                assert!(media.path(id, false).exists());
            }
            drop(cat);
            image.fail.set(false);
            let mut cat = image.reopen().await;
            media.reopen();
            crate::recovery::recover_all(&mut cat, &mut media, &mut scratch)
                .await
                .unwrap();
            assert_eq!(
                cat.get(id, &mut scratch)
                    .await
                    .unwrap()
                    .unwrap()
                    .state
                    .audio,
                AudioState::Queued
            );
            assert!(!media.path(id, false).exists());
            assert!(!media.path(id, true).exists());
        }
    });
}
#[test]
fn discarded_partial_frees_space_before_reclaiming_playable_audio() {
    // Regression (t-109): restart cleanup must not recycle read audio unnecessarily.
    use crate::media_test_support::available;
    block_on(async {
        let image = Image::new();
        image.sync_to_disk.set(false);
        let mut cat = image.create().await;
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
        )
        .await;
        let job = queued(&mut cat, "new", Some(3), &mut scratch).await;
        std::fs::write(media.path(job.id, true), [2; 4096]).unwrap();
        media.other_bytes = media.capacity - reserve(media.capacity) - 8192;
        let start = DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
            .await
            .unwrap();
        assert!(matches!(start, Start::Ready(_)));
        assert_eq!(std::fs::read(media.path(old, false)).unwrap().len(), 4096);
        assert_eq!(
            cat.get(old, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Available
        );
    });
}
#[test]
fn known_final_chunk_uses_actual_pending_bytes_without_unneeded_reclaim() {
    // Regression (t-109): a known final short chunk must fit its actual remaining budget.
    block_on(async {
        for length in [3, 5000] {
            let image = Image::new();
            image.sync_to_disk.set(false);
            let mut cat = image.create().await;
            let mut media = HostMedia::new(root_for(&image));
            media.physical_sync = false;
            let mut scratch = [0; MAX_RECORD_BYTES];
            let job = queued(&mut cat, "a", Some(length), &mut scratch).await;
            let id = job.id;
            media.other_bytes = media.capacity - reserve(media.capacity) - length;
            let Start::Ready(mut s) =
                DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
                    .await
                    .unwrap()
            else {
                panic!("known body fits")
            };
            s.response(200, Some(length), None).unwrap();
            let bytes = std::vec![1;length as usize];
            for chunk in bytes.chunks(AUDIO_CHUNK) {
                assert_eq!(
                    s.push(&mut cat, &mut media, chunk, &mut scratch)
                        .await
                        .unwrap(),
                    ChunkOutcome::Written
                );
            }
            assert_eq!(
                s.finish(&mut cat, &mut media, true, &mut scratch)
                    .await
                    .unwrap(),
                DownloadOutcome::Complete
            );
            assert_eq!(
                std::fs::read(media.path(id, false)).unwrap().len(),
                length as usize
            );
        }
    });
}
#[test]
fn ordinary_body_failure_cleans_up_and_can_retry_without_reboot() {
    // Regression (t-109): network retry must restore Queued rather than strand Downloading.
    block_on(async {
        let image = Image::new();
        image.sync_to_disk.set(false);
        let mut cat = image.create().await;
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let job = queued(&mut cat, "a", Some(3), &mut scratch).await;
        let id = job.id;
        let Start::Ready(mut s) =
            DownloadSession::start(&mut cat, &mut media, job.clone(), &mut scratch)
                .await
                .unwrap()
        else {
            panic!()
        };
        s.response(200, Some(3), None).unwrap();
        s.push(&mut cat, &mut media, b"a", &mut scratch)
            .await
            .unwrap();
        assert_eq!(
            s.finish(&mut cat, &mut media, false, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::RetryLater
        );
        assert_eq!(
            cat.get(id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Queued
        );
        assert!(!media.path(id, true).exists());
        assert!(matches!(
            DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
                .await
                .unwrap(),
            Start::Ready(_)
        ));
    });
}
#[test]
fn changed_expected_length_before_publication_restarts_current_representation() {
    // Regression (t-109): metadata reconciliation during a transfer cannot certify stale audio.
    block_on(async {
        let image = Image::new();
        image.sync_to_disk.set(false);
        let mut cat = image.create().await;
        let mut media = HostMedia::new(root_for(&image));
        media.physical_sync = false;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let job = queued(&mut cat, "a", Some(3), &mut scratch).await;
        let id = job.id;
        let Start::Ready(mut s) = DownloadSession::start(&mut cat, &mut media, job, &mut scratch)
            .await
            .unwrap()
        else {
            panic!()
        };
        s.response(200, Some(3), None).unwrap();
        s.push(&mut cat, &mut media, b"abc", &mut scratch)
            .await
            .unwrap();
        queued(&mut cat, "a", Some(4), &mut scratch).await;
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::RetryLater
        );
        assert!(!media.path(id, false).exists());
        assert_eq!(
            cat.get(id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Queued
        );
    });
}
