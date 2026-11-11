//! Raw allocation-unit media store (t-121, ADR-0020).
//!
//! Podcast audio lives in 1 MiB units of raw sectors; ownership is a `u`
//! record in the catalogue, committed together with the bitmap and counter
//! (`crate::units`). Crash rule: a unit is owned by a record before any data
//! sector is written to it, so a crash can leave a partially written owned
//! unit, never an unowned one, and recovery's `unlink_temp` returns it.
//!
//! Every operation's cost is bounded by one chunk and at most one catalogue
//! commit, independent of how much media is stored. Any error disables the
//! store until it is reopened (reboot), like the catalogue itself.
use crate::{
    catalogue::{Catalogue, EpisodeId, Error},
    media::{AUDIO_CHUNK, FILE_PAGE, MediaStore, Space},
    units::{
        MAX_RECORD_UNITS, PAGE_BYTES, RecordState, UNIT_BYTES, UNIT_SECTORS, UnitTable,
        encode_record,
    },
};
use allocator_api2::{alloc::Allocator, boxed::Box, vec::Vec};
use ekv::flash::Flash;
use theo_storage::littlefs::SectorIo;

const SECTOR: usize = 512;
/// Bitmap pages one unlink may dirty (64 x 32 GiB): beyond any SD card.
const MAX_DIRTY_PAGES: usize = 64;

#[derive(Debug)]
pub enum UnitError<E> {
    Catalogue(Error<E>),
    Device(littlefs_rust::Error),
    /// No free unit is left.
    Full,
    /// Bad input or state (wrong id, empty/oversize chunk or episode).
    Invalid,
    /// Published audio already exists for this id.
    Exists,
    /// A previous error disabled the store until reopen.
    Disabled,
    /// The bitmap or a buffer could not be allocated.
    Alloc,
}

/// The single in-progress temporary episode, held in RAM.
struct Temp {
    id: Option<EpisodeId>,
    len: u64,
    units: heapless::Vec<u32, MAX_RECORD_UNITS>,
    /// Bytes of the sector at `len`, written once full or at sync.
    tail: [u8; SECTOR],
}

impl Temp {
    fn clear(&mut self) {
        self.id = None;
        self.len = 0;
        self.units.clear();
    }
    /// Device sector holding byte `at` of this temp.
    fn sector(&self, at: u64) -> Option<u32> {
        let unit = *self.units.get((at / UNIT_BYTES) as usize)?;
        unit.checked_mul(UNIT_SECTORS)?
            .checked_add(((at % UNIT_BYTES) / SECTOR as u64) as u32)
    }
}

pub struct UnitMedia<D: SectorIo, A: Allocator + Clone> {
    device: D,
    pub(crate) table: UnitTable<A>,
    buf: Vec<u8, A>,
    temp: Box<Temp, A>,
    enabled: bool,
}

impl<D: SectorIo, A: Allocator + Clone> UnitMedia<D, A> {
    /// Load the unit table of `units` units from the catalogue.
    pub async fn open_in<F: Flash>(
        cat: &mut Catalogue<F>,
        device: D,
        units: u32,
        alloc: A,
    ) -> Result<Self, UnitError<F::Error>> {
        let mut table = UnitTable::new_in(units, alloc.clone()).map_err(|_| UnitError::Alloc)?;
        cat.load_units(&mut table)
            .await
            .map_err(UnitError::Catalogue)?;
        let mut buf = Vec::new_in(alloc.clone());
        buf.try_reserve_exact(PAGE_BYTES)
            .map_err(|_| UnitError::Alloc)?;
        buf.resize(PAGE_BYTES, 0);
        let temp = Box::try_new_in(
            Temp {
                id: None,
                len: 0,
                units: heapless::Vec::new(),
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
        })
    }

    #[cfg(test)]
    pub(crate) fn device(&self) -> &D {
        &self.device
    }

