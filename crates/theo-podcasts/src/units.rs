//! Raw media allocation units, persisted in the catalogue (t-121, ADR-0020).
//!
//! Bitmap pages (`b`), the used counter (`m/units`) and per-episode unit
//! records (`u`) only ever change together in one EKV transaction, so the
//! bitmap always equals the union of records.
use crate::catalogue::{Catalogue, EpisodeId, Error, commit_error, read_error, write_error};
use crate::media::FILE_PAGE;
use allocator_api2::{
    alloc::{AllocError, Allocator},
    vec::Vec,
};
use ekv::flash::Flash;

pub use theo_storage::card_layout::UNIT_SECTORS; // one definition (Task 1)
pub const UNIT_BYTES: u64 = UNIT_SECTORS as u64 * 512;
pub const UNITS_PER_PAGE: u32 = 32_768;
pub const PAGE_BYTES: usize = 4096;
pub const RECORD_HEADER: usize = 9;
pub const MAX_RECORD_UNITS: usize = (4096 - RECORD_HEADER) / 4;
pub(crate) const UNITS_KEY: &[u8] = b"m/units";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordState {
    Temp = 0,
    Published = 1,
}

pub struct UnitTable<A: Allocator> {
    bitmap: Vec<u8, A>,
    units: u32,
    used: u32,
    cursor: u32,
}

impl<A: Allocator> UnitTable<A> {
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
    pub fn units(&self) -> u32 {
        self.units
    }
    pub fn used(&self) -> u32 {
        self.used
    }
    pub fn free(&self) -> u32 {
        self.units - self.used
    }
    pub fn pages(&self) -> u16 {
        (self.bitmap.len() / PAGE_BYTES) as u16
    }
    pub fn page_of(&self, unit: u32) -> u16 {
        (unit / UNITS_PER_PAGE) as u16
    }
    pub fn page(&self, p: u16) -> &[u8] {
        &self.bitmap[p as usize * PAGE_BYTES..][..PAGE_BYTES]
    }
    pub(crate) fn page_mut(&mut self, p: u16) -> &mut [u8] {
        &mut self.bitmap[p as usize * PAGE_BYTES..][..PAGE_BYTES]
    }
    fn bit(&self, unit: u32) -> bool {
        self.bitmap[(unit / 8) as usize] & (1 << (unit % 8)) != 0
    }
    fn set(&mut self, unit: u32, on: bool) {
        let byte = &mut self.bitmap[(unit / 8) as usize];
        if on {
            *byte |= 1 << (unit % 8)
        } else {
            *byte &= !(1 << (unit % 8))
        }
    }
    /// Next-fit: spreads writes and keeps a sequential download contiguous.
    pub fn allocate(&mut self) -> Option<u32> {
        for step in 0..self.units {
            let unit = (self.cursor + step) % self.units;
            if !self.bit(unit) {
                self.set(unit, true);
                self.used += 1;
                self.cursor = (unit + 1) % self.units;
                return Some(unit);
            }
        }
        None
    }
    pub fn release(&mut self, unit: u32) -> bool {
        if unit >= self.units || !self.bit(unit) {
            return false;
        }
        self.set(unit, false);
        self.used -= 1;
        true
    }
    /// Recount after loading; bits past `units` must be clear.
    fn recount(&mut self) -> Option<u32> {
        let mut count = 0u32;
        for (i, byte) in self.bitmap.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    if (i * 8 + bit) as u32 >= self.units {
                        return None;
                    }
                    count += 1;
                }
            }
        }
        Some(count)
    }
}

pub fn encode_record(state: RecordState, len: u64, units: &[u32], out: &mut [u8]) -> usize {
    assert!(units.len() <= MAX_RECORD_UNITS && out.len() >= RECORD_HEADER + units.len() * 4);
    out[0] = state as u8;
    out[1..9].copy_from_slice(&len.to_le_bytes());
    for (i, unit) in units.iter().enumerate() {
        out[RECORD_HEADER + i * 4..][..4].copy_from_slice(&unit.to_le_bytes());
    }
    RECORD_HEADER + units.len() * 4
}

pub struct RecordView<'a> {
    pub state: RecordState,
    pub len: u64,
    units: &'a [u8],
}
impl<'a> RecordView<'a> {
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let units = bytes.get(RECORD_HEADER..)?;
        if units.len() % 4 != 0 || units.len() / 4 > MAX_RECORD_UNITS {
            return None;
        }
        let state = match bytes[0] {
            0 => RecordState::Temp,
            1 => RecordState::Published,
            _ => return None,
        };
        let len = u64::from_le_bytes(bytes[1..9].try_into().ok()?);
        if len > (units.len() / 4) as u64 * UNIT_BYTES {
            return None;
        }
        Some(Self { state, len, units })
    }
    pub fn count(&self) -> usize {
        self.units.len() / 4
    }
    pub fn unit(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.units[i * 4..][..4].try_into().unwrap_or([0; 4]))
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

// Consumed by `UnitMedia` (t-121, next tasks).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by UnitMedia (t-121 Task 4)")
)]
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
    /// `dirty` must be strictly increasing (keys are written in lexical order).
    pub(crate) async fn commit_units<A: Allocator>(
        &mut self,
        table: &UnitTable<A>,
        dirty: &[u16],
        id: EpisodeId,
        record: Option<&[u8]>,
    ) -> Result<(), Error<F::Error>> {
        debug_assert!(dirty.windows(2).all(|w| w[0] < w[1]));
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

    pub(crate) async fn unit_record<'a>(
        &mut self,
        id: EpisodeId,
        buf: &'a mut [u8],
    ) -> Result<Option<RecordView<'a>>, Error<F::Error>> {
        self.begin()?;
        let result = async {
            let tx = self.db.read_transaction().await;
            match tx.read(&record_key(id), buf).await {
                Ok(n) if RecordView::decode(&buf[..n]).is_some() => Ok(Some(n)),
                Ok(_) => Err(Error::Corrupt),
                Err(ekv::ReadError::KeyNotFound) => Ok(None),
                Err(e) => Err(read_error(e)),
            }
        }
        .await;
        Ok(self
            .finish(result)?
            .and_then(|n| RecordView::decode(&buf[..n])))
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
                if k != 9 || key[0] != b'u' {
                    return Err(Error::Corrupt);
                }
                let id = u64::from_be_bytes(key[1..].try_into().map_err(|_| Error::Corrupt)?);
                if id == 0 {
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
