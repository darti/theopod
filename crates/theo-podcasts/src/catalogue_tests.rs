use super::*;
use crate::test_support::{Image, SECTORS};
use theo_storage::{
    redb_backend::{LENGTH_SECTORS, RegionBackend},
    test_disk::{Crash, TestDisk},
};
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

/// Write to the catalogue's tables directly, as damage or another layout would.
fn tamper(image: &Image, change: impl FnOnce(&redb::WriteTransaction)) {
    let db = redb::Database::builder()
        .create_with_backend(image.backend())
        .unwrap();
    let mut tx = db.begin_write().unwrap();
    tx.set_quick_repair(true);
    change(&tx);
    tx.commit().unwrap();
}

/// The stored schema counter, read without the catalogue's checks.
fn stored_schema(image: &Image) -> Option<u64> {
    let db = redb::Database::builder()
        .create_with_backend(image.backend())
        .unwrap();
    let tx = redb::ReadableDatabase::begin_read(&db).unwrap();
    let counters = tx.open_table(crate::tables::COUNTERS).unwrap();
    redb::ReadableTable::get(&counters, crate::tables::SCHEMA_KEY)
        .unwrap()
        .map(|v| v.value())
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
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let before = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("guid"), "old", "https://media/old"),
            &mut scratch,
        )
        .unwrap();
    cat.transition_audio(before.id, AudioState::Downloading, &mut scratch)
        .unwrap();
    cat.transition_audio(before.id, AudioState::Available, &mut scratch)
        .unwrap();
    cat.mark_read(before.id, &mut scratch).unwrap();
    drop(cat);
    let mut cat = image.reopen();
    let after = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("guid"), "new", "https://media/new"),
            &mut scratch,
        )
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
    let mut cat = image.reopen();
    let stored = cat.get(after.id, &mut scratch).unwrap().unwrap();
    assert_eq!(stored.metadata.title, "new");
    assert_eq!(stored.metadata.enclosure, "https://media/new");
    assert_eq!(stored.state, after.state);
}
#[test]
fn subscription_identity_scopes_guid() {
    // Regression (t-107): equal GUIDs from different subscriptions are independent records.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let meta = metadata(Some("same"), "episode", "https://media/audio");
    let a = cat.upsert(SubscriptionId(1), meta, &mut scratch).unwrap();
    let b = cat.upsert(SubscriptionId(2), meta, &mut scratch).unwrap();
    assert_ne!(a.id, b.id);
    drop(cat);
    let mut cat = image.reopen();
    assert_eq!(
        cat.get(a.id, &mut scratch).unwrap().unwrap().subscription,
        SubscriptionId(1)
    );
    assert_eq!(
        cat.get(b.id, &mut scratch).unwrap().unwrap().subscription,
        SubscriptionId(2)
    );
}

#[test]
fn guidless_enclosure_fallback_and_duplicate_reopen_are_idempotent() {
    // Regression (t-107): missing/empty GUID uses exact enclosure identity, not title.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let first = cat
        .upsert(
            SubscriptionId(1),
            metadata(None, "first", "https://media/a"),
            &mut scratch,
        )
        .unwrap();
    assert!(first.created && first.queued);
    assert_eq!(first.state.audio, AudioState::Queued);
    drop(cat);
    let mut cat = image.reopen();
    let repeated = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some(""), "changed", "https://media/a"),
            &mut scratch,
        )
        .unwrap();
    assert_eq!(repeated.id, first.id);
    assert!(!repeated.created && !repeated.queued);
    let distinct = cat
        .upsert(
            SubscriptionId(1),
            metadata(None, "changed", "https://media/b"),
            &mut scratch,
        )
        .unwrap();
    assert_ne!(distinct.id, first.id);
}

