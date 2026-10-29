//! Fixed-length async FAT file transport for the EKV page store.
//!
//! The filesystem stays exclusively borrowed for the lifetime of this handle.
//! Close it before handing the mount back to its owner. Any error/cancellation
//! requires discarding the mount, following the firmware storage policy.

use hadris_fat::embedded::{File, async_::Fat};
use hadris_fs::OpenOptions;
use hadris_io::{ErrorKind, SeekFrom};
use hadris_storage::async_::BlockDevice;

use crate::page_store::SectorFile;

pub struct FatPageFile<'a, 'mount, D, const FILES: usize> {
    volume: &'a mut Fat<'mount, D, FILES>,
    file: File<'mount>,
    len: u64,
}

impl<'a, 'mount, D: BlockDevice, const FILES: usize> FatPageFile<'a, 'mount, D, FILES> {
    /// Open an existing file, never create or truncate it. Allocation belongs to
    /// a separate initialization operation, before publishing the database.
    pub async fn open(
        volume: &'a mut Fat<'mount, D, FILES>,
        name: &str,
    ) -> Result<Self, ErrorKind> {
        let file = volume
            .open(volume.root(), name, OpenOptions::new().read().write())
            .await
            .map_err(|e| e.kind())?;
        let len = match volume.stat(&file).await {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                let kind = error.kind();
                let _ = volume.close(file).await;
                return Err(kind);
            }
        };
        Ok(Self { volume, file, len })
    }

    pub async fn close(self) -> Result<(), ErrorKind> {
        self.volume.close(self.file).await.map_err(|e| e.kind())?;
        self.volume.sync().await.map_err(|e| e.kind())
    }

    fn seek(&mut self, sector: u64) -> Result<(), ErrorKind> {
        let start = sector.checked_mul(512).ok_or(ErrorKind::InvalidInput)?;
        if start.checked_add(512).is_none_or(|end| end > self.len) {
            return Err(ErrorKind::InvalidInput);
        }
        self.volume
            .seek(&self.file, SeekFrom::Start(start))
            .map_err(|e| e.kind())?;
        Ok(())
    }
}

impl<D: BlockDevice, const FILES: usize> SectorFile for FatPageFile<'_, '_, D, FILES> {
    type Error = ErrorKind;
    fn byte_len(&self) -> u64 {
        self.len
    }
    async fn read_sector(&mut self, sector: u64, bytes: &mut [u8; 512]) -> Result<(), ErrorKind> {
        self.seek(sector)?;
        let mut read = 0;
        while read < bytes.len() {
            let n = self
                .volume
                .read(&self.file, &mut bytes[read..])
                .await
                .map_err(|e| e.kind())?;
            if n == 0 {
                return Err(ErrorKind::Corrupt);
            }
            read += n;
        }
        Ok(())
    }
    async fn write_sector(&mut self, sector: u64, bytes: &[u8; 512]) -> Result<(), ErrorKind> {
        self.seek(sector)?;
        if self
            .volume
            .write(&self.file, bytes)
            .await
            .map_err(|e| e.kind())?
            != bytes.len()
        {
            return Err(ErrorKind::NoSpace);
        }
        Ok(())
    }
    async fn sync(&mut self) -> Result<(), ErrorKind> {
        self.volume.sync().await.map_err(|e| e.kind())
    }
}

#[cfg(test)]
#[path = "fat_page_file_tests.rs"]
mod fat_page_file_tests;
