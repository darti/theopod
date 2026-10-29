//! Whole-sector SD backing and bounded log operations for the safe LittleFS API.

use littlefs_rust::{Config, Error, Filesystem, OpenFlags, SeekFrom, Storage};

use crate::LogBuffer;
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

/// Explicit provisioning only: a valid existing LittleFS is preserved. An I/O
/// failure never becomes permission to format. Normal boot must use mount.
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
pub trait SectorIo {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; SECTOR_SIZE]) -> Result<(), Error>;
    fn write_sector(&mut self, sector: u32, data: &[u8; SECTOR_SIZE]) -> Result<(), Error>;
    fn sync(&mut self) -> Result<(), Error>;
}

pub struct SectorStorage<D> {
    device: D,
    geometry: Geometry,
    enabled: bool,
}

impl<D: SectorIo> SectorStorage<D> {
    pub fn new(device: D, geometry: Geometry) -> Self {
        Self {
            device,
            geometry,
            enabled: true,
        }
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

impl<D: SectorIo> Storage for SectorStorage<D> {
    fn read(&mut self, block: u32, offset: u32, buf: &mut [u8]) -> Result<(), Error> {
        let start = self.sector(block, offset, buf.len())?;
        for (index, chunk) in buf.as_chunks_mut::<SECTOR_SIZE>().0.iter_mut().enumerate() {
            self.io(|device| device.read_sector(start + index as u32, chunk))?;
        }
        Ok(())
    }

    fn write(&mut self, block: u32, offset: u32, data: &[u8]) -> Result<(), Error> {
        let start = self.sector(block, offset, data.len())?;
        for (index, chunk) in data.as_chunks::<SECTOR_SIZE>().0.iter().enumerate() {
            self.io(|device| device.write_sector(start + index as u32, chunk))?;
        }
        Ok(())
    }

    fn erase(&mut self, block: u32) -> Result<(), Error> {
        let start = self.sector(block, 0, BLOCK_SIZE as usize)?;
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
