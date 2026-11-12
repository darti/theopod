//! Destructive initialization of a v3 card layout (ADR-0021, ADR-0022).
//!
//! [`provision_v3`] unpublishes the old contents, formats the log volume,
//! writes an empty database length record, and only then publishes both
//! descriptors. The first catalogue open creates the tables. An interrupted
//! run leaves a blank card: no descriptor and no mountable whole-card
//! LittleFS. The host `prepare_image` tool and the explicit
//! `sd-provision-v3` firmware build both call it; [`Target`] decides which
//! card the firmware may erase. Nothing else may call it.
use allocator_api2::alloc::Allocator;
use core::fmt;
use littlefs_rust::{Error, Filesystem, OpenFlags};
use theo_storage::{
    card_layout::{CardLayout, DESCRIPTOR_SECTORS, LayoutError},
    littlefs::{Geometry, SectorIo, SectorStorage},
    redb_backend::RegionBackend,
    regions::split,
};

pub use crate::provision_guard::{CardId, CatalogueHead, Refusal, Request, Target, parse_request};

/// Why provisioning stopped; no descriptor was published.
#[derive(Debug)]
pub enum ProvisionError {
    /// The card capacity cannot hold a v3 layout; nothing was written.
    Layout(LayoutError),
    /// Clearing the old descriptor sectors failed.
    Unpublish(Error),
    /// Formatting the log volume failed.
    Log(Error),
    /// Writing the database length record failed.
    Database(Error),
    /// Syncing the regions or writing a descriptor failed.
    Publish(Error),
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => write!(f, "card too small for layout v3 ({error:?})"),
            Self::Unpublish(error) => write!(f, "clearing old descriptors failed ({error:?})"),
            Self::Log(error) => write!(f, "formatting the log volume failed ({error:?})"),
            Self::Database(error) => {
                write!(f, "writing the database length record failed ({error:?})")
            }
            Self::Publish(error) => write!(f, "publishing the descriptors failed ({error:?})"),
        }
    }
}

/// Erase `device` and publish a v3 layout for a card of `card_sectors`.
///
/// Destructive: the descriptor sectors, the database length record and the
/// log volume are overwritten; the media region is left as is (units are
/// claimed through the catalogue). Buffers come from `allocator`.
/// `device.sync()` is called only at the two durability barriers below.
pub fn provision_v3<D: SectorIo, A: Allocator + Clone>(
    device: &mut D,
    card_sectors: u64,
    allocator: A,
) -> Result<CardLayout, ProvisionError> {
    let layout = CardLayout::new(card_sectors).map_err(ProvisionError::Layout)?;
    let geometry = Geometry::new(layout.log().sectors() as u64).map_err(ProvisionError::Log)?;
    // Sectors 0 and 8 also hold a whole-card LittleFS's two superblocks
    // (4 KiB blocks): clearing them first turns an interrupted run into a
    // blank card rather than a half-overwritten mountable filesystem.
    for sector in DESCRIPTOR_SECTORS {
        device
            .write_sector(sector, &[0; 512])
            .map_err(ProvisionError::Unpublish)?;
    }
    device.sync().map_err(ProvisionError::Unpublish)?;
    {
        let (mut db, log, _media) = split(Deferred(&mut *device), layout);
        format_log(log, geometry, allocator).map_err(ProvisionError::Log)?;
        // An empty database: the first catalogue open creates the tables.
        RegionBackend::format(&mut db).map_err(ProvisionError::Database)?;
    }
    // Both regions are durable before either descriptor makes them
    // discoverable; one published copy is then already a complete layout.
    device.sync().map_err(ProvisionError::Publish)?;
    for sector in DESCRIPTOR_SECTORS {
        device
            .write_sector(sector, &layout.encode())
            .map_err(ProvisionError::Publish)?;
        device.sync().map_err(ProvisionError::Publish)?;
    }
    Ok(layout)
}

fn format_log<D: SectorIo, A: Allocator>(
    log: D,
    geometry: Geometry,
    allocator: A,
) -> Result<(), Error> {
    let mut storage = SectorStorage::try_new_in(log, geometry, allocator)?;
    Filesystem::format(&mut storage, &geometry.config())?;
    let fs = Filesystem::mount(storage, geometry.config()).map_err(|(error, _)| error)?;
    fs.open("/theo.log", OpenFlags::WRITE | OpenFlags::CREATE)?
        .close()?;
    fs.unmount().map(drop)
}

/// Lends the device to the region split and defers its syncs.
///
/// Nothing can discover the regions before the descriptors, so their
/// durability needs only the one barrier before publishing, not a device
/// flush per LittleFS or length-record sync.
struct Deferred<'a, D>(&'a mut D);
impl<D: SectorIo> SectorIo for Deferred<'_, D> {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        self.0.read_sector(sector, out)
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        self.0.write_sector(sector, data)
    }
    fn sync(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn check(&mut self) -> Result<(), Error> {
        self.0.check()
    }
    fn device_failed(&self) -> bool {
        self.0.device_failed()
    }
}

#[cfg(test)]
#[path = "provision_tests.rs"]
mod tests;
