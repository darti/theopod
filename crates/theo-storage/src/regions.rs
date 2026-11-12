//! Unique shared-sector owner: all three ranges retain one synchronous device.
//! Use only on the isolated storage core; Rc handles never cross cores.
//! No constructor performs I/O, provisioning or descriptor publication.
use crate::{
    card_layout::{CardLayout, Region},
    littlefs::SectorIo,
};
use alloc::rc::Rc;
use core::cell::RefCell;
use littlefs_rust::Error;

type Owner<D> = Rc<RefCell<Option<D>>>;
pub struct RegionIo<D> {
    owner: Owner<D>,
    region: Region,
}

/// The caller must supply a descriptor validated against actual card capacity.
/// The three returned handles (database, log, media) cannot be cloned or used to
/// construct overlapping regions.
pub fn split<D: SectorIo>(
    device: D,
    layout: CardLayout,
) -> (RegionIo<D>, RegionIo<D>, RegionIo<D>) {
    let owner = Rc::new(RefCell::new(Some(device)));
    (
        RegionIo {
            owner: owner.clone(),
            region: layout.database(),
        },
        RegionIo {
            owner: owner.clone(),
            region: layout.log(),
        },
        RegionIo {
            owner,
            region: layout.media(),
        },
    )
}
impl<D: SectorIo> RegionIo<D> {
    /// A failed call drops the shared device, disabling every region, only
    /// when the device itself failed; a soft (deadline) error fails this call
    /// alone and leaves the other regions usable (t-121).
    fn access<T>(&mut self, call: impl FnOnce(&mut D) -> Result<T, Error>) -> Result<T, Error> {
        let mut owner = self.owner.borrow_mut();
        let device = owner.as_mut().ok_or(Error::Io)?;
        let result = call(device);
        if result.is_err() && device.device_failed() {
            owner.take();
        }
        result
    }
}
impl<D: SectorIo> SectorIo for RegionIo<D> {
    fn read_sector(&mut self, relative: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        let sector = self
            .region
            .translate(relative as u64)
            .map_err(|_| Error::Invalid)?;
        self.access(|device| device.read_sector(sector, out))
    }
    fn write_sector(&mut self, relative: u32, data: &[u8; 512]) -> Result<(), Error> {
        let sector = self
            .region
            .translate(relative as u64)
            .map_err(|_| Error::Invalid)?;
        self.access(|device| device.write_sector(sector, data))
    }
    fn sync(&mut self) -> Result<(), Error> {
        self.access(|device| device.sync())
    }
    fn check(&mut self) -> Result<(), Error> {
        self.access(|device| device.check())
    }
    fn device_failed(&self) -> bool {
        self.owner
            .borrow()
            .as_ref()
            .is_none_or(SectorIo::device_failed)
    }
}

pub enum Discovered<D: SectorIo> {
    Legacy(crate::littlefs::SectorStorage<D>),
    Regions {
        database: RegionIo<D>,
        log: RegionIo<D>,
        media: RegionIo<D>,
        layout: CardLayout,
    },
}
/// Read-only discovery. Missing descriptors try legacy geometry, never format.
pub fn discover<D: SectorIo>(mut device: D, card_sectors: u64) -> Result<Discovered<D>, Error> {
    let geometry = crate::littlefs::Geometry::new(card_sectors)?;
    let (mut first, mut second) = ([0; 512], [0; 512]);
    device.read_sector(0, &mut first)?;
    device.read_sector(8, &mut second)?;
    match CardLayout::select(&first, &second, card_sectors).map_err(|_| Error::Corrupt)? {
        None => Ok(Discovered::Legacy(
            crate::littlefs::SectorStorage::try_new_in(
                device,
                geometry,
                allocator_api2::alloc::Global,
            )?,
        )),
        Some(layout) => {
            let (database, log, media) = split(device, layout);
            Ok(Discovered::Regions {
                database,
                log,
                media,
                layout,
            })
        }
    }
}

#[cfg(test)]
#[path = "regions_tests.rs"]
mod tests;
