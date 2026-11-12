//! Raw allocation-unit media store (t-121, ADR-0020).
//!
//! Podcast audio lives in 1 MiB units of raw sectors; ownership is a `u`
//! record of extents in the catalogue, committed together with the bitmap
//! and counter (`crate::units`). Crash rule: a unit is owned by a record
//! before any data sector is written to it, so a crash can leave a partially
//! written owned unit, never an unowned one, and recovery's `unlink_temp`
//! returns it.
//!
//! Each operation's cost is bounded by one chunk and a fixed number of
//! catalogue commits (at most two for `reset_temp`: free the previous temp,
//! then reserve; at most one otherwise), independent of how much media is
//! stored. A known-length download reserves all its units up front, so
//! streaming it makes no commit; an unknown-length one grows in doubling
//! batches of at most 64 units. Input and state errors (`Invalid`, `Exists`,
//! `Full`) are detected before any change and leave the store enabled;
//! catalogue, device and allocation errors disable it until it is reopened
//! (reboot), since RAM may no longer match the card.
use crate::{
    catalogue::{Catalogue, EpisodeId, Error},
    media::{AUDIO_CHUNK, FILE_PAGE, MediaStore, Space},
    units::{
        Extent, MAX_EXTENTS, MAX_UNITS, PAGE_BYTES, RecordState, SECTOR, UNIT_SECTORS,
        UNITS_PER_PAGE, UnitTable, encode_record,
    },
};
use allocator_api2::{alloc::Allocator, boxed::Box, vec::Vec};
use core::fmt;
use theo_storage::littlefs::SectorIo;

pub use crate::units::UNIT_BYTES;

/// Bitmap pages one commit may dirty: every page of the largest table.
const MAX_DIRTY_PAGES: usize = 64;
/// Most units one growth batch adds.
const MAX_BATCH: u32 = 64;

// A whole u32-sector media region fits the dirty-page vector, so freeing a
// record can never overflow it.
const _: () = assert!(MAX_UNITS.div_ceil(UNITS_PER_PAGE) as usize <= MAX_DIRTY_PAGES);

type Dirty = heapless::Vec<u16, MAX_DIRTY_PAGES>;

/// A unit media store failure.
#[derive(Debug)]
pub enum UnitError {
    /// The catalogue failed or holds inconsistent unit records; disables the store.
    Catalogue(Error),
    /// A raw sector write or sync failed; disables the store.
    Device(littlefs_rust::Error),
    /// Not enough free units; the store stays enabled.
    Full,
    /// Bad input or state (wrong id, empty or oversize chunk, an episode
    /// needing more than `MAX_EXTENTS` extents); the store stays enabled.
    Invalid,
    /// Published audio already exists for this id; the store stays enabled.
    Exists,
    /// A previous error disabled the store until reopen.
    Disabled,
    /// The bitmap or a buffer could not be allocated; disables the store.
    Alloc,
}

/// The single in-progress temporary episode, held in RAM.
struct Temp {
    id: Option<EpisodeId>,
    len: u64,
    extents: heapless::Vec<Extent, MAX_EXTENTS>,
    /// Units owned by `extents`.
    owned: u32,
    /// `len` at the last sync, while nothing was appended since.
    synced: Option<u64>,
    /// Bytes of the sector at `len`, written once full or at sync.
    tail: [u8; SECTOR],
}

impl Temp {
    fn clear(&mut self) {
        self.id = None;
        self.len = 0;
        self.extents.clear();
        self.owned = 0;
        self.synced = None;
    }
    fn capacity(&self) -> u64 {
        u64::from(self.owned) * UNIT_BYTES
    }
    /// Device sector holding byte `at` of this temp.
    fn sector(&self, at: u64) -> Option<u32> {
        let mut index = u32::try_from(at / UNIT_BYTES).ok()?;
        let offset = u32::try_from((at % UNIT_BYTES) / SECTOR as u64).ok()?;
        for extent in &self.extents {
            match index.checked_sub(extent.count) {
                Some(rest) => index = rest,
                None => {
                    return extent
                        .start
                        .checked_add(index)?
                        .checked_mul(UNIT_SECTORS)?
                        .checked_add(offset);
                }
            }
        }
        None
    }
}

/// New units planned for one commit, not yet claimed in the table.
struct Plan {
    runs: heapless::Vec<Extent, MAX_EXTENTS>,
    units: u32,
    dirty: Dirty,
}

