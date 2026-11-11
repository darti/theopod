//! Raw media allocation units, persisted in the catalogue (t-121, ADR-0020).
//!
//! Bitmap pages (`b`), the used counter (`m/units`) and per-episode unit
//! records (`u`) only ever change together in one EKV transaction, so the
//! bitmap always equals the union of the records' extents.
use crate::catalogue::{Catalogue, EpisodeId, Error, commit_error, read_error, write_error};
use crate::media::FILE_PAGE;
use allocator_api2::{
    alloc::{AllocError, Allocator},
    vec::Vec,
};
use core::fmt;
use ekv::flash::Flash;

pub(crate) use theo_storage::card_layout::UNIT_SECTORS;
/// Bytes in one raw sector of the media region.
pub(crate) const SECTOR: usize = theo_storage::littlefs::SECTOR_SIZE;
/// Bytes in one media allocation unit (1 MiB).
pub const UNIT_BYTES: u64 = UNIT_SECTORS as u64 * SECTOR as u64;
/// Units covered by one bitmap page.
pub(crate) const UNITS_PER_PAGE: u32 = 32_768;
/// Bytes of one bitmap page, and the largest unit record.
pub(crate) const PAGE_BYTES: usize = 4096;
/// Record prefix: `state u8`, then `len u64` LE.
pub(crate) const RECORD_HEADER: usize = 9;
/// One extent: `start u32` LE, then `count u32` LE.
const EXTENT_BYTES: usize = 8;
/// Extents one unit record can hold (510).
pub(crate) const MAX_EXTENTS: usize = (PAGE_BYTES - RECORD_HEADER) / EXTENT_BYTES;
/// Units addressable with `u32` media-region sectors (2 TiB).
pub(crate) const MAX_UNITS: u32 = ((u32::MAX as u64 + 1) / UNIT_SECTORS as u64) as u32;
pub(crate) const UNITS_KEY: &[u8] = b"m/units";

const _: () = assert!(RECORD_HEADER + MAX_EXTENTS * EXTENT_BYTES <= PAGE_BYTES);
const _: () = assert!(UNITS_PER_PAGE as usize == PAGE_BYTES * 8);

/// Whether a unit record is still being downloaded or holds playable audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RecordState {
    /// Owned by the in-progress download; never playable.
    Temp = 0,
    /// Complete audio of `len` bytes.
    Published = 1,
}

/// `count` contiguous units starting at unit `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    /// First unit.
    pub start: u32,
    /// Number of units, at least one in a stored record.
    pub count: u32,
}

impl Extent {
    /// One past the last unit, `None` on overflow.
    pub fn end(self) -> Option<u32> {
        self.start.checked_add(self.count)
    }
}

/// In-RAM bitmap of every media unit, with its used counter and next-fit cursor.
pub(crate) struct UnitTable<A: Allocator> {
    bitmap: Vec<u8, A>,
    units: u32,
    used: u32,
    cursor: u32,
}

impl<A: Allocator> fmt::Debug for UnitTable<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitTable")
            .field("units", &self.units)
            .field("used", &self.used)
            .field("cursor", &self.cursor)
            .field("pages", &self.pages())
            .finish()
    }
}

