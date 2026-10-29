use std::{
    cell::RefCell,
    future::Future,
    rc::Rc,
    task::{Context, Poll, Waker},
};

use ekv::{
    Config, Database,
    config::{CRC, ERASE_VALUE, PAGE_SIZE},
    flash::{Flash, PageID},
};
use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;

const PAGES: usize = 32;
type Db = Database<Nor, NoopRawMutex>;

#[derive(Clone, Debug)]
struct Operation {
    erase: bool,
    page: PageID,
    offset: usize,
    data: Vec<u8>,
}

use crate::page_store::{PageStore, SectorFile, file_len};
#[derive(Clone)]
struct Mem(Vec<u8>);
impl SectorFile for Mem {
    type Error = ();
    fn byte_len(&self) -> u64 {
        self.0.len() as u64
    }
    async fn read_sector(&mut self, s: u64, b: &mut [u8; 512]) -> Result<(), ()> {
        b.copy_from_slice(&self.0[s as usize * 512..][..512]);
        Ok(())
    }
    async fn write_sector(&mut self, s: u64, b: &[u8; 512]) -> Result<(), ()> {
        self.0[s as usize * 512..][..512].copy_from_slice(b);
        Ok(())
    }
    async fn sync(&mut self) -> Result<(), ()> {
        Ok(())
    }
}
struct State {
    store: PageStore<Mem>,
    operations: Vec<Operation>,
    cut: Option<(usize, usize)>,
}
#[derive(Clone)]
struct Nor(Rc<RefCell<State>>);
impl Nor {
    fn new(bytes: Vec<u8>) -> Self {
        let store = if bytes.len() == PAGES * PAGE_SIZE {
            complete(PageStore::create(
                Mem(vec![0; file_len(PAGES) as usize]),
                PAGES,
            ))
            .unwrap()
        } else {
            complete(PageStore::open(Mem(bytes), PAGES)).unwrap()
        };
        Self(Rc::new(RefCell::new(State {
            store,
            operations: vec![],
            cut: None,
        })))
    }
    async fn mutate(&mut self, operation: Operation) {
        let cut = {
            let mut s = self.0.borrow_mut();
            let cut = s.cut.filter(|(i, _)| *i == s.operations.len());
            // PageStore exposes only complete previous or complete replacement pages.
            // Test cancellation on both sides of every logical Flash write.
            if cut.is_none_or(|(_, prefix)| prefix == operation.data.len()) {
                if operation.erase {
                    use ekv::flash::Flash;
                    s.store.erase(operation.page).await.unwrap();
                } else {
                    s.store
                        .replace(operation.page.index(), operation.offset, &operation.data)
                        .await
                        .unwrap();
                }
            }
            s.operations.push(operation);
            cut.is_some()
        };
        if cut {
            std::future::pending::<()>().await;
        }
    }
    fn snapshot(&self) -> Vec<u8> {
        let mut s = self.0.borrow_mut();
        // Swapping out only for extraction; preserve the store by reopening.
        let replacement = complete(PageStore::create(
            Mem(vec![0; file_len(PAGES) as usize]),
            PAGES,
        ))
        .unwrap();
        let old = std::mem::replace(&mut s.store, replacement);
        let data = old.into_inner().0;
        s.store = complete(PageStore::open(Mem(data.clone()), PAGES)).unwrap();
        data
    }
}
impl Flash for Nor {
    type Error = ();
    fn page_count(&self) -> usize {
        PAGES
    }
    async fn erase(&mut self, p: PageID) -> Result<(), ()> {
        self.mutate(Operation {
            erase: true,
            page: p,
            offset: 0,
            data: vec![ERASE_VALUE; PAGE_SIZE],
        })
        .await;
        Ok(())
    }
    async fn read(&mut self, p: PageID, o: usize, b: &mut [u8]) -> Result<(), ()> {
        self.0
            .borrow_mut()
            .store
            .read_bytes(p.index(), o, b)
            .await
            .map_err(|_| ())
    }
    async fn write(&mut self, p: PageID, o: usize, b: &[u8]) -> Result<(), ()> {
        self.mutate(Operation {
            erase: false,
            page: p,
            offset: o,
            data: b.to_vec(),
        })
        .await;
        Ok(())
    }
}

fn complete<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("unexpected pending flash operation"),
    }
}

