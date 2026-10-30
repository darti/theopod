//! Disposable feasibility adapter: both ranges retain one synchronous device.
//! Use only on the isolated storage core; Rc handles never cross cores.
//! No constructor performs I/O, provisioning or descriptor publication.
use alloc::rc::Rc;
use core::cell::RefCell;
use littlefs_rust::Error;
use theo_storage::{
    card_layout::{CardLayout, Region},
    littlefs::SectorIo,
    page_store::SectorFile,
};

type Owner<D> = Rc<RefCell<Option<D>>>;
pub struct RegionIo<D> {
    owner: Owner<D>,
    region: Region,
}

/// The caller must supply a descriptor validated against actual card capacity.
/// The two returned handles cannot be cloned or used to construct overlapping regions.
pub fn split<D: SectorIo>(device: D, layout: CardLayout) -> (RegionIo<D>, RegionIo<D>) {
    let owner = Rc::new(RefCell::new(Some(device)));
    (
        RegionIo {
            owner: owner.clone(),
            region: layout.database(),
        },
        RegionIo {
            owner,
            region: layout.media(),
        },
    )
}
impl<D: SectorIo> RegionIo<D> {
    fn access<T>(&mut self, call: impl FnOnce(&mut D) -> Result<T, Error>) -> Result<T, Error> {
        let mut owner = self.owner.borrow_mut();
        let result = call(owner.as_mut().ok_or(Error::Io)?);
        if result.is_err() {
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
}

// Clearing the one owned device drops its unique sector client in firmware.
// Cancellation therefore disables media and database together, including late replies.
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
        let guard = Guard::begin(&self.io.owner)?;
        (self.before_io)()?;
        let relative = u32::try_from(relative).map_err(|_| Error::Invalid)?;
        self.io.read_sector(relative, out)?;
        embassy_futures::yield_now().await;
        guard.complete()
    }
    async fn write_sector(&mut self, relative: u64, data: &[u8; 512]) -> Result<(), Error> {
        let guard = Guard::begin(&self.io.owner)?;
        (self.before_io)()?;
        let relative = u32::try_from(relative).map_err(|_| Error::Invalid)?;
        self.io.write_sector(relative, data)?;
        embassy_futures::yield_now().await;
        guard.complete()
    }
    async fn sync(&mut self) -> Result<(), Error> {
        let guard = Guard::begin(&self.io.owner)?;
        (self.before_io)()?;
        self.io.sync()?;
        embassy_futures::yield_now().await;
        guard.complete()
    }
}
