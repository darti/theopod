use crate::page_store::*;
use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;
use std::{
    cell::Cell,
    io::{Read, Seek, SeekFrom, Write},
    rc::Rc,
};
struct Raw {
    file: std::fs::File,
    writes: Rc<Cell<usize>>,
    cut: Option<usize>,
}
impl Raw {
    fn open(path: &std::path::Path) -> Self {
        Self {
            file: std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap(),
            writes: Rc::new(Cell::new(0)),
            cut: None,
        }
    }
}
impl SectorFile for Raw {
    type Error = std::io::Error;
    fn byte_len(&self) -> u64 {
        self.file.metadata().unwrap().len()
    }
    async fn read_sector(&mut self, s: u64, b: &mut [u8; 512]) -> Result<(), Self::Error> {
        self.file.seek(SeekFrom::Start(s * 512))?;
        self.file.read_exact(b)
    }
    async fn write_sector(&mut self, s: u64, b: &[u8; 512]) -> Result<(), Self::Error> {
        self.file.seek(SeekFrom::Start(s * 512))?;
        let n = self.writes.get();
        self.writes.set(n + 1);
        if self.cut == Some(n) {
            self.file.write_all(&b[..217])?;
            self.file.sync_all()?;
            return Err(std::io::Error::other("torn sector"));
        }
        self.file.write_all(b)
    }
    async fn sync(&mut self) -> Result<(), Self::Error> {
        self.file.sync_all()
    }
}
#[test]
fn raw_file_interruptions_and_adapter() {
    futures::executor::block_on(async {
        let directory = std::env::var("THEO_T106_DIR").unwrap();
        let dir = std::path::Path::new(&directory);
        let base = dir.join("baseline.db");
        let work = dir.join("cut.db");
        std::fs::File::create(&base)
            .unwrap()
            .set_len(file_len(32))
            .unwrap();
        let store = PageStore::create(Raw::open(&base), 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        db.format().await.unwrap();
        crate::adapter::batch(&db, &[(b"a", Some(b"old")), (b"b", Some(b"old"))])
            .await
            .ok()
            .unwrap();
        drop(db);
        std::fs::copy(&base, &work).unwrap();
        let raw = Raw::open(&work);
        let count = raw.writes.clone();
        let store = PageStore::open(raw, 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        crate::adapter::recover(&db).await.unwrap();
        crate::adapter::batch(&db, &[(b"a", Some(b"new")), (b"b", Some(b"new"))])
            .await
            .ok()
            .unwrap();
        let writes = count.get();
        drop(db);
        for cut in 0..=writes {
            std::fs::copy(&base, &work).unwrap();
            let mut raw = Raw::open(&work);
            raw.cut = Some(cut);
            let store = PageStore::open(raw, 32).await.unwrap();
            let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
            let _ = crate::adapter::batch(&db, &[(b"a", Some(b"new")), (b"b", Some(b"new"))]).await;
            drop(db);
            let store = PageStore::open(Raw::open(&work), 32).await.unwrap();
            let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
            crate::adapter::recover(&db).await.unwrap();
            let (mut a, mut b) = ([0; 8], [0; 8]);
            let al = crate::adapter::get(&db, b"a", &mut a).await.unwrap();
            let bl = crate::adapter::get(&db, b"b", &mut b).await.unwrap();
            assert_eq!(&a[..al], &b[..bl], "cut {cut}");
            assert!(a[..al] == *b"old" || a[..al] == *b"new");
        }
        println!(
            "raw host file: {} sector interruption points + successful endpoint",
            writes
        );
        let store = PageStore::open(Raw::open(&base), 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        crate::adapter::put(&db, b"a", b"updated")
            .await
            .ok()
            .unwrap();
        drop(db);
        let store = PageStore::open(Raw::open(&base), 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        let mut out = [0; 16];
        let n = crate::adapter::get(&db, b"a", &mut out).await.unwrap();
        assert_eq!(&out[..n], b"updated");
        crate::adapter::batch(&db, &[(b"b", None)])
            .await
            .ok()
            .unwrap();
        drop(db);
        let store = PageStore::open(Raw::open(&base), 32).await.unwrap();
        let db = ekv::Database::<_, NoopRawMutex>::new(store, ekv::Config::default());
        let mut seen = 0;
        crate::adapter::scan(&db, &mut [0; 64], &mut [0; 4096], |k, v| {
            assert_eq!(k, b"a");
            assert_eq!(v, b"updated");
            seen += 1;
        })
        .await
        .unwrap();
        assert_eq!(seen, 1);
        println!("Database inline bytes {}", std::mem::size_of_val(&db));
        drop(db);
        let mut raw = Raw::open(&base);
        let count = raw.writes.clone();
        let mut header = [0; 512];
        raw.read_sector(0, &mut header).await.unwrap();
        header[8] ^= 1;
        raw.write_sector(0, &header).await.unwrap();
        let before = count.get();
        assert!(matches!(
            PageStore::open(raw, 32).await,
            Err(PageError::Format)
        ));
        assert_eq!(count.get(), before);
    });
}
