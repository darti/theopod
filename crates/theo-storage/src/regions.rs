//! Unique shared-sector owner: all three ranges retain one synchronous device.
//! Use only on the isolated storage core; Rc handles never cross cores.
//! No constructor performs I/O, provisioning or descriptor publication.
use crate::{
    card_layout::{CardLayout, Region},
    littlefs::SectorIo,
    page_store::SectorFile,
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

// Clearing the one owned device drops its unique sector client in firmware.
// Cancellation therefore disables media and database together, including late replies.
// A sector call that returns an error is not a cancellation: it disarms the
// guard and leaves the decision to `RegionIo::access` (t-121).
struct Guard<D> {
    owner: Owner<D>,
    complete: bool,
}
impl<D> Guard<D> {
    fn begin(owner: &Owner<D>) -> Result<Self, Error> {
        if owner.borrow().is_none() {
            return Err(Error::Io);
        }
        Ok(Self {
            owner: owner.clone(),
            complete: false,
        })
    }
    fn complete(mut self) -> Result<(), Error> {
        if self.owner.borrow().is_none() {
            return Err(Error::Io);
        }
        self.complete = true;
        Ok(())
    }
    fn disarm(mut self) {
        self.complete = true;
    }
    /// Disarm on a returned error; otherwise yield, then complete.
    async fn finish(self, result: Result<(), Error>) -> Result<(), Error> {
        if let Err(error) = result {
            self.disarm();
            return Err(error);
        }
        embassy_futures::yield_now().await;
        self.complete()
    }
}
impl<D> Drop for Guard<D> {
    fn drop(&mut self) {
        if !self.complete {
            self.owner.borrow_mut().take();
        }
    }
}

pub struct AsyncRegion<D, B = fn() -> Result<(), Error>> {
    io: RegionIo<D>,
    before_io: B,
}
impl<D: SectorIo> AsyncRegion<D> {
    pub fn new(io: RegionIo<D>) -> Self {
        Self {
            io,
            before_io: || Ok(()),
        }
    }
}
impl<D: SectorIo, B: FnMut() -> Result<(), Error>> AsyncRegion<D, B> {
    /// Hook restores this request's deadline before each synchronous sector.
    /// It must not await, retain a device borrow, or extend an expired deadline.
    pub fn with_before_io(io: RegionIo<D>, before_io: B) -> Self {
        Self { io, before_io }
    }
}
impl<D: SectorIo, B: FnMut() -> Result<(), Error>> SectorFile for AsyncRegion<D, B> {
    type Error = Error;
    fn byte_len(&self) -> u64 {
        self.io.region.byte_len()
    }
    async fn read_sector(&mut self, relative: u64, out: &mut [u8; 512]) -> Result<(), Error> {
        let relative = u32::try_from(relative).map_err(|_| Error::Invalid)?;
        let guard = Guard::begin(&self.io.owner)?;
        (self.before_io)()?;
        let result = self.io.read_sector(relative, out);
        guard.finish(result).await
    }
    async fn write_sector(&mut self, relative: u64, data: &[u8; 512]) -> Result<(), Error> {
        let relative = u32::try_from(relative).map_err(|_| Error::Invalid)?;
        let guard = Guard::begin(&self.io.owner)?;
        (self.before_io)()?;
        let result = self.io.write_sector(relative, data);
        guard.finish(result).await
    }
    async fn sync(&mut self) -> Result<(), Error> {
        let guard = Guard::begin(&self.io.owner)?;
        (self.before_io)()?;
        let result = self.io.sync();
        guard.finish(result).await
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