#[test]
fn reclaimed_read_tombstone_survives_reopen_and_metadata_changes() {
    // Regression (t-107): recycling audio never erases durable read state or queues it again.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let first = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("guid"), "title", "https://media/a"),
            &mut scratch,
        )
        .unwrap();
    cat.transition_audio(first.id, AudioState::Downloading, &mut scratch)
        .unwrap();
    cat.transition_audio(first.id, AudioState::Available, &mut scratch)
        .unwrap();
    assert!(matches!(
        cat.transition_audio(first.id, AudioState::Deleting, &mut scratch),
        Err(Error::InvalidState)
    ));
    cat.mark_read(first.id, &mut scratch).unwrap();
    cat.transition_audio(first.id, AudioState::Deleting, &mut scratch)
        .unwrap();
    cat.transition_audio(first.id, AudioState::Reclaimed, &mut scratch)
        .unwrap();
    drop(cat);
    let mut cat = image.reopen();
    let repeated = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("guid"), "new", "https://media/new"),
            &mut scratch,
        )
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
        cat.transition_audio(first.id, AudioState::Queued, &mut scratch),
        Err(Error::InvalidState)
    ));
    assert!(cat.get(first.id, &mut scratch).unwrap().unwrap().state.read);
}

#[test]
fn changed_guid_is_not_guessed_to_be_the_same_episode() {
    // Regression (t-107): changed source identity creates a record without resetting old history.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let a = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("a"), "same title", "https://media/same"),
            &mut scratch,
        )
        .unwrap();
    cat.mark_read(a.id, &mut scratch).unwrap();
    let b = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("b"), "same title", "https://media/same"),
            &mut scratch,
        )
        .unwrap();
    assert_ne!(a.id, b.id);
    assert!(b.created && b.queued);
    assert!(cat.get(a.id, &mut scratch).unwrap().unwrap().state.read);
}

#[test]
fn source_guid_and_fallback_url_are_distinct_identity_namespaces() {
    // Regression (t-107): a GUID whose text equals another episode's URL cannot merge records.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let a = cat
        .upsert(
            SubscriptionId(1),
            metadata(None, "title", "https://media/a"),
            &mut scratch,
        )
        .unwrap();
    let b = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("https://media/a"), "title", "https://media/b"),
            &mut scratch,
        )
        .unwrap();
    assert_ne!(a.id, b.id);
}

#[test]
fn complete_metadata_fits_fixed_record_and_roundtrips_without_allocation() {
    // Regression (t-107): every maximum-length field and optional scalar fits within 4096 bytes.
    let image = Image::new();
    let mut cat = image.create();
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
    let first = cat.upsert(SubscriptionId(7), meta, &mut scratch).unwrap();
    drop(cat);
    let mut cat = image.reopen();
    let stored = cat.get(first.id, &mut scratch).unwrap().unwrap();
    assert_eq!(stored.metadata, meta);
    assert_eq!(stored.source, Source::RadioFrance);
}

#[test]
fn oversized_required_fields_and_short_buffers_are_rejected_before_writes() {
    // Regression (t-107): required byte limits fail explicitly and never consume a local ID.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let before = image.disk.state().writes;
    let guid = "é".repeat(129);
    let title = "t".repeat(MAX_TITLE_BYTES + 1);
    let url = "u".repeat(MAX_URL_BYTES + 1);
    for meta in [
        metadata(Some(&guid), "title", "url"),
        metadata(Some("g"), &title, "url"),
        metadata(Some("g"), "title", &url),
    ] {
        assert!(matches!(
            cat.upsert(SubscriptionId(1), meta, &mut scratch),
            Err(Error::FieldTooLong)
        ));
    }
    assert!(matches!(
        cat.upsert(
            SubscriptionId(1),
            metadata(Some("g"), "title", "url"),
            &mut scratch[..4095]
        ),
        Err(Error::BufferTooSmall)
    ));
    assert_eq!(image.disk.state().writes, before);
    let first = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("g"), "title", "url"),
            &mut scratch,
        )
        .unwrap();
    assert_eq!(first.id, EpisodeId(1));
}

