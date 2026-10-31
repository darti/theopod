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

pub enum Discovered<D: SectorIo> {
    Legacy(theo_storage::littlefs::SectorStorage<D>),
    Regions(RegionIo<D>, RegionIo<D>, CardLayout),
}
/// Read-only discovery. Missing descriptors try legacy geometry, never format.
pub fn discover<D: SectorIo>(mut device: D, card_sectors: u64) -> Result<Discovered<D>, Error> {
    let geometry = theo_storage::littlefs::Geometry::new(card_sectors)?;
    let (mut first, mut second) = ([0; 512], [0; 512]);
    device.read_sector(0, &mut first)?;
    device.read_sector(8, &mut second)?;
    match CardLayout::select(&first, &second, card_sectors).map_err(|_| Error::Corrupt)? {
        None => Ok(Discovered::Legacy(
            theo_storage::littlefs::SectorStorage::new(device, geometry),
        )),
        Some(layout) => {
            let (db, media) = split(device, layout);
            Ok(Discovered::Regions(db, media, layout))
        }
    }
}

/// Host-image experiment only. Caller supplies an exclusively owned new image;
/// a nonblank descriptor/superblock prefix is rejected before the first write.
/// This is never called by normal discovery or firmware boot.
pub async fn provision_new_image<D: SectorIo>(
    mut device: D,
    layout: CardLayout,
) -> Result<(), Error> {
    for sector in theo_storage::card_layout::DESCRIPTOR_SECTORS {
        let mut header = [0; 512];
        device.read_sector(sector, &mut header)?;
        if !header.iter().all(|b| *b == 0xff) && !header.iter().all(|b| *b == 0) {
            return Err(Error::Exists);
        }
    }
    let owner = Rc::new(RefCell::new(Some(device)));
    let db = RegionIo {
        owner: owner.clone(),
        region: layout.database(),
    };
    let media = RegionIo {
        owner: owner.clone(),
        region: layout.media(),
    };
    {
        let geometry = theo_storage::littlefs::Geometry::new(layout.media().sectors() as u64)?;
        let mut storage = theo_storage::littlefs::SectorStorage::new(media, geometry);
        littlefs_rust::Filesystem::format(&mut storage, &geometry.config())?;
        let fs = littlefs_rust::Filesystem::mount(storage, geometry.config())
            .map_err(|(error, _)| error)?;
        let mut log = theo_storage::LogBuffer::<64>::new();
        log.push(b"provisioned image\n");
        theo_storage::littlefs::append_log(&fs, &mut log, "/theo.log", "/theo.1.log")?;
        fs.unmount()?;
    }
    {
        let store =
            theo_storage::page_store::PageStore::create(AsyncRegion::new(db), layout.pages())
                .await
                .map_err(|_| Error::Io)?;
        let db = ekv::Database::<_, embassy_sync_06::blocking_mutex::raw::NoopRawMutex>::new(
            store,
            ekv::Config::default(),
        );
        db.format().await.map_err(|_| Error::Io)?;
        let mut tx = db.write_transaction().await;
        tx.write(b"schema", &1u32.to_le_bytes())
            .await
            .map_err(|_| Error::Io)?;
        tx.commit().await.map_err(|_| Error::Io)?;
    }
    // All filesystem/DB handles are gone and both regions were synced. Only
    // the unique owner remains; no other future can write during publication.
    assert_eq!(Rc::strong_count(&owner), 1);
    let mut owner = owner.borrow_mut();
    let device = owner.as_mut().ok_or(Error::Io)?;
    let header = layout.encode();
    for sector in theo_storage::card_layout::DESCRIPTOR_SECTORS {
        device.write_sector(sector, &header)?;
        device.sync()?;
    }
    Ok(())
}
