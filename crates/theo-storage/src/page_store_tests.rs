use super::*;
use alloc::rc::Rc;
use alloc::{vec, vec::Vec};
use core::cell::Cell;
use futures::executor::block_on;

extern crate std;
use std::io::{Read, Seek, SeekFrom, Write};

struct HostFile(std::fs::File);
impl SectorFile for HostFile {
    type Error = std::io::Error;
    fn byte_len(&self) -> u64 {
        self.0.metadata().unwrap().len()
    }
    async fn read_sector(&mut self, sector: u64, bytes: &mut [u8; 512]) -> Result<(), Self::Error> {
        self.0.seek(SeekFrom::Start(sector * 512))?;
        self.0.read_exact(bytes)
    }
    async fn write_sector(&mut self, sector: u64, bytes: &[u8; 512]) -> Result<(), Self::Error> {
        self.0.seek(SeekFrom::Start(sector * 512))?;
        self.0.write_all(bytes)
    }
    async fn sync(&mut self) -> Result<(), Self::Error> {
        self.0.sync_all()
    }
}

#[derive(Clone)]
struct Disk {
    bytes: Vec<u8>,
    remaining: Option<usize>,
    writes: Rc<Cell<usize>>,
    fail_sync: bool,
    hang_write: bool,
    durable: Vec<u8>,
    sync_remaining: Option<usize>,
}

impl Disk {
    fn new(pages: usize) -> Self {
        Self {
            bytes: vec![0; file_len(pages) as usize],
            remaining: None,
            writes: Rc::new(Cell::new(0)),
            fail_sync: false,
            hang_write: false,
            durable: vec![0; file_len(pages) as usize],
            sync_remaining: None,
        }
    }
}

impl SectorFile for Disk {
    type Error = ();
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
    async fn read_sector(&mut self, sector: u64, data: &mut [u8; 512]) -> Result<(), ()> {
        let start = sector as usize * 512;
        data.copy_from_slice(&self.bytes[start..start + 512]);
        Ok(())
    }
    async fn write_sector(&mut self, sector: u64, data: &[u8; 512]) -> Result<(), ()> {
        if self.hang_write {
            core::future::pending::<()>().await;
        }
        self.writes.set(self.writes.get() + 1);
        let start = sector as usize * 512;
        if let Some(left) = self.remaining.as_mut() {
            if *left == 0 {
                self.bytes[start..start + 217].copy_from_slice(&data[..217]);
                return Err(());
            }
            *left -= 1;
        }
        self.bytes[start..start + 512].copy_from_slice(data);
        Ok(())
    }
    async fn sync(&mut self) -> Result<(), ()> {
        if self.fail_sync {
            return Err(());
        }
        if let Some(left) = self.sync_remaining.as_mut() {
            if *left == 0 {
                return Err(());
            }
            *left -= 1;
        }
        self.durable.copy_from_slice(&self.bytes);
        Ok(())
    }
}

// Regression guard: a sector tear must not damage acknowledged bytes in the logical page.
#[test]
fn interrupted_page_replacement_keeps_previous_page() {
    block_on(async {
        let mut store = PageStore::create(Disk::new(32), 32).await.unwrap();
        store.replace(0, 0, &[1, 2, 3, 4]).await.unwrap();
        let baseline = store.into_inner();
        for cut in 0..=10 {
            let mut disk = baseline.clone();
            disk.remaining = Some(cut);
            let mut store = PageStore::open(disk, 32).await.unwrap();
            let result = store.replace(0, 4, &[5, 6, 7, 8]).await;
            let mut disk = store.into_inner();
            disk.remaining = None;
            let mut recovered = PageStore::open(disk, 32).await.unwrap();
            let mut bytes = [0; 8];
            recovered.read_bytes(0, 0, &mut bytes).await.unwrap();
            assert_eq!(&bytes[..4], &[1, 2, 3, 4], "cut {cut}");
            if result.is_ok() {
                assert_eq!(&bytes[4..], &[5, 6, 7, 8]);
            } else {
                assert!(bytes[4..] == [255; 4] || bytes[4..] == [5, 6, 7, 8]);
            }
        }
    });
}

