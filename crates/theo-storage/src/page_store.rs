//! Sector-safe copy-on-write pages for EKV on a preallocated file.
//!
//! The backend must preserve sectors outside a failed write, and honour sync
//! ordering. Ordinary SD cards do not guarantee this on physical power loss.

use allocator_api2::{
    alloc::{Allocator, Global},
    vec::Vec,
};
use core::fmt::Debug;
use ekv::flash::{Flash, PageID};

const SECTOR: usize = 512;
const PAGE: usize = ekv::config::PAGE_SIZE;
const PAYLOAD_SECTORS: usize = PAGE / SECTOR;
const SLOT_SECTORS: usize = 1 + PAYLOAD_SECTORS;
const _: () = assert!(PAGE.is_multiple_of(SECTOR));

/// A fixed-size, preallocated file. Writes never change FAT allocation or length.
/// Each method must complete exactly one sector, and sync must order durable
/// writes. The caller retains exclusive ownership of the filesystem mount.
#[allow(async_fn_in_trait)]
pub trait SectorFile {
    type Error: Debug;
    fn byte_len(&self) -> u64;
    async fn read_sector(
        &mut self,
        sector: u64,
        data: &mut [u8; SECTOR],
    ) -> Result<(), Self::Error>;
    async fn write_sector(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), Self::Error>;
    async fn sync(&mut self) -> Result<(), Self::Error>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum PageError<E> {
    Io(E),
    Format,
    Corrupted,
    Bounds,
    NotErased,
    Disabled,
    GenerationExhausted,
    Allocation,
}

#[derive(Clone, Copy)]
struct Version {
    slot: usize,
    generation: u64,
    crc: u32,
}

pub const fn file_len(pages: usize) -> u64 {
    (1 + pages as u64 * 2 * SLOT_SECTORS as u64) * SECTOR as u64
}

/// Copy-on-write flash emulation. Drop and reopen after any failed or cancelled
/// I/O; methods mark the instance disabled before their first await.
pub struct PageStore<F, A: Allocator = Global> {
    file: F,
    versions: Vec<Option<Version>, A>,
    page: Vec<u8, A>,
    enabled: bool,
}

impl<F: SectorFile> PageStore<F> {
    pub async fn create(file: F, pages: usize) -> Result<Self, PageError<F::Error>> {
        Self::create_in(file, pages, Global).await
    }
    pub async fn open(file: F, pages: usize) -> Result<Self, PageError<F::Error>> {
        Self::open_in(file, pages, Global).await
    }
}

impl<F: SectorFile, A: Allocator + Clone> PageStore<F, A> {
    fn new_in(file: F, pages: usize, allocator: A) -> Result<Self, PageError<F::Error>> {
        if !(32..=ekv::config::MAX_PAGE_COUNT).contains(&pages)
            || file.byte_len() != file_len(pages)
        {
            return Err(PageError::Bounds);
        }
        let mut versions = Vec::new_in(allocator.clone());
        versions
            .try_reserve_exact(pages)
            .map_err(|_| PageError::Allocation)?;
        versions.resize(pages, None);
        let mut page = Vec::new_in(allocator);
        page.try_reserve_exact(PAGE)
            .map_err(|_| PageError::Allocation)?;
        page.resize(PAGE, 0);
        Ok(Self {
            file,
            versions,
            page,
            enabled: true,
        })
    }

    /// Destructive initialization, only for a newly created unpublished file.
    /// Publish the file only after this succeeds. Existing files use open.
    pub async fn create_in(
        file: F,
        pages: usize,
        allocator: A,
    ) -> Result<Self, PageError<F::Error>> {
        let mut this = Self::new_in(file, pages, allocator)?;
        this.enabled = false;
        // Invalidate the format marker before initializing slots.
        this.file
            .write_sector(0, &[0; SECTOR])
            .await
            .map_err(PageError::Io)?;
        this.file.sync().await.map_err(PageError::Io)?;
        this.page.fill(ekv::config::ERASE_VALUE);
        for page in 0..pages {
            this.file
                .write_sector(slot_sector(page, 1), &[0; SECTOR])
                .await
                .map_err(PageError::Io)?;
            this.save(
                page,
                Version {
                    slot: 0,
                    generation: 0,
                    crc: 0,
                },
            )
            .await?;
        }
        this.file
            .write_sector(0, &format_header(pages))
            .await
            .map_err(PageError::Io)?;
        this.file.sync().await.map_err(PageError::Io)?;
        this.enabled = true;
        Ok(this)
    }

