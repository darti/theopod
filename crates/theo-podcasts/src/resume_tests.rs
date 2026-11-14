use super::*;
use crate::{
    download::{ChunkOutcome, DownloadOutcome, DownloadSession, Start},
    media::{AUDIO_CHUNK, MediaStore},
    media_test_support::{HostMedia, queued, root_for, start_with},
    network_policy::ContentRange,
    reclaim::Reclaim,
    test_support::Image,
};
use futures::executor::block_on;
use std::vec::Vec;

const ETAG: &str = "\"v1\"";

fn body(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

fn range(first: u64, total: u64) -> Option<ContentRange> {
    Some(ContentRange {
        first,
        last: total - 1,
        total,
    })
}

fn audio(cat: &mut Catalogue, id: EpisodeId) -> AudioState {
    let mut scratch = [0; MAX_RECORD_BYTES];
    cat.get(id, &mut scratch).unwrap().unwrap().state.audio
}

async fn begin<M: MediaStore>(
    cat: &mut Catalogue,
    media: &mut M,
    resume: &mut Resume,
    job: &DownloadJob,
    scratch: &mut [u8],
) -> DownloadSession
where
    M::Error: core::fmt::Debug,
{
    match start_with(cat, media, resume, job.clone(), scratch)
        .await
        .unwrap()
    {
        Start::Ready(session) => session,
        _ => panic!("not started"),
    }
}

/// Push all of `bytes`, resending a chunk while the session is pending.
async fn push_all<M: MediaStore>(
    session: &mut DownloadSession,
    cat: &mut Catalogue,
    media: &mut M,
    bytes: &[u8],
    scratch: &mut [u8],
) where
    M::Error: core::fmt::Debug,
{
    let mut reclaim = Reclaim::new();
    if session.restarting() {
        session.restart(cat, media).await.unwrap();
    }
    for chunk in bytes.chunks(AUDIO_CHUNK) {
        loop {
            match session
                .push(cat, media, &mut reclaim, chunk, scratch)
                .await
                .unwrap()
            {
                ChunkOutcome::Written => break,
                ChunkOutcome::Pending => (),
                ChunkOutcome::WaitingForSpace => panic!("no space"),
            }
        }
    }
}

/// Start `job` afresh, write `written` bytes of `full` and fail the network.
async fn interrupted(
    cat: &mut Catalogue,
    media: &mut HostMedia,
    resume: &mut Resume,
    job: &DownloadJob,
    full: &[u8],
    written: usize,
    validator: Option<&str>,
) {
    let scratch = &mut [0; MAX_RECORD_BYTES][..];
    let mut s = begin(cat, media, resume, job, scratch).await;
    assert_eq!(s.from(), 0);
    s.response(200, Some(full.len() as u64), None, validator, resume)
        .unwrap();
    push_all(&mut s, cat, media, &full[..written], scratch).await;
    assert_eq!(
        s.abort(cat, media, resume, scratch).await.unwrap(),
        DownloadOutcome::RetryLater
    );
}

fn host(image: &Image) -> HostMedia {
    let mut media = HostMedia::new(root_for(image));
    media.physical_sync = false;
    media
}

#[test]
fn resumed_206_completes_the_exact_bytes_and_publishes() {
    // Regression (t-156): a network failure keeps the partial; the next attempt appends from it.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let full = body(10_000, 1);
        let job = queued(&mut cat, "a", Some(10_000), &mut scratch);
        let id = job.id;
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            5_096,
            Some(ETAG),
        )
        .await;
        assert_eq!(audio(&mut cat, id), AudioState::Queued);
        assert_eq!(resume.kept(), Some(id));
        assert_eq!(media.temp_len(id), Some(5_096));
        let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        assert_eq!(s.from(), 5_096);
        assert_eq!(resume.validator(), Some(ETAG));
        assert_eq!(audio(&mut cat, id), AudioState::Downloading);
        s.response(
            206,
            Some(4_904),
            range(5_096, 10_000),
            Some(ETAG),
            &mut resume,
        )
        .unwrap();
        push_all(&mut s, &mut cat, &mut media, &full[5_096..], &mut scratch).await;
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::Complete
        );
        assert_eq!(std::fs::read(media.path(id, false)).unwrap(), full);
        assert_eq!(audio(&mut cat, id), AudioState::Available);
        assert!(resume.kept().is_none());
    });
}