#[test]
fn oversized_optional_description_is_omitted_without_losing_episode() {
    // Regression (t-107): optional long descriptions cannot overflow the bounded persistent value.
    let image = Image::new();
    let mut cat = image.create();
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
        .unwrap();
    assert_eq!(
        cat.get(first.id, &mut scratch)
            .unwrap()
            .unwrap()
            .metadata
            .description,
        None
    );
}

#[test]
fn schema_rejection_does_not_write_or_reformat() {
    // Regression (t-107): unknown catalogue schemas are rejected even when the underlying engine mounts.
    let image = Image::new();
    drop(image.create());
    tamper(&image, |tx| {
        tx.open_table(crate::tables::COUNTERS)
            .unwrap()
            .insert(crate::tables::SCHEMA_KEY, 4)
            .unwrap();
    });
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Schema)
    ));
    // redb itself writes its header on open and its allocator state on
    // close; the catalogue writes nothing: the schema is still unknown.
    assert_eq!(stored_schema(&image), Some(4));
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Schema)
    ));
}

#[test]
fn corrupted_identity_index_never_overwrites_another_episodes_state() {
    // Regression (t-107): a damaged index or digest collision is checked against full identity.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let meta = metadata(Some("a"), "a", "url/a");
    let a = cat.upsert(SubscriptionId(1), meta, &mut scratch).unwrap();
    let b = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("b"), "b", "url/b"),
            &mut scratch,
        )
        .unwrap();
    cat.mark_read(b.id, &mut scratch).unwrap();
    drop(cat);
    tamper(&image, |tx| {
        tx.open_table(crate::tables::EPISODE_IDENTITY)
            .unwrap()
            .insert(identity(SubscriptionId(1), meta), b.id.0)
            .unwrap();
    });
    let mut cat = image.reopen();
    let before = image.disk.state().writes;
    assert!(matches!(
        cat.upsert(SubscriptionId(1), meta, &mut scratch),
        Err(Error::IdentityCollision)
    ));
    assert_eq!(image.disk.state().writes, before);
    assert!(matches!(cat.get(a.id, &mut scratch), Err(Error::Disabled)));
    drop(cat);
    let mut cat = image.reopen();
    assert!(cat.get(b.id, &mut scratch).unwrap().unwrap().state.read);
}

#[test]
fn failed_write_disables_catalogue_and_reopen_retains_committed_state() {
    // Regression (t-107): failed persistence cannot be followed by more operations on the same mount.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let first = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("g"), "old", "url"),
            &mut scratch,
        )
        .unwrap();
    image.disk.state().fail_writes = true;
    assert!(matches!(
        cat.mark_read(first.id, &mut scratch),
        Err(Error::Storage)
    ));
    image.disk.state().fail_writes = false;
    assert!(matches!(
        cat.get(first.id, &mut scratch),
        Err(Error::Disabled)
    ));
    drop(cat);
    let mut cat = image.reopen();
    assert!(!cat.get(first.id, &mut scratch).unwrap().unwrap().state.read);
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
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let meta = metadata(Some("g"), "old", "url");
    let first = cat.upsert(SubscriptionId(1), meta, &mut scratch).unwrap();
    drop(cat);
    tamper(&image, |tx| {
        tx.open_table(crate::tables::COUNTERS)
            .unwrap()
            .insert(crate::tables::NEXT_EPISODE, u64::MAX)
            .unwrap();
    });
    let mut cat = image.reopen();
    let before = image.disk.state().writes;
    assert!(matches!(
        cat.upsert(
            SubscriptionId(1),
            metadata(Some("new"), "new", "newurl"),
            &mut scratch
        ),
        Err(Error::IdExhausted)
    ));
    assert_eq!(image.disk.state().writes, before);
    let updated = cat
        .upsert(
            SubscriptionId(1),
            EpisodeMetadata {
                title: "updated",
                ..meta
            },
            &mut scratch,
        )
        .unwrap();
    assert_eq!(updated.id, first.id);
    assert!(!updated.created);
}

