use super::*;
use futures::executor::block_on;
use std::{
    cell::Cell,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};
use theo_storage::page_store::{PageStore, SectorFile, file_len};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Image {
    path: PathBuf,
    writes: Rc<Cell<usize>>,
    fail: Rc<Cell<bool>>,
    hang: Rc<Cell<bool>>,
    cut: Rc<Cell<Option<usize>>>,
    prefix: Rc<Cell<usize>>,
    sync_to_disk: Rc<Cell<bool>>,
}
impl Image {
    fn new() -> Self {
        let path = std::env::temp_dir().join(std::format!(
            "theo-catalogue-{}-{}.db",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap()
            .set_len(file_len(32))
            .unwrap();
        Self {
            path,
            writes: Rc::new(Cell::new(0)),
            fail: Rc::new(Cell::new(false)),
            hang: Rc::new(Cell::new(false)),
            cut: Rc::new(Cell::new(None)),
            prefix: Rc::new(Cell::new(0)),
            sync_to_disk: Rc::new(Cell::new(true)),
        }
    }
    fn file(&self) -> HostFile {
        HostFile {
            file: OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.path)
                .unwrap(),
            writes: self.writes.clone(),
            fail: self.fail.clone(),
            hang: self.hang.clone(),
            cut: self.cut.clone(),
            prefix: self.prefix.clone(),
            sync_to_disk: self.sync_to_disk.clone(),
        }
    }
    async fn create(&self) -> Catalogue<PageStore<HostFile>> {
        Catalogue::create(PageStore::create(self.file(), 32).await.unwrap())
            .await
            .unwrap()
    }
    async fn reopen(&self) -> Catalogue<PageStore<HostFile>> {
        Catalogue::open(PageStore::open(self.file(), 32).await.unwrap())
            .await
            .unwrap()
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
struct HostFile {
    file: File,
    writes: Rc<Cell<usize>>,
    fail: Rc<Cell<bool>>,
    hang: Rc<Cell<bool>>,
    cut: Rc<Cell<Option<usize>>>,
    prefix: Rc<Cell<usize>>,
    sync_to_disk: Rc<Cell<bool>>,
}
impl SectorFile for HostFile {
    type Error = std::io::Error;
    fn byte_len(&self) -> u64 {
        self.file.metadata().unwrap().len()
    }
    async fn read_sector(&mut self, sector: u64, data: &mut [u8; 512]) -> Result<(), Self::Error> {
        self.file.seek(SeekFrom::Start(sector * 512))?;
        self.file.read_exact(data)
    }
    async fn write_sector(&mut self, sector: u64, data: &[u8; 512]) -> Result<(), Self::Error> {
        if self.hang.get() {
            core::future::pending::<()>().await;
        }
        if self.fail.get() {
            return Err(std::io::Error::other("injected write failure"));
        }
        let position = self.writes.get();
        self.writes.set(position + 1);
        self.file.seek(SeekFrom::Start(sector * 512))?;
        if self.cut.get() == Some(position) {
            self.file.write_all(&data[..self.prefix.get()])?;
            if self.sync_to_disk.get() {
                self.file.sync_all()?;
            }
            return Err(std::io::Error::other("simulated torn sector"));
        }
        self.file.write_all(data)
    }
    async fn sync(&mut self) -> Result<(), Self::Error> {
        if self.sync_to_disk.get() {
            self.file.sync_all()
        } else {
            Ok(())
        }
    }
}
fn metadata<'a>(guid: Option<&'a str>, title: &'a str, url: &'a str) -> EpisodeMetadata<'a> {
    EpisodeMetadata {
        guid,
        title,
        enclosure: url,
        description: None,
        published: None,
        enclosure_bytes: None,
        duration_seconds: None,
    }
}

#[test]
fn read_audio_stays_available() {
    // Regression (t-107): marking read preserves playback availability.
    let mut state = EpisodeState {
        read: false,
        audio: AudioState::Available,
    };
    mark_read(&mut state);
    assert!(state.read);
    assert_eq!(state.audio, AudioState::Available);
}
#[test]
fn reclaimed_read_is_not_queued() {
    // Regression (t-107): retained read tombstones prevent automatic redownload.
    assert!(!should_queue(&EpisodeState {
        read: true,
        audio: AudioState::Reclaimed
    }));
}
#[test]
fn metadata_update_preserves_state() {
    // Regression (t-107): persistent local ID/read/audio survive title and enclosure changes.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let before = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("guid"), "old", "https://media/old"),
                &mut scratch,
            )
            .await
            .unwrap();
        cat.transition_audio(before.id, AudioState::Downloading, &mut scratch)
            .await
            .unwrap();
        cat.transition_audio(before.id, AudioState::Available, &mut scratch)
            .await
            .unwrap();
        cat.mark_read(before.id, &mut scratch).await.unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let after = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("guid"), "new", "https://media/new"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(after.id, before.id);
        assert!(!after.created && !after.queued);
        assert_eq!(
            after.state,
            EpisodeState {
                read: true,
                audio: AudioState::Available
            }
        );
        drop(cat);
        let mut cat = image.reopen().await;
        let stored = cat.get(after.id, &mut scratch).await.unwrap().unwrap();
        assert_eq!(stored.metadata.title, "new");
        assert_eq!(stored.metadata.enclosure, "https://media/new");
        assert_eq!(stored.state, after.state);
    });
}
#[test]
fn subscription_identity_scopes_guid() {
    // Regression (t-107): equal GUIDs from different subscriptions are independent records.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let meta = metadata(Some("same"), "episode", "https://media/audio");
        let a = cat
            .upsert(SubscriptionId(1), meta, &mut scratch)
            .await
            .unwrap();
        let b = cat
            .upsert(SubscriptionId(2), meta, &mut scratch)
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
        drop(cat);
        let mut cat = image.reopen().await;
        assert_eq!(
            cat.get(a.id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .subscription,
            SubscriptionId(1)
        );
        assert_eq!(
            cat.get(b.id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .subscription,
            SubscriptionId(2)
        );
    });
}

