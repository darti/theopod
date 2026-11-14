//! Whole-sector SD backing and bounded log operations for the safe LittleFS API.

use littlefs_rust::{Config, Error, Filesystem, OpenFlags, SeekFrom, Storage};

use crate::LogBuffer;
use allocator_api2::{
    alloc::{Allocator, Global},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

/// One in-flight request. Dropping its future before success permanently
/// poisons the unique storage session, including any late worker reply.
pub struct RequestGuard<'a> {
    poison: &'a AtomicBool,
    completed: bool,
}

impl<'a> RequestGuard<'a> {
    pub fn begin(poison: &'a AtomicBool) -> Result<Self, Error> {
        if poison.load(Ordering::Acquire) {
            return Err(Error::Io);
        }
        Ok(Self {
            poison,
            completed: false,
        })
    }

    pub fn complete(mut self) -> Result<(), Error> {
        if self.poison.load(Ordering::Acquire) {
            return Err(Error::Io);
        }
        self.completed = true;
        Ok(())
    }
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.poison.store(true, Ordering::Release);
        }
    }
}

pub const SECTOR_SIZE: usize = 512;
pub const BLOCK_SIZE: u32 = 4096;
const SECTORS_PER_BLOCK: u32 = BLOCK_SIZE / SECTOR_SIZE as u32;

/// Mount an existing volume and prepare it for bounded foreground writes.
///
/// Never formats the medium. See [`prepare_for_writes`] for what preparation
/// costs and why it runs here rather than on the first append.
pub fn mount_for_writes<S: Storage>(
    storage: S,
    geometry: Geometry,
) -> Result<Filesystem<S>, Error> {
    let fs = Filesystem::mount(storage, geometry.config()).map_err(|(error, _)| error)?;
    prepare_for_writes(&fs)?;
    Ok(fs)
}

/// Run LittleFS's deferred startup maintenance before publishing the volume.
///
/// LittleFS otherwise defers its consistency pass and first free-block scan
/// to the first allocation, charging them to a 500 ms foreground append. Both
/// run here instead, under the 2 s mount budget. The consistency pass may
/// write (orphan and pending-move repair); it never formats. The free-block
/// scan reads every file's block list, so its cost grows with stored data:
/// it is paid once per mount here, and again whenever the lookahead window is
/// exhausted (see [`Geometry::config`]). A failure disables the session like
/// any other storage error.
pub fn prepare_for_writes<S: Storage>(fs: &Filesystem<S>) -> Result<(), Error> {
    fs.gc()
}

/// Explicit provisioning only: a valid existing LittleFS is preserved. An I/O
/// failure never becomes permission to format. Normal boot must use
/// [`mount_for_writes`]; callers prepare a provisioned volume the same way.
pub fn provision_once<S: Storage>(
    storage: S,
    geometry: Geometry,
) -> Result<(Filesystem<S>, bool), Error> {
    match Filesystem::mount(storage, geometry.config()) {
        Ok(fs) => Ok((fs, false)),
        Err((Error::Corrupt, mut storage)) => {
            Filesystem::format(&mut storage, &geometry.config())?;
            Filesystem::mount(storage, geometry.config())
                .map(|fs| (fs, true))
                .map_err(|(error, _)| error)
        }
        Err((error, _)) => Err(error),
    }
}

#[derive(Clone, Copy)]
pub struct Geometry {
    blocks: u32,
}

impl Geometry {
    pub fn new(sectors: u64) -> Result<Self, Error> {
        if !(16..=u32::MAX as u64).contains(&sectors) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            blocks: (sectors / SECTORS_PER_BLOCK as u64) as u32,
        })
    }

    pub fn block_count(self) -> u32 {
        self.blocks
    }

    pub fn config(self) -> Config {
        let mut config = Config::new(BLOCK_SIZE, self.blocks);
        config.read_size = SECTOR_SIZE as u32;
        config.prog_size = SECTOR_SIZE as u32;
        config.cache_size = BLOCK_SIZE;
        config.lookahead_size = 128;
        // The SD controller already performs physical wear leveling.
        config.block_cycles = -1;
        config
    }
}