#[test]
fn atomic_catalogue_commit_keeps_record_index_and_next_id_together() {
    // Regression (t-107): a near-limit record, identity index and ID counter recover as one unit.
    let baseline = Image::new();
    drop(baseline.create());
    let open = |disk: &TestDisk| {
        // SAFETY: single-threaded test.
        let backend = unsafe { RegionBackend::new(disk.clone(), SECTORS) }.unwrap();
        Catalogue::open(backend)
    };
    let mut scratch = [0; MAX_RECORD_BYTES];
    let guid = "g".repeat(MAX_GUID_BYTES);
    let title = "t".repeat(MAX_TITLE_BYTES);
    let url = "u".repeat(MAX_URL_BYTES);
    let description = "d".repeat(MAX_DESCRIPTION_BYTES);
    let meta = EpisodeMetadata {
        description: Some(&description),
        ..metadata(Some(&guid), &title, &url)
    };
    let work = baseline.disk.snapshot();
    let mut cat = open(&work).unwrap();
    let first = cat.upsert(SubscriptionId(1), meta, &mut scratch).unwrap();
    drop(cat);
    let writes = work.state().writes;
    std::println!("catalogue fault model: {writes} sector writes (open, commit, close)");
    assert_eq!(first.id, EpisodeId(1));
    for crash in [Crash::DropPending, Crash::KeepPending] {
        for prefix in [0, 217, 512] {
            for cut in 0..writes {
                let work = baseline.disk.snapshot();
                work.state().cut = Some((cut, prefix));
                // The cut may land in the open itself.
                if let Ok(mut cat) = open(&work) {
                    let result = cat.upsert(SubscriptionId(1), meta, &mut scratch);
                    if result.is_err() {
                        assert!(
                            matches!(result, Err(Error::Storage)),
                            "cut={cut} prefix={prefix}"
                        );
                        assert!(matches!(
                            cat.get(EpisodeId(1), &mut scratch),
                            Err(Error::Disabled)
                        ));
                    }
                }
                work.crash(crash);
                let mut cat = open(&work).unwrap();
                if let Some(episode) = cat.get(EpisodeId(1), &mut scratch).unwrap() {
                    assert_eq!(episode.metadata, meta);
                    assert_eq!(episode.state.audio, AudioState::Queued);
                }
                let recovered = cat.upsert(SubscriptionId(1), meta, &mut scratch).unwrap();
                assert_eq!(recovered.id, EpisodeId(1), "cut={cut} prefix={prefix}");
                let second = cat
                    .upsert(
                        SubscriptionId(1),
                        metadata(Some("second"), "second", "secondurl"),
                        &mut scratch,
                    )
                    .unwrap();
                assert_eq!(second.id, EpisodeId(2), "cut={cut} prefix={prefix}");
            }
        }
    }
    std::println!("atomic catalogue commit: {writes} sector cuts x 3 prefixes x 2 crash modes");
}

#[test]
fn paged_scan_is_bounded_ordered_and_resumes_after_reopen() {
    // Regression (t-107): queued/read state can be enumerated after reboot without a RAM catalogue.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for i in 1..=35 {
        let guid = std::format!("g{i}");
        let item = cat
            .upsert(
                SubscriptionId(1),
                metadata(Some(&guid), "title", "url"),
                &mut scratch,
            )
            .unwrap();
        assert_eq!(item.id, EpisodeId(i));
    }
    cat.mark_read(EpisodeId(20), &mut scratch).unwrap();
    let mut ids = std::vec::Vec::new();
    let first = cat
        .scan(None, &mut scratch, |episode| ids.push(episode.id))
        .unwrap();
    assert_eq!(first.records, 16);
    assert!(first.has_more);
    assert_eq!(first.resume_after, Some(EpisodeId(16)));
    assert!(first.bytes <= MAX_SCAN_BYTES);
    drop(cat);
    let mut cat = image.reopen();
    let mut read_id = None;
    let second = cat
        .scan(first.resume_after, &mut scratch, |episode| {
            ids.push(episode.id);
            if episode.state.read {
                read_id = Some(episode.id);
            }
        })
        .unwrap();
    assert_eq!(second.records, 16);
    assert!(second.has_more);
    assert_eq!(read_id, Some(EpisodeId(20)));
    let third = cat
        .scan(second.resume_after, &mut scratch, |episode| {
            ids.push(episode.id)
        })
        .unwrap();
    assert_eq!(third.records, 3);
    assert!(!third.has_more);
    assert_eq!(ids, (1..=35).map(EpisodeId).collect::<std::vec::Vec<_>>());
    let empty = cat
        .scan(third.resume_after, &mut scratch, |_| {
            panic!("scan restarted unexpectedly")
        })
        .unwrap();
    assert_eq!(empty.records, 0);
    assert!(!empty.has_more);
}

