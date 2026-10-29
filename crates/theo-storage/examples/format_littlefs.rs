//! Explicit initial provisioning, using exactly the firmware's geometry.
//! Never invoked at firmware boot. Open only an existing unmounted device/image.

use littlefs_rust::{Error, Filesystem};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
};
use theo_storage::littlefs::{Geometry, SectorIo, SectorStorage};

struct HostDisk(File);
impl SectorIo for HostDisk {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        self.0
            .seek(SeekFrom::Start(sector as u64 * 512))
            .map_err(|_| Error::Io)?;
        self.0.read_exact(out).map_err(|_| Error::Io)
    }
    fn write_sector(&mut self, sector: u32, bytes: &[u8; 512]) -> Result<(), Error> {
        self.0
            .seek(SeekFrom::Start(sector as u64 * 512))
            .map_err(|_| Error::Io)?;
        self.0.write_all(bytes).map_err(|_| Error::Io)
    }
    fn sync(&mut self) -> Result<(), Error> {
        self.0.sync_all().map_err(|_| Error::Io)
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !matches!(args.len(), 2 | 4)
        || args[0] != "--format"
        || (args.len() == 4 && args[2] != "--sectors")
    {
        return Err("usage: format_littlefs --format EXISTING_PATH [--sectors EXACT_CARD_SECTORS]\nDestructively replaces the filesystem; unmount and back up the device first.".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&args[1])
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let sectors = if args.len() == 4 {
        args[3].parse::<u64>().map_err(|e| e.to_string())?
    } else if metadata.is_file() && metadata.len().is_multiple_of(512) {
        metadata.len() / 512
    } else {
        return Err(
            "raw devices require --sectors with the exact card capacity in 512-byte sectors".into(),
        );
    };
    let geometry = Geometry::new(sectors).map_err(|e| e.to_string())?;
    if metadata.is_file() && metadata.len() != sectors * 512 {
        return Err("image length must equal the supplied sector count times 512".into());
    }
    let mut disk = HostDisk(file);
    // Probe the final sector before the first destructive operation.
    disk.read_sector((sectors - 1) as u32, &mut [0; 512])
        .map_err(|e| e.to_string())?;
    let mut storage = SectorStorage::new(disk, geometry);
    Filesystem::format(&mut storage, &geometry.config()).map_err(|e| e.to_string())?;
    let fs = Filesystem::mount(storage, geometry.config()).map_err(|(e, _)| e.to_string())?;
    fs.unmount().map_err(|e| e.to_string())?;
    println!(
        "Formatted {}: {} LittleFS blocks of 4096 bytes ({} SD sectors)",
        args[1],
        geometry.block_count(),
        sectors
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