#[test]
fn guidless_enclosure_fallback_and_duplicate_reopen_are_idempotent() {
    // Regression (t-107): missing/empty GUID uses exact enclosure identity, not title.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let first = cat
            .upsert(
                SubscriptionId(1),
                metadata(None, "first", "https://media/a"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert!(first.created && first.queued);
        assert_eq!(first.state.audio, AudioState::Queued);
        drop(cat);
        let mut cat = image.reopen().await;
        let repeated = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some(""), "changed", "https://media/a"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(repeated.id, first.id);
        assert!(!repeated.created && !repeated.queued);
        let distinct = cat
            .upsert(
                SubscriptionId(1),
                metadata(None, "changed", "https://media/b"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_ne!(distinct.id, first.id);
    });
}

#[test]
fn reclaimed_read_tombstone_survives_reopen_and_metadata_changes() {
    // Regression (t-107): recycling audio never erases durable read state or queues it again.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let first = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("guid"), "title", "https://media/a"),
                &mut scratch,
            )
            .await
            .unwrap();
        cat.transition_audio(first.id, AudioState::Downloading, &mut scratch)
            .await
            .unwrap();
        cat.transition_audio(first.id, AudioState::Available, &mut scratch)
            .await
            .unwrap();
        assert!(matches!(
            cat.transition_audio(first.id, AudioState::Deleting, &mut scratch)
                .await,
            Err(Error::InvalidState)
        ));
        cat.mark_read(first.id, &mut scratch).await.unwrap();
        cat.transition_audio(first.id, AudioState::Deleting, &mut scratch)
            .await
            .unwrap();
        cat.transition_audio(first.id, AudioState::Reclaimed, &mut scratch)
            .await
            .unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let repeated = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("guid"), "new", "https://media/new"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(repeated.id, first.id);
        assert!(!repeated.queued);
        assert_eq!(
            repeated.state,
            EpisodeState {
                read: true,
                audio: AudioState::Reclaimed
            }
        );
        assert!(matches!(
            cat.transition_audio(first.id, AudioState::Queued, &mut scratch)
                .await,
            Err(Error::InvalidState)
        ));
        assert!(
            cat.get(first.id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .read
        );
    });
}

#[test]
fn changed_guid_is_not_guessed_to_be_the_same_episode() {
    // Regression (t-107): changed source identity creates a record without resetting old history.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let a = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("a"), "same title", "https://media/same"),
                &mut scratch,
            )
            .await
            .unwrap();
        cat.mark_read(a.id, &mut scratch).await.unwrap();
        let b = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("b"), "same title", "https://media/same"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
        assert!(b.created && b.queued);
        assert!(
            cat.get(a.id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .read
        );
    });
}

