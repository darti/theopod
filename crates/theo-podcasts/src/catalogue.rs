//! Persistent identities and local state in a redb database (ADR-0022).
use crate::tables::*;
use core::{borrow::Borrow, ops::Bound};
use redb::{
    Database, Key, ReadTransaction, ReadableDatabase, ReadableTable, StorageBackend,
    TableDefinition, WriteTransaction,
};
use sha2::{Digest, Sha256};
use theo_storage::{littlefs::SectorIo, redb_backend::RegionBackend};

pub const MAX_RECORD_BYTES: usize = 4096;
pub const MAX_SCAN_RECORDS: usize = 16;
pub const MAX_SCAN_BYTES: usize = 16 * 1024;
pub const MAX_GUID_BYTES: usize = 256;
pub const MAX_TITLE_BYTES: usize = 512;
pub const MAX_URL_BYTES: usize = 2048;
pub const MAX_DESCRIPTION_BYTES: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpisodeId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    RadioFrance,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioState {
    Missing = 0,
    Queued = 1,
    Downloading = 2,
    Available = 3,
    Deleting = 4,
    Reclaimed = 5,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpisodeState {
    pub read: bool,
    pub audio: AudioState,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpisodeMetadata<'a> {
    pub guid: Option<&'a str>,
    pub title: &'a str,
    pub enclosure: &'a str,
    pub description: Option<&'a str>,
    pub published: Option<i64>,
    pub enclosure_bytes: Option<u64>,
    pub duration_seconds: Option<u32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Episode<'a> {
    pub id: EpisodeId,
    pub subscription: SubscriptionId,
    pub source: Source,
    pub state: EpisodeState,
    pub metadata: EpisodeMetadata<'a>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpsertOutcome {
    pub id: EpisodeId,
    pub state: EpisodeState,
    pub created: bool,
    pub queued: bool,
}
/// Resume with `scan(page.resume_after, ...)` while `has_more` is true.
/// Byte accounting includes encoded record keys and values, not borrowed views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanPage {
    pub resume_after: Option<EpisodeId>,
    pub has_more: bool,
    pub records: usize,
    pub bytes: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Storage,
    Corrupt,
    Schema,
    FieldTooLong,
    InvalidMetadata,
    BufferTooSmall,
    NotFound,
    IdExhausted,
    IdentityCollision,
    InvalidState,
    Full,
    Disabled,
}
pub fn mark_read(state: &mut EpisodeState) {
    state.read = true;
}
/// Missing unread audio needs one queue admission. Queued work is already admitted.
pub fn should_queue(state: &EpisodeState) -> bool {
    !state.read && state.audio == AudioState::Missing
}

/// redb page cache; smaller caches cost more writes per commit (ADR-0022).
pub const CACHE_BYTES: usize = 256 * 1024;
const AUDIO_OFFSET: usize = 18;
const READ_OFFSET: usize = 19;
/// Encoded size of an episode key in schema 2, kept in scan byte accounting.
const KEY_BYTES: usize = 9;

/// Map any redb error: region growth refused → `Full`, device failure →
/// `Storage`, unknown layout → `Schema`, anything else → `Corrupt`.
pub(crate) fn db_error(error: impl Into<redb::Error>) -> Error {
    use alloc::string::ToString;
    match error.into() {
        redb::Error::Io(e)
            if e.to_string()
                .contains(theo_storage::redb_backend::REGION_FULL) =>
        {
            Error::Full
        }
        redb::Error::Io(_) | redb::Error::PreviousIo | redb::Error::DatabaseClosed => {
            Error::Storage
        }
        redb::Error::TableDoesNotExist(_) | redb::Error::UpgradeRequired(_) => Error::Schema,
        _ => Error::Corrupt,
    }
}

/// redb's open-time refusal of a non-empty file without its magic number.
/// std builds tag it `InvalidData`, no_std builds keep only this message,
/// so the message is what both can match (redb is pinned, ADR-0022).
///
/// This text is redb `=4.4.0`'s (`page_manager.rs`, `io::invalid_data`). A
/// redb upgrade must re-check it: `region_without_redb_magic_is_corrupt`
/// and `redb_refusal_text_matches_not_redb` fail if the message changes.
pub(crate) const NOT_REDB: &str = "Not a redb database";

