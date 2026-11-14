//! In-memory sector device with fault injection, for host tests.
//!
//! Writes since the last `sync` stay pending: [`TestDisk::crash`] either
//! drops them (nothing after the last barrier reached the card) or keeps
//! them (everything up to the failure did, torn sector included).

extern crate std;

use crate::littlefs::{SECTOR_SIZE, SectorIo};
use alloc::rc::Rc;
use core::cell::{RefCell, RefMut};
use littlefs_rust::Error;
use std::collections::BTreeMap;

type Sector = [u8; SECTOR_SIZE];

/// Shared handle: clones see the same sectors and counters.
#[derive(Clone, Debug)]
pub struct TestDisk(Rc<RefCell<State>>);

#[derive(Debug)]
pub struct State {
    /// Sectors never written read as this byte (`0xA5` by default: redb
    /// must not rely on zero-filled growth).
    pub fill: u8,
    pub sectors: u32,
    durable: BTreeMap<u32, Sector>,
    pending: BTreeMap<u32, Sector>,
    pub reads: usize,
    pub writes: usize,
    pub syncs: usize,
    /// Every write fails while set.
    pub fail_writes: bool,
    /// `(n, prefix)`: write number `n` (0-based, counting from creation)
    /// stores only its first `prefix` bytes, then fails.
    pub cut: Option<(usize, usize)>,
}

/// What a crash keeps of the writes after the last sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Crash {
    DropPending,
    KeepPending,
}

impl TestDisk {
    pub fn new(sectors: u32) -> Self {
        Self(Rc::new(RefCell::new(State {
            fill: 0xA5,
            sectors,
            durable: BTreeMap::new(),
            pending: BTreeMap::new(),
            reads: 0,
            writes: 0,
            syncs: 0,
            fail_writes: false,
            cut: None,
        })))
    }
    pub fn state(&self) -> RefMut<'_, State> {
        self.0.borrow_mut()
    }
    /// Power loss: resolve pending writes and clear faults.
    pub fn crash(&self, mode: Crash) {
        let mut s = self.state();
        let pending = core::mem::take(&mut s.pending);
        if mode == Crash::KeepPending {
            s.durable.extend(pending);
        }
        s.fail_writes = false;
        s.cut = None;
    }
    /// An independent copy of the durable and pending sectors, counters reset.
    pub fn snapshot(&self) -> Self {
        let s = self.0.borrow();
        let copy = Self::new(s.sectors);
        {
            let mut c = copy.state();
            c.fill = s.fill;
            c.durable = s.durable.clone();
            c.pending = s.pending.clone();
        }
        copy
    }
    pub fn reset_counters(&self) {
        let mut s = self.state();
        s.reads = 0;
        s.writes = 0;
        s.syncs = 0;
    }
}

impl SectorIo for TestDisk {
    fn read_sector(&mut self, sector: u32, out: &mut Sector) -> Result<(), Error> {
        let mut s = self.state();
        if sector >= s.sectors {
            return Err(Error::Io);
        }
        s.reads += 1;
        *out = match s.pending.get(&sector).or_else(|| s.durable.get(&sector)) {
            Some(data) => *data,
            None => [s.fill; SECTOR_SIZE],
        };
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &Sector) -> Result<(), Error> {
        let mut s = self.state();
        if sector >= s.sectors || s.fail_writes {
            return Err(Error::Io);
        }
        let n = s.writes;
        s.writes += 1;
        if let Some((cut, prefix)) = s.cut
            && cut == n
        {
            let mut torn = match s.pending.get(&sector).or_else(|| s.durable.get(&sector)) {
                Some(old) => *old,
                None => [s.fill; SECTOR_SIZE],
            };
            torn[..prefix].copy_from_slice(&data[..prefix]);
            s.pending.insert(sector, torn);
            s.fail_writes = true;
            return Err(Error::Io);
        }
        s.pending.insert(sector, *data);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), Error> {
        let mut s = self.state();
        if s.fail_writes {
            return Err(Error::Io);
        }
        s.syncs += 1;
        let pending = core::mem::take(&mut s.pending);
        s.durable.extend(pending);
        Ok(())
    }
    fn check(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
