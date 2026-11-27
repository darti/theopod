//! The sector device contract shared by every storage layer.
//!
//! [`SectorIo`] is what a card (or a test disk) implements. It is neutral:
//! LittleFS, the redb region backend and the unit media store all sit on it.
//! The error type is [`SectorError`], currently the vendored LittleFS error
//! (ADR-0012) so device implementations need no conversion.

/// Failure of a sector operation.
pub type SectorError = littlefs_rust::Error;

/// Bytes per card sector.
pub const SECTOR_SIZE: usize = 512;

/// Synchronous sector contract. Implementations must wait for card completion
/// and impose their own operation deadline. A failed write may damage its
/// sector, but must not affect other sectors; sync orders completed writes.
///
/// Implement this trait to back [`SectorStorage`](crate::littlefs::SectorStorage) with a block device.
pub trait SectorIo {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; SECTOR_SIZE]) -> Result<(), SectorError>;
    fn write_sector(&mut self, sector: u32, data: &[u8; SECTOR_SIZE]) -> Result<(), SectorError>;
    /// Writes whole consecutive sectors from `first`, in order.
    ///
    /// `data` must be a nonzero number of whole sectors, else `Invalid`
    /// before any I/O. The default writes them one by one; a device that
    /// can write several sectors in one command (SD CMD25) overrides it.
    /// A failure may leave any prefix of the batch written.
    fn write_sectors(&mut self, first: u32, data: &[u8]) -> Result<(), SectorError> {
        whole_sectors(data)?;
        for (i, sector) in data.as_chunks::<SECTOR_SIZE>().0.iter().enumerate() {
            self.write_sector(
                first.checked_add(i as u32).ok_or(SectorError::Invalid)?,
                sector,
            )?;
        }
        Ok(())
    }
    fn sync(&mut self) -> Result<(), SectorError>;
    /// Fail without device I/O once the current operation may not proceed.
    ///
    /// [`SectorStorage`](crate::littlefs::SectorStorage) calls this before serving a cached sector, so cache
    /// hits stay bounded by the same deadline and disable state as real I/O.
    /// Devices without deadlines return `Ok(())`; wrappers must forward it.
    fn check(&mut self) -> Result<(), SectorError>;
    /// True when the device itself is unusable (not merely past a deadline).
    /// Wrappers that share a device use it to decide whether to drop it.
    fn device_failed(&self) -> bool {
        true
    }
}

/// Sectors in `data`, which must be a nonzero number of whole sectors.
pub fn whole_sectors(data: &[u8]) -> Result<u32, SectorError> {
    if data.is_empty() || !data.len().is_multiple_of(SECTOR_SIZE) {
        return Err(SectorError::Invalid);
    }
    u32::try_from(data.len() / SECTOR_SIZE).map_err(|_| SectorError::Invalid)
}