#[test]
fn source_guid_and_fallback_url_are_distinct_identity_namespaces() {
    // Regression (t-107): a GUID whose text equals another episode's URL cannot merge records.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let a = cat
            .upsert(
                SubscriptionId(1),
                metadata(None, "title", "https://media/a"),
                &mut scratch,
            )
            .await
            .unwrap();
        let b = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("https://media/a"), "title", "https://media/b"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
    });
}

#[test]
fn complete_metadata_fits_fixed_record_and_roundtrips_without_allocation() {
    // Regression (t-107): every maximum-length field and optional scalar fits within 4096 bytes.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let guid = "é".repeat(128);
        let title = "t".repeat(MAX_TITLE_BYTES);
        let url = "u".repeat(MAX_URL_BYTES);
        let description = "d".repeat(MAX_DESCRIPTION_BYTES);
        let meta = EpisodeMetadata {
            guid: Some(&guid),
            title: &title,
            enclosure: &url,
            description: Some(&description),
            published: Some(i64::MIN),
            enclosure_bytes: Some(u64::MAX),
            duration_seconds: Some(u32::MAX),
        };
        let first = cat
            .upsert(SubscriptionId(7), meta, &mut scratch)
            .await
            .unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let stored = cat.get(first.id, &mut scratch).await.unwrap().unwrap();
        assert_eq!(stored.metadata, meta);
        assert_eq!(stored.source, Source::RadioFrance);
    });
}

#[test]
fn oversized_required_fields_and_short_buffers_are_rejected_before_writes() {
    // Regression (t-107): required byte limits fail explicitly and never consume a local ID.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let before = image.writes.get();
        let guid = "é".repeat(129);
        let title = "t".repeat(MAX_TITLE_BYTES + 1);
        let url = "u".repeat(MAX_URL_BYTES + 1);
        for meta in [
            metadata(Some(&guid), "title", "url"),
            metadata(Some("g"), &title, "url"),
            metadata(Some("g"), "title", &url),
        ] {
            assert!(matches!(
                cat.upsert(SubscriptionId(1), meta, &mut scratch).await,
                Err(Error::FieldTooLong)
            ));
        }
        assert!(matches!(
            cat.upsert(
                SubscriptionId(1),
                metadata(Some("g"), "title", "url"),
                &mut scratch[..4095]
            )
            .await,
            Err(Error::BufferTooSmall)
        ));
        assert_eq!(image.writes.get(), before);
        let first = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("g"), "title", "url"),
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(first.id, EpisodeId(1));
    });
}

#[test]
fn oversized_optional_description_is_omitted_without_losing_episode() {
    // Regression (t-107): optional long descriptions cannot overflow the bounded persistent value.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let description = "d".repeat(MAX_DESCRIPTION_BYTES + 1);
        let first = cat
            .upsert(
                SubscriptionId(1),
                EpisodeMetadata {
                    description: Some(&description),
                    ..metadata(Some("g"), "title", "url")
                },
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(
            cat.get(first.id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .metadata
                .description,
            None
        );
    });
}

#[test]
fn schema_rejection_does_not_write_or_reformat() {
    // Regression (t-107): unknown catalogue schemas are rejected even when the underlying engine mounts.
    block_on(async {
        let image = Image::new();
        let db = ekv::Database::<_, NoopRawMutex>::new(
            PageStore::create(image.file(), 32).await.unwrap(),
            ekv::Config::default(),
        );
        db.format().await.unwrap();
        let mut tx = db.write_transaction().await;
        tx.write(NEXT_KEY, &1u64.to_le_bytes()).await.unwrap();
        tx.write(SCHEMA_KEY, &2u32.to_le_bytes()).await.unwrap();
        tx.commit().await.unwrap();
        drop(db);
        let before = image.writes.get();
        let result = Catalogue::open(PageStore::open(image.file(), 32).await.unwrap()).await;
        assert!(matches!(result, Err(Error::Schema)));
        assert_eq!(image.writes.get(), before);
    });
}