// Regression guard: mounting incompatible data must never reformat it.
#[test]
fn wrong_schema_is_rejected_without_writes() {
    block_on(async {
        let store = PageStore::create(Disk::new(32), 32).await.unwrap();
        let mut disk = store.into_inner();
        disk.bytes[8] ^= 1;
        let counter = disk.writes.clone();
        let before = counter.get();
        assert!(matches!(
            PageStore::open(disk, 32).await,
            Err(PageError::Format)
        ));
        assert_eq!(counter.get(), before);
    });
}

// Regression guard: cancellation never permits continued use of uncertain storage state.
#[test]
fn cancelled_write_disables_instance() {
    block_on(async {
        let mut store = PageStore::create(Disk::new(32), 32).await.unwrap();
        store.file.hang_write = true;
        {
            let write = store.replace(0, 0, &[1; 4]);
            futures::pin_mut!(write);
            assert!(futures::poll!(write).is_pending());
        }
        assert_eq!(
            store.read_bytes(0, 0, &mut [0; 4]).await,
            Err(PageError::Disabled)
        );
    });
}

// Regression guard: a failed durability barrier disables the mount just like a failed write.
#[test]
fn failed_sync_disables_instance() {
    block_on(async {
        let mut store = PageStore::create(Disk::new(32), 32).await.unwrap();
        store.file.fail_sync = true;
        assert_eq!(store.replace(0, 0, &[1; 4]).await, Err(PageError::Io(())));
        assert_eq!(store.replace(0, 0, &[1; 4]).await, Err(PageError::Disabled));
    });
}

// Regression guard: losing volatile sectors at any durability barrier preserves the acknowledged page.
#[test]
fn power_loss_at_each_sync_barrier_preserves_previous_bytes() {
    block_on(async {
        let mut store = PageStore::create(Disk::new(32), 32).await.unwrap();
        store.replace(0, 0, &[1, 2, 3, 4]).await.unwrap();
        let baseline = store.into_inner();
        for barrier in 0..=3 {
            let mut disk = baseline.clone();
            disk.sync_remaining = Some(barrier);
            let mut store = PageStore::open(disk, 32).await.unwrap();
            let result = store.replace(0, 4, &[5, 6, 7, 8]).await;
            let mut disk = store.into_inner();
            disk.sync_remaining = None;
            disk.bytes.copy_from_slice(&disk.durable);
            let mut recovered = PageStore::open(disk, 32).await.unwrap();
            let mut data = [0; 8];
            recovered.read_bytes(0, 0, &mut data).await.unwrap();
            assert_eq!(&data[..4], &[1, 2, 3, 4]);
            let expected = if result.is_ok() {
                [5, 6, 7, 8]
            } else {
                [255; 4]
            };
            assert_eq!(&data[4..], &expected);
        }
    });
}

// Regression guard: erasing one logical page must preserve all other pages through slot reuse.
#[test]
fn erase_and_slot_reuse_preserve_other_pages() {
    block_on(async {
        let mut store = PageStore::create(Disk::new(32), 32).await.unwrap();
        store.replace(1, 0, &[42; 4]).await.unwrap();
        for value in 1..=4 {
            store.replace(0, 0, &[value; 4]).await.unwrap();
            store.erase_page(0).await.unwrap();
        }
        let mut recovered = PageStore::open(store.into_inner(), 32).await.unwrap();
        let mut bytes = [0; 4];
        recovered.read_bytes(0, 0, &mut bytes).await.unwrap();
        assert_eq!(bytes, [255; 4]);
        recovered.read_bytes(1, 0, &mut bytes).await.unwrap();
        assert_eq!(bytes, [42; 4]);
    });
}

// Regression guard: two bad copies must report corruption instead of silently returning erased bytes.
#[test]
fn corrupt_pages_are_not_implicitly_formatted() {
    block_on(async {
        let mut store = PageStore::create(Disk::new(32), 32).await.unwrap();
        store.replace(0, 0, &[42; 4]).await.unwrap();
        let mut disk = store.into_inner();
        disk.bytes[(slot_sector(0, 0) * 512) as usize] ^= 1;
        disk.bytes[(slot_sector(0, 1) * 512) as usize] ^= 1;
        let writes = disk.writes.clone();
        let before = writes.get();
        let mut recovered = PageStore::open(disk, 32).await.unwrap();
        assert_eq!(
            recovered.read_bytes(0, 0, &mut [0; 4]).await,
            Err(PageError::Corrupted)
        );
        assert_eq!(writes.get(), before);
    });
}