/// Synchronous sector contract. Implementations must wait for card completion
/// and impose their own operation deadline. A failed write may damage its
/// sector, but must not affect other sectors; sync orders completed writes.
///
/// Implement this trait to back [`SectorStorage`] with a block device.
pub trait SectorIo {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; SECTOR_SIZE]) -> Result<(), Error>;
    fn write_sector(&mut self, sector: u32, data: &[u8; SECTOR_SIZE]) -> Result<(), Error>;
    /// Writes whole consecutive sectors from `first`, in order.
    ///
    /// `data` must be a nonzero number of whole sectors, else `Invalid`
    /// before any I/O. The default writes them one by one; a device that
    /// can write several sectors in one command (SD CMD25) overrides it.
    /// A failure may leave any prefix of the batch written.
    fn write_sectors(&mut self, first: u32, data: &[u8]) -> Result<(), Error> {
        whole_sectors(data)?;
        for (i, sector) in data.as_chunks::<SECTOR_SIZE>().0.iter().enumerate() {
            self.write_sector(first.checked_add(i as u32).ok_or(Error::Invalid)?, sector)?;
        }
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error>;
    /// Fail without device I/O once the current operation may not proceed.
    ///
    /// [`SectorStorage`] calls this before serving a cached sector, so cache
    /// hits stay bounded by the same deadline and disable state as real I/O.
    /// Devices without deadlines return `Ok(())`; wrappers must forward it.
    fn check(&mut self) -> Result<(), Error>;
    /// True when the device itself is unusable (not merely past a deadline).
    /// Wrappers that share a device use it to decide whether to drop it.
    fn device_failed(&self) -> bool {
        true
    }
}

/// Sectors in `data`, which must be a nonzero number of whole sectors.
pub fn whole_sectors(data: &[u8]) -> Result<u32, Error> {
    if data.is_empty() || !data.len().is_multiple_of(SECTOR_SIZE) {
        return Err(Error::Invalid);
    }
    u32::try_from(data.len() / SECTOR_SIZE).map_err(|_| Error::Invalid)
}

// Enough for two metadata blocks plus a file tail and verification reads.
// Read-through only: write data must never satisfy LittleFS's media readback.
const CACHED_SECTORS: usize = 32;
const _: () = assert!(CACHED_SECTORS > 0);

struct CachedSector {
    sector: Option<u32>,
    used: u64,
    bytes: [u8; SECTOR_SIZE],
}

/// Bytes the read cache reserves: 32 x 528 = 16,896 (ADR-0019).
pub const READ_CACHE_BYTES: usize = CACHED_SECTORS * core::mem::size_of::<CachedSector>();

/// LittleFS [`Storage`] over whole SD sectors, with a bounded read cache.
///
/// The adapter must own its sectors exclusively for the life of the mount:
/// cached bytes are never revalidated against other writers. Any failed
/// device call disables it permanently, cache hits included. The cache is
/// read-through only (writes and erases invalidate, never fill it), so
/// LittleFS's post-write readback always reaches the medium.
pub struct SectorStorage<D, A: Allocator = Global> {
    device: D,
    geometry: Geometry,
    enabled: bool,
    cache: Vec<CachedSector, A>,
    clock: u64,
}

impl<D: SectorIo> SectorStorage<D> {
    /// Host and tooling constructor on the global heap.
    ///
    /// # Panics
    ///
    /// If the read cache cannot be reserved; firmware uses [`Self::try_new_in`].
    pub fn new(device: D, geometry: Geometry) -> Self {
        Self::try_new_in(device, geometry, Global).expect("sector read cache allocation")
    }
}

impl<D: SectorIo, A: Allocator> SectorStorage<D, A> {
    /// Reserve the bounded read cache in the caller's memory domain.
    /// Firmware uses PSRAM; a failed reservation performs no device I/O.
    pub fn try_new_in(device: D, geometry: Geometry, allocator: A) -> Result<Self, Error> {
        let mut cache = Vec::new_in(allocator);
        cache
            .try_reserve_exact(CACHED_SECTORS)
            .map_err(|_| Error::NoMemory)?;
        Ok(Self {
            device,
            geometry,
            enabled: true,
            cache,
            clock: 0,
        })
    }

    fn invalidate(&mut self, start: u32, len: u32) {
        for entry in &mut self.cache {
            if entry
                .sector
                .is_some_and(|sector| (start..start + len).contains(&sector))
            {
                entry.sector = None;
            }
        }
    }