#[test]
fn corrupted_identity_index_never_overwrites_another_episodes_state() {
    // Regression (t-107): a damaged index or digest collision is checked against full identity.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let meta = metadata(Some("a"), "a", "url/a");
        let a = cat
            .upsert(SubscriptionId(1), meta, &mut scratch)
            .await
            .unwrap();
        let b = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("b"), "b", "url/b"),
                &mut scratch,
            )
            .await
            .unwrap();
        cat.mark_read(b.id, &mut scratch).await.unwrap();
        drop(cat);
        let db = ekv::Database::<_, NoopRawMutex>::new(
            PageStore::open(image.file(), 32).await.unwrap(),
            ekv::Config::default(),
        );
        db.mount().await.unwrap();
        let mut tx = db.write_transaction().await;
        tx.write(
            &identity_key(SubscriptionId(1), meta),
            &b.id.0.to_le_bytes(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(db);
        let mut cat = image.reopen().await;
        let before = image.writes.get();
        assert!(matches!(
            cat.upsert(SubscriptionId(1), meta, &mut scratch).await,
            Err(Error::IdentityCollision)
        ));
        assert_eq!(image.writes.get(), before);
        assert!(matches!(
            cat.get(a.id, &mut scratch).await,
            Err(Error::Disabled)
        ));
        drop(cat);
        let mut cat = image.reopen().await;
        assert!(
            cat.get(b.id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .read
        );
    });
}

#[test]
fn failed_write_disables_catalogue_and_reopen_retains_committed_state() {
    // Regression (t-107): failed persistence cannot be followed by more operations on the same mount.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let first = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some("g"), "old", "url"),
                &mut scratch,
            )
            .await
            .unwrap();
        image.fail.set(true);
        assert!(matches!(
            cat.mark_read(first.id, &mut scratch).await,
            Err(Error::Storage(_))
        ));
        image.fail.set(false);
        assert!(matches!(
            cat.get(first.id, &mut scratch).await,
            Err(Error::Disabled)
        ));
        drop(cat);
        let mut cat = image.reopen().await;
        assert!(
            !cat.get(first.id, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .read
        );
    });
}

#[test]
fn cancelled_operation_permanently_disables_catalogue_owner() {
    // Regression (t-107): cancellation does not make the backend or cached ID counter reusable.
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        image.hang.set(true);
        {
            let mut future = pin!(cat.upsert(
                SubscriptionId(1),
                metadata(Some("g"), "title", "url"),
                &mut scratch
            ));
            assert!(matches!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
        }
        image.hang.set(false);
        assert!(matches!(
            cat.get(EpisodeId(1), &mut scratch).await,
            Err(Error::Disabled)
        ));
        drop(cat);
        let mut cat = image.reopen().await;
        assert!(cat.get(EpisodeId(1), &mut scratch).await.unwrap().is_none());
        assert_eq!(
            cat.upsert(
                SubscriptionId(1),
                metadata(Some("g"), "title", "url"),
                &mut scratch
            )
            .await
            .unwrap()
            .id,
            EpisodeId(1)
        );
    });
}

#[test]
fn codec_rejects_truncation_reserved_flags_and_invalid_state() {
    // Regression (t-107): malformed persisted values cannot invent valid lifecycle states.
    let mut bytes = [0; MAX_RECORD_BYTES];
    let episode = Episode {
        id: EpisodeId(1),
        subscription: SubscriptionId(1),
        source: Source::RadioFrance,
        state: EpisodeState {
            read: false,
            audio: AudioState::Queued,
        },
        metadata: metadata(Some("g"), "title", "url"),
    };
    let len = encode(episode, &mut bytes).unwrap();
    assert_eq!(decode(&bytes[..len]), Some(episode));
    for n in 0..len {
        assert!(decode(&bytes[..n]).is_none());
    }
    for (index, value) in [
        (0, 2),
        (1, 2),
        (AUDIO_OFFSET, 6),
        (READ_OFFSET, 2),
        (20, 0x80),
        (AUDIO_OFFSET, 5),
    ] {
        let mut bad = bytes;
        bad[index] = value;
        assert!(decode(&bad[..len]).is_none());
    }
    bytes[len] = 0;
    assert!(decode(&bytes[..len + 1]).is_none());
}