#[test]
fn resumed_unit_media_continues_its_unsynced_tail() {
    // Regression (t-156): the raw-unit store keeps the temp, its extents and its RAM tail sector.
    use crate::{media_test_support::Disk, unit_media::UnitMedia};
    use allocator_api2::alloc::Global;
    use theo_storage::{card_layout::UNIT_SECTORS, littlefs::SectorIo};
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media =
            UnitMedia::open_in(&mut cat, Disk::storing(20 * UNIT_SECTORS), 20, Global).unwrap();
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let full = body(10_000, 2);
        let job = queued(&mut cat, "a", Some(10_000), &mut scratch);
        let id = job.id;
        let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        s.response(200, Some(10_000), None, Some(ETAG), &mut resume)
            .unwrap();
        push_all(&mut s, &mut cat, &mut media, &full[..5_000], &mut scratch).await;
        s.abort(&mut cat, &mut media, &mut resume, &mut scratch)
            .await
            .unwrap();
        assert_eq!(media.temp_len(id), Some(5_000));
        let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        assert_eq!(s.from(), 5_000);
        s.response(
            206,
            Some(5_000),
            range(5_000, 10_000),
            Some(ETAG),
            &mut resume,
        )
        .unwrap();
        push_all(&mut s, &mut cat, &mut media, &full[5_000..], &mut scratch).await;
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::Complete
        );
        assert_eq!(media.audio_len(&mut cat, id).await.unwrap(), Some(10_000));
        let mut buf = [0; 4096];
        let record = cat.unit_record(id, &mut buf).unwrap().unwrap();
        let first = record.extents().next().unwrap().start * UNIT_SECTORS;
        let mut stored = Vec::new();
        let mut sector = [0; 512];
        for i in 0..10_000u32.div_ceil(512) {
            media
                .device_mut()
                .read_sector(first + i, &mut sector)
                .unwrap();
            stored.extend_from_slice(&sector);
        }
        assert_eq!(&stored[..10_000], full.as_slice());
    });
}

#[test]
fn resume_answered_with_200_restarts_from_zero_in_the_same_response() {
    // Regression (t-156): If-Range did not match, so the server sent the whole new representation.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let old = body(9_000, 3);
        let new = body(9_000, 4);
        let job = queued(&mut cat, "a", Some(9_000), &mut scratch);
        let id = job.id;
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &old,
            4_096,
            Some(ETAG),
        )
        .await;
        let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        assert_eq!(s.from(), 4_096);
        s.response(200, Some(9_000), None, Some("\"v2\""), &mut resume)
            .unwrap();
        assert_eq!(resume.validator(), Some("\"v2\""));
        push_all(&mut s, &mut cat, &mut media, &new, &mut scratch).await;
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::Complete
        );
        assert_eq!(std::fs::read(media.path(id, false)).unwrap(), new);
    });
}

#[test]
fn mismatched_content_range_aborts_and_discards_the_partial() {
    // Regression (t-156): only the exact `N-(total-1)/total` range may extend the kept bytes.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let full = body(9_000, 5);
        for (i, status, length, content) in [
            (0, 206, Some(4_904), range(4_097, 9_000)),
            (1, 206, Some(4_904), range(4_096, 9_001)),
            (2, 206, Some(4_000), range(4_096, 9_000)),
            (3, 206, Some(4_904), None),
            (4, 200, Some(9_000), range(0, 9_000)),
            (5, 416, None, None),
        ] {
            let mut resume = Resume::new();
            let job = queued(&mut cat, &std::format!("m{i}"), Some(9_000), &mut scratch);
            let id = job.id;
            interrupted(
                &mut cat,
                &mut media,
                &mut resume,
                &job,
                &full,
                4_096,
                Some(ETAG),
            )
            .await;
            let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
            assert_eq!(s.from(), 4_096);
            assert_eq!(
                s.response(status, length, content, Some(ETAG), &mut resume),
                Err(DownloadOutcome::RetryLater),
                "case {i}"
            );
            s.abort(&mut cat, &mut media, &mut resume, &mut scratch)
                .await
                .unwrap();
            assert!(resume.kept().is_none(), "case {i}");
            assert!(!media.path(id, true).exists(), "case {i}");
            assert_eq!(audio(&mut cat, id), AudioState::Queued);
        }
    });
}