impl<A: Allocator> UnitTable<A> {
    /// An empty table of `units` units, its bitmap allocated in `alloc`.
    pub fn new_in(units: u32, alloc: A) -> Result<Self, AllocError> {
        let pages = units.div_ceil(UNITS_PER_PAGE).max(1) as usize;
        let mut bitmap = Vec::new_in(alloc);
        bitmap
            .try_reserve_exact(pages * PAGE_BYTES)
            .map_err(|_| AllocError)?;
        bitmap.resize(pages * PAGE_BYTES, 0);
        Ok(Self {
            bitmap,
            units,
            used: 0,
            cursor: 0,
        })
    }
    /// Units in the media region.
    pub fn units(&self) -> u32 {
        self.units
    }
    /// Allocated units.
    pub fn used(&self) -> u32 {
        self.used
    }
    /// Free units.
    pub fn free(&self) -> u32 {
        self.units.saturating_sub(self.used)
    }
    /// Where the next-fit scan starts.
    pub(crate) fn cursor(&self) -> u32 {
        if self.cursor < self.units {
            self.cursor
        } else {
            0
        }
    }
    pub(crate) fn pages(&self) -> u16 {
        (self.bitmap.len() / PAGE_BYTES) as u16
    }
    pub(crate) fn page_of(&self, unit: u32) -> u16 {
        (unit / UNITS_PER_PAGE) as u16
    }
    pub(crate) fn page(&self, p: u16) -> &[u8] {
        &self.bitmap[p as usize * PAGE_BYTES..][..PAGE_BYTES]
    }
    pub(crate) fn page_mut(&mut self, p: u16) -> &mut [u8] {
        &mut self.bitmap[p as usize * PAGE_BYTES..][..PAGE_BYTES]
    }
    /// Whether `unit` is allocated; units past the end read as allocated.
    pub(crate) fn is_allocated(&self, unit: u32) -> bool {
        unit >= self.units
            || self
                .bitmap
                .get((unit / 8) as usize)
                .is_none_or(|byte| byte & (1 << (unit % 8)) != 0)
    }
    fn set(&mut self, unit: u32, on: bool) {
        if let Some(byte) = self.bitmap.get_mut((unit / 8) as usize) {
            if on {
                *byte |= 1 << (unit % 8)
            } else {
                *byte &= !(1 << (unit % 8))
            }
        }
    }
    /// First free unit in `from..end`, skipping full bytes.
    fn next_free(&self, from: u32, end: u32) -> Option<u32> {
        let mut unit = from;
        while unit < end {
            let index = (unit / 8) as usize;
            let shift = unit % 8;
            let ones = (self.bitmap.get(index)? >> shift).trailing_ones();
            if ones < 8 - shift {
                return Some(unit + ones).filter(|&free| free < end);
            }
            unit = u32::try_from(index + 1).ok()?.checked_mul(8)?;
        }
        None
    }
    /// Next-fit: the first free run of up to `max` contiguous units at or
    /// after `from` (normally the cursor), wrapping once to the start. A run
    /// never wraps. Changes nothing, so callers can plan several runs and
    /// refuse before any mutation; `claim` then marks them. Next-fit spreads
    /// writes and keeps a sequential download contiguous.
    pub(crate) fn find_run(&self, from: u32, max: u32) -> Option<Extent> {
        if max == 0 {
            return None;
        }
        let from = if from < self.units { from } else { 0 };
        let start = self
            .next_free(from, self.units)
            .or_else(|| self.next_free(0, from))?;
        let mut count = 0;
        while count < max && !self.is_allocated(start.checked_add(count)?) {
            count += 1;
        }
        Some(Extent { start, count })
    }
    /// Mark a run found by `find_run` and move the cursor past it.
    pub(crate) fn claim(&mut self, run: Extent) {
        for unit in run.start..run.end().unwrap_or(self.units).min(self.units) {
            if !self.is_allocated(unit) {
                self.set(unit, true);
                self.used = self.used.saturating_add(1);
            }
        }
        self.cursor = run.end().filter(|&end| end < self.units).unwrap_or(0);
    }
    /// Free one allocated unit; `false` (and no change) if it is not.
    pub fn release(&mut self, unit: u32) -> bool {
        if self.is_allocated(unit) && unit < self.units {
            let Some(used) = self.used.checked_sub(1) else {
                return false;
            };
            self.set(unit, false);
            self.used = used;
            return true;
        }
        false
    }
    /// Recount after loading; bits past `units` must be clear.
    fn recount(&self) -> Option<u32> {
        let mut count = 0u32;
        for (i, &byte) in self.bitmap.iter().enumerate() {
            if byte == 0 {
                continue;
            }
            let last = u64::try_from(i).ok()? * 8 + u64::from(7 - byte.leading_zeros());
            if last >= u64::from(self.units) {
                return None;
            }
            count = count.checked_add(byte.count_ones())?;
        }
        Some(count)
    }
}