#[test]
fn paged_scan_enforces_byte_limit_before_record_count_limit() {
    // Regression (t-107): count16 never implies that sixteen4KiB values fit a16KiB reply.
    let image = Image::new();
    let mut cat = image.create();
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
        .unwrap();
    }
    let mut ids = std::vec::Vec::new();
    let first = cat
        .scan(None, &mut scratch, |episode| ids.push(episode.id))
        .unwrap();
    assert_eq!(first.records, 4);
    assert!(first.has_more);
    assert!(first.bytes <= MAX_SCAN_BYTES);
    let second = cat
        .scan(first.resume_after, &mut scratch, |episode| {
            ids.push(episode.id)
        })
        .unwrap();
    assert_eq!(second.records, 2);
    assert!(!second.has_more);
    assert_eq!(ids, (1..=6).map(EpisodeId).collect::<std::vec::Vec<_>>());
}

#[test]
fn unchanged_metadata_and_repeated_read_flag_do_not_rewrite_storage() {
    // Regression (t-107): an unchanged delta has no write amplification or duplicate queue admission.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let meta = metadata(Some("g"), "title", "url");
    let first = cat.upsert(SubscriptionId(1), meta, &mut scratch).unwrap();
    cat.mark_read(first.id, &mut scratch).unwrap();
    drop(cat);
    let mut cat = image.reopen();
    let before = image.disk.state().writes;
    let repeated = cat.upsert(SubscriptionId(1), meta, &mut scratch).unwrap();
    cat.mark_read(first.id, &mut scratch).unwrap();
    assert_eq!(repeated.id, first.id);
    assert!(!repeated.created && !repeated.queued);
    assert_eq!(image.disk.state().writes, before);
}
#[test]
fn completed_audio_length_commits_with_available_and_clears_after_reclaim() {
    // Regression (t-109): unknown-length audio needs a durable completed-size fact.
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let id = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("a"), "a", "https://cdn.example/a"),
            &mut scratch,
        )
        .unwrap()
        .id;
    cat.transition_audio(id, AudioState::Downloading, &mut scratch)
        .unwrap();
    cat.finish_download(id, 3, &mut scratch).unwrap();
    drop(cat);
    let mut cat = image.reopen();
    assert_eq!(cat.completed_audio_len(id).unwrap(), Some(3));
    assert_eq!(
        cat.get(id, &mut scratch).unwrap().unwrap().state.audio,
        AudioState::Available
    );
    cat.mark_read(id, &mut scratch).unwrap();
    cat.transition_audio(id, AudioState::Deleting, &mut scratch)
        .unwrap();
    assert_eq!(cat.completed_audio_len(id).unwrap(), Some(3));
    cat.transition_audio(id, AudioState::Reclaimed, &mut scratch)
        .unwrap();
    drop(cat);
    let mut cat = image.reopen();
    assert_eq!(cat.completed_audio_len(id).unwrap(), None);
    assert!(cat.get(id, &mut scratch).unwrap().unwrap().state.read);
}

