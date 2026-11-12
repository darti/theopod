//! redb storage over one card region (ADR-0022).
//!
//! Sector 0 of the region holds the length record (magic, logical length,
//! CRC); the redb file is the rest, byte `n` in region sector `1 + n / 512`.
//! Growth is not zero-filled: redb tolerates arbitrary bytes past its last
//! write (measured; the tests fill fresh sectors with `0xA5`).

use crate::{
    crc::crc32,
    littlefs::{SECTOR_SIZE, SectorIo},
};
use core::{cell::RefCell, fmt};
use littlefs_rust::Error as DeviceError;
use redb::{StorageBackend, io};

const MAGIC: &[u8; 8] = b"THEORDB1";
/// Sectors before the redb file: the length record.
pub const LENGTH_SECTORS: u32 = 1;
/// Message of the error `set_len` returns past the region; the catalogue
/// maps it to `Full`.
pub const REGION_FULL: &str = "theo: database region full";
const SECTOR: u64 = SECTOR_SIZE as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendError {
    /// The region is smaller than the length record.
    Geometry,
    /// No valid length record: not provisioned, or damaged.
    Corrupt,
    Device(DeviceError),
}

/// redb's [`StorageBackend`] over a region device.
pub struct RegionBackend<D> {
    inner: RefCell<Inner<D>>,
}
struct Inner<D> {
    device: D,
    capacity: u64,
    len: u64,
}

// SAFETY: `RegionBackend::new` is unsafe and requires every use and the
// drop of the backend, and of any `redb::Database` built on it, to happen
// on the thread (core) that created it. Under that contract no `&self`
// reaches another thread, so the `RefCell` is never borrowed concurrently
// and `D` never moves between threads.
unsafe impl<D> Send for RegionBackend<D> {}
// SAFETY: see `Send` above.
unsafe impl<D> Sync for RegionBackend<D> {}

impl<D> fmt::Debug for RegionBackend<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegionBackend")
    }
}

impl<D: SectorIo> RegionBackend<D> {
    /// Write an empty length record: the region holds an empty database.
    ///
    /// Destructive; explicit provisioning only (ADR-0021).
    pub fn format(device: &mut D) -> Result<(), DeviceError> {
        device.write_sector(0, &record(0))?;
        device.sync()
    }

    /// Open a provisioned region of `sectors` sectors.
    ///
    /// # Safety
    ///
    /// The backend and any `redb::Database` built on it must be used and
    /// dropped only on the thread (core) that calls this: the backend claims
    /// `Send + Sync` for redb without being thread-safe.
    pub unsafe fn new(mut device: D, sectors: u32) -> Result<Self, BackendError> {
        if sectors <= LENGTH_SECTORS {
            return Err(BackendError::Geometry);
        }
        let capacity = u64::from(sectors - LENGTH_SECTORS) * SECTOR;
        let mut sector = [0; SECTOR_SIZE];
        device
            .read_sector(0, &mut sector)
            .map_err(BackendError::Device)?;
        let len = decode(&sector)
            .filter(|&len| len <= capacity)
            .ok_or(BackendError::Corrupt)?;
        Ok(Self {
            inner: RefCell::new(Inner {
                device,
                capacity,
                len,
            }),
        })
    }
}

fn record(len: u64) -> [u8; SECTOR_SIZE] {
    let mut sector = [0; SECTOR_SIZE];
    sector[..8].copy_from_slice(MAGIC);
    sector[8..16].copy_from_slice(&len.to_le_bytes());
    let crc = crc32(&sector[..16]);
    sector[16..20].copy_from_slice(&crc.to_le_bytes());
    sector
}
fn decode(sector: &[u8; SECTOR_SIZE]) -> Option<u64> {
    let crc = u32::from_le_bytes(sector[16..20].try_into().ok()?);
    if sector[..8] != *MAGIC || crc != crc32(&sector[..16]) {
        return None;
    }
    Some(u64::from_le_bytes(sector[8..16].try_into().ok()?))
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
        // Durable before redb writes into the new space.
        inner.device.write_sector(0, &record(len)).map_err(device)?;
        inner.device.sync().map_err(device)?;
        inner.len = len;
        Ok(())
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
