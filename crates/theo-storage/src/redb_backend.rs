//! redb storage over one card region (ADR-0022).
//!
//! Region sectors 0 and 1 hold two copies of the length record (magic,
//! generation, logical length, CRC-32); the redb file is the rest, byte `n`
//! in region sector `LENGTH_SECTORS + n / 512`. The valid copy with the
//! higher generation is current; `set_len` writes the next generation into
//! the other copy, so a write torn by power loss leaves the current one
//! intact and the region reopens with the old or the new length.
//! A record is *fresh* (distinct magic `THEORDBF`, also covered by the CRC)
//! from `format` until the catalogue's first table-creating commit is
//! durable: only a fresh region may be reset at boot
//! ([`RegionBackend::reset_unfinished_creation`]); a record written as
//! `THEORDB1` never is (ADR-0015). Every new generation keeps the current fresh-ness except
//! [`FreshFlag::clear`], so a torn record write reopens with the old or the
//! new state, fresh-ness included.
//! Growth is not zero-filled: redb tolerates arbitrary bytes past its last
//! write (measured; the tests fill fresh sectors with `0xA5`).

use crate::{
    crc::crc32,
    littlefs::{SECTOR_SIZE, SectorIo},
};
use alloc::rc::Rc;
use core::{cell::RefCell, fmt};
use littlefs_rust::Error as DeviceError;
use redb::{StorageBackend, io};

/// Magic of a length record whose catalogue has been created.
const MAGIC: &[u8; 8] = b"THEORDB1";
/// Magic of a fresh length record: provisioned, catalogue not yet created.
const FRESH_MAGIC: &[u8; 8] = b"THEORDBF";
/// redb's file magic number: redb `=4.4.0`, `tree_store/page_store/
/// header.rs` (`MAGICNUMBER`), checked first by `TransactionalMemory::new`
/// in `page_manager.rs` ("Not a redb database: magic number mismatch").
/// redb writes it last when it creates a database, after the rest of the
/// header is durable. A redb upgrade must re-check it
/// (`redb_magic_matches_a_created_database`).
pub const REDB_MAGIC: [u8; 9] = [b'r', b'e', b'd', b'b', 0x1A, 0x0A, 0xA9, 0x0D, 0x0A];
/// Sectors before the redb file: the two copies of the length record.
pub const LENGTH_SECTORS: u32 = 2;
/// Message of the error `set_len` returns past the region; the catalogue
/// maps it to `Full`.
pub const REGION_FULL: &str = "theo: database region full";
const SECTOR: u64 = SECTOR_SIZE as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendError {
    /// The region has no room past the length records.
    Geometry,
    /// No valid length record copy: not provisioned, or damaged.
    Corrupt,
    Device(DeviceError),
}

/// redb's [`StorageBackend`] over a region device.
pub struct RegionBackend<D> {
    /// Shared with [`FreshFlag`], which outlives the move into redb.
    inner: Rc<RefCell<Inner<D>>>,
}

/// Clears a fresh region's flag once the catalogue's tables are durable.
///
/// Shares the backend's state, so it keeps the generation scheme intact
/// while redb owns the backend. Not `Send`: it stays on the backend's
/// execution context.
pub struct FreshFlag<D> {
    inner: Rc<RefCell<Inner<D>>>,
}
struct Inner<D> {
    device: D,
    capacity: u64,
    len: u64,
    /// Generation of the current length record copy.
    generation: u64,
    /// Region sector (0 or 1) holding the current copy.
    current: u32,
    /// The current copy is fresh (`FRESH_MAGIC`).
    fresh: bool,
}

// SAFETY: `RegionBackend::new` is unsafe and requires every use and the
// drop of the backend, and of any `redb::Database` built on it, to happen
// on the execution context (thread or task) that created it. Under that
// contract no `&self` reaches another thread, so the `RefCell` is never borrowed concurrently
// and `D` never moves between threads. The `Rc` count is touched only by
// the backend and its `FreshFlag`s; those are `!Send`, so they stay on the
// same context, and the contract keeps the backend there too.
unsafe impl<D> Send for RegionBackend<D> {}
// SAFETY: see `Send` above.
unsafe impl<D> Sync for RegionBackend<D> {}

impl<D> fmt::Debug for RegionBackend<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegionBackend")
    }
}

impl<D: SectorIo> RegionBackend<D> {
    /// Write an empty, fresh length record: the region holds an empty
    /// database that the first catalogue open creates.
    ///
    /// Copy 0 gets generation 1 and length 0; copy 1 is zeroed (invalid),
    /// so a copy left by an earlier use cannot outrank the new record.
    /// Destructive; explicit provisioning only (ADR-0021).
    pub fn format(device: &mut D) -> Result<(), DeviceError> {
        device.write_sector(0, &record(1, 0, true))?;
        device.write_sector(1, &[0; SECTOR_SIZE])?;
        device.sync()
    }