/// Encode a unit record into `out`; `None` if `extents` exceeds
/// `MAX_EXTENTS` or `out` is too small. Returns the record length.
pub(crate) fn encode_record(
    state: RecordState,
    len: u64,
    extents: &[Extent],
    out: &mut [u8],
) -> Option<usize> {
    if extents.len() > MAX_EXTENTS {
        return None;
    }
    let n = RECORD_HEADER + extents.len() * EXTENT_BYTES;
    let out = out.get_mut(..n)?;
    let (header, body) = out.split_at_mut(RECORD_HEADER);
    header[0] = state as u8;
    header[1..].copy_from_slice(&len.to_le_bytes());
    for (slot, extent) in body
        .as_chunks_mut::<EXTENT_BYTES>()
        .0
        .iter_mut()
        .zip(extents)
    {
        let (start, count) = slot.split_at_mut(4);
        start.copy_from_slice(&extent.start.to_le_bytes());
        count.copy_from_slice(&extent.count.to_le_bytes());
    }
    Some(n)
}

/// A validated unit record borrowed from a read buffer.
#[derive(Debug)]
pub(crate) struct RecordView<'a> {
    /// Temp or published.
    pub state: RecordState,
    /// Bytes of audio, at most `total_units() * UNIT_BYTES`.
    pub len: u64,
    extents: &'a [u8],
    total: u32,
}

impl<'a> RecordView<'a> {
    /// Decode and validate a record: known state, whole extents, at most
    /// `MAX_EXTENTS`, non-empty non-overflowing extents, `len` within them.
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let state = match bytes.first()? {
            0 => RecordState::Temp,
            1 => RecordState::Published,
            _ => return None,
        };
        let len = u64::from_le_bytes(bytes.get(1..RECORD_HEADER)?.try_into().ok()?);
        let extents = bytes.get(RECORD_HEADER..)?;
        if !extents.len().is_multiple_of(EXTENT_BYTES) || extents.len() / EXTENT_BYTES > MAX_EXTENTS
        {
            return None;
        }
        let mut view = Self {
            state,
            len,
            extents,
            total: 0,
        };
        let mut total = 0u32;
        for extent in view.extents() {
            if extent.count == 0 {
                return None;
            }
            extent.end()?;
            total = total.checked_add(extent.count)?;
        }
        if len > u64::from(total).checked_mul(UNIT_BYTES)? {
            return None;
        }
        view.total = total;
        Some(view)
    }
    /// Number of extents.
    pub fn extent_count(&self) -> usize {
        self.extents.len() / EXTENT_BYTES
    }
    /// Extent `i`, if present.
    pub fn extent(&self, i: usize) -> Option<Extent> {
        let bytes = self.extents.get(i.checked_mul(EXTENT_BYTES)?..)?;
        Some(Extent {
            start: u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?),
            count: u32::from_le_bytes(bytes.get(4..EXTENT_BYTES)?.try_into().ok()?),
        })
    }
    /// The extents in record order.
    pub fn extents(&self) -> impl Iterator<Item = Extent> + '_ {
        (0..self.extent_count()).filter_map(|i| self.extent(i))
    }
    /// Units owned by the record, summed over its extents.
    pub fn total_units(&self) -> u32 {
        self.total
    }
}

fn page_key(p: u16) -> [u8; 3] {
    let b = p.to_be_bytes();
    [b'b', b[0], b[1]]
}
fn record_key(id: EpisodeId) -> [u8; 9] {
    let mut k = [0; 9];
    k[0] = b'u';
    k[1..].copy_from_slice(&id.0.to_be_bytes());
    k
}

impl<F: Flash> Catalogue<F> {
    /// Fill `table` from the catalogue, checking bitmap against the counter.
    pub(crate) async fn load_units<A: Allocator>(
        &mut self,
        table: &mut UnitTable<A>,
    ) -> Result<(), Error<F::Error>> {
        self.begin()?;
        let result = async {
            let tx = self.db.read_transaction().await;
            let mut counter = [0; 4];
            match tx.read(UNITS_KEY, &mut counter).await {
                Ok(4) => (),
                Ok(_) | Err(ekv::ReadError::KeyNotFound) => return Err(Error::Corrupt),
                Err(e) => return Err(read_error(e)),
            }
            let counter = u32::from_le_bytes(counter);
            for p in 0..table.pages() {
                let page = table.page_mut(p);
                match tx.read(&page_key(p), page).await {
                    Ok(PAGE_BYTES) => (),
                    Ok(_) => return Err(Error::Corrupt),
                    Err(ekv::ReadError::KeyNotFound) => page.fill(0),
                    Err(e) => return Err(read_error(e)),
                }
            }
            if table.recount() != Some(counter) {
                return Err(Error::Corrupt);
            }
            table.used = counter;
            table.cursor = 0;
            Ok(())
        }
        .await;
        self.finish(result)
    }

