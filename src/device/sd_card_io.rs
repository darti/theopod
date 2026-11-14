//! The card as `SectorIo`, driven from core1 with blocking SPI (ADR-0023).

use aligned::{A4, Aligned};
use alloc::rc::Rc;
#[cfg(feature = "sd-check")]
use alloc::vec::Vec;
use allocator_api2::boxed::Box;
use core::cell::Cell;
#[cfg(feature = "sd-check")]
use core::cell::RefCell;
use embassy_time::Instant;
use littlefs_rust::Error;
use theo_storage::littlefs::SectorIo;

#[cfg(feature = "sd-check")]
use super::STACK_SIZE;
use super::{HalDevice, disable, is_disabled};

/// Sectors one multi-block write sends: one 4 KB audio chunk.
pub(super) const STAGED_SECTORS: usize = 8;

/// Distinct sectors traced per request by the `sd-check` read histogram.
#[cfg(feature = "sd-check")]
pub(super) const TRACED_SECTORS: usize = 256;

#[cfg(feature = "sd-check")]
pub(super) struct Measurements {
    pub(super) stack_bottom: usize,
    pub(super) min_sampled_headroom: Cell<usize>,
    pub(super) reads: Cell<usize>,
    pub(super) writes: Cell<usize>,
    pub(super) syncs: Cell<usize>,
    pub(super) io_us: Cell<u64>,
    pub(super) max_io_us: Cell<u64>,
    pub(super) read_sectors: RefCell<Vec<(u32, usize)>>,
    pub(super) untracked_reads: Cell<usize>,
}

#[cfg(feature = "sd-check")]
impl Measurements {
    // Sample a synchronous callback's stack address without reading memory
    // owned by the running stack. This is sampled margin, not a high-water scan.
    #[inline(never)]
    pub(super) fn sample(&self) {
        let marker = 0u8;
        let address = core::hint::black_box(&marker) as *const u8 as usize;
        let margin = address
            .checked_sub(self.stack_bottom)
            .filter(|margin| *margin <= STACK_SIZE)
            .unwrap_or(0);
        self.min_sampled_headroom
            .set(self.min_sampled_headroom.get().min(margin));
    }
}

pub(super) type Staged =
    Box<[Aligned<A4, [u8; 512]>; STAGED_SECTORS], &'static esp_alloc::ExternalMemory>;

/// What a card operation does, for `sd-check` counts and fault injection.
#[derive(Clone, Copy, PartialEq)]
enum Op {
    Read,
    Write,
    Sync,
}

/// An admitted operation; under `sd-check`, when it started.
struct Started {
    #[cfg(feature = "sd-check")]
    at: Instant,
}

/// The card itself, driven from this core with blocking SPI (ADR-0023).
pub(super) struct CardIo {
    pub(super) sectors: u64,
    pub(super) device: HalDevice,
    /// Whole sectors of one multi-block write, aligned for `sdio`. In
    /// PSRAM: core1's stack is the owner's (t-121).
    pub(super) staged: Staged,
    #[cfg(feature = "sd-check")]
    pub(super) measurements: Rc<Measurements>,
    /// The request's (hard) deadline: an operation finishing after it fails.
    pub(super) deadline: Rc<Cell<Instant>>,
    /// `deadline - OWNER_MARGIN_MS`: no new operation starts after it.
    pub(super) soft: Rc<Cell<Instant>>,
}