#[test]
fn without_a_strong_validator_a_failure_restarts_from_zero() {
    // Regression (t-156): a weak ETag (mapped to no validator) cannot make If-Range safe.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let full = body(9_000, 6);
        let job = queued(&mut cat, "a", Some(9_000), &mut scratch);
        let id = job.id;
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            4_096,
            crate::network_policy::range_validator("W/\"v1\"", ""),
        )
        .await;
        assert!(resume.kept().is_none());
        assert!(!media.path(id, true).exists());
        assert_eq!(audio(&mut cat, id), AudioState::Queued);
        let s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        assert_eq!(s.from(), 0);
    });
}

#[test]
fn failure_before_any_written_chunk_is_not_kept() {
    // Regression (t-156): nothing to resume from, so recover as before.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let full = body(9_000, 7);
        let job = queued(&mut cat, "a", Some(9_000), &mut scratch);
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            0,
            Some(ETAG),
        )
        .await;
        assert!(resume.kept().is_none());
        assert!(!media.path(job.id, true).exists());
    });
}

#[test]
fn changed_enclosure_or_read_episode_is_not_resumed() {
    // Regression (t-156): the kept bytes belong to the job as it was; any change discards them.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let full = body(9_000, 8);
        // Changed enclosure: the new job starts from zero.
        let mut resume = Resume::new();
        let job = queued(&mut cat, "moved", Some(9_000), &mut scratch);
        let id = job.id;
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            4_096,
            Some(ETAG),
        )
        .await;
        cat.upsert(
            SubscriptionId(1),
            EpisodeMetadata {
                guid: Some("moved"),
                title: "moved",
                enclosure: "https://cdn.example/other",
                description: None,
                published: Some(1),
                enclosure_bytes: Some(9_000),
                duration_seconds: None,
            },
            &mut scratch,
        )
        .unwrap();
        let moved = DownloadJob::from_episode(cat.get(id, &mut scratch).unwrap().unwrap()).unwrap();
        assert!(
            resume
                .offer(&mut cat, &mut media, &mut scratch)
                .await
                .unwrap()
                .is_none()
        );
        let s = begin(&mut cat, &mut media, &mut resume, &moved, &mut scratch).await;
        assert_eq!(s.from(), 0);
        assert!(resume.kept().is_none());
        // Changed length: discarded too.
        let mut resume = Resume::new();
        let job = queued(&mut cat, "longer", Some(9_000), &mut scratch);
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            4_096,
            Some(ETAG),
        )
        .await;
        let longer = queued(&mut cat, "longer", Some(9_001), &mut scratch);
        let s = begin(&mut cat, &mut media, &mut resume, &longer, &mut scratch).await;
        assert_eq!(s.from(), 0);
        // Read: never resumed, and its partial is recovered (unlinked).
        let mut resume = Resume::new();
        let job = queued(&mut cat, "read", Some(9_000), &mut scratch);
        let id = job.id;
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            4_096,
            Some(ETAG),
        )
        .await;
        cat.mark_read(id, &mut scratch).unwrap();
        assert!(
            resume
                .offer(&mut cat, &mut media, &mut scratch)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            start_with(&mut cat, &mut media, &mut resume, job, &mut scratch)
                .await
                .unwrap(),
            Start::Skip
        ));
        assert!(resume.kept().is_none());
        assert!(!media.path(id, true).exists());
    });
}

#[test]
fn a_job_resumes_at_most_five_times_then_restarts_from_zero() {
    // Regression (t-156): a server that keeps cutting the body cannot pin a partial forever.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let full = body(9_000, 9);
        let job = queued(&mut cat, "a", Some(9_000), &mut scratch);
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            1_000,
            Some(ETAG),
        )
        .await;
        let mut at = 1_000;
        for _ in 0..MAX_RESUMES {
            let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
            assert_eq!(s.from(), at);
            s.response(
                206,
                Some(9_000 - at),
                range(at, 9_000),
                Some(ETAG),
                &mut resume,
            )
            .unwrap();
            push_all(
                &mut s,
                &mut cat,
                &mut media,
                &full[at as usize..at as usize + 1_000],
                &mut scratch,
            )
            .await;
            at += 1_000;
            s.abort(&mut cat, &mut media, &mut resume, &mut scratch)
                .await
                .unwrap();
            assert!(resume.kept().is_some());
        }
        let s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        assert_eq!(s.from(), 0);
        assert_eq!(media.temp_len(job.id), Some(0));
    });
}