/// Map an open error: a region that is not a redb database is `Corrupt`
/// (not a device failure); everything else as [`db_error`].
fn open_error(error: redb::DatabaseError) -> Error {
    use alloc::string::ToString;
    match &error {
        redb::DatabaseError::Storage(redb::StorageError::Io(e))
            if e.to_string().contains(NOT_REDB) =>
        {
            Error::Corrupt
        }
        _ => db_error(error),
    }
}

/// Unique database owner. Metadata views borrow the caller's scratch buffer,
/// never the database; no record allocation or competing writer is hidden
/// here. Storage, format and capacity errors disable this owner until it is
/// dropped and reopened. The service must also invalidate its shared
/// filesystem session, as it does for every failed storage request.
pub struct Catalogue {
    /// The current operation's write transaction, committed by `done` or
    /// aborted by `finish`; never held across operations. Declared before
    /// `db`, so it is dropped (aborted) first.
    tx: Option<WriteTransaction>,
    pub(crate) db: Database,
    next: u64,
    enabled: bool,
    /// Inside [`Self::begin_batch`]: `done` keeps the transaction open, so
    /// a podcast request makes at most one commit (ADR-0022).
    batch: bool,
    /// Commits made so far: a request checks whether a step committed.
    commits: u64,
}

/// Reads see the last committed state, or a batch's held writes.
pub(crate) enum View<'a> {
    Read(ReadTransaction),
    Write(&'a WriteTransaction),
}
impl View<'_> {
    /// Copy a byte value into `scratch`; its length, or `None` if absent.
    pub(crate) fn bytes<'k, K: Key + 'static>(
        &self,
        table: TableDefinition<K, &'static [u8]>,
        key: impl Borrow<K::SelfType<'k>>,
        scratch: &mut [u8],
    ) -> Result<Option<usize>, Error> {
        match self {
            Self::Read(tx) => copy(&tx.open_table(table).map_err(db_error)?, key, scratch),
            Self::Write(tx) => copy(&tx.open_table(table).map_err(db_error)?, key, scratch),
        }
    }
    fn number<'k, K: Key + 'static>(
        &self,
        table: TableDefinition<K, u64>,
        key: impl Borrow<K::SelfType<'k>>,
    ) -> Result<Option<u64>, Error> {
        match self {
            Self::Read(tx) => number(&tx.open_table(table).map_err(db_error)?, key),
            Self::Write(tx) => number(&tx.open_table(table).map_err(db_error)?, key),
        }
    }
}

/// Copy a byte value into `scratch`; its length, or `None` if absent.
pub(crate) fn copy<'k, K: Key + 'static>(
    table: &impl ReadableTable<K, &'static [u8]>,
    key: impl Borrow<K::SelfType<'k>>,
    scratch: &mut [u8],
) -> Result<Option<usize>, Error> {
    let Some(value) = table.get(key).map_err(db_error)? else {
        return Ok(None);
    };
    let value = value.value();
    scratch
        .get_mut(..value.len())
        .ok_or(Error::Corrupt)?
        .copy_from_slice(value);
    Ok(Some(value.len()))
}
/// A `u64` value, or `None` if absent.
pub(crate) fn number<'k, K: Key + 'static>(
    table: &impl ReadableTable<K, u64>,
    key: impl Borrow<K::SelfType<'k>>,
) -> Result<Option<u64>, Error> {
    Ok(table.get(key).map_err(db_error)?.map(|v| v.value()))
}