    /// Persist dirty bitmap pages, the counter and one record atomically.
    /// `dirty` must be strictly increasing (keys are written in lexical
    /// order); anything else is `Corrupt` and writes nothing.
    pub(crate) async fn commit_units<A: Allocator>(
        &mut self,
        table: &UnitTable<A>,
        dirty: &[u16],
        id: EpisodeId,
        record: Option<&[u8]>,
    ) -> Result<(), Error<F::Error>> {
        if !dirty.windows(2).all(|w| w[0] < w[1]) || dirty.iter().any(|&p| p >= table.pages()) {
            return Err(Error::Corrupt);
        }
        self.begin()?;
        let result = async {
            let mut tx = self.db.write_transaction().await;
            // `b` < `m/units` < `u`: lexical order holds.
            for &p in dirty {
                tx.write(&page_key(p), table.page(p))
                    .await
                    .map_err(write_error)?;
            }
            tx.write(UNITS_KEY, &table.used().to_le_bytes())
                .await
                .map_err(write_error)?;
            match record {
                Some(r) => tx.write(&record_key(id), r).await.map_err(write_error)?,
                None => tx.delete(&record_key(id)).await.map_err(write_error)?,
            }
            tx.commit().await.map_err(commit_error)
        }
        .await;
        self.finish(result)
    }

    /// The validated unit record of `id`, read into `buf`.
    pub(crate) async fn unit_record<'a>(
        &mut self,
        id: EpisodeId,
        buf: &'a mut [u8],
    ) -> Result<Option<RecordView<'a>>, Error<F::Error>> {
        self.begin()?;
        let result = async {
            let tx = self.db.read_transaction().await;
            match tx.read(&record_key(id), buf).await {
                Ok(n) => Ok(Some(n)),
                Err(ekv::ReadError::KeyNotFound) => Ok(None),
                Err(e) => Err(read_error(e)),
            }
        }
        .await;
        let read = self.finish(result)?;
        match read {
            None => Ok(None),
            Some(n) => buf
                .get(..n)
                .and_then(RecordView::decode)
                .map(Some)
                .ok_or(Error::Corrupt),
        }
    }

    /// Up to `FILE_PAGE` episode ids holding unit records, after `after`.
    pub(crate) async fn unit_ids(
        &mut self,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
        buf: &mut [u8],
    ) -> Result<usize, Error<F::Error>> {
        use core::ops::Bound;
        self.begin()?;
        let result = async {
            let lower = record_key(after.unwrap_or(EpisodeId(0)));
            let start = if after.is_some() {
                Bound::Excluded(lower.as_slice())
            } else {
                Bound::Included(b"u".as_slice())
            };
            let tx = self.db.read_transaction().await;
            let mut cursor = tx
                .read_range((start, Bound::Excluded(b"v".as_slice())))
                .await
                .map_err(|e| match e {
                    ekv::Error::Flash(e) => Error::Storage(e),
                    ekv::Error::Corrupted => Error::Corrupt,
                })?;
            let mut key = [0; 9];
            let mut count = 0;
            while count < FILE_PAGE {
                let Some((k, _)) = cursor.next(&mut key, buf).await.map_err(|e| match e {
                    ekv::CursorError::Flash(e) => Error::Storage(e),
                    _ => Error::Corrupt,
                })?
                else {
                    break;
                };
                let [b'u', id @ ..] = key else {
                    return Err(Error::Corrupt);
                };
                let id = u64::from_be_bytes(id);
                if k != key.len() || id == 0 {
                    return Err(Error::Corrupt);
                }
                out[count] = EpisodeId(id);
                count += 1;
            }
            Ok(count)
        }
        .await;
        self.finish(result)
    }
}

#[cfg(test)]
#[path = "units_tests.rs"]
mod tests;