#[test]
fn exhausted_ids_do_not_prevent_updates_or_overwrite_old_records() {
    // Regression (t-107): ID exhaustion rejects only new identities and does not consume writes.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let meta = metadata(Some("g"), "old", "url");
        let first = cat
            .upsert(SubscriptionId(1), meta, &mut scratch)
            .await
            .unwrap();
        drop(cat);
        let db = ekv::Database::<_, NoopRawMutex>::new(
            PageStore::open(image.file(), 32).await.unwrap(),
            ekv::Config::default(),
        );
        db.mount().await.unwrap();
        let mut tx = db.write_transaction().await;
        tx.write(NEXT_KEY, &u64::MAX.to_le_bytes()).await.unwrap();
        tx.commit().await.unwrap();
        drop(db);
        let mut cat = image.reopen().await;
        let before = image.writes.get();
        assert!(matches!(
            cat.upsert(
                SubscriptionId(1),
                metadata(Some("new"), "new", "newurl"),
                &mut scratch
            )
            .await,
            Err(Error::IdExhausted)
        ));
        assert_eq!(image.writes.get(), before);
        let updated = cat
            .upsert(
                SubscriptionId(1),
                EpisodeMetadata {
                    title: "updated",
                    ..meta
                },
                &mut scratch,
            )
            .await
            .unwrap();
        assert_eq!(updated.id, first.id);
        assert!(!updated.created);
    });
}

#[test]
#[ignore = "real-file sector interruption sweep: just test-catalogue-recovery"]
fn atomic_catalogue_commit_keeps_record_index_and_next_id_together() {
    // Regression (t-107): a near-limit record, identity index and ID counter recover as one unit.
    block_on(async {
        let baseline = Image::new();
        drop(baseline.create().await);
        let work = Image::new();
        // File writes are unbuffered and immediately visible to reopen in this
        // fault model. sync is a model barrier, not an OS/power-loss claim.
        // All regular persistence/reopen tests retain actual sync_all.
        work.sync_to_disk.set(false);
        let mut scratch = [0; MAX_RECORD_BYTES];
        let guid = "g".repeat(MAX_GUID_BYTES);
        let title = "t".repeat(MAX_TITLE_BYTES);
        let url = "u".repeat(MAX_URL_BYTES);
        let description = "d".repeat(MAX_DESCRIPTION_BYTES);
        let meta = EpisodeMetadata {
            description: Some(&description),
            ..metadata(Some(&guid), &title, &url)
        };
        std::fs::copy(&baseline.path, &work.path).unwrap();
        work.writes.set(0);
        let mut cat = work.reopen().await;
        let first = cat
            .upsert(SubscriptionId(1), meta, &mut scratch)
            .await
            .unwrap();
        let writes = work.writes.get();
        std::println!("catalogue fault model: {writes} sector boundaries");
        drop(cat);
        assert_eq!(first.id, EpisodeId(1));
        for prefix in [0, 217, 512] {
            for cut in 0..writes {
                std::fs::copy(&baseline.path, &work.path).unwrap();
                work.writes.set(0);
                work.cut.set(Some(cut));
                work.prefix.set(prefix);
                let mut cat = work.reopen().await;
                assert!(
                    matches!(
                        cat.upsert(SubscriptionId(1), meta, &mut scratch).await,
                        Err(Error::Storage(_))
                    ),
                    "cut={cut} prefix={prefix}"
                );
                assert!(matches!(
                    cat.get(EpisodeId(1), &mut scratch).await,
                    Err(Error::Disabled)
                ));
                drop(cat);
                work.cut.set(None);
                let mut cat = work.reopen().await;
                if let Some(episode) = cat.get(EpisodeId(1), &mut scratch).await.unwrap() {
                    assert_eq!(episode.metadata, meta);
                    assert_eq!(episode.state.audio, AudioState::Queued);
                }
                let recovered = cat
                    .upsert(SubscriptionId(1), meta, &mut scratch)
                    .await
                    .unwrap();
                assert_eq!(recovered.id, EpisodeId(1), "cut={cut} prefix={prefix}");
                let second = cat
                    .upsert(
                        SubscriptionId(1),
                        metadata(Some("second"), "second", "secondurl"),
                        &mut scratch,
                    )
                    .await
                    .unwrap();
                assert_eq!(second.id, EpisodeId(2), "cut={cut} prefix={prefix}");
                if cut % 32 == 0 {
                    std::println!("catalogue cut={cut}/{writes} prefix={prefix}");
                }
            }
        }
        std::println!(
            "atomic catalogue commit: {writes} real-file sector cuts x 3 persisted prefixes"
        );
    });
}

