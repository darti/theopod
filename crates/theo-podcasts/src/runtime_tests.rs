use super::*;
use crate::{
    media_test_support::{HostMedia, root_for},
    test_support::Image,
};
use futures::executor::block_on;
const FEED: &str = "https://rss-rf.aerion.me/rss/00000000-0000-0000-0000-000000000001";
#[test]
fn owner_imports_downloads_and_marks_read_through_bounded_commands() {
    // Regression (t-110): the real owner connects persisted follows, feed parsing, audio publication and read state.
    block_on(async {
        let image = Image::new();
        let cat = image.create();
        let media = HostMedia::new(root_for(&image));
        let mut owner = Runtime::new(cat, media);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let Reply::Followed(id) = owner
            .handle(Command::Follow(FEED.try_into().unwrap()), &mut scratch)
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(id, SubscriptionId(1));
        owner
            .handle(
                Command::SyncStart(id, FEED.try_into().unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        owner
            .handle(
                Command::SyncHeaders {
                    status: 200,
                    etag: Default::default(),
                    modified: Default::default(),
                },
                &mut scratch,
            )
            .await
            .unwrap();
        let xml = b"<rss><channel><item><guid>one</guid><title>Episode</title><enclosure url=\"https://media.radiofrance-podcast.net/audio.mp3\" length=\"4\"/></item></channel></rss>";
        owner
            .handle(
                Command::SyncChunk(heapless::Vec::from_slice(xml).unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        assert!(matches!(
            owner.handle(Command::SyncEnd, &mut scratch).await.unwrap(),
            Reply::Synced(_)
        ));
        let Reply::Job { job: Some(job), .. } = owner
            .handle(Command::NextJob(None), &mut scratch)
            .await
            .unwrap()
        else {
            panic!()
        };
        let episode = job.id;
        assert!(matches!(
            owner
                .handle(Command::DownloadStart(job), &mut scratch)
                .await
                .unwrap(),
            Reply::Ready
        ));
        owner
            .handle(
                Command::DownloadHeaders {
                    status: 200,
                    length: Some(4),
                    partial: false,
                },
                &mut scratch,
            )
            .await
            .unwrap();
        owner
            .handle(
                Command::DownloadChunk(heapless::Vec::from_slice(b"1234").unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        assert!(matches!(
            owner
                .handle(Command::DownloadEnd(true), &mut scratch)
                .await
                .unwrap(),
            Reply::Downloaded(DownloadOutcome::Complete)
        ));
        owner
            .handle(Command::MarkRead(episode), &mut scratch)
            .await
            .unwrap();
        let e = owner.catalogue.get(episode, &mut scratch).unwrap().unwrap();
        assert!(e.state.read);
        assert_eq!(e.state.audio, AudioState::Available);
        drop(owner);
        assert!(
            image
                .reopen()
                .get(episode, &mut scratch)
                .unwrap()
                .unwrap()
                .state
                .read
        );
    });
}
#[test]
fn malformed_feed_does_not_disable_owner_but_storage_failure_does() {
    // Regression (t-110): transport/parser rejection leaves logs usable; media errors invalidate queued domain work.
    block_on(async {
        let image = Image::new();
        let mut owner = Runtime::new(image.create(), HostMedia::new(root_for(&image)));
        let mut scratch = [0; MAX_RECORD_BYTES];
        owner
            .handle(
                Command::SyncStart(SubscriptionId(1), FEED.try_into().unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(
            owner
                .handle(
                    Command::SyncHeaders {
                        status: 500,
                        etag: Default::default(),
                        modified: Default::default()
                    },
                    &mut scratch
                )
                .await
                .err(),
            Some(Error::Invalid)
        );
        owner
            .handle(Command::Follow(FEED.try_into().unwrap()), &mut scratch)
            .await
            .unwrap();
        owner.media.fail_at = Some(owner.media.steps);
        assert_eq!(
            owner.handle(Command::Recover, &mut scratch).await.err(),
            None
        );
        assert_eq!(
            owner.handle(Command::Recover, &mut scratch).await.err(),
            Some(Error::Storage)
        );
        assert_eq!(
            owner
                .handle(Command::Follow(FEED.try_into().unwrap()), &mut scratch)
                .await
                .err(),
            Some(Error::Storage)
        );
    });
}
#[test]
fn job_continuations_visit_every_queued_episode_in_one_page() {
    // Regression (t-110): returning the scan page's last ID must not skip queued jobs.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut scratch = [0; MAX_RECORD_BYTES];
        for guid in ["one", "two", "three"] {
            cat.upsert(
                SubscriptionId(1),
                EpisodeMetadata {
                    guid: Some(guid),
                    title: guid,
                    enclosure: "https://media.radiofrance-podcast.net/audio.mp3",
                    description: None,
                    published: None,
                    enclosure_bytes: None,
                    duration_seconds: None,
                },
                &mut scratch,
            )
            .unwrap();
        }
        let mut owner = Runtime::new(cat, HostMedia::new(root_for(&image)));
        let mut after = None;
        let mut visited = std::vec::Vec::new();
        loop {
            let Reply::Job {
                job,
                after: next,
                done,
            } = owner
                .handle(Command::NextJob(after), &mut scratch)
                .await
                .unwrap()
            else {
                panic!("expected job page")
            };
            if let Some(job) = job {
                visited.push(job.id.0);
            }
            if done {
                break;
            }
            assert_ne!(next, after);
            after = next;
        }
        assert_eq!(visited, [1, 2, 3]);
    });
}
#[test]
fn a_second_sync_start_cannot_replace_an_unfinished_import() {
    // Regression (t-110): concurrent sync triggers must preserve the active parser/session.
    block_on(async {
        let image = Image::new();
        let mut owner = Runtime::new(image.create(), HostMedia::new(root_for(&image)));
        let mut scratch = [0; MAX_RECORD_BYTES];
        owner
            .handle(
                Command::SyncStart(SubscriptionId(1), FEED.try_into().unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        owner
            .handle(
                Command::SyncHeaders {
                    status: 200,
                    etag: Default::default(),
                    modified: Default::default(),
                },
                &mut scratch,
            )
            .await
            .unwrap();
        let xml = b"<rss><channel><item><guid>one</guid><title>Episode</title><enclosure url=\"https://media.radiofrance-podcast.net/audio.mp3\"/></item></channel></rss>";
        owner
            .handle(
                Command::SyncChunk(heapless::Vec::from_slice(xml).unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(
            owner
                .handle(
                    Command::SyncStart(SubscriptionId(2), FEED.try_into().unwrap()),
                    &mut scratch
                )
                .await
                .err(),
            Some(Error::Invalid)
        );
        assert!(matches!(
            owner.handle(Command::SyncEnd, &mut scratch).await.unwrap(),
            Reply::Synced(_)
        ));
    });
}

#[test]
fn owner_exposes_committed_validators_and_accepts_not_modified() {
    // Regression (t-110): the transport must use a completed single-page checkpoint for HTTP304.
    block_on(async {
        let image = Image::new();
        let mut owner = Runtime::new(image.create(), HostMedia::new(root_for(&image)));
        let mut scratch = [0; MAX_RECORD_BYTES];
        owner
            .handle(
                Command::SyncStart(SubscriptionId(1), FEED.try_into().unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        owner
            .handle(
                Command::SyncHeaders {
                    status: 200,
                    etag: "v1".try_into().unwrap(),
                    modified: Default::default(),
                },
                &mut scratch,
            )
            .await
            .unwrap();
        let xml = b"<rss><channel></channel></rss>";
        owner
            .handle(
                Command::SyncChunk(heapless::Vec::from_slice(xml).unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        assert!(matches!(
            owner.handle(Command::SyncEnd, &mut scratch).await.unwrap(),
            Reply::Synced(_)
        ));
        owner
            .handle(
                Command::SyncStart(SubscriptionId(1), FEED.try_into().unwrap()),
                &mut scratch,
            )
            .await
            .unwrap();
        let Reply::Conditionals { etag, modified } = owner
            .handle(Command::SyncConditionals, &mut scratch)
            .await
            .unwrap()
        else {
            panic!("expected validators")
        };
        assert_eq!(etag, "v1");
        assert!(modified.is_empty());
        owner
            .handle(
                Command::SyncHeaders {
                    status: 304,
                    etag: Default::default(),
                    modified: Default::default(),
                },
                &mut scratch,
            )
            .await
            .unwrap();
        assert!(matches!(
            owner.handle(Command::SyncEnd, &mut scratch).await.unwrap(),
            Reply::Synced(_)
        ));
    });
}
#[test]
fn cancelled_audio_append_stays_unpublished_and_recovery_requeues_after_reopen() {
    // Regression (t-110): a sleep during file I/O rejects queued writes; only a fresh boot may recover it.
    use crate::recovery::recover_one;
    use futures::FutureExt;
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut scratch = [0; MAX_RECORD_BYTES];
        let id = cat
            .upsert(
                SubscriptionId(1),
                EpisodeMetadata {
                    guid: Some("cancelled"),
                    title: "Cancelled",
                    enclosure: "https://media.radiofrance-podcast.net/audio.mp3",
                    description: None,
                    published: None,
                    enclosure_bytes: None,
                    duration_seconds: None,
                },
                &mut scratch,
            )
            .unwrap()
            .id;
        let job = DownloadJob::from_episode(cat.get(id, &mut scratch).unwrap().unwrap()).unwrap();
        let mut owner = Runtime::new(cat, HostMedia::new(root_for(&image)));
        owner
            .handle(Command::DownloadStart(job), &mut scratch)
            .await
            .unwrap();
        owner
            .handle(
                Command::DownloadHeaders {
                    status: 200,
                    length: None,
                    partial: false,
                },
                &mut scratch,
            )
            .await
            .unwrap();
        owner.media.hang_append = true;
        assert!(
            owner
                .handle(
                    Command::DownloadChunk(heapless::Vec::from_slice(b"partial").unwrap()),
                    &mut scratch
                )
                .now_or_never()
                .is_none()
        );
        assert_eq!(
            owner
                .handle(Command::MarkRead(id), &mut scratch)
                .await
                .err(),
            Some(Error::Storage)
        );
        assert!(!owner.media.path(id, false).exists());
        let Runtime {
            catalogue,
            mut media,
            ..
        } = owner;
        drop(catalogue);
        media.reopen();
        let mut cat = image.reopen();
        recover_one(&mut cat, &mut media, id, &mut scratch)
            .await
            .unwrap();
        assert_eq!(
            cat.get(id, &mut scratch).unwrap().unwrap().state.audio,
            AudioState::Queued
        );
    });
}

/// HostMedia whose `space()` can fail with a fatal or a non-fatal error.
struct Classified {
    inner: HostMedia,
    fail: Option<bool>,
}
#[derive(Debug)]
enum ClassifiedError {
    Io,
    Refused,
}
impl MediaStore for Classified {
    type Error = ClassifiedError;
    fn is_fatal(error: &ClassifiedError) -> bool {
        matches!(error, ClassifiedError::Io)
    }
    fn active(&self, id: EpisodeId) -> bool {
        MediaStore::active(&self.inner, id)
    }
    async fn space(&mut self, cat: &mut Catalogue) -> Result<Space, ClassifiedError> {
        match self.fail {
            Some(true) => Err(ClassifiedError::Io),
            Some(false) => Err(ClassifiedError::Refused),
            None => self.inner.space(cat).await.map_err(|_| ClassifiedError::Io),
        }
    }
    async fn reset_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        expected: Option<u64>,
    ) -> Result<(), ClassifiedError> {
        let r = self.inner.reset_temp(cat, id, expected).await;
        r.map_err(|_| ClassifiedError::Io)
    }
    async fn append_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        bytes: &[u8],
    ) -> Result<(), ClassifiedError> {
        let r = self.inner.append_temp(cat, id, bytes).await;
        r.map_err(|_| ClassifiedError::Io)
    }
    async fn sync_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<u64, ClassifiedError> {
        let r = self.inner.sync_temp(cat, id).await;
        r.map_err(|_| ClassifiedError::Io)
    }
    async fn publish_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<(), ClassifiedError> {
        let r = self.inner.publish_temp(cat, id).await;
        r.map_err(|_| ClassifiedError::Io)
    }
    async fn unlink_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<(), ClassifiedError> {
        let r = self.inner.unlink_temp(cat, id).await;
        r.map_err(|_| ClassifiedError::Io)
    }
    async fn unlink_audio(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<(), ClassifiedError> {
        let r = self.inner.unlink_audio(cat, id).await;
        r.map_err(|_| ClassifiedError::Io)
    }
    async fn audio_len(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<Option<u64>, ClassifiedError> {
        let r = self.inner.audio_len(cat, id).await;
        r.map_err(|_| ClassifiedError::Io)
    }
    async fn list_ids(
        &mut self,
        cat: &mut Catalogue,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> Result<usize, ClassifiedError> {
        let r = self.inner.list_ids(cat, after, out).await;
        r.map_err(|_| ClassifiedError::Io)
    }
}

// Regression (t-121): a non-fatal store error rejects only the request and
// the runtime keeps answering; a fatal one still disables it.
#[test]
fn non_fatal_media_error_is_rejected_not_disabling() {
    block_on(async {
        let image = Image::new();
        let mut cat = image.create();
        let mut scratch = [0; MAX_RECORD_BYTES];
        let id = cat
            .upsert(
                SubscriptionId(1),
                EpisodeMetadata {
                    guid: Some("one"),
                    title: "one",
                    enclosure: "https://cdn.example/audio",
                    description: None,
                    published: Some(1),
                    enclosure_bytes: Some(4),
                    duration_seconds: None,
                },
                &mut scratch,
            )
            .unwrap()
            .id;
        let job = DownloadJob::from_episode(cat.get(id, &mut scratch).unwrap().unwrap()).unwrap();
        let media = Classified {
            inner: HostMedia::new(root_for(&image)),
            fail: Some(false),
        };
        let mut owner = Runtime::new(cat, media);
        assert_eq!(
            owner
                .handle(Command::DownloadStart(job.clone()), &mut scratch)
                .await
                .err(),
            Some(Error::Invalid)
        );
        assert!(
            owner
                .handle(Command::NextJob(None), &mut scratch)
                .await
                .is_ok()
        );
        owner.media.fail = Some(true);
        assert_eq!(
            owner
                .handle(Command::DownloadStart(job), &mut scratch)
                .await
                .err(),
            Some(Error::Storage)
        );
        owner.media.fail = None;
        assert_eq!(
            owner
                .handle(Command::NextJob(None), &mut scratch)
                .await
                .err(),
            Some(Error::Storage)
        );
    });
}