// Regression guard: actual EKV transactions survive file close and reopen, including read tombstones.
#[test]
fn ekv_roundtrip_on_disk() {
    use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;
    block_on(async {
        let path = std::env::temp_dir().join(alloc::format!("theo-ekv-{}.db", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(file_len(32)).unwrap();
        let store = PageStore::create(HostFile(file), 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        db.format().await.unwrap();
        let mut tx = db.write_transaction().await;
        tx.write(b"episode/1", b"available-unread").await.unwrap();
        tx.write(b"episode/2", b"available-unread").await.unwrap();
        tx.commit().await.unwrap();
        // Drop without commit must not publish the new value.
        let mut tx = db.write_transaction().await;
        tx.write(b"episode/1", b"wrong").await.unwrap();
        drop(tx);
        drop(db);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let store = PageStore::open(HostFile(file), 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        db.mount().await.unwrap();
        let mut value = [0; 64];
        {
            let tx = db.read_transaction().await;
            let len = tx.read(b"episode/1", &mut value).await.unwrap();
            assert_eq!(&value[..len], b"available-unread");
        }
        let mut tx = db.write_transaction().await;
        tx.write(b"episode/1", b"reclaimed-read").await.unwrap();
        tx.delete(b"episode/2").await.unwrap();
        tx.commit().await.unwrap();
        drop(db);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let store = PageStore::open(HostFile(file), 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        db.mount().await.unwrap();
        {
            let tx = db.read_transaction().await;
            let mut cursor = tx.read_all().await.unwrap();
            let mut key = [0; 64];
            let (k, v) = cursor.next(&mut key, &mut value).await.unwrap().unwrap();
            assert_eq!(&key[..k], b"episode/1");
            assert_eq!(&value[..v], b"reclaimed-read");
            assert_eq!(cursor.next(&mut key, &mut value).await.unwrap(), None);
        }
        drop(db);
        std::fs::remove_file(path).unwrap();
    });
}

// Regression guard: a torn sector during an EKV multi-key commit must recover all or none.
#[test]
fn interrupted_ekv_commit_is_atomic() {
    use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;
    block_on(async {
        let mut store = PageStore::create(Disk::new(32), 32).await.unwrap();
        {
            let db = ekv::Database::<_, NoopRawMutex>::new(&mut store, ekv::Config::default());
            db.format().await.unwrap();
            let mut tx = db.write_transaction().await;
            tx.write(b"a", b"old").await.unwrap();
            tx.write(b"b", b"old").await.unwrap();
            tx.commit().await.unwrap();
        }
        let baseline = store.into_inner();
        let before = baseline.writes.get();
        let mut probe = PageStore::open(baseline.clone(), 32).await.unwrap();
        {
            let db = ekv::Database::<_, NoopRawMutex>::new(&mut probe, ekv::Config::default());
            let mut tx = db.write_transaction().await;
            tx.write(b"a", b"new").await.unwrap();
            tx.write(b"b", b"new").await.unwrap();
            tx.commit().await.unwrap();
        }
        let writes = baseline.writes.get() - before;
        for cut in 0..=writes {
            let mut disk = baseline.clone();
            disk.remaining = Some(cut);
            let mut store = PageStore::open(disk, 32).await.unwrap();
            {
                let db = ekv::Database::<_, NoopRawMutex>::new(&mut store, ekv::Config::default());
                let mut tx = db.write_transaction().await;
                if tx.write(b"a", b"new").await.is_ok() && tx.write(b"b", b"new").await.is_ok() {
                    let _ = tx.commit().await;
                }
            }
            let mut disk = store.into_inner();
            disk.remaining = None;
            let store = PageStore::open(disk, 32).await.unwrap();
            let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
            db.mount().await.unwrap();
            let tx = db.read_transaction().await;
            let mut a = [0; 8];
            let mut b = [0; 8];
            let alen = tx.read(b"a", &mut a).await.unwrap();
            let blen = tx.read(b"b", &mut b).await.unwrap();
            assert_eq!(
                &a[..alen],
                &b[..blen],
                "mixed transaction at sector cut {cut}"
            );
            assert!(a[..alen] == *b"old" || a[..alen] == *b"new");
        }
    });
}
