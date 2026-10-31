//! Checked, versioned SD region descriptors. Parsing never writes or formats media.
//!
//! Two immutable descriptor copies occupy sectors 0 and 8. Provisioning must
//! initialize/sync both regions before publishing either descriptor. Missing
//! descriptors only permit trying the legacy whole-card LittleFS mount; they
//! never authorize creation, migration, or formatting (ADR-0015).

use crate::page_store::{crc32, file_len};

pub const DESCRIPTOR_SECTORS: [u32; 2] = [0, 8];
const DATABASE_START: u32 = 16;
const MIN_MEDIA_SECTORS: u32 = 65536; // 32 MiB; the download reserve is separate.
const MAGIC: &[u8; 8] = b"THEOSD01";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    Geometry,
    Version,
    Checksum,
    Conflict,
}

/// A bounded sector range, constructible only through a checked layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    start: u32,
    sectors: u32,
}

impl Region {
    pub const fn start(self) -> u32 {
        self.start
    }
    pub const fn sectors(self) -> u32 {
        self.sectors
    }
    pub const fn byte_len(self) -> u64 {
        self.sectors as u64 * 512
    }
    pub fn translate(self, relative_sector: u64) -> Result<u32, LayoutError> {
        if relative_sector >= self.sectors as u64 {
            return Err(LayoutError::Geometry);
        }
        self.start
            .checked_add(relative_sector as u32)
            .ok_or(LayoutError::Geometry)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardLayout {
    card_sectors: u64,
    pages: u32,
    database: Region,
    media: Region,
}

impl CardLayout {
    pub fn new(card_sectors: u64, pages: usize) -> Result<Self, LayoutError> {
        if card_sectors > u32::MAX as u64 || !(32..=4096).contains(&pages) {
            return Err(LayoutError::Geometry);
        }
        let database_sectors = file_len(pages) / 512;
        let media_start = (DATABASE_START as u64 + database_sectors).next_multiple_of(8);
        let media_sectors = card_sectors
            .checked_sub(media_start)
            .ok_or(LayoutError::Geometry)?
            / 8
            * 8;
        if media_sectors < MIN_MEDIA_SECTORS as u64 {
            return Err(LayoutError::Geometry);
        }
        Ok(Self {
            card_sectors,
            pages: pages as u32,
            database: Region {
                start: DATABASE_START,
                sectors: database_sectors as u32,
            },
            media: Region {
                start: media_start as u32,
                sectors: media_sectors as u32,
            },
        })
    }
    pub const fn database(self) -> Region {
        self.database
    }
    pub const fn media(self) -> Region {
        self.media
    }
    pub const fn pages(self) -> usize {
        self.pages as usize
    }

    /// Canonical immutable descriptor; encoding does not publish or provision it.
    pub fn encode(self) -> [u8; 512] {
        let mut bytes = [0; 512];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&512u32.to_le_bytes());
        for (offset, value) in [
            (16, self.card_sectors),
            (24, self.database.start as u64),
            (32, self.database.sectors as u64),
            (40, self.media.start as u64),
            (48, self.media.sectors as u64),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[56..60].copy_from_slice(&self.pages.to_le_bytes());
        checksum(&mut bytes);
        bytes
    }

    fn decode(bytes: &[u8; 512], card_sectors: u64) -> Result<Option<Self>, LayoutError> {
        if &bytes[..8] != MAGIC {
            return Ok(None);
        }
        if u32::from_le_bytes(bytes[508..].try_into().unwrap()) != crc32(&bytes[..508]) {
            return Err(LayoutError::Checksum);
        }
        if bytes[8..12] != 1u32.to_le_bytes() {
            return Err(LayoutError::Version);
        }
        let pages = u32::from_le_bytes(bytes[56..60].try_into().unwrap()) as usize;
        let layout = Self::new(card_sectors, pages)?;
        // Exact canonical encoding also checks card size, sector size, offsets,
        // capacities and every reserved byte. No arbitrary overlapping ranges.
        if layout.encode() != *bytes {
            return Err(LayoutError::Geometry);
        }
        Ok(Some(layout))
    }

    /// None means descriptor absent: try legacy mount without formatting.
    /// A valid copy survives a torn copy. Unknown versions/geometries or two
    /// conflicting valid copies must stop region mounting rather than guess.
    pub fn select(
        first: &[u8; 512],
        second: &[u8; 512],
        card_sectors: u64,
    ) -> Result<Option<Self>, LayoutError> {
        let first = Self::decode(first, card_sectors);
        let second = Self::decode(second, card_sectors);
        for result in [&first, &second] {
            if let Err(
                error @ (LayoutError::Version | LayoutError::Geometry | LayoutError::Conflict),
            ) = result
            {
                return Err(*error);
            }
        }
        match (first, second) {
            (Ok(Some(a)), Ok(Some(b))) if a != b => Err(LayoutError::Conflict),
            (Ok(Some(layout)), _) | (_, Ok(Some(layout))) => Ok(Some(layout)),
            (Ok(None), Ok(None)) => Ok(None),
            _ => Err(LayoutError::Checksum),
        }
    }
}

fn checksum(bytes: &mut [u8; 512]) {
    let crc = crc32(&bytes[..508]);
    bytes[508..].copy_from_slice(&crc.to_le_bytes());
}

#[cfg(test)]
#[path = "card_layout_tests.rs"]
mod tests;
