//! Persistent identities and local state on the selected EKV backend.
use ekv::flash::Flash;

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
#[derive(Debug)]
pub enum Error<E> {
    Storage(E),
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
    Cancelled,
    Disabled,
}
pub fn mark_read(state: &mut EpisodeState) {
    state.read = true;
}
/// Missing unread audio needs one queue admission. Queued work is already admitted.
pub fn should_queue(state: &EpisodeState) -> bool {
    !state.read && state.audio == AudioState::Missing
}

use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;
use sha2::{Digest, Sha256};
const SCHEMA: u32 = 1;
const NEXT_KEY: &[u8] = b"m/next";
const SCHEMA_KEY: &[u8] = b"m/schema";
const AUDIO_OFFSET: usize = 18;
const READ_OFFSET: usize = 19;
const _: () =
    assert!(ekv::config::MAX_KEY_SIZE >= 42 && ekv::config::MAX_VALUE_SIZE >= MAX_RECORD_BYTES);

/// Unique database owner. Metadata views borrow the caller's scratch buffer,
/// never the database; no record allocation or competing writer is hidden here.
/// Storage/format/capacity errors and dropped operation futures disable this
/// owner until it is discarded and reopened. The service must also invalidate
/// its shared filesystem session, as it does for every failed storage request.
pub struct Catalogue<F: Flash> {
    db: ekv::Database<F, NoopRawMutex>,
    next: u64,
    enabled: bool,
}
impl<F: Flash> Catalogue<F> {
    /// Destructive: only for a newly initialized, unpublished backend.
    pub async fn create(flash: F) -> Result<Self, Error<F::Error>> {
        let db = ekv::Database::new(flash, ekv::Config::default());
        db.format()
            .await
            .map_err(|ekv::FormatError::Flash(e)| Error::Storage(e))?;
        let mut tx = db.write_transaction().await;
        // Lexical order is part of EKV's transaction contract.
        tx.write(NEXT_KEY, &1u64.to_le_bytes())
            .await
            .map_err(write_error)?;
        tx.write(SCHEMA_KEY, &SCHEMA.to_le_bytes())
            .await
            .map_err(write_error)?;
        tx.commit().await.map_err(commit_error)?;
        Ok(Self {
            db,
            next: 1,
            enabled: true,
        })
    }
    /// Read-only mount/schema validation. Never formats or migrates on failure.
    pub async fn open(flash: F) -> Result<Self, Error<F::Error>> {
        let db = ekv::Database::new(flash, ekv::Config::default());
        db.mount().await.map_err(|e| match e {
            ekv::MountError::Flash(e) => Error::Storage(e),
            ekv::MountError::Corrupted => Error::Corrupt,
        })?;
        let tx = db.read_transaction().await;
        let mut schema = [0; 4];
        match tx.read(SCHEMA_KEY, &mut schema).await {
            Ok(4) if u32::from_le_bytes(schema) == SCHEMA => (),
            Err(ekv::ReadError::Flash(e)) => return Err(Error::Storage(e)),
            _ => return Err(Error::Schema),
        }
        let mut next = [0; 8];
        if tx.read(NEXT_KEY, &mut next).await.map_err(read_error)? != 8 {
            return Err(Error::Corrupt);
        }
        let next = u64::from_le_bytes(next);
        if next == 0 {
            return Err(Error::Corrupt);
        }
        drop(tx);
        Ok(Self {
            db,
            next,
            enabled: true,
        })
    }
    /// Last completely reconciled history. Partial imports never replace this.
    pub async fn sync_checkpoint(
        &mut self,
        id: SubscriptionId,
        scratch: &mut [u8],
    ) -> Result<Option<crate::sync::Checkpoint>, Error<F::Error>> {
        self.check(scratch)?;
        if id.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        self.begin()?;
        let result = async {
            let tx = self.db.read_transaction().await;
            match tx
                .read(&sync_key(id), &mut scratch[..MAX_RECORD_BYTES])
                .await
            {
                Ok(n) => crate::sync::Checkpoint::decode(&scratch[..n])
                    .map(Some)
                    .ok_or(Error::Corrupt),
                Err(ekv::ReadError::KeyNotFound) => Ok(None),
                Err(e) => Err(read_error(e)),
            }
        }
        .await;
        self.finish(result)
    }
    pub(crate) async fn save_sync_checkpoint(
        &mut self,
        id: SubscriptionId,
        checkpoint: &crate::sync::Checkpoint,
        scratch: &mut [u8],
    ) -> Result<(), Error<F::Error>> {
        self.check(scratch)?;
        if id.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        let n = checkpoint.encode(scratch).ok_or(Error::InvalidMetadata)?;
        self.begin()?;
        let result = async {
            let mut tx = self.db.write_transaction().await;
            tx.write(&sync_key(id), &scratch[..n])
                .await
                .map_err(write_error)?;
            tx.commit().await.map_err(commit_error)
        }
        .await;
        self.finish(result)
    }
    fn begin(&mut self) -> Result<(), Error<F::Error>> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        self.enabled = false;
        Ok(())
    }
    fn finish<T>(&mut self, result: Result<T, Error<F::Error>>) -> Result<T, Error<F::Error>> {
        if !matches!(
            &result,
            Err(Error::Storage(_)
                | Error::Corrupt
                | Error::Schema
                | Error::IdentityCollision
                | Error::Full
                | Error::Cancelled
                | Error::Disabled)
        ) {
            self.enabled = true;
        }
        result
    }
    fn check(&self, scratch: &[u8]) -> Result<(), Error<F::Error>> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        if scratch.len() < MAX_RECORD_BYTES {
            return Err(Error::BufferTooSmall);
        }
        Ok(())
    }
    pub async fn get<'a>(
        &mut self,
        id: EpisodeId,
        scratch: &'a mut [u8],
    ) -> Result<Option<Episode<'a>>, Error<F::Error>> {
        self.check(scratch)?;
        self.begin()?;
        let result = self
            .load(id, scratch)
            .await
            .map(|record| record.map(|(episode, _)| episode));
        self.finish(result)
    }
    async fn load<'a>(
        &self,
        id: EpisodeId,
        scratch: &'a mut [u8],
    ) -> Result<Option<(Episode<'a>, usize)>, Error<F::Error>> {
        let tx = self.db.read_transaction().await;
        let n = match tx
            .read(&episode_key(id), &mut scratch[..MAX_RECORD_BYTES])
            .await
        {
            Ok(n) => n,
            Err(ekv::ReadError::KeyNotFound) => return Ok(None),
            Err(e) => return Err(read_error(e)),
        };
        let episode = decode(&scratch[..n]).ok_or(Error::Corrupt)?;
        if episode.id != id || id.0 >= self.next {
            return Err(Error::Corrupt);
        }
        Ok(Some((episode, n)))
    }
    pub async fn upsert(
        &mut self,
        subscription: SubscriptionId,
        metadata: EpisodeMetadata<'_>,
        scratch: &mut [u8],
    ) -> Result<UpsertOutcome, Error<F::Error>> {
        self.check(scratch)?;
        let metadata = normalized(metadata)?;
        if subscription.0 == 0 {
            return Err(Error::InvalidMetadata);
        }
        self.begin()?;
        let result = self.upsert_inner(subscription, metadata, scratch).await;
        self.finish(result)
    }
    async fn upsert_inner(
        &mut self,
        subscription: SubscriptionId,
        metadata: EpisodeMetadata<'_>,
        scratch: &mut [u8],
    ) -> Result<UpsertOutcome, Error<F::Error>> {
        let index = identity_key(subscription, metadata);
        let mut value = [0; 8];
        let existing = {
            let tx = self.db.read_transaction().await;
            match tx.read(&index, &mut value).await {
                Ok(8) => Some(EpisodeId(u64::from_le_bytes(value))),
                Ok(_) => return Err(Error::Corrupt),
                Err(ekv::ReadError::KeyNotFound) => None,
                Err(e) => return Err(read_error(e)),
            }
        };
        let (id, mut state, created) = if let Some(id) = existing {
            let (episode, _) = self.load(id, scratch).await?.ok_or(Error::Corrupt)?;
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
        let mut tx = self.db.write_transaction().await;
        tx.write(&episode_key(id), &scratch[..n])
            .await
            .map_err(write_error)?;
        if created {
            tx.write(&index, &id.0.to_le_bytes())
                .await
                .map_err(write_error)?;
            tx.write(NEXT_KEY, &(self.next + 1).to_le_bytes())
                .await
                .map_err(write_error)?;
        }
        tx.commit().await.map_err(commit_error)?;
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
    pub async fn scan(
        &mut self,
        after: Option<EpisodeId>,
        scratch: &mut [u8],
        emit: impl for<'record> FnMut(Episode<'record>),
    ) -> Result<ScanPage, Error<F::Error>> {
        self.check(scratch)?;
        self.begin()?;
        let result = self.scan_inner(after, scratch, emit).await;
        self.finish(result)
    }
    async fn scan_inner(
        &self,
        after: Option<EpisodeId>,
        scratch: &mut [u8],
        mut emit: impl for<'record> FnMut(Episode<'record>),
    ) -> Result<ScanPage, Error<F::Error>> {
        use core::ops::Bound;
        let lower = episode_key(after.unwrap_or(EpisodeId(0)));
        let start = if after.is_some() {
            Bound::Excluded(lower.as_slice())
        } else {
            Bound::Included(b"e".as_slice())
        };
        let tx = self.db.read_transaction().await;
        let mut cursor = tx
            .read_range((start, Bound::Excluded(b"f".as_slice())))
            .await
            .map_err(|e| match e {
                ekv::Error::Flash(e) => Error::Storage(e),
                ekv::Error::Corrupted => Error::Corrupt,
            })?;
        let mut key = [0; 9];
        let mut page = ScanPage {
            resume_after: None,
            has_more: false,
            records: 0,
            bytes: 0,
        };
        while let Some((k, n)) = cursor
            .next(&mut key, &mut scratch[..MAX_RECORD_BYTES])
            .await
            .map_err(|e| match e {
                ekv::CursorError::Flash(e) => Error::Storage(e),
                _ => Error::Corrupt,
            })?
        {
            if page.records == MAX_SCAN_RECORDS || page.bytes + k + n > MAX_SCAN_BYTES {
                page.has_more = true;
                break;
            }
            if k != 9 || key[0] != b'e' {
                return Err(Error::Corrupt);
            }
            let id = EpisodeId(u64::from_be_bytes(
                key[1..].try_into().map_err(|_| Error::Corrupt)?,
            ));
            let episode = decode(&scratch[..n]).ok_or(Error::Corrupt)?;
            if episode.id != id || id.0 >= self.next {
                return Err(Error::Corrupt);
            }
            emit(episode);
            page.resume_after = Some(id);
            page.records += 1;
            page.bytes += k + n;
        }
        Ok(page)
    }
    pub async fn mark_read(
        &mut self,
        id: EpisodeId,
        scratch: &mut [u8],
    ) -> Result<(), Error<F::Error>> {
        self.check(scratch)?;
        self.begin()?;
        let result = self.state_inner(id, None, scratch).await;
        self.finish(result)
    }
    pub async fn transition_audio(
        &mut self,
        id: EpisodeId,
        next: AudioState,
        scratch: &mut [u8],
    ) -> Result<(), Error<F::Error>> {
        self.check(scratch)?;
        self.begin()?;
        let result = self.state_inner(id, Some(next), scratch).await;
        self.finish(result)
    }
    async fn state_inner(
        &self,
        id: EpisodeId,
        next: Option<AudioState>,
        scratch: &mut [u8],
    ) -> Result<(), Error<F::Error>> {
        let (mut state, n) = {
            let (episode, n) = self.load(id, scratch).await?.ok_or(Error::NotFound)?;
            (episode.state, n)
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
        let mut tx = self.db.write_transaction().await;
        tx.write(&episode_key(id), &scratch[..n])
            .await
            .map_err(write_error)?;
        tx.commit().await.map_err(commit_error)
    }
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
fn normalized<E>(mut metadata: EpisodeMetadata<'_>) -> Result<EpisodeMetadata<'_>, Error<E>> {
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
fn episode_key(id: EpisodeId) -> [u8; 9] {
    let mut key = [0; 9];
    key[0] = b'e';
    key[1..].copy_from_slice(&id.0.to_be_bytes());
    key
}
fn identity_key(subscription: SubscriptionId, metadata: EpisodeMetadata<'_>) -> [u8; 42] {
    let mut key = [0; 42];
    key[0] = b'i';
    key[1..9].copy_from_slice(&subscription.0.to_be_bytes());
    key[9] = if metadata.guid.is_some() { 1 } else { 2 };
    key[10..].copy_from_slice(&Sha256::digest(
        metadata.guid.unwrap_or(metadata.enclosure).as_bytes(),
    ));
    key
}
fn read_error<E>(error: ekv::ReadError<E>) -> Error<E> {
    match error {
        ekv::ReadError::Flash(e) => Error::Storage(e),
        _ => Error::Corrupt,
    }
}
fn write_error<E>(error: ekv::WriteError<E>) -> Error<E> {
    match error {
        ekv::WriteError::Flash(e) => Error::Storage(e),
        ekv::WriteError::Full => Error::Full,
        ekv::WriteError::TransactionCanceled => Error::Cancelled,
        _ => Error::Corrupt,
    }
}
fn commit_error<E>(error: ekv::CommitError<E>) -> Error<E> {
    match error {
        ekv::CommitError::Flash(e) => Error::Storage(e),
        ekv::CommitError::TransactionCanceled => Error::Cancelled,
        _ => Error::Corrupt,
    }
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
#[path = "catalogue_tests.rs"]
mod tests;

fn sync_key(id: SubscriptionId) -> [u8; 9] {
    let mut key = [0; 9];
    key[0] = b's';
    key[1..].copy_from_slice(&id.0.to_be_bytes());
    key
}