impl Catalogue {
    /// Open the catalogue; create its tables if the database has none.
    ///
    /// Only an explicitly provisioned region holds a database without
    /// tables (`RegionBackend` refuses a missing length record), so this
    /// never formats user data. A database with tables is only validated:
    /// a missing table or another schema is refused, never repaired.
    ///
    /// A fresh region (provisioned, catalogue not yet created) whose redb
    /// file lacks its magic is a creation cut short: it is reset to empty
    /// and created again. The fresh flag is cleared once the tables are
    /// durable, so a region that may hold data is never reset (ADR-0015).
    pub fn open<D: SectorIo + 'static>(backend: RegionBackend<D>) -> Result<Self, Error> {
        backend.reset_unfinished_creation().map_err(db_error)?;
        let fresh_flag = backend.fresh_flag();
        let catalogue = Self::open_database(backend)?;
        if let Some(flag) = fresh_flag {
            flag.clear().map_err(db_error)?;
        }
        Ok(catalogue)
    }

    fn open_database(backend: impl StorageBackend) -> Result<Self, Error> {
        let db = Database::builder()
            .set_cache_size(CACHE_BYTES)
            .create_with_backend(backend)
            .map_err(open_error)?;
        let mut catalogue = Self {
            tx: None,
            db,
            next: 0,
            enabled: true,
            batch: false,
            commits: 0,
        };
        let fresh = {
            let tx = catalogue.db.begin_read().map_err(db_error)?;
            tx.list_tables().map_err(db_error)?.next().is_none()
        };
        if fresh {
            catalogue.create_tables()?;
        }
        catalogue.validate()?;
        Ok(catalogue)
    }

    fn create_tables(&mut self) -> Result<(), Error> {
        let mut tx = self.db.begin_write().map_err(db_error)?;
        tx.set_quick_repair(true);
        tx.open_table(EPISODES).map_err(db_error)?;
        tx.open_table(EPISODE_IDENTITY).map_err(db_error)?;
        tx.open_table(AUDIO).map_err(db_error)?;
        tx.open_table(SUBSCRIPTIONS).map_err(db_error)?;
        tx.open_table(SHOWS).map_err(db_error)?;
        tx.open_table(SYNC).map_err(db_error)?;
        tx.open_table(UNIT_PAGES).map_err(db_error)?;
        tx.open_table(UNIT_EXTENTS).map_err(db_error)?;
        tx.open_table(EVICTABLE).map_err(db_error)?;
        {
            let mut counters = tx.open_table(COUNTERS).map_err(db_error)?;
            for (key, value) in [
                (SCHEMA_KEY, SCHEMA),
                (NEXT_EPISODE, 1),
                (NEXT_SUBSCRIPTION, 1),
                (UNITS_USED, 0),
            ] {
                counters.insert(key, value).map_err(db_error)?;
            }
        }
        tx.commit().map_err(db_error)
    }

    /// Read-only schema check. Never formats or migrates.
    fn validate(&mut self) -> Result<(), Error> {
        let tx = self.db.begin_read().map_err(db_error)?;
        let counters = tx.open_table(COUNTERS).map_err(db_error)?;
        if number(&counters, SCHEMA_KEY)? != Some(SCHEMA) {
            return Err(Error::Schema);
        }
        let next = number(&counters, NEXT_EPISODE)?
            .filter(|&n| n != 0)
            .ok_or(Error::Corrupt)?;
        number(&counters, NEXT_SUBSCRIPTION)?
            .filter(|&n| n != 0)
            .ok_or(Error::Corrupt)?;
        number(&counters, UNITS_USED)?.ok_or(Error::Corrupt)?;
        tx.open_table(EPISODES).map_err(db_error)?;
        tx.open_table(EPISODE_IDENTITY).map_err(db_error)?;
        tx.open_table(AUDIO).map_err(db_error)?;
        tx.open_table(SUBSCRIPTIONS).map_err(db_error)?;
        tx.open_table(SHOWS).map_err(db_error)?;
        tx.open_table(SYNC).map_err(db_error)?;
        tx.open_table(UNIT_PAGES).map_err(db_error)?;
        tx.open_table(UNIT_EXTENTS).map_err(db_error)?;
        tx.open_table(EVICTABLE).map_err(db_error)?;
        self.next = next;
        Ok(())
    }

    /// Make sure a write transaction is open, begun with quick repair.
    pub(crate) fn write(&mut self) -> Result<&WriteTransaction, Error> {
        if self.tx.is_none() {
            let mut tx = self.db.begin_write().map_err(db_error)?;
            tx.set_quick_repair(true);
            self.tx = Some(tx);
        }
        self.tx.as_ref().ok_or(Error::Corrupt)
    }
    /// Commit the open write transaction, if any.
    fn commit(&mut self) -> Result<(), Error> {
        match self.tx.take() {
            Some(tx) => {
                tx.commit().map_err(db_error)?;
                self.commits += 1;
                Ok(())
            }
            None => Ok(()),
        }
    }
    /// End an operation's writes: commit its transaction, unless a batch
    /// holds it until [`Self::end_batch`].
    pub(crate) fn done(&mut self) -> Result<(), Error> {
        if self.batch {
            return Ok(());
        }
        self.commit()
    }
    /// Commits made since open. A request compares it before and after a
    /// step to keep to one commit per request (ADR-0022).
    pub(crate) fn commits(&self) -> u64 {
        self.commits
    }
    /// Hold the writes of the next operations in one write transaction,
    /// committed by [`Self::end_batch`]: a podcast request makes at most one
    /// commit. Reads inside the batch see its held writes. A fatal error
    /// inside the batch drops the held writes and disables the owner (a
    /// media store may already have applied them in RAM).
    pub(crate) fn begin_batch(&mut self) -> Result<(), Error> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        if self.batch || self.tx.is_some() {
            return Err(Error::InvalidState);
        }
        self.batch = true;
        Ok(())
    }
    /// End a batch: commit its held writes if `commit`. Otherwise drop
    /// them; held writes then disable the owner and fail as `Storage`, since
    /// the media store may have applied them in RAM. A batch that wrote
    /// nothing leaves the owner as the failed operation left it.
    ///
    /// Commit after success, and after an error detected before any change
    /// (the held writes are then exactly the earlier operations').
    pub(crate) fn end_batch(&mut self, commit: bool) -> Result<(), Error> {
        if !self.batch {
            return Err(Error::InvalidState);
        }
        self.batch = false;
        if !self.enabled {
            self.tx = None;
            return Err(Error::Disabled);
        }
        if !commit {
            return match self.tx.take() {
                Some(_) => {
                    self.enabled = false;
                    Err(Error::Storage)
                }
                None => Ok(()),
            };
        }
        self.begin()?;
        let result = self.commit();
        self.finish(result)
    }
    pub(crate) fn view(&self) -> Result<View<'_>, Error> {
        match &self.tx {
            Some(tx) => Ok(View::Write(tx)),
            None => self.db.begin_read().map(View::Read).map_err(db_error),
        }
    }

    /// Enter an operation: the owner is disabled until `finish` re-enables it.
    pub(crate) fn begin(&mut self) -> Result<(), Error> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        self.enabled = false;
        Ok(())
    }
    /// Leave an operation. Fatal errors keep the owner disabled and drop
    /// (abort) the open transaction. Other errors are detected before any
    /// write, so the owner stays usable.
    pub(crate) fn finish<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        match &result {
            Err(e) if is_fatal(*e) => {
                self.tx = None;
            }
            Err(_) => {
                // Detected before any write: a batch keeps the writes of
                // its earlier operations.
                if !self.batch {
                    self.tx = None;
                }
                self.enabled = true;
            }
            Ok(_) => self.enabled = true,
        }
        result
    }
    fn check(&self, scratch: &[u8]) -> Result<(), Error> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        if scratch.len() < MAX_RECORD_BYTES {
            return Err(Error::BufferTooSmall);
        }
        Ok(())
    }

    pub fn completed_audio_len(&mut self, id: EpisodeId) -> Result<Option<u64>, Error> {
        self.begin()?;
        let result = self
            .view()
            .and_then(|view| match view.number(AUDIO, id.0)? {
                Some(0) => Err(Error::Corrupt),
                bytes => Ok(bytes),
            });
        self.finish(result)
    }
    /// Called only after durable temporary-file publication by the media owner.
    pub(crate) fn finish_download(
        &mut self,
        id: EpisodeId,
        bytes: u64,
        scratch: &mut [u8],
    ) -> Result<(), Error> {
        self.check(scratch)?;
        if bytes == 0 {
            return Err(Error::InvalidMetadata);
        }
        self.begin()?;
        let result = self.finish_download_inner(id, bytes, scratch);
        self.finish(result)
    }
    fn finish_download_inner(
        &mut self,
        id: EpisodeId,
        bytes: u64,
        scratch: &mut [u8],
    ) -> Result<(), Error> {
        let (episode, n) = self.load(id, scratch)?.ok_or(Error::NotFound)?;
        if episode.state.audio != AudioState::Downloading {
            return Err(Error::InvalidState);
        }
        let (before, published) = (episode.state, episode.metadata.published);
        let after = EpisodeState {
            audio: AudioState::Available,
            ..before
        };
        scratch[AUDIO_OFFSET] = AudioState::Available as u8;
        let tx = self.write()?;
        tx.open_table(AUDIO)
            .map_err(db_error)?
            .insert(id.0, bytes)
            .map_err(db_error)?;
        tx.open_table(EPISODES)
            .map_err(db_error)?
            .insert(id.0, &scratch[..n])
            .map_err(db_error)?;
        reindex(tx, id, Some((before, published)), (after, published))?;
        self.done()
    }
    /// Last completely reconciled history. Partial imports never replace this.
    pub fn sync_checkpoint(
        &mut self,
        id: SubscriptionId,
        scratch: &mut [u8],
    ) -> Result<Option<crate::sync::Checkpoint>, Error> {
        self.check(scratch)?;
        if id.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        self.begin()?;
        let result = self.view().and_then(|view| {
            match view.bytes(SYNC, id.0, &mut scratch[..MAX_RECORD_BYTES])? {
                Some(n) => crate::sync::Checkpoint::decode(&scratch[..n])
                    .map(Some)
                    .ok_or(Error::Corrupt),
                None => Ok(None),
            }
        });
        self.finish(result)
    }
    pub(crate) fn save_sync_checkpoint(
        &mut self,
        id: SubscriptionId,
        checkpoint: &crate::sync::Checkpoint,
        scratch: &mut [u8],
    ) -> Result<(), Error> {
        self.check(scratch)?;
        if id.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        let n = checkpoint.encode(scratch).ok_or(Error::InvalidMetadata)?;
        self.begin()?;
        let result = self.write().and_then(|tx| {
            tx.open_table(SYNC)
                .map_err(db_error)?
                .insert(id.0, &scratch[..n])
                .map_err(db_error)?;
            Ok(())
        });
        let result = result.and_then(|()| self.done());
        self.finish(result)
    }
    pub fn get<'a>(
        &mut self,
        id: EpisodeId,
        scratch: &'a mut [u8],
    ) -> Result<Option<Episode<'a>>, Error> {
        self.check(scratch)?;
        self.begin()?;
        let result = self
            .load(id, scratch)
            .map(|record| record.map(|(episode, _)| episode));
        self.finish(result)
    }
    fn load<'a>(
        &self,
        id: EpisodeId,
        scratch: &'a mut [u8],
    ) -> Result<Option<(Episode<'a>, usize)>, Error> {
        let Some(n) = self
            .view()?
            .bytes(EPISODES, id.0, &mut scratch[..MAX_RECORD_BYTES])?
        else {
            return Ok(None);
        };
        let episode = decode(&scratch[..n]).ok_or(Error::Corrupt)?;
        if episode.id != id || id.0 >= self.next {
            return Err(Error::Corrupt);
        }
        Ok(Some((episode, n)))
    }
    pub fn upsert(
        &mut self,
        subscription: SubscriptionId,
        metadata: EpisodeMetadata<'_>,
        scratch: &mut [u8],
    ) -> Result<UpsertOutcome, Error> {
        self.check(scratch)?;
        let metadata = normalized(metadata)?;
        if subscription.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        self.begin()?;
        let result = self.upsert_inner(subscription, metadata, scratch);
        self.finish(result)
    }
    fn upsert_inner(
        &mut self,
        subscription: SubscriptionId,
        metadata: EpisodeMetadata<'_>,
        scratch: &mut [u8],
    ) -> Result<UpsertOutcome, Error> {
        let index = identity(subscription, metadata);
        let existing = self.view()?.number(EPISODE_IDENTITY, index)?.map(EpisodeId);
        let mut old = None;
        let (id, mut state, created) = if let Some(id) = existing {
            let (episode, _) = self.load(id, scratch)?.ok_or(Error::Corrupt)?;
            old = Some((episode.state, episode.metadata.published));
            if episode.subscription != subscription || !same_identity(episode.metadata, metadata) {
                return Err(Error::IdentityCollision);
            }
            if episode.metadata == metadata && !should_queue(&episode.state) {
                return Ok(UpsertOutcome {
                    id,
                    state: episode.state,
                    created: false,
                    queued: false,
                });
            }
            (id, episode.state, false)
        } else {
            self.next.checked_add(1).ok_or(Error::IdExhausted)?;
            (
                EpisodeId(self.next),
                EpisodeState {
                    read: false,
                    audio: AudioState::Missing,
                },
                true,
            )
        };
        let queued = should_queue(&state);
        if queued {
            state.audio = AudioState::Queued;
        }
        let n = encode(
            Episode {
                id,
                subscription,
                source: Source::RadioFrance,
                state,
                metadata,
            },
            scratch,
        )
        .ok_or(Error::BufferTooSmall)?;
        let next = self.next;
        let tx = self.write()?;
        tx.open_table(EPISODES)
            .map_err(db_error)?
            .insert(id.0, &scratch[..n])
            .map_err(db_error)?;
        reindex(tx, id, old, (state, metadata.published))?;
        if created {
            tx.open_table(EPISODE_IDENTITY)
                .map_err(db_error)?
                .insert(index, id.0)
                .map_err(db_error)?;
            tx.open_table(COUNTERS)
                .map_err(db_error)?
                .insert(NEXT_EPISODE, next + 1)
                .map_err(db_error)?;
        }
        self.done()?;
        if created {
            self.next += 1;
        }
        Ok(UpsertOutcome {
            id,
            state,
            created,
            queued,
        })
    }
    /// Bounded import-order scan. The synchronous callback may copy IDs or
    /// state, but cannot retain a view into the reused caller buffer. No read
    /// transaction or cursor escapes this operation. One lookahead record may
    /// be read to detect continuation; it is not emitted or skipped on resume.
    pub fn scan(
        &mut self,
        after: Option<EpisodeId>,
        scratch: &mut [u8],
        emit: impl for<'record> FnMut(Episode<'record>),
    ) -> Result<ScanPage, Error> {
        self.check(scratch)?;
        self.begin()?;
        let result = self.scan_inner(after, scratch, emit);
        self.finish(result)
    }
    fn scan_inner(
        &self,
        after: Option<EpisodeId>,
        scratch: &mut [u8],
        mut emit: impl for<'record> FnMut(Episode<'record>),
    ) -> Result<ScanPage, Error> {
        let tx = self.db.begin_read().map_err(db_error)?;
        let table = tx.open_table(EPISODES).map_err(db_error)?;
        let lower = after.map_or(Bound::Unbounded, |a| Bound::Excluded(a.0));
        let range = table.range((lower, Bound::Unbounded)).map_err(db_error)?;
        let mut page = ScanPage {
            resume_after: None,
            has_more: false,
            records: 0,
            bytes: 0,
        };
        for entry in range {
            let (key, value) = entry.map_err(db_error)?;
            let (id, value) = (EpisodeId(key.value()), value.value());
            let n = value.len();
            if page.records == MAX_SCAN_RECORDS || page.bytes + KEY_BYTES + n > MAX_SCAN_BYTES {
                page.has_more = true;
                break;
            }
            scratch
                .get_mut(..n)
                .filter(|_| n <= MAX_RECORD_BYTES)
                .ok_or(Error::Corrupt)?
                .copy_from_slice(value);
            let episode = decode(&scratch[..n]).ok_or(Error::Corrupt)?;
            if episode.id != id || id.0 >= self.next {
                return Err(Error::Corrupt);
            }
            emit(episode);
            page.resume_after = Some(id);
            page.records += 1;
            page.bytes += KEY_BYTES + n;
        }
        Ok(page)
    }
    pub fn mark_read(&mut self, id: EpisodeId, scratch: &mut [u8]) -> Result<(), Error> {
        self.check(scratch)?;
        self.begin()?;
        let result = self.state_inner(id, None, scratch);
        self.finish(result)
    }
    pub fn transition_audio(
        &mut self,
        id: EpisodeId,
        next: AudioState,
        scratch: &mut [u8],
    ) -> Result<(), Error> {
        self.check(scratch)?;
        self.begin()?;
        let result = self.state_inner(id, Some(next), scratch);
        self.finish(result)
    }
    fn state_inner(
        &mut self,
        id: EpisodeId,
        next: Option<AudioState>,
        scratch: &mut [u8],
    ) -> Result<(), Error> {
        let (mut state, n, published) = {
            let (episode, n) = self.load(id, scratch)?.ok_or(Error::NotFound)?;
            (episode.state, n, episode.metadata.published)
        };
        let previous = state;
        if let Some(next) = next {
            if !valid_transition(state, next) {
                return Err(Error::InvalidState);
            }
            state.audio = next;
        } else {
            mark_read(&mut state);
        }
        if state == previous {
            return Ok(());
        }
        scratch[AUDIO_OFFSET] = state.audio as u8;
        scratch[READ_OFFSET] = u8::from(state.read);
        let tx = self.write()?;
        if matches!(state.audio, AudioState::Missing | AudioState::Reclaimed) {
            tx.open_table(AUDIO)
                .map_err(db_error)?
                .remove(id.0)
                .map_err(db_error)?;
        }
        tx.open_table(EPISODES)
            .map_err(db_error)?
            .insert(id.0, &scratch[..n])
            .map_err(db_error)?;
        reindex(tx, id, Some((previous, published)), (state, published))?;
        self.done()
    }
    /// The first evictable episode in the reclaim policy's order whose audio
    /// is not `active`, among the first `MAX_SCAN_RECORDS` index entries:
    /// one bounded range read, no episode scan (ADR-0022).
    pub fn first_evictable(
        &mut self,
        active: impl Fn(EpisodeId) -> bool,
    ) -> Result<Option<EpisodeId>, Error> {
        self.begin()?;
        let result = self.first_evictable_inner(active);
        self.finish(result)
    }
    fn first_evictable_inner(
        &self,
        active: impl Fn(EpisodeId) -> bool,
    ) -> Result<Option<EpisodeId>, Error> {
        let tx = self.db.begin_read().map_err(db_error)?;
        let table = tx.open_table(EVICTABLE).map_err(db_error)?;
        for entry in table.iter().map_err(db_error)?.take(MAX_SCAN_RECORDS) {
            let id = EpisodeId(entry.map_err(db_error)?.0.value().2);
            if id.0 == 0 || id.0 >= self.next {
                return Err(Error::Corrupt);
            }
            if !active(id) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}

/// Whether the reclaim policy may evict an episode in `state`.
pub(crate) fn evictable(state: EpisodeState) -> bool {
    state.read && state.audio == AudioState::Available
}
/// The eviction index key: the reclaim policy's order.
pub(crate) fn evict_key(id: EpisodeId, published: Option<i64>) -> (u8, i64, u64) {
    (u8::from(published.is_none()), published.unwrap_or(0), id.0)
}
/// Keep `EVICTABLE` in step with an episode write, in its transaction.
fn reindex(
    tx: &WriteTransaction,
    id: EpisodeId,
    old: Option<(EpisodeState, Option<i64>)>,
    (state, published): (EpisodeState, Option<i64>),
) -> Result<(), Error> {
    let old = old
        .filter(|&(s, _)| evictable(s))
        .map(|(_, p)| evict_key(id, p));
    let new = evictable(state).then(|| evict_key(id, published));
    if old == new {
        return Ok(());
    }
    let mut table = tx.open_table(EVICTABLE).map_err(db_error)?;
    if let Some(key) = old {
        table.remove(key).map_err(db_error)?;
    }
    if let Some(key) = new {
        table.insert(key, ()).map_err(db_error)?;
    }
    Ok(())
}

/// Errors that may follow a write, or that leave the database suspect:
/// they disable the owner until reopen.
pub(crate) fn is_fatal(error: Error) -> bool {
    matches!(
        error,
        Error::Storage
            | Error::Corrupt
            | Error::Schema
            | Error::IdentityCollision
            | Error::Full
            | Error::Disabled
    )
}
fn valid_transition(state: EpisodeState, next: AudioState) -> bool {
    use AudioState::*;
    if state.audio == next {
        return true;
    }
    match (state.audio, next) {
        (Missing, Queued) | (Queued, Downloading) | (Downloading, Queued) => !state.read,
        (Queued | Downloading | Available, Missing) | (Downloading, Available) => true,
        (Available, Deleting) | (Deleting, Reclaimed) => state.read,
        _ => false,
    }
}
fn normalized(mut metadata: EpisodeMetadata<'_>) -> Result<EpisodeMetadata<'_>, Error> {
    metadata.guid = metadata.guid.filter(|guid| !guid.is_empty());
    if metadata.title.is_empty() || metadata.enclosure.is_empty() {
        return Err(Error::InvalidMetadata);
    }
    if metadata.guid.is_some_and(|s| s.len() > MAX_GUID_BYTES)
        || metadata.title.len() > MAX_TITLE_BYTES
        || metadata.enclosure.len() > MAX_URL_BYTES
    {
        return Err(Error::FieldTooLong);
    }
    metadata.description = metadata
        .description
        .filter(|s| s.len() <= MAX_DESCRIPTION_BYTES);
    Ok(metadata)
}
fn same_identity(a: EpisodeMetadata<'_>, b: EpisodeMetadata<'_>) -> bool {
    match (a.guid, b.guid) {
        (Some(a), Some(b)) => a == b,
        (None, None) => a.enclosure == b.enclosure,
        _ => false,
    }
}
fn identity(subscription: SubscriptionId, metadata: EpisodeMetadata<'_>) -> (u64, u8, [u8; 32]) {
    (
        subscription.0,
        if metadata.guid.is_some() { 1 } else { 2 },
        Sha256::digest(metadata.guid.unwrap_or(metadata.enclosure).as_bytes()).into(),
    )
}

struct Writer<'a> {
    bytes: &'a mut [u8],
    pos: usize,
}
impl Writer<'_> {
    fn put(&mut self, value: &[u8]) -> Option<()> {
        let end = self.pos.checked_add(value.len())?;
        self.bytes.get_mut(self.pos..end)?.copy_from_slice(value);
        self.pos = end;
        Some(())
    }
    fn text(&mut self, value: &str) -> Option<()> {
        self.put(&u16::try_from(value.len()).ok()?.to_le_bytes())?;
        self.put(value.as_bytes())
    }
}
fn encode(episode: Episode<'_>, scratch: &mut [u8]) -> Option<usize> {
    let m = episode.metadata;
    let flags = u8::from(m.guid.is_some())
        | u8::from(m.description.is_some()) << 1
        | u8::from(m.published.is_some()) << 2
        | u8::from(m.enclosure_bytes.is_some()) << 3
        | u8::from(m.duration_seconds.is_some()) << 4;
    let mut w = Writer {
        bytes: scratch.get_mut(..MAX_RECORD_BYTES)?,
        pos: 0,
    };
    w.put(&[1, 1])?;
    w.put(&episode.subscription.0.to_le_bytes())?;
    w.put(&episode.id.0.to_le_bytes())?;
    w.put(&[
        episode.state.audio as u8,
        u8::from(episode.state.read),
        flags,
    ])?;
    if let Some(v) = m.published {
        w.put(&v.to_le_bytes())?;
    }
    if let Some(v) = m.enclosure_bytes {
        w.put(&v.to_le_bytes())?;
    }
    if let Some(v) = m.duration_seconds {
        w.put(&v.to_le_bytes())?;
    }
    if let Some(v) = m.guid {
        w.text(v)?;
    }
    w.text(m.title)?;
    w.text(m.enclosure)?;
    if let Some(v) = m.description {
        w.text(v)?;
    }
    Some(w.pos)
}
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn n<const N: usize>(&mut self) -> Option<[u8; N]> {
        let value = self.take(N)?;
        value.try_into().ok()
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let value = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(value)
    }
    fn text(&mut self, max: usize) -> Option<&'a str> {
        let n = u16::from_le_bytes(self.n()?) as usize;
        if n > max {
            return None;
        }
        core::str::from_utf8(self.take(n)?).ok()
    }
}
fn decode(bytes: &[u8]) -> Option<Episode<'_>> {
    let mut r = Reader { bytes, pos: 0 };
    if r.n::<2>()? != [1, 1] {
        return None;
    }
    let subscription = SubscriptionId(u64::from_le_bytes(r.n()?));
    let id = EpisodeId(u64::from_le_bytes(r.n()?));
    let audio = match r.n::<1>()?[0] {
        0 => AudioState::Missing,
        1 => AudioState::Queued,
        2 => AudioState::Downloading,
        3 => AudioState::Available,
        4 => AudioState::Deleting,
        5 => AudioState::Reclaimed,
        _ => return None,
    };
    let read = match r.n::<1>()?[0] {
        0 => false,
        1 => true,
        _ => return None,
    };
    let flags = r.n::<1>()?[0];
    if flags & !31 != 0 {
        return None;
    }
    let published = if flags & 4 != 0 {
        Some(i64::from_le_bytes(r.n()?))
    } else {
        None
    };
    let enclosure_bytes = if flags & 8 != 0 {
        Some(u64::from_le_bytes(r.n()?))
    } else {
        None
    };
    let duration_seconds = if flags & 16 != 0 {
        Some(u32::from_le_bytes(r.n()?))
    } else {
        None
    };
    let guid = if flags & 1 != 0 {
        Some(r.text(MAX_GUID_BYTES)?)
    } else {
        None
    };
    let title = r.text(MAX_TITLE_BYTES)?;
    let enclosure = r.text(MAX_URL_BYTES)?;
    let description = if flags & 2 != 0 {
        Some(r.text(MAX_DESCRIPTION_BYTES)?)
    } else {
        None
    };
    if r.pos != bytes.len()
        || subscription.0 == 0
        || id.0 == 0
        || title.is_empty()
        || enclosure.is_empty()
        || guid.is_some_and(str::is_empty)
        || (!read && matches!(audio, AudioState::Deleting | AudioState::Reclaimed))
    {
        return None;
    }
    Some(Episode {
        id,
        subscription,
        source: Source::RadioFrance,
        state: EpisodeState { read, audio },
        metadata: EpisodeMetadata {
            guid,
            title,
            enclosure,
            description,
            published,
            enclosure_bytes,
            duration_seconds,
        },
    })
}
#[cfg(test)]
#[path = "catalogue_create_tests.rs"]
mod create_tests;
#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod tests;