    fn read_sector(&mut self, sector: u32, out: &mut [u8; SECTOR_SIZE]) -> Result<(), Error> {
        self.clock = self.clock.saturating_add(1);
        let clock = self.clock;
        if let Some(index) = self
            .cache
            .iter()
            .position(|entry| entry.sector == Some(sector))
        {
            // A hit performs no device I/O, so it must still honour the
            // operation deadline: an expired request fails and disables.
            self.io(SectorIo::check)?;
            let entry = &mut self.cache[index];
            entry.used = clock;
            out.copy_from_slice(&entry.bytes);
            return Ok(());
        }
        self.io(|device| device.read_sector(sector, out))?;
        let slot = match self.cache.iter().position(|entry| entry.sector.is_none()) {
            Some(index) => index,
            None if self.cache.len() < CACHED_SECTORS => {
                // Capacity was reserved before any device I/O: no reallocation.
                self.cache.push(CachedSector {
                    sector: None,
                    used: 0,
                    bytes: [0; SECTOR_SIZE],
                });
                self.cache.len() - 1
            }
            None => self
                .cache
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used)
                .map_or(0, |(index, _)| index),
        };
        let entry = &mut self.cache[slot];
        entry.sector = Some(sector);
        entry.used = clock;
        entry.bytes.copy_from_slice(out);
        Ok(())
    }

    fn sector(&self, block: u32, offset: u32, len: usize) -> Result<u32, Error> {
        if !self.enabled {
            return Err(Error::Io);
        }
        if block >= self.geometry.blocks
            || !offset.is_multiple_of(SECTOR_SIZE as u32)
            || !len.is_multiple_of(SECTOR_SIZE)
            || (offset as u64 + len as u64) > BLOCK_SIZE as u64
        {
            return Err(Error::Invalid);
        }
        Ok(block * SECTORS_PER_BLOCK + offset / SECTOR_SIZE as u32)
    }

    fn io(&mut self, op: impl FnOnce(&mut D) -> Result<(), Error>) -> Result<(), Error> {
        if !self.enabled {
            return Err(Error::Io);
        }
        self.enabled = false;
        op(&mut self.device)?;
        self.enabled = true;
        Ok(())
    }
}

impl<D: SectorIo, A: Allocator> Storage for SectorStorage<D, A> {
    fn read(&mut self, block: u32, offset: u32, buf: &mut [u8]) -> Result<(), Error> {
        let start = self.sector(block, offset, buf.len())?;
        for (index, chunk) in buf.as_chunks_mut::<SECTOR_SIZE>().0.iter_mut().enumerate() {
            self.read_sector(start + index as u32, chunk)?;
        }
        Ok(())
    }

    fn write(&mut self, block: u32, offset: u32, data: &[u8]) -> Result<(), Error> {
        let start = self.sector(block, offset, data.len())?;
        self.invalidate(start, (data.len() / SECTOR_SIZE) as u32);
        for (index, chunk) in data.as_chunks::<SECTOR_SIZE>().0.iter().enumerate() {
            self.io(|device| device.write_sector(start + index as u32, chunk))?;
        }
        Ok(())
    }

    fn erase(&mut self, block: u32) -> Result<(), Error> {
        let start = self.sector(block, 0, BLOCK_SIZE as usize)?;
        self.invalidate(start, SECTORS_PER_BLOCK);
        for index in 0..SECTORS_PER_BLOCK {
            self.io(|device| device.write_sector(start + index, &[0xff; SECTOR_SIZE]))?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), Error> {
        self.io(SectorIo::sync)
    }
}

pub fn append_log<S: Storage, const N: usize>(
    fs: &Filesystem<S>,
    buffer: &mut LogBuffer<N>,
    name: &str,
    archive: &str,
) -> Result<(), Error> {
    if buffer.is_empty() {
        return Ok(());
    }
    let size = match fs.stat(name) {
        Ok(metadata) => metadata.size as u64,
        Err(Error::NoEntry) => 0,
        Err(error) => return Err(error),
    };
    if crate::rotation_needed(size, buffer.len()) {
        match fs.remove(archive) {
            Ok(()) | Err(Error::NoEntry) => {}
            Err(error) => return Err(error),
        }
        fs.rename(name, archive)?;
    }
    let file = fs.open(
        name,
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::APPEND,
    )?;
    let mut count = 0;
    let result = (|| {
        while count < buffer.len() {
            let written = file.write(&buffer.pending()[count..])? as usize;
            if written == 0 {
                return Err(Error::NoSpace);
            }
            count += written;
        }
        file.sync()
    })();
    let closed = file.close();
    result?;
    closed?;
    buffer.consume(count);
    Ok(())
}

#[derive(Debug)]
pub struct LogTail {
    pub size: u32,
    pub len: usize,
    pub bytes: [u8; 384],
}

pub fn log_tail<S: Storage>(fs: &Filesystem<S>, name: &str) -> Result<LogTail, Error> {
    let mut tail = LogTail {
        size: 0,
        len: 0,
        bytes: [0; 384],
    };
    tail.size = match fs.stat(name) {
        Ok(metadata) => metadata.size,
        Err(Error::NoEntry) => return Ok(tail),
        Err(error) => return Err(error),
    };
    tail.len = (tail.size as usize).min(tail.bytes.len());
    let file = fs.open(name, OpenFlags::READ)?;
    let result = (|| {
        file.seek(SeekFrom::End(-(tail.len as i32)))?;
        let mut read = 0;
        while read < tail.len {
            let count = file.read(&mut tail.bytes[read..tail.len])? as usize;
            if count == 0 {
                return Err(Error::Io);
            }
            read += count;
        }
        Ok(())
    })();
    let closed = file.close();
    result?;
    closed?;
    Ok(tail)
}