async fn update(db: &Db, version: u64) {
    let value = version.to_le_bytes();
    let mut transaction = db.write_transaction().await;
    transaction.write(b"a", &value).await.unwrap();
    transaction.write(b"b", &value).await.unwrap();
    transaction.commit().await.unwrap();
}

fn baseline() -> Vec<u8> {
    assert!(CRC);
    let flash = Nor::new(vec![ERASE_VALUE; PAGES * PAGE_SIZE]);
    let db = Db::new(flash.clone(), Config::default());
    complete(db.format()).unwrap();
    complete(update(&db, 1));
    let bytes = flash.snapshot();
    bytes
}

fn commit_reproduction() -> (Vec<u8>, u64, u64, Vec<Operation>) {
    let baseline = baseline();
    let (reference_flash, reference_db) = mounted(baseline.clone());
    complete(update(&reference_db, 2));
    let operations = reference_flash.0.borrow().operations.clone();

    (baseline, 1, 2, operations)
}

fn metadata_rollover_reproduction() -> (Vec<u8>, u64, u64, Vec<Operation>) {
    let mut baseline = baseline();
    let metadata_magic = 0x1d81bcde_u32.to_le_bytes();

    for version in 2..=500 {
        let (flash, db) = mounted(baseline.clone());
        complete(update(&db, version));
        let state = flash.0.borrow();
        if state.operations.iter().any(|operation| {
            !operation.erase && operation.offset == 0 && operation.data.starts_with(&metadata_magic)
        }) {
            println!(
                "metadata rollover: old={} new={} operations={}",
                version - 1,
                version,
                state.operations.len()
            );
            return (baseline, version - 1, version, state.operations.clone());
        }
        drop(state);
        baseline = flash.snapshot();
    }

    panic!("test did not reach metadata page replacement");
}

fn read_version(db: &Db) -> Result<u64, String> {
    complete(async {
        let transaction = db.read_transaction().await;
        let mut a = [0; 8];
        let mut b = [0; 8];
        let a_len = transaction
            .read(b"a", &mut a)
            .await
            .map_err(|error| format!("{error:?}"))?;
        let b_len = transaction
            .read(b"b", &mut b)
            .await
            .map_err(|error| format!("{error:?}"))?;
        if a_len != 8 || b_len != 8 {
            return Err(format!("unexpected value lengths: {a_len}, {b_len}"));
        }
        if a != b {
            return Err("transaction exposed mixed versions".into());
        }
        Ok(u64::from_le_bytes(a))
    })
}