/// Media store over raw 1 MiB units of a media region.
pub struct UnitMedia<D: SectorIo, A: Allocator + Clone> {
    device: D,
    pub(crate) table: UnitTable<A>,
    buf: Vec<u8, A>,
    temp: Box<Temp, A>,
    enabled: bool,
    #[cfg(test)]
    pub(crate) commits: usize,
}

impl<D: SectorIo, A: Allocator + Clone> fmt::Debug for UnitMedia<D, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitMedia")
            .field("table", &self.table)
            .field("temp", &self.temp.id)
            .field("temp_len", &self.temp.len)
            .field("temp_units", &self.temp.owned)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl<D: SectorIo, A: Allocator + Clone> UnitMedia<D, A> {
    /// Load the unit table of `units` units from the catalogue. `device`
    /// addresses the media region from its first sector.
    pub fn open_in(
        cat: &mut Catalogue,
        device: D,
        units: u32,
        alloc: A,
    ) -> Result<Self, UnitError> {
        if units > MAX_UNITS {
            return Err(UnitError::Invalid);
        }
        let mut table = UnitTable::new_in(units, alloc.clone()).map_err(|_| UnitError::Alloc)?;
        cat.load_units(&mut table).map_err(UnitError::Catalogue)?;
        let mut buf = Vec::new_in(alloc.clone());
        buf.try_reserve_exact(PAGE_BYTES)
            .map_err(|_| UnitError::Alloc)?;
        buf.resize(PAGE_BYTES, 0);
        let temp = Box::try_new_in(
            Temp {
                id: None,
                len: 0,
                extents: heapless::Vec::new(),
                owned: 0,
                synced: None,
                tail: [0; SECTOR],
            },
            alloc,
        )
        .map_err(|_| UnitError::Alloc)?;
        Ok(Self {
            device,
            table,
            buf,
            temp,
            enabled: true,
            #[cfg(test)]
            commits: 0,
        })
    }

    #[cfg(test)]
    pub(crate) fn device(&self) -> &D {
        &self.device
    }

    fn check(&self) -> Result<(), UnitError> {
        if self.enabled {
            Ok(())
        } else {
            Err(UnitError::Disabled)
        }
    }

    /// Catalogue, device and allocation failures disable the store: RAM
    /// state may no longer match the card. Input and state errors are
    /// detected before any change and leave it enabled.
    fn guard<T>(&mut self, result: Result<T, UnitError>) -> Result<T, UnitError> {
        if matches!(
            result,
            Err(UnitError::Catalogue(_) | UnitError::Device(_) | UnitError::Alloc)
        ) {
            self.enabled = false;
        }
        result
    }

    fn write(&mut self, sector: Option<u32>) -> Result<(), UnitError> {
        // Ownership is checked before writing; a missing sector is a bug
        // that leaves RAM suspect, so it disables the store.
        let sector = sector.ok_or(UnitError::Catalogue(Error::Corrupt))?;
        self.device
            .write_sector(sector, &self.temp.tail)
            .map_err(UnitError::Device)
    }

    /// Commit the dirty pages, the counter and `record` (`None` deletes it).
    fn commit(
        &mut self,
        cat: &mut Catalogue,
        dirty: &[u16],
        id: EpisodeId,
        record: Option<usize>,
    ) -> Result<(), UnitError> {
        #[cfg(test)]
        {
            self.commits += 1;
        }
        let record = match record {
            Some(n) => Some(
                self.buf
                    .get(..n)
                    .ok_or(UnitError::Catalogue(Error::Corrupt))?,
            ),
            None => None,
        };
        cat.commit_units(&self.table, dirty, id, record)
            .map_err(UnitError::Catalogue)
    }

    /// Encode the temp's record into `buf`.
    fn encode_temp(&mut self, state: RecordState) -> Option<usize> {
        encode_record(state, self.temp.len, &self.temp.extents, &mut self.buf)
    }

    /// Free the record of `id` if it is in `state`; idempotent when absent.
    fn unlink(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        state: RecordState,
    ) -> Result<(), UnitError> {
        if state == RecordState::Temp && self.temp.id == Some(id) {
            self.temp.clear();
        }
        let Some(record) = cat
            .unit_record(id, &mut self.buf)
            .map_err(UnitError::Catalogue)?
        else {
            return Ok(());
        };
        if record.state != state {
            return Ok(());
        }
        // Validate every extent before changing the table.
        let mut dirty = Dirty::new();
        for extent in record.extents() {
            let end = extent.end().ok_or(UnitError::Catalogue(Error::Corrupt))?;
            if (extent.start..end).any(|unit| !self.table.is_allocated(unit))
                || end > self.table.units()
            {
                return Err(UnitError::Catalogue(Error::Corrupt));
            }
            for page in self.table.page_of(extent.start)..=self.table.page_of(end - 1) {
                if let Err(at) = dirty.binary_search(&page) {
                    // Unreachable for `MAX_UNITS` (const assert above).
                    dirty.insert(at, page).map_err(|_| UnitError::Invalid)?;
                }
            }
        }
        for extent in record.extents() {
            for unit in extent.start..extent.start.saturating_add(extent.count) {
                self.table.release(unit);
            }
        }
        self.commit(cat, &dirty, id, None)
    }

    /// Plan up to `want` new units after the temp's extents, next-fit from
    /// the table cursor. Stops early when a new extent or dirty page would
    /// not fit. Changes nothing.
    fn plan(&self, want: u32) -> Plan {
        let mut plan = Plan {
            runs: heapless::Vec::new(),
            units: 0,
            dirty: Dirty::new(),
        };
        let units = self.table.units();
        let origin = self.table.cursor();
        // Position in next-fit order: planned units are not claimed yet, so
        // a scan that comes back around must stop before reaching them.
        let distance = |unit: u32| {
            if unit >= origin {
                u64::from(unit - origin)
            } else {
                u64::from(unit) + u64::from(units) - u64::from(origin)
            }
        };
        let mut extents = self.temp.extents.len();
        let mut last_end = self.temp.extents.last().and_then(|e| e.end());
        let mut cursor = origin;
        let mut reached = 0u64;
        while plan.units < want {
            let Some(mut run) = self.table.find_run(cursor, want - plan.units) else {
                break;
            };
            if distance(run.start) < reached {
                break;
            }
            if run.start < origin {
                run.count = run.count.min(origin - run.start);
            }
            let merges = last_end == Some(run.start);
            if !merges && extents == MAX_EXTENTS {
                break;
            }
            let Some(end) = run.end() else {
                break;
            };
            let mut dirty = plan.dirty.clone();
            let pages = self.table.page_of(run.start)..=self.table.page_of(end - 1);
            if pages
                .into_iter()
                .any(|page| match dirty.binary_search(&page) {
                    Ok(_) => false,
                    Err(at) => dirty.insert(at, page).is_err(),
                })
                || plan.runs.push(run).is_err()
            {
                break;
            }
            let Some(units) = plan.units.checked_add(run.count) else {
                break;
            };
            plan.dirty = dirty;
            plan.units = units;
            extents += usize::from(!merges);
            last_end = Some(end);
            reached = distance(run.start) + u64::from(run.count);
            cursor = end;
        }
        plan
    }

    /// Claim `plan` in the table and append it to the temp's extents;
    /// `None` if the extents overflow, which `plan` rules out.
    fn apply(&mut self, plan: &Plan) -> Option<()> {
        for &run in &plan.runs {
            self.table.claim(run);
            match self.temp.extents.last_mut() {
                Some(last) if last.end() == Some(run.start) => {
                    last.count = last.count.checked_add(run.count)?
                }
                _ => self.temp.extents.push(run).ok()?,
            }
            self.temp.owned = self.temp.owned.checked_add(run.count)?;
        }
        Some(())
    }

    /// Claim `plan` and encode the temp record; returns the pages to commit
    /// and the record length. Synchronous so the 4 KiB plan never lives in a
    /// future: the owner future is built on core1's stack (t-121).
    fn stage(&mut self, plan: Plan) -> Result<(Dirty, usize), UnitError> {
        // `plan` checked extent room, so this cannot fail; if it did, RAM
        // is suspect and the store disables.
        self.apply(&plan)
            .ok_or(UnitError::Catalogue(Error::Corrupt))?;
        let n = self
            .encode_temp(RecordState::Temp)
            .ok_or(UnitError::Catalogue(Error::Corrupt))?;
        Ok((plan.dirty, n))
    }

    /// Own staged units in one commit, before any data reaches them.
    fn own(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        (dirty, n): (Dirty, usize),
    ) -> Result<(), UnitError> {
        self.commit(cat, &dirty, id, Some(n))
    }

    fn reset(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        expected: Option<u64>,
    ) -> Result<(), UnitError> {
        if id.0 == 0 {
            return Err(UnitError::Invalid);
        }
        let want = match expected.map(|n| n.div_ceil(UNIT_BYTES)) {
            None | Some(0) => 0,
            Some(n) => u32::try_from(n).map_err(|_| UnitError::Full)?,
        };
        // Read-only checks first: `Exists` and `Full` change nothing.
        let own = match cat
            .unit_record(id, &mut self.buf)
            .map_err(UnitError::Catalogue)?
        {
            Some(r) if r.state == RecordState::Published => return Err(UnitError::Exists),
            Some(r) => r.total_units(),
            None => 0,
        };
        let other = match self.temp.id {
            Some(other) if other != id => self.temp.owned,
            _ => 0,
        };
        let free = u64::from(self.table.free()) + u64::from(own) + u64::from(other);
        if u64::from(want) > free {
            return Err(UnitError::Full);
        }
        if let Some(other) = self.temp.id.filter(|&other| other != id) {
            self.unlink(cat, other, RecordState::Temp)?;
        }
        if own > 0 || self.temp.id == Some(id) {
            self.unlink(cat, id, RecordState::Temp)?;
        }
        self.temp.clear();
        if want > 0 {
            // Scoped so the plan is not part of this future (see `stage`).
            let staged = {
                let plan = self.plan(want);
                if plan.units < want {
                    // Free units exist but would need more than `MAX_EXTENTS`.
                    return Err(UnitError::Invalid);
                }
                self.temp.id = Some(id);
                self.stage(plan)
            };
            let result = match staged {
                Ok(staged) => self.own(cat, id, staged),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                self.temp.clear();
                return Err(error);
            }
        }
        self.temp.id = Some(id);
        Ok(())
    }

    /// Own the next batch of free units (as many as already owned, 1 to 64)
    /// in one commit, before any data reaches them.
    fn grow(&mut self, cat: &mut Catalogue, id: EpisodeId) -> Result<(), UnitError> {
        if self.table.free() == 0 {
            return Err(UnitError::Full);
        }
        // Scoped so the plan is not part of this future (see `stage`).
        let staged = {
            let plan = self.plan(self.temp.owned.clamp(1, MAX_BATCH));
            if plan.units == 0 {
                // Free units exist, but none merges and the extents are full.
                return Err(UnitError::Invalid);
            }
            self.stage(plan)?
        };
        self.own(cat, id, staged)
    }

    fn append(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        mut bytes: &[u8],
    ) -> Result<(), UnitError> {
        if bytes.is_empty() || bytes.len() > AUDIO_CHUNK || self.temp.id != Some(id) {
            return Err(UnitError::Invalid);
        }
        let end = self
            .temp
            .len
            .checked_add(bytes.len() as u64)
            .ok_or(UnitError::Invalid)?;
        // A chunk is far smaller than a unit: one batch always covers it.
        if end > self.temp.capacity() {
            self.grow(cat, id)?;
        }
        self.temp.synced = None;
        while !bytes.is_empty() {
            let at = (self.temp.len % SECTOR as u64) as usize;
            let n = bytes.len().min(SECTOR - at);
            self.temp.tail[at..at + n].copy_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if at + n == SECTOR {
                self.write(self.temp.sector(self.temp.len))?;
            }
            self.temp.len = self
                .temp
                .len
                .checked_add(n as u64)
                .ok_or(UnitError::Catalogue(Error::Corrupt))?;
        }
        Ok(())
    }

    fn sync(&mut self, id: EpisodeId) -> Result<u64, UnitError> {
        if self.temp.id != Some(id) {
            return Err(UnitError::Invalid);
        }
        if self.temp.synced == Some(self.temp.len) {
            return Ok(self.temp.len);
        }
        let at = (self.temp.len % SECTOR as u64) as usize;
        if at != 0 {
            self.temp.tail[at..].fill(0);
            self.write(self.temp.sector(self.temp.len))?;
        }
        self.device.sync().map_err(UnitError::Device)?;
        self.temp.synced = Some(self.temp.len);
        Ok(self.temp.len)
    }

    /// Drop the owned units past `keep` from the temp's extents and table.
    /// `None` if RAM is inconsistent (an extent missing or not allocated).
    fn trim(&mut self, keep: u32) -> Option<Dirty> {
        let mut dirty = Dirty::new();
        let mut excess = self.temp.owned.saturating_sub(keep);
        while excess > 0 {
            let last = self.temp.extents.last_mut()?;
            let cut = excess.min(last.count);
            last.count = last.count.checked_sub(cut)?;
            let from = last.start.checked_add(last.count)?;
            if last.count == 0 {
                self.temp.extents.pop();
            }
            for unit in from..from.checked_add(cut)? {
                if !self.table.release(unit) {
                    return None;
                }
                let page = self.table.page_of(unit);
                if let Err(at) = dirty.binary_search(&page) {
                    dirty.insert(at, page).ok()?;
                }
            }
            excess = excess.checked_sub(cut)?;
            self.temp.owned = self.temp.owned.checked_sub(cut)?;
        }
        Some(dirty)
    }

    fn publish(&mut self, cat: &mut Catalogue, id: EpisodeId) -> Result<(), UnitError> {
        if self.temp.id != Some(id) {
            return Err(
                match cat
                    .unit_record(id, &mut self.buf)
                    .map_err(UnitError::Catalogue)?
                    .map(|r| r.state)
                {
                    Some(RecordState::Published) => UnitError::Exists,
                    _ => UnitError::Invalid,
                },
            );
        }
        if self.temp.len == 0 {
            return Err(UnitError::Invalid);
        }
        self.sync(id)?;
        let keep = u32::try_from(self.temp.len.div_ceil(UNIT_BYTES))
            .map_err(|_| UnitError::Catalogue(Error::Corrupt))?;
        // Owned units always cover `len`; a failure here means RAM is suspect.
        let dirty = self
            .trim(keep)
            .ok_or(UnitError::Catalogue(Error::Corrupt))?;
        let n = self
            .encode_temp(RecordState::Published)
            .ok_or(UnitError::Catalogue(Error::Corrupt))?;
        self.commit(cat, &dirty, id, Some(n))?;
        self.temp.clear();
        Ok(())
    }

    fn published_len(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<Option<u64>, UnitError> {
        Ok(cat
            .unit_record(id, &mut self.buf)
            .map_err(UnitError::Catalogue)?
            .filter(|r| r.state == RecordState::Published)
            .map(|r| r.len))
    }
}

impl<D: SectorIo, A: Allocator + Clone> MediaStore for UnitMedia<D, A> {
    type Error = UnitError;
    /// `Invalid`, `Exists` and `Full` are detected before any change.
    fn is_fatal(error: &Self::Error) -> bool {
        !matches!(
            error,
            UnitError::Invalid | UnitError::Exists | UnitError::Full
        )
    }
    /// Always false: playback must not read units before leases exist
    /// (ADR-0020), so nothing is ever active.
    fn active(&self, _: EpisodeId) -> bool {
        false
    }
    async fn space(&mut self, _: &mut Catalogue) -> Result<Space, Self::Error> {
        self.check()?;
        let tail = self.temp.capacity().saturating_sub(self.temp.len);
        Ok(Space {
            capacity: u64::from(self.table.units()) * UNIT_BYTES,
            free: u64::from(self.table.free()) * UNIT_BYTES + tail,
        })
    }
    async fn reset_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        expected: Option<u64>,
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.reset(cat, id, expected);
        self.guard(result)
    }
    async fn append_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.append(cat, id, bytes);
        self.guard(result)
    }
    async fn sync_temp(&mut self, _: &mut Catalogue, id: EpisodeId) -> Result<u64, Self::Error> {
        self.check()?;
        let result = self.sync(id);
        self.guard(result)
    }
    async fn publish_temp(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.publish(cat, id);
        self.guard(result)
    }
    async fn unlink_temp(&mut self, cat: &mut Catalogue, id: EpisodeId) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.unlink(cat, id, RecordState::Temp);
        self.guard(result)
    }
    async fn unlink_audio(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.unlink(cat, id, RecordState::Published);
        self.guard(result)
    }
    async fn audio_len(
        &mut self,
        cat: &mut Catalogue,
        id: EpisodeId,
    ) -> Result<Option<u64>, Self::Error> {
        self.check()?;
        let result = self.published_len(cat, id);
        self.guard(result)
    }
    async fn list_ids(
        &mut self,
        cat: &mut Catalogue,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> Result<usize, Self::Error> {
        self.check()?;
        let result = cat.unit_ids(after, out).map_err(UnitError::Catalogue);
        self.guard(result)
    }
}

#[cfg(test)]
#[path = "unit_media_tests.rs"]
mod tests;