    fn check<E>(&self) -> Result<(), UnitError<E>> {
        if self.enabled {
            Ok(())
        } else {
            Err(UnitError::Disabled)
        }
    }

    /// Any failure disables the store: RAM state may no longer match the card.
    fn guard<T, E>(&mut self, result: Result<T, UnitError<E>>) -> Result<T, UnitError<E>> {
        if result.is_err() {
            self.enabled = false;
        }
        result
    }

    fn write<E>(&mut self, sector: Option<u32>) -> Result<(), UnitError<E>> {
        let sector = sector.ok_or(UnitError::Invalid)?;
        self.device
            .write_sector(sector, &self.temp.tail)
            .map_err(UnitError::Device)
    }

    async fn record_state<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<Option<RecordState>, UnitError<F::Error>> {
        Ok(cat
            .unit_record(id, &mut self.buf)
            .await
            .map_err(UnitError::Catalogue)?
            .map(|r| r.state))
    }

    /// Free the record of `id` if it is in `state`; idempotent when absent.
    async fn unlink<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
        state: RecordState,
    ) -> Result<(), UnitError<F::Error>> {
        if state == RecordState::Temp && self.temp.id == Some(id) {
            self.temp.clear();
        }
        let Some(record) = cat
            .unit_record(id, &mut self.buf)
            .await
            .map_err(UnitError::Catalogue)?
        else {
            return Ok(());
        };
        if record.state != state {
            return Ok(());
        }
        let mut dirty = heapless::Vec::<u16, MAX_DIRTY_PAGES>::new();
        for i in 0..record.count() {
            let unit = record.unit(i);
            if !self.table.release(unit) {
                return Err(UnitError::Catalogue(Error::Corrupt));
            }
            let page = self.table.page_of(unit);
            if let Err(at) = dirty.binary_search(&page) {
                dirty
                    .insert(at, page)
                    .map_err(|_| UnitError::Catalogue(Error::Corrupt))?;
            }
        }
        cat.commit_units(&self.table, &dirty, id, None)
            .await
            .map_err(UnitError::Catalogue)
    }

    async fn reset<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), UnitError<F::Error>> {
        if id.0 == 0 {
            return Err(UnitError::Invalid);
        }
        if let Some(other) = self.temp.id.filter(|&other| other != id) {
            self.unlink(cat, other, RecordState::Temp).await?;
        }
        match self.record_state(cat, id).await? {
            Some(RecordState::Published) => return Err(UnitError::Exists),
            Some(RecordState::Temp) => self.unlink(cat, id, RecordState::Temp).await?,
            None => (),
        }
        self.temp.clear();
        self.temp.id = Some(id);
        Ok(())
    }

    /// Own the next unit in the catalogue before any data reaches it.
    async fn grow<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), UnitError<F::Error>> {
        if self.temp.units.is_full() {
            return Err(UnitError::Invalid);
        }
        let unit = self.table.allocate().ok_or(UnitError::Full)?;
        self.temp.units.push(unit).map_err(|_| UnitError::Invalid)?;
        let n = encode_record(
            RecordState::Temp,
            self.temp.len,
            &self.temp.units,
            &mut self.buf,
        );
        cat.commit_units(
            &self.table,
            &[self.table.page_of(unit)],
            id,
            Some(&self.buf[..n]),
        )
        .await
        .map_err(UnitError::Catalogue)
    }

    async fn append<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
        mut bytes: &[u8],
    ) -> Result<(), UnitError<F::Error>> {
        if bytes.is_empty() || bytes.len() > AUDIO_CHUNK || self.temp.id != Some(id) {
            return Err(UnitError::Invalid);
        }
        while !bytes.is_empty() {
            if self.temp.len == self.temp.units.len() as u64 * UNIT_BYTES {
                self.grow(cat, id).await?;
            }
            let at = (self.temp.len % SECTOR as u64) as usize;
            let n = bytes.len().min(SECTOR - at);
            self.temp.tail[at..at + n].copy_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if at + n == SECTOR {
                self.write(self.temp.sector(self.temp.len))?;
            }
            self.temp.len += n as u64;
        }
        Ok(())
    }

    fn sync<E>(&mut self, id: EpisodeId) -> Result<u64, UnitError<E>> {
        if self.temp.id != Some(id) {
            return Err(UnitError::Invalid);
        }
        let at = (self.temp.len % SECTOR as u64) as usize;
        if at != 0 {
            self.temp.tail[at..].fill(0);
            self.write(self.temp.sector(self.temp.len))?;
        }
        self.device.sync().map_err(UnitError::Device)?;
        Ok(self.temp.len)
    }

    async fn publish<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), UnitError<F::Error>> {
        if self.temp.id != Some(id) {
            return Err(match self.record_state(cat, id).await? {
                Some(RecordState::Published) => UnitError::Exists,
                _ => UnitError::Invalid,
            });
        }
        if self.temp.len == 0 {
            return Err(UnitError::Invalid);
        }
        let len = self.sync(id)?;
        let n = encode_record(RecordState::Published, len, &self.temp.units, &mut self.buf);
        cat.commit_units(&self.table, &[], id, Some(&self.buf[..n]))
            .await
            .map_err(UnitError::Catalogue)?;
        self.temp.clear();
        Ok(())
    }

    async fn published_len<F: Flash>(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<Option<u64>, UnitError<F::Error>> {
        Ok(cat
            .unit_record(id, &mut self.buf)
            .await
            .map_err(UnitError::Catalogue)?
            .filter(|r| r.state == RecordState::Published)
            .map(|r| r.len))
    }
}

