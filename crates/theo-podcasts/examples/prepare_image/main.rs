//! Host-only creation of a new podcast SD image; never opens existing media.
use littlefs_rust::Error;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use theo_storage::littlefs::SectorIo;
struct ImageIo(File);
impl ImageIo {
    #[cfg(test)]
    fn open(path: &Path) -> std::io::Result<Self> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(Self)
    }
}
impl SectorIo for ImageIo {
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
    /// `provision_v2` syncs only at its barriers, so this stays cheap.
    fn sync(&mut self) -> Result<(), Error> {
        self.0.sync_all().map_err(|_| Error::Io)
    }
    fn check(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
fn create_image(path: &Path, sectors: u64) -> Result<(), String> {
    use theo_storage::card_layout::CardLayout;
    // Refuse a capacity the layout cannot hold before creating any file.
    CardLayout::new(sectors, theo_podcasts::provision::PAGES).map_err(report)?;
    // O_EXCL refuses an existing file, symlink or raw-device path. This tool
    // only creates a new regular image; copying it to hardware is separate.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(report)?;
    file.set_len(sectors * 512).map_err(report)?;
    let mut image = ImageIo(file);
    futures::executor::block_on(theo_podcasts::provision::provision_v2(
        &mut image,
        sectors,
        allocator_api2::alloc::Global,
    ))
    .map(drop)
    .map_err(report)
}
fn report(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
fn run(args: &[String]) -> Result<(), String> {
    if args.len() != 4 || args[0] != "--create-image" || args[2] != "--sectors" {
        return Err(
            "usage: prepare_image --create-image NEW_IMAGE_PATH --sectors EXACT_CARD_SECTORS"
                .into(),
        );
    }
    let sectors = args[3].parse().map_err(report)?;
    create_image(Path::new(&args[1]), sectors)?;
    println!(
        "Created podcast image {} ({} sectors); existing media was not modified.",
        args[1], sectors
    );
    Ok(())
}
fn main() {
    if let Err(error) = run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
#[path = "prepare_image_tests.rs"]
mod tests;
