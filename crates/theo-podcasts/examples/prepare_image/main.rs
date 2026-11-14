//! Host-only creation of a new podcast SD image; never opens existing media.
use littlefs_rust::Error;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use theo_storage::littlefs::SectorIo;
struct ImageIo(File, bool);
impl ImageIo {
    #[cfg(test)]
    fn open(path: &Path) -> std::io::Result<Self> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(|file| Self(file, true))
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
    fn sync(&mut self) -> Result<(), Error> {
        if self.1 {
            self.0.sync_all().map_err(|_| Error::Io)
        } else {
            Ok(())
        }
    }
    fn check(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
fn create_image(path: &Path, sectors: u64) -> Result<(), String> {
    use theo_storage::{
        card_layout::{CardLayout, DESCRIPTOR_SECTORS},
        littlefs::{Geometry, SectorStorage},
        page_store::PageStore,
        regions::{AsyncRegion, split},
    };
    let layout = CardLayout::new(sectors, 4096).map_err(report)?;
    // O_EXCL refuses an existing file, symlink or raw-device path. This tool
    // only creates a new regular image; copying it to hardware is separate.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(report)?;
    file.set_len(sectors * 512).map_err(report)?;
    // Initialization is unpublished: defer physical flushes until every region
    // is ready. The single sync before descriptor publication flushes all of
    // this inode's writes. A reset before then cannot discover the new layout.
    let (db, log, _media) = split(ImageIo(file.try_clone().map_err(report)?, false), layout);
    let geometry = Geometry::new(layout.log().sectors() as u64).map_err(report)?;
    let mut storage = SectorStorage::new(log, geometry);
    littlefs_rust::Filesystem::format(&mut storage, &geometry.config()).map_err(report)?;
    let fs =
        littlefs_rust::Filesystem::mount(storage, geometry.config()).map_err(|(e, _)| report(e))?;
    fs.open(
        "/theo.log",
        littlefs_rust::OpenFlags::WRITE | littlefs_rust::OpenFlags::CREATE,
    )
    .map_err(report)?
    .close()
    .map_err(report)?;
    fs.unmount().map_err(report)?;
    futures::executor::block_on(async {
        let store = PageStore::create(AsyncRegion::new(db), layout.pages())
            .await
            .map_err(report)?;
        let catalogue = theo_podcasts::catalogue::Catalogue::create(store)
            .await
            .map_err(report)?;
        drop(catalogue);
        Ok::<_, String>(())
    })?;
    let mut publisher = ImageIo(file, true);
    // Both regions and catalogue schema are durable before either descriptor
    // makes the layout discoverable. A failed preparation leaves no descriptor.
    publisher.sync().map_err(report)?;
    for sector in DESCRIPTOR_SECTORS {
        publisher
            .write_sector(sector, &layout.encode())
            .map_err(report)?;
        publisher.sync().map_err(report)?;
    }
    Ok(())
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