fn mount_after_cut(
    baseline: &[u8],
    old_version: u64,
    new_version: u64,
    cut_at: usize,
    prefix: usize,
) -> Result<(), String> {
    let (flash, db) = mounted(baseline.to_vec());
    flash.0.borrow_mut().cut = Some((cut_at, prefix));

    let mut commit = Box::pin(update(&db, new_version));
    assert!(matches!(
        commit
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    drop(commit);
    drop(db);

    let rebooted = Db::new(Nor::new(flash.snapshot()), Config::default());
    complete(rebooted.mount()).map_err(|error| format!("{error:?}"))?;
    let recovered_version = read_version(&rebooted)?;
    if recovered_version != old_version && recovered_version != new_version {
        return Err(format!("unexpected recovered version {recovered_version}"));
    }
    Ok(())
}

fn exercise_each_write(
    baseline: &[u8],
    old_version: u64,
    new_version: u64,
    operations: &[Operation],
) {
    let mut failure_report = Vec::new();

    for (cut_at, operation) in operations.iter().enumerate() {
        let mut working = Vec::new();
        let mut failing = Vec::new();
        for prefix in [0, operation.data.len()] {
            match mount_after_cut(baseline, old_version, new_version, cut_at, prefix) {
                Ok(()) => working.push(prefix),
                Err(error) => failing.push(format!("{prefix} ({error})")),
            }
        }

        println!(
            "write {cut_at}: page={} offset={} len={} working={working:?} failing={failing:?}",
            operation.page.index(),
            operation.offset,
            operation.data.len()
        );

        if !failing.is_empty() {
            failure_report.push(format!(
                "write {cut_at} ({operation:?})\n  working prefixes: {working:?}\n  failing prefixes: {failing:?}"
            ));
        }
    }

    assert!(
        failure_report.is_empty(),
        "power-loss failures:\n{}",
        failure_report.join("\n")
    );
}

#[test]
fn mount_survives_each_interrupted_write_during_commit() {
    // Regression (t-116): committed pairs survive every logical write/erase boundary.
    let (baseline, old_version, new_version, operations) = commit_reproduction();
    exercise_each_write(&baseline, old_version, new_version, &operations);
}

#[test]
fn mount_survives_each_interrupted_write_during_metadata_rollover() {
    // Regression (t-116): incomplete replacement metadata must not hide committed keys.
    let (baseline, old_version, new_version, operations) = metadata_rollover_reproduction();
    exercise_each_write(&baseline, old_version, new_version, &operations);
}

fn mounted(bytes: Vec<u8>) -> (Nor, Db) {
    let flash = Nor::new(bytes);
    let db = Db::new(flash.clone(), Config::default());
    complete(db.mount()).unwrap();
    flash.0.borrow_mut().operations.clear();
    (flash, db)
}

#[derive(Clone)]
struct SectorFault {
    bytes: Rc<RefCell<Vec<u8>>>,
    count: Rc<std::cell::Cell<usize>>,
    cut: Option<usize>,
    prefix: usize,
    cancel: bool,
}
impl SectorFault {
    async fn boundary(&self) -> Result<(), ()> {
        let index = self.count.get();
        self.count.set(index + 1);
        if self.cut == Some(index) {
            if self.cancel {
                std::future::pending::<()>().await;
            }
            return Err(());
        }
        Ok(())
    }
}
impl SectorFile for SectorFault {
    type Error = ();
    fn byte_len(&self) -> u64 {
        self.bytes.borrow().len() as u64
    }
    async fn read_sector(&mut self, sector: u64, data: &mut [u8; 512]) -> Result<(), ()> {
        data.copy_from_slice(&self.bytes.borrow()[sector as usize * 512..][..512]);
        Ok(())
    }
    async fn write_sector(&mut self, sector: u64, data: &[u8; 512]) -> Result<(), ()> {
        let n = if self.cut == Some(self.count.get()) {
            self.prefix
        } else {
            512
        };
        self.bytes.borrow_mut()[sector as usize * 512..][..n].copy_from_slice(&data[..n]);
        self.boundary().await
    }
    async fn sync(&mut self) -> Result<(), ()> {
        self.boundary().await
    }
}
async fn fault_update(
    db: &Database<PageStore<SectorFault>, NoopRawMutex>,
    version: u64,
) -> Result<(), String> {
    let mut tx = db.write_transaction().await;
    for key in [b"a", b"b"] {
        tx.write(key, &version.to_le_bytes())
            .await
            .map_err(|e| format!("{e:?}"))?;
    }
    tx.commit().await.map_err(|e| format!("{e:?}"))
}
#[test]
fn metadata_rollover_survives_sector_and_sync_faults() {
    // Regression (t-116): exercise actual COW sector writes and barriers, including dropped futures.
    let (baseline, old, new, _) = metadata_rollover_reproduction();
    let make = |cut, prefix, cancel| {
        let file = SectorFault {
            bytes: Rc::new(RefCell::new(baseline.clone())),
            count: Rc::new(std::cell::Cell::new(0)),
            cut,
            prefix,
            cancel,
        };
        let store = complete(PageStore::open(file.clone(), PAGES)).unwrap();
        let db = Database::<_, NoopRawMutex>::new(store, Config::default());
        complete(db.mount()).unwrap();
        (file, db)
    };
    let (file, db) = make(None, 512, false);
    complete(fault_update(&db, new)).unwrap();
    let boundaries = file.count.get();
    for cancel in [false, true] {
        for prefix in [0, 217, 512] {
            for cut in 0..boundaries {
                let (file, db) = make(Some(cut), prefix, cancel);
                let mut future = Box::pin(fault_update(&db, new));
                let result = future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()));
                if cancel {
                    assert!(matches!(result, Poll::Pending));
                } else {
                    assert!(matches!(result, Poll::Ready(Err(_))));
                }
                drop(future);
                drop(db);
                let recovered = Db::new(Nor::new(file.bytes.borrow().clone()), Config::default());
                complete(recovered.mount()).unwrap();
                let value = read_version(&recovered)
                    .unwrap_or_else(|e| panic!("cut={cut} prefix={prefix} cancel={cancel}: {e}"));
                assert!(
                    value == old || value == new,
                    "cut={cut} prefix={prefix} cancel={cancel}"
                );
            }
        }
    }
    println!("rollover: {boundaries} sector/sync boundaries x 3 prefixes x error/cancellation");
}

#[path = "history_tests.rs"]
mod history_tests;