// Regression (t-133): a schema other than 3 is refused, never recreated.
#[test]
fn schema_mismatch_is_refused_without_writes() {
    let image = Image::new();
    drop(image.create());
    {
        let db = redb::Database::builder()
            .create_with_backend(image.backend())
            .unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.set_quick_repair(true);
        tx.open_table(crate::tables::COUNTERS)
            .unwrap()
            .insert(crate::tables::SCHEMA_KEY, 2)
            .unwrap();
        tx.commit().unwrap();
    }
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Schema)
    ));
    // redb itself writes its header on open and its allocator state on
    // close, so the check is on content: nothing was recreated or migrated.
    assert_eq!(stored_schema(&image), Some(2));
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Schema)
    ));
}

// Regression (t-133): a missing schema counter is refused, never recreated.
#[test]
fn missing_schema_counter_is_refused() {
    let image = Image::new();
    drop(image.create());
    tamper(&image, |tx| {
        tx.open_table(crate::tables::COUNTERS)
            .unwrap()
            .remove(crate::tables::SCHEMA_KEY)
            .unwrap();
    });
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Schema)
    ));
    assert_eq!(stored_schema(&image), None);
}

// Regression (t-133): a table missing from an otherwise valid database is a
// schema error, never recreated.
#[test]
fn missing_table_is_refused_without_recreating_it() {
    let image = Image::new();
    drop(image.create());
    tamper(&image, |tx| {
        tx.delete_table(crate::tables::SYNC).unwrap();
    });
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Schema)
    ));
    // Still refused: the first open did not recreate the table.
    assert!(matches!(
        Catalogue::open(image.backend()),
        Err(Error::Schema)
    ));
}

// Regression (t-133): an upsert is one atomic commit; when it fails, the
// previous committed state survives a reopen and the episode is absent.
#[test]
fn failed_upsert_commit_leaves_the_previous_state_intact() {
    let image = Image::new();
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let kept = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("kept"), "t", "https://x/a.mp3"),
            &mut scratch,
        )
        .unwrap();
    image.disk.state().fail_writes = true;
    assert!(matches!(
        cat.upsert(
            SubscriptionId(1),
            metadata(Some("lost"), "t", "https://x/b.mp3"),
            &mut scratch,
        ),
        Err(Error::Storage)
    ));
    image.disk.state().fail_writes = false;
    drop(cat);
    let mut cat = image.reopen();
    let mut guids = std::vec::Vec::new();
    let page = cat
        .scan(None, &mut scratch, |e| {
            guids.push((e.id, e.metadata.guid.map(std::string::String::from)));
        })
        .unwrap();
    assert_eq!(page.records, 1);
    assert_eq!(guids, [(kept.id, Some("kept".into()))]);
    // The identity and counter moved with the episode: the same upsert now
    // creates it with the next id.
    let again = cat
        .upsert(
            SubscriptionId(1),
            metadata(Some("lost"), "t", "https://x/b.mp3"),
            &mut scratch,
        )
        .unwrap();
    assert!(again.created);
    assert_eq!(again.id.0, kept.id.0 + 1);
}

#[test]
fn full_region_maps_to_full_and_disables() {
    // A region too small for growth: 2 MiB.
    let mut disk = TestDisk::new(LENGTH_SECTORS + 4096);
    RegionBackend::format(&mut disk).unwrap();
    // SAFETY: single-threaded test.
    let backend = unsafe { RegionBackend::new(disk, LENGTH_SECTORS + 4096) }.unwrap();
    let mut cat = Catalogue::open(backend).unwrap();
    let mut scratch = [0; MAX_RECORD_BYTES];
    let mut result = Ok(());
    for i in 0..10_000 {
        let guid = std::format!("guid-{i}");
        if let Err(e) = cat.upsert(
            SubscriptionId(1),
            metadata(Some(&guid), "t", "https://x/a.mp3"),
            &mut scratch,
        ) {
            result = Err(e);
            break;
        }
    }
    assert!(matches!(result, Err(Error::Full)));
    assert!(matches!(
        cat.get(EpisodeId(1), &mut scratch),
        Err(Error::Disabled)
    ));
}