    pub async fn open_in(file: F, pages: usize, allocator: A) -> Result<Self, PageError<F::Error>> {
        let mut this = Self::new_in(file, pages, allocator)?;
        let mut header = [0; SECTOR];
        this.file
            .read_sector(0, &mut header)
            .await
            .map_err(PageError::Io)?;
        if header != format_header(pages) {
            return Err(PageError::Format);
        }
        Ok(this)
    }

    pub fn into_inner(self) -> F {
        self.file
    }

    fn begin(&mut self, page: usize, offset: usize, len: usize) -> Result<(), PageError<F::Error>> {
        if !self.enabled {
            return Err(PageError::Disabled);
        }
        if page >= self.versions.len() || offset.checked_add(len).is_none_or(|end| end > PAGE) {
            return Err(PageError::Bounds);
        }
        self.enabled = false;
        Ok(())
    }

    async fn payload(&mut self, page: usize, slot: usize) -> Result<(), PageError<F::Error>> {
        for index in 0..PAYLOAD_SECTORS {
            let mut sector = [0; SECTOR];
            self.file
                .read_sector(slot_sector(page, slot) + 1 + index as u64, &mut sector)
                .await
                .map_err(PageError::Io)?;
            self.page[index * SECTOR..(index + 1) * SECTOR].copy_from_slice(&sector);
        }
        Ok(())
    }

    async fn load(&mut self, page: usize) -> Result<Version, PageError<F::Error>> {
        if let Some(version) = self.versions[page] {
            self.payload(page, version.slot).await?;
            if crc32(&self.page) != version.crc {
                return Err(PageError::Corrupted);
            }
            return Ok(version);
        }
        let mut chosen: Option<Version> = None;
        for slot in 0..2 {
            let mut header = [0; SECTOR];
            self.file
                .read_sector(slot_sector(page, slot), &mut header)
                .await
                .map_err(PageError::Io)?;
            if &header[..8] != b"THEOPG01"
                || u64_at(&header, 8) != page as u64
                || u32_at(&header, 28) != crc32(&header[..28])
            {
                continue;
            }
            let version = Version {
                slot,
                generation: u64_at(&header, 16),
                crc: u32_at(&header, 24),
            };
            self.payload(page, slot).await?;
            if crc32(&self.page) == version.crc
                && chosen.is_none_or(|old| version.generation > old.generation)
            {
                chosen = Some(version);
            }
        }
        let version = chosen.ok_or(PageError::Corrupted)?;
        self.payload(page, version.slot).await?;
        if crc32(&self.page) != version.crc {
            return Err(PageError::Corrupted);
        }
        self.versions[page] = Some(version);
        Ok(version)
    }

    async fn save(&mut self, page: usize, mut version: Version) -> Result<(), PageError<F::Error>> {
        let first = slot_sector(page, version.slot);
        // Invalidate an obsolete slot before overwriting its payload.
        self.file
            .write_sector(first, &[0; SECTOR])
            .await
            .map_err(PageError::Io)?;
        self.file.sync().await.map_err(PageError::Io)?;
        for (index, sector) in self.page.as_chunks::<SECTOR>().0.iter().enumerate() {
            self.file
                .write_sector(first + 1 + index as u64, sector)
                .await
                .map_err(PageError::Io)?;
        }
        self.file.sync().await.map_err(PageError::Io)?;
        version.crc = crc32(&self.page);
        let mut header = [0; SECTOR];
        header[..8].copy_from_slice(b"THEOPG01");
        header[8..16].copy_from_slice(&(page as u64).to_le_bytes());
        header[16..24].copy_from_slice(&version.generation.to_le_bytes());
        header[24..28].copy_from_slice(&version.crc.to_le_bytes());
        let crc = crc32(&header[..28]);
        header[28..32].copy_from_slice(&crc.to_le_bytes());
        self.file
            .write_sector(first, &header)
            .await
            .map_err(PageError::Io)?;
        self.file.sync().await.map_err(PageError::Io)?;
        self.versions[page] = Some(version);
        Ok(())
    }