    /// Open a provisioned region of `sectors` sectors.
    ///
    /// # Safety
    ///
    /// The backend and any `redb::Database` built on it must be used and
    /// dropped only on the execution context (thread or task) that calls this:
    /// the backend claims `Send + Sync` for redb without being thread-safe.
    pub unsafe fn new(mut device: D, sectors: u32) -> Result<Self, BackendError> {
        if sectors <= LENGTH_SECTORS {
            return Err(BackendError::Geometry);
        }
        let capacity = u64::from(sectors - LENGTH_SECTORS) * SECTOR;
        let mut copies = [[0; SECTOR_SIZE]; LENGTH_SECTORS as usize];
        for (index, sector) in (0..).zip(copies.iter_mut()) {
            device
                .read_sector(index, sector)
                .map_err(BackendError::Device)?;
        }
        let (current, copy) = current(&copies, capacity).ok_or(BackendError::Corrupt)?;
        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                device,
                capacity,
                len: copy.len,
                generation: copy.generation,
                current,
                fresh: copy.fresh,
            })),
        })
    }

    /// Whether the region is fresh: provisioned, catalogue not yet created.
    pub fn is_fresh(&self) -> bool {
        self.inner.borrow().fresh
    }

    /// A handle that clears the fresh flag later; `None` if not fresh.
    pub fn fresh_flag(&self) -> Option<FreshFlag<D>> {
        self.is_fresh().then(|| FreshFlag {
            inner: Rc::clone(&self.inner),
        })
    }

    /// Undo an interrupted database creation: on a fresh region whose
    /// non-empty file lacks redb's magic, set the length back to 0 so redb
    /// creates the database again. Whether it reset.
    ///
    /// A region that is not fresh is never touched (ADR-0015): its file
    /// may hold data, and its missing magic stays `Corrupt`.
    pub fn reset_unfinished_creation(&self) -> Result<bool, io::Error> {
        let mut inner = self.inner.borrow_mut();
        if !inner.fresh || inner.len == 0 {
            return Ok(false);
        }
        let mut head = [0; SECTOR_SIZE];
        inner
            .device
            .read_sector(LENGTH_SECTORS, &mut head)
            .map_err(device)?;
        if is_redb_file(inner.len, &head) {
            return Ok(false);
        }
        inner.write_record(0, true)?;
        Ok(true)
    }
}

impl<D: SectorIo> FreshFlag<D> {
    /// Write the next generation, same length, no longer fresh; durable on
    /// return. Call only once the catalogue's tables are committed.
    pub fn clear(self) -> Result<(), io::Error> {
        let mut inner = self.inner.borrow_mut();
        let len = inner.len;
        inner.write_record(len, false)
    }
}

/// The current length record among the region's copies: the valid copy
/// (magic, CRC, length within `capacity`) with the highest generation.
fn current(
    copies: &[[u8; SECTOR_SIZE]; LENGTH_SECTORS as usize],
    capacity: u64,
) -> Option<(u32, Record)> {
    let mut best: Option<(u32, Record)> = None;
    for (index, sector) in (0..).zip(copies) {
        let Some(copy) = decode(sector).filter(|copy| copy.len <= capacity) else {
            continue;
        };
        if best.is_none_or(|(_, b)| copy.generation > b.generation) {
            best = Some((index, copy));
        }
    }
    best
}

/// Whether a redb file of `len` bytes starting with `head` carries redb's
/// magic number, as redb checks it on open.
pub fn is_redb_file(len: u64, head: &[u8; SECTOR_SIZE]) -> bool {
    len >= REDB_MAGIC.len() as u64 && head[..REDB_MAGIC.len()] == REDB_MAGIC
}

/// Whether a database region of `sectors` sectors is `Corrupt` as the
/// catalogue would report it, from its first `LENGTH_SECTORS + 1` sectors
/// (the length record copies, then the start of the redb file): no valid
/// length record, or a non-empty file without redb's magic. Reads no
/// device, opens no database: for the provisioning guard.
pub fn region_is_corrupt(
    head: &[[u8; SECTOR_SIZE]; LENGTH_SECTORS as usize + 1],
    sectors: u32,
) -> bool {
    let Some(capacity) = sectors
        .checked_sub(LENGTH_SECTORS)
        .filter(|&n| n > 0)
        .map(|n| u64::from(n) * SECTOR)
    else {
        return true;
    };
    let [first, second, file] = head;
    match current(&[*first, *second], capacity) {
        None => true,
        Some((_, copy)) => copy.len > 0 && !is_redb_file(copy.len, file),
    }
}