#[test]
fn episode_read_during_the_download_is_recovered_not_kept() {
    // Regression (t-156): a read Downloading episode cannot go back to Queued; recover it as before.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let full = body(9_000, 10);
        let job = queued(&mut cat, "a", Some(9_000), &mut scratch);
        let id = job.id;
        let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        s.response(200, Some(9_000), None, Some(ETAG), &mut resume)
            .unwrap();
        push_all(&mut s, &mut cat, &mut media, &full[..4_096], &mut scratch).await;
        cat.mark_read(id, &mut scratch).unwrap();
        assert_eq!(
            s.abort(&mut cat, &mut media, &mut resume, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::RetryLater
        );
        assert!(resume.kept().is_none());
        assert!(!media.path(id, true).exists());
        assert_eq!(audio(&mut cat, id), AudioState::Missing);
    });
}

#[test]
fn resumed_attempt_failing_before_its_headers_keeps_the_partial() {
    // Regression (t-156): a reconnect failure (DNS, TLS, reset, 429) must not throw valid bytes away.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let full = body(9_000, 11);
        let job = queued(&mut cat, "a", Some(9_000), &mut scratch);
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            4_096,
            Some(ETAG),
        )
        .await;
        for _ in 0..2 * MAX_RESUMES {
            let s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
            assert_eq!(s.from(), 4_096);
            s.abort(&mut cat, &mut media, &mut resume, &mut scratch)
                .await
                .unwrap();
            assert_eq!(resume.kept(), Some(job.id));
            assert_eq!(audio(&mut cat, job.id), AudioState::Queued);
        }
        // Attempts that never reached the body do not use up the cap.
        let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        s.response(
            206,
            Some(4_904),
            range(4_096, 9_000),
            Some(ETAG),
            &mut resume,
        )
        .unwrap();
        push_all(&mut s, &mut cat, &mut media, &full[4_096..], &mut scratch).await;
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::Complete
        );
        assert_eq!(std::fs::read(media.path(job.id, false)).unwrap(), full);
    });
}

#[test]
fn resume_answered_with_a_200_of_another_length_restarts_when_the_feed_gave_none() {
    // Regression (t-156): a changed representation may change length; only the feed's length binds it.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut resume = Resume::new();
        let old = body(9_000, 12);
        let new = body(9_500, 13);
        let job = queued(&mut cat, "a", None, &mut scratch);
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &old,
            4_096,
            Some(ETAG),
        )
        .await;
        let mut s = begin(&mut cat, &mut media, &mut resume, &job, &mut scratch).await;
        assert_eq!(s.from(), 4_096);
        s.response(200, Some(9_500), None, Some("\"v2\""), &mut resume)
            .unwrap();
        push_all(&mut s, &mut cat, &mut media, &new, &mut scratch).await;
        assert_eq!(
            s.finish(&mut cat, &mut media, true, &mut scratch)
                .await
                .unwrap(),
            DownloadOutcome::Complete
        );
        assert_eq!(std::fs::read(media.path(job.id, false)).unwrap(), new);
    });
}

#[test]
fn a_complete_body_is_not_kept_for_an_unsatisfiable_range() {
    // Regression (t-156): `Range: bytes=total-` would get a 416 and lose the whole file.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut media = host(&image);
        let mut resume = Resume::new();
        let full = body(9_000, 14);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let job = queued(&mut cat, "a", Some(9_000), &mut scratch);
        interrupted(
            &mut cat,
            &mut media,
            &mut resume,
            &job,
            &full,
            9_000,
            Some(ETAG),
        )
        .await;
        assert!(resume.kept().is_none());
        assert!(!media.path(job.id, true).exists());
    });
}