impl<F: Flash, D: SectorIo, A: Allocator + Clone> MediaStore<F> for UnitMedia<D, A> {
    type Error = UnitError<F::Error>;
    fn active(&self, _: EpisodeId) -> bool {
        false
    } // Playback/leases are not yet implemented.
    async fn space(&mut self, _: &mut Catalogue<F>) -> Result<Space, Self::Error> {
        self.check()?;
        let tail = self.temp.units.len() as u64 * UNIT_BYTES - self.temp.len;
        Ok(Space {
            capacity: u64::from(self.table.units()) * UNIT_BYTES,
            free: u64::from(self.table.free()) * UNIT_BYTES + tail,
        })
    }
    async fn reset_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.reset(cat, id).await;
        self.guard(result)
    }
    async fn append_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.append(cat, id, bytes).await;
        self.guard(result)
    }
    async fn sync_temp(&mut self, _: &mut Catalogue<F>, id: EpisodeId) -> Result<u64, Self::Error> {
        self.check()?;
        let result = self.sync(id);
        self.guard(result)
    }
    async fn publish_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.publish(cat, id).await;
        self.guard(result)
    }
    async fn unlink_temp(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.unlink(cat, id, RecordState::Temp).await;
        self.guard(result)
    }
    async fn unlink_audio(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<(), Self::Error> {
        self.check()?;
        let result = self.unlink(cat, id, RecordState::Published).await;
        self.guard(result)
    }
    async fn audio_len(
        &mut self,
        cat: &mut Catalogue<F>,
        id: EpisodeId,
    ) -> Result<Option<u64>, Self::Error> {
        self.check()?;
        let result = self.published_len(cat, id).await;
        self.guard(result)
    }
    async fn list_ids(
        &mut self,
        cat: &mut Catalogue<F>,
        after: Option<EpisodeId>,
        out: &mut [EpisodeId; FILE_PAGE],
    ) -> Result<usize, Self::Error> {
        self.check()?;
        let result = cat
            .unit_ids(after, out, &mut self.buf)
            .await
            .map_err(UnitError::Catalogue);
        self.guard(result)
    }
}

#[cfg(test)]
#[path = "unit_media_tests.rs"]
mod tests;