/// One decoded copy of the length record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    generation: u64,
    len: u64,
    fresh: bool,
}

/// Layout: magic (8: `THEORDB1`, or `THEORDBF` while fresh), generation
/// (8, LE), length (8, LE), CRC-32 of the preceding 24 bytes (4, LE); the
/// rest of the sector is zero.
fn record(generation: u64, len: u64, fresh: bool) -> [u8; SECTOR_SIZE] {
    let mut sector = [0; SECTOR_SIZE];
    sector[..8].copy_from_slice(if fresh { FRESH_MAGIC } else { MAGIC });
    sector[8..16].copy_from_slice(&generation.to_le_bytes());
    sector[16..24].copy_from_slice(&len.to_le_bytes());
    let crc = crc32(&sector[..24]);
    sector[24..28].copy_from_slice(&crc.to_le_bytes());
    sector
}
fn decode(sector: &[u8; SECTOR_SIZE]) -> Option<Record> {
    let crc = u32::from_le_bytes(sector[24..28].try_into().ok()?);
    let fresh = match &sector[..8] {
        magic if magic == MAGIC => false,
        magic if magic == FRESH_MAGIC => true,
        _ => return None,
    };
    if crc != crc32(&sector[..24]) {
        return None;
    }
    Some(Record {
        generation: u64::from_le_bytes(sector[8..16].try_into().ok()?),
        len: u64::from_le_bytes(sector[16..24].try_into().ok()?),
        fresh,
    })
}
fn device(_: DeviceError) -> io::Error {
    io::Error::other("theo: sector device failed")
}
fn bounds() -> io::Error {
    io::Error::other("theo: access past the database length")
}

impl<D: SectorIo> Inner<D> {
    fn check(&self, offset: u64, len: usize) -> Result<(), io::Error> {
        match offset.checked_add(len as u64) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(bounds()),
        }
    }
    fn sector(offset: u64) -> Result<u32, io::Error> {
        u32::try_from(offset / SECTOR + u64::from(LENGTH_SECTORS)).map_err(|_| bounds())
    }
    /// Write the next generation into the other copy and sync; a torn
    /// write leaves the current copy intact.
    fn write_record(&mut self, len: u64, fresh: bool) -> Result<(), io::Error> {
        let generation = self.generation.checked_add(1).ok_or_else(bounds)?;
        let target = 1 - self.current;
        self.device
            .write_sector(target, &record(generation, len, fresh))
            .map_err(device)?;
        // Durable before redb writes into the new space.
        self.device.sync().map_err(device)?;
        self.len = len;
        self.generation = generation;
        self.current = target;
        self.fresh = fresh;
        Ok(())
    }
}

impl<D: SectorIo + 'static> StorageBackend for RegionBackend<D> {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.inner.borrow().len)
    }

    fn read(&self, mut offset: u64, mut out: &mut [u8]) -> Result<(), io::Error> {
        let mut inner = self.inner.borrow_mut();
        inner.check(offset, out.len())?;
        let mut sector = [0; SECTOR_SIZE];
        while !out.is_empty() {
            let at = (offset % SECTOR) as usize;
            let n = out.len().min(SECTOR_SIZE - at);
            inner
                .device
                .read_sector(Inner::<D>::sector(offset)?, &mut sector)
                .map_err(device)?;
            out[..n].copy_from_slice(&sector[at..at + n]);
            out = &mut out[n..];
            offset += n as u64;
        }
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        let mut inner = self.inner.borrow_mut();
        if len > inner.capacity {
            return Err(io::Error::other(REGION_FULL));
        }
        let fresh = inner.fresh;
        inner.write_record(len, fresh)
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        self.inner.borrow_mut().device.sync().map_err(device)
    }

    fn write(&self, mut offset: u64, mut data: &[u8]) -> Result<(), io::Error> {
        let mut inner = self.inner.borrow_mut();
        inner.check(offset, data.len())?;
        let mut sector = [0; SECTOR_SIZE];
        while !data.is_empty() {
            let at = (offset % SECTOR) as usize;
            let n = data.len().min(SECTOR_SIZE - at);
            let index = Inner::<D>::sector(offset)?;
            if n == SECTOR_SIZE {
                sector.copy_from_slice(&data[..SECTOR_SIZE]);
            } else {
                // Edge sector: keep the bytes redb does not overwrite.
                inner
                    .device
                    .read_sector(index, &mut sector)
                    .map_err(device)?;
                sector[at..at + n].copy_from_slice(&data[..n]);
            }
            inner.device.write_sector(index, &sector).map_err(device)?;
            data = &data[n..];
            offset += n as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "redb_backend_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "redb_backend_crash_tests.rs"]
mod crash_tests;