#[test]
fn paged_scan_is_bounded_ordered_and_resumes_after_reopen() {
    // Regression (t-107): queued/read state can be enumerated after reboot without a RAM catalogue.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        for i in 1..=35 {
            let guid = std::format!("g{i}");
            let item = cat
                .upsert(
                    SubscriptionId(1),
                    metadata(Some(&guid), "title", "url"),
                    &mut scratch,
                )
                .await
                .unwrap();
            assert_eq!(item.id, EpisodeId(i));
        }
        cat.mark_read(EpisodeId(20), &mut scratch).await.unwrap();
        let mut ids = std::vec::Vec::new();
        let first = cat
            .scan(None, &mut scratch, |episode| ids.push(episode.id))
            .await
            .unwrap();
        assert_eq!(first.records, 16);
        assert!(first.has_more);
        assert_eq!(first.resume_after, Some(EpisodeId(16)));
        assert!(first.bytes <= MAX_SCAN_BYTES);
        drop(cat);
        let mut cat = image.reopen().await;
        let mut read_id = None;
        let second = cat
            .scan(first.resume_after, &mut scratch, |episode| {
                ids.push(episode.id);
                if episode.state.read {
                    read_id = Some(episode.id);
                }
            })
            .await
            .unwrap();
        assert_eq!(second.records, 16);
        assert!(second.has_more);
        assert_eq!(read_id, Some(EpisodeId(20)));
        let third = cat
            .scan(second.resume_after, &mut scratch, |episode| {
                ids.push(episode.id)
            })
            .await
            .unwrap();
        assert_eq!(third.records, 3);
        assert!(!third.has_more);
        assert_eq!(ids, (1..=35).map(EpisodeId).collect::<std::vec::Vec<_>>());
        let empty = cat
            .scan(third.resume_after, &mut scratch, |_| {
                panic!("scan restarted unexpectedly")
            })
            .await
            .unwrap();
        assert_eq!(empty.records, 0);
        assert!(!empty.has_more);
    });
}

#[test]
fn paged_scan_enforces_byte_limit_before_record_count_limit() {
    // Regression (t-107): count16 never implies that sixteen4KiB values fit a16KiB reply.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let title = "t".repeat(MAX_TITLE_BYTES);
        let url = "u".repeat(MAX_URL_BYTES);
        let description = "d".repeat(MAX_DESCRIPTION_BYTES);
        for i in 0..6 {
            let guid = std::format!("g{i:0255}");
            cat.upsert(
                SubscriptionId(1),
                EpisodeMetadata {
                    description: Some(&description),
                    ..metadata(Some(&guid), &title, &url)
                },
                &mut scratch,
            )
            .await
            .unwrap();
        }
        let mut ids = std::vec::Vec::new();
        let first = cat
            .scan(None, &mut scratch, |episode| ids.push(episode.id))
            .await
            .unwrap();
        assert_eq!(first.records, 4);
        assert!(first.has_more);
        assert!(first.bytes <= MAX_SCAN_BYTES);
        let second = cat
            .scan(first.resume_after, &mut scratch, |episode| {
                ids.push(episode.id)
            })
            .await
            .unwrap();
        assert_eq!(second.records, 2);
        assert!(!second.has_more);
        assert_eq!(ids, (1..=6).map(EpisodeId).collect::<std::vec::Vec<_>>());
    });
}

#[test]
fn unchanged_metadata_and_repeated_read_flag_do_not_rewrite_storage() {
    // Regression (t-107): an unchanged delta has no write amplification or duplicate queue admission.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let meta = metadata(Some("g"), "title", "url");
        let first = cat
            .upsert(SubscriptionId(1), meta, &mut scratch)
            .await
            .unwrap();
        cat.mark_read(first.id, &mut scratch).await.unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let before = image.writes.get();
        let repeated = cat
            .upsert(SubscriptionId(1), meta, &mut scratch)
            .await
            .unwrap();
        cat.mark_read(first.id, &mut scratch).await.unwrap();
        assert_eq!(repeated.id, first.id);
        assert!(!repeated.created && !repeated.queued);
        assert_eq!(image.writes.get(), before);
    });
}