    /// Actual reserved buffer payload; excludes allocator metadata and the file object.
    pub fn buffer_bytes(&self) -> usize {
        self.versions.capacity() * core::mem::size_of::<Option<Version>>() + self.page.capacity()
    }

    pub async fn read_bytes(
        &mut self,
        page: usize,
        offset: usize,
        bytes: &mut [u8],
    ) -> Result<(), PageError<F::Error>> {
        self.begin(page, offset, bytes.len())?;
        self.load(page).await?;
        bytes.copy_from_slice(&self.page[offset..offset + bytes.len()]);
        self.enabled = true;
        Ok(())
    }

    pub async fn replace(
        &mut self,
        page: usize,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), PageError<F::Error>> {
        self.begin(page, offset, bytes.len())?;
        let old = self.load(page).await?;
        if self.page[offset..offset + bytes.len()]
            .iter()
            .any(|b| *b != ekv::config::ERASE_VALUE)
        {
            return Err(PageError::NotErased);
        }
        self.page[offset..offset + bytes.len()].copy_from_slice(bytes);
        let generation = old
            .generation
            .checked_add(1)
            .ok_or(PageError::GenerationExhausted)?;
        self.save(
            page,
            Version {
                slot: 1 - old.slot,
                generation,
                crc: 0,
            },
        )
        .await?;
        self.enabled = true;
        Ok(())
    }

    async fn erase_page(&mut self, page: usize) -> Result<(), PageError<F::Error>> {
        self.begin(page, 0, PAGE)?;
        let old = self.load(page).await?;
        self.page.fill(ekv::config::ERASE_VALUE);
        let generation = old
            .generation
            .checked_add(1)
            .ok_or(PageError::GenerationExhausted)?;
        self.save(
            page,
            Version {
                slot: 1 - old.slot,
                generation,
                crc: 0,
            },
        )
        .await?;
        self.enabled = true;
        Ok(())
    }
}

impl<F: SectorFile, A: Allocator + Clone> Flash for PageStore<F, A> {
    type Error = PageError<F::Error>;
    fn page_count(&self) -> usize {
        self.versions.len()
    }
    async fn erase(&mut self, page: PageID) -> Result<(), Self::Error> {
        self.erase_page(page.index()).await
    }
    async fn read(
        &mut self,
        page: PageID,
        offset: usize,
        data: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.read_bytes(page.index(), offset, data).await
    }
    async fn write(&mut self, page: PageID, offset: usize, data: &[u8]) -> Result<(), Self::Error> {
        self.replace(page.index(), offset, data).await
    }
}

fn slot_sector(page: usize, slot: usize) -> u64 {
    1 + (page * 2 + slot) as u64 * SLOT_SECTORS as u64
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn format_header(pages: usize) -> [u8; SECTOR] {
    use ekv::config::*;
    let mut bytes = [0; SECTOR];
    bytes[..8].copy_from_slice(b"THEODB01");
    for (index, value) in [
        pages,
        PAGE_SIZE,
        ALIGN,
        MAX_PAGE_COUNT,
        MAX_CHUNK_SIZE,
        MAX_KEY_SIZE,
        MAX_VALUE_SIZE,
        BRANCHING_FACTOR,
        SCRATCH_PAGE_COUNT,
        ERASE_VALUE as usize,
        CRC as usize,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[8 + index * 8..16 + index * 8].copy_from_slice(&(value as u64).to_le_bytes());
    }
    bytes
}

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(test)]
#[path = "page_store_tests.rs"]
mod page_store_tests;