impl CardIo {
    /// Runs between card operations, so passing the soft deadline fails
    /// only the current scope and leaves the card usable (t-121). A
    /// disabled card fails everything.
    fn ensure_live(&self) -> Result<(), Error> {
        #[cfg(feature = "sd-check")]
        self.measurements.sample();
        if is_disabled() || Instant::now() >= self.soft.get() {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }

    /// Admits one card operation: checks the deadlines, counts it and
    /// injects `sd-check` faults. Pair with [`Self::finish`].
    fn begin(&self, op: Op) -> Result<Started, Error> {
        self.ensure_live()?;
        #[cfg(feature = "sd-check")]
        {
            let count = match op {
                Op::Read => &self.measurements.reads,
                Op::Write => &self.measurements.writes,
                Op::Sync => &self.measurements.syncs,
            };
            count.set(count.get() + 1);
            if op == Op::Write {
                match option_env!("THEO_SD_FAULT") {
                    Some("full") => {
                        disable();
                        return Err(Error::NoSpace);
                    }
                    Some("readonly" | "removed") => {
                        disable();
                        return Err(Error::Io);
                    }
                    Some("timeout") => {
                        while Instant::now() < self.deadline.get() {}
                        disable();
                        return Err(Error::Io);
                    }
                    _ => {}
                }
            }
        }
        #[cfg(not(feature = "sd-check"))]
        let _ = op;
        Ok(Started {
            #[cfg(feature = "sd-check")]
            at: Instant::now(),
        })
    }

    /// Ends an operation. A card error, or finishing past the hard
    /// deadline, disables the card until reboot.
    fn finish(&self, started: Started, result: Result<(), sdio::MmcError>) -> Result<(), Error> {
        #[cfg(feature = "sd-check")]
        {
            let elapsed = started.at.elapsed().as_micros();
            self.measurements
                .io_us
                .set(self.measurements.io_us.get() + elapsed);
            self.measurements
                .max_io_us
                .set(self.measurements.max_io_us.get().max(elapsed));
        }
        #[cfg(not(feature = "sd-check"))]
        let _ = started;
        if result.is_err() || Instant::now() >= self.deadline.get() {
            disable();
            return Err(Error::Io);
        }
        Ok(())
    }

    #[cfg(feature = "sd-check")]
    fn trace_read(&self, sector: u32) {
        let mut trace = self.measurements.read_sectors.borrow_mut();
        if let Some((_, hits)) = trace.iter_mut().find(|(key, _)| *key == sector) {
            *hits += 1;
        } else if trace.len() < TRACED_SECTORS {
            trace.push((sector, 1));
        } else {
            self.measurements
                .untracked_reads
                .set(self.measurements.untracked_reads.get() + 1);
        }
    }
}

impl Drop for CardIo {
    /// The last scope holding the card is gone: storage stays offline.
    fn drop(&mut self) {
        disable();
    }
}

impl SectorIo for CardIo {
    // SPI and delays block, so each `block_on` below completes on its first
    // poll; every wait inside is a bounded loop (ADR-0023).
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        #[cfg(feature = "sd-check")]
        self.trace_read(sector);
        let started = self.begin(Op::Read)?;
        let mut block = Aligned::<A4, _>([0u8; 512]);
        let result = embassy_futures::block_on(block_device_driver::BlockDevice::read(
            &mut self.device,
            sector,
            core::slice::from_mut(&mut block),
        ));
        self.finish(started, result)?;
        out.copy_from_slice(&*block);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        let started = self.begin(Op::Write)?;
        let block = Aligned::<A4, _>(*data);
        let result = embassy_futures::block_on(block_device_driver::BlockDevice::write(
            &mut self.device,
            sector,
            core::slice::from_ref(&block),
        ));
        self.finish(started, result)
    }
    /// Writes up to [`STAGED_SECTORS`] sectors per multi-block command
    /// (CMD25, t-153): the card programs them together.
    fn write_sectors(&mut self, first: u32, data: &[u8]) -> Result<(), Error> {
        theo_storage::littlefs::whole_sectors(data)?;
        for (group, bytes) in data.chunks(STAGED_SECTORS * 512).enumerate() {
            let at = first
                .checked_add((group * STAGED_SECTORS) as u32)
                .ok_or(Error::Invalid)?;
            let sectors = bytes.as_chunks::<512>().0;
            for (slot, sector) in self.staged.iter_mut().zip(sectors) {
                slot.copy_from_slice(sector);
            }
            let started = self.begin(Op::Write)?;
            let result = embassy_futures::block_on(block_device_driver::BlockDevice::write(
                &mut self.device,
                at,
                &self.staged[..sectors.len()],
            ));
            self.finish(started, result)?;
        }
        Ok(())
    }
    /// Each write already waits until the card releases busy; nothing is
    /// cached in this layer.
    fn sync(&mut self) -> Result<(), Error> {
        let started = self.begin(Op::Sync)?;
        self.finish(started, Ok(()))
    }
    fn check(&mut self) -> Result<(), Error> {
        self.ensure_live()
    }
    /// Card errors and overrun deadlines disable the card; a passed soft
    /// deadline does not.
    fn device_failed(&self) -> bool {
        is_disabled()
    }
}
