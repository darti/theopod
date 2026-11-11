//! Core 1 owns LittleFS; core 0 keeps the async SD HAL device. Only owned messages cross
//! cores. Its synchronous callbacks may wait for async I2C while core 0 keeps
//! running touch/IMU tasks that can release that bus (ADR-0012).

use alloc::{rc::Rc, vec::Vec};
use allocator_api2::{alloc::AllocError, boxed::Box};
#[cfg(feature = "sd-check")]
use core::cell::RefCell;
use core::{
    cell::Cell,
    sync::atomic::{AtomicBool, Ordering},
};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use embassy_time::{Duration, Instant, with_deadline};
use esp_hal::{
    peripherals::{CPU_CTRL, FROM_CPU_INTR1, GPIO1, GPIO2, GPIO3, SPI3},
    system::Stack,
};
use littlefs_rust::{Error, Filesystem};
use static_cell::{ConstStaticCell, StaticCell};
use theo_podcasts::{
    catalogue::Catalogue,
    runtime::{self, Runtime},
    unit_media::{UnitError, UnitMedia},
};
use theo_storage::{
    LogBuffer,
    littlefs::{self, Geometry, LogTail, RequestGuard, SectorIo, SectorStorage},
};
use theo_storage::{
    card_layout::CardLayout,
    page_store::{PageError, PageStore},
    regions::{self, AsyncRegion, RegionIo},
    scope::{OWNER_MARGIN_MS, ReplyKind, RequestKind, after_failure, failure_scope},
};

use super::{SdInitError, io};
use theo_storage::sector_bus::{Operation, Request as SectorRequest, SectorBus, SectorClient};

const IO_BUDGET: Duration = Duration::from_millis(500);
/// Budget of the mount request: descriptors, log mount and catalogue open.
const MOUNT_BUDGET: Duration = Duration::from_secs(2);
// The owner's margin must leave it time to work within every request budget.
const _: () = assert!(
    0 < OWNER_MARGIN_MS
        && OWNER_MARGIN_MS < IO_BUDGET.as_millis()
        && OWNER_MARGIN_MS < MOUNT_BUDGET.as_millis()
);
pub(super) static SECTORS: SectorBus = SectorBus::new();
static COMMANDS: Channel<CriticalSectionRawMutex, Request, 1> = Channel::new();
static REPLY: Signal<CriticalSectionRawMutex, Result<Reply, Error>> = Signal::new();
const STACK_SIZE: usize = 48 * 1024;
static STACK: ConstStaticCell<Stack<STACK_SIZE>> = ConstStaticCell::new(Stack::new());

pub type PodcastRequest = Psram<runtime::Command>;
enum Command {
    Domain(PodcastRequest),
    Mount,
    Append(Vec<u8>, LogTarget),
    Tail,
    #[cfg(feature = "sd-check")]
    RotationStart,
    #[cfg(feature = "sd-check")]
    RotationFinish,
}
enum LogTarget {
    Live,
    #[cfg(feature = "sd-check")]
    Rotation,
}
impl LogTarget {
    fn paths(self) -> (&'static str, &'static str) {
        match self {
            Self::Live => ("/theo.log", "/theo.1.log"),
            #[cfg(feature = "sd-check")]
            Self::Rotation => ("/sdtest2.log", "/sdtest2.1"),
        }
    }
}
impl Command {
    fn kind(&self) -> RequestKind {
        match self {
            Self::Mount => RequestKind::Mount,
            Self::Domain(_) => RequestKind::Podcast,
            Self::Append(..) | Self::Tail => RequestKind::Log,
            #[cfg(feature = "sd-check")]
            Self::RotationStart | Self::RotationFinish => RequestKind::Log,
        }
    }
}
struct Request {
    command: Command,
    deadline: Instant,
}
// Fixed inline replies keep the one-slot mailbox bounded without allocation.
#[allow(clippy::large_enum_variant)]
enum Reply {
    Domain(runtime::Reply),
    Done,
    Tail(LogTail),
    /// The log scope is offline until reboot; the bus and podcasts may not be.
    Offline,
}

static LOGS_OFFLINE_WARNED: AtomicBool = AtomicBool::new(false);
/// Log records then stay on serial only; warn once per boot.
fn warn_logs_offline() {
    if !LOGS_OFFLINE_WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("microSD logs offline until reboot; records stay on serial");
    }
}

/// Unique frontend token; a dropped token permanently disables this session.
/// No filesystem or HAL reference is ever handed to the calling executor.
#[derive(Debug)]
pub struct SdStorage {
    _private: (),
}

impl Drop for SdStorage {
    fn drop(&mut self) {
        SECTORS.disable();
    }
}

impl SdStorage {
    async fn request(&mut self, command: Command, budget: Duration) -> Result<Reply, Error> {
        let guard = RequestGuard::begin(SECTORS.poison_flag())?;
        let deadline = Instant::now() + budget;
        let result = with_deadline(deadline, async {
            COMMANDS.send(Request { command, deadline }).await;
            REPLY.wait().await
        })
        .await
        .map_err(|_| Error::Io)??;
        guard.complete()?;
        Ok(result)
    }

    async fn request_done(&mut self, command: Command, budget: Duration) -> Result<(), Error> {
        match self.request(command, budget).await? {
            Reply::Done => Ok(()),
            _ => Err(Error::Io),
        }
    }
}

/// Identify the card on core 0, then start the isolated filesystem owner.
pub async fn init(
    spawner: embassy_executor::Spawner,
    core: (CPU_CTRL<'static>, FROM_CPU_INTR1<'static>),
    spi: SPI3<'static>,
    clk: GPIO2<'static>,
    mosi: GPIO1<'static>,
    miso: GPIO3<'static>,
    expander: &'static crate::device::Expander,
) -> Result<SdStorage, SdInitError> {
    let guard = RequestGuard::begin(SECTORS.poison_flag()).map_err(|_| SdInitError::Host)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let (device, sectors) =
        with_deadline(deadline, super::init_card(spi, clk, mosi, miso, expander))
            .await
            .map_err(|_| SdInitError::Card(sdio::MmcError::Timeout))??;
    let geometry =
        Geometry::new(sectors).map_err(|_| SdInitError::Card(sdio::MmcError::BlockSize))?;
    spawner.spawn(io::serve(device).expect("SD I/O once"));
    let (cpu, interrupt) = core;
    let stack = STACK.take();
    let stack_bottom = stack.bottom() as usize;
    esp_rtos::start_second_core(cpu, interrupt, stack, move || {
        static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
        EXECUTOR
            .init(esp_rtos::embassy::Executor::new())
            .run(|spawner| {
                spawner.spawn(worker(geometry, sectors, stack_bottom).expect("SD owner once"));
            });
    });
    guard.complete().map_err(|_| SdInitError::Host)?;
    Ok(SdStorage { _private: () })
}

pub async fn mount(mut storage: SdStorage) -> Result<SdStorage, Error> {
    storage.request_done(Command::Mount, MOUNT_BUDGET).await?;
    Ok(storage)
}

pub async fn append_log<const N: usize>(
    storage: &mut SdStorage,
    buffer: &mut LogBuffer<N>,
) -> Result<(), Error> {
    // The sole caller has a 4 KiB bounded queue. Reject accidental unbounded
    // requests before allocating a cross-core copy.
    if buffer.len() > 4096 {
        return Err(Error::Invalid);
    }
    match storage
        .request(
            Command::Append(buffer.pending().to_vec(), LogTarget::Live),
            IO_BUDGET,
        )
        .await?
    {
        Reply::Done => {}
        // Completed request: the bus stays healthy for podcasts.
        Reply::Offline => warn_logs_offline(),
        _ => return Err(Error::Io),
    }
    buffer.consume(buffer.len());
    Ok(())
}

pub fn disable() {
    SECTORS.disable();
}
pub fn guard() -> Result<RequestGuard<'static>, Error> {
    RequestGuard::begin(SECTORS.poison_flag())
}

pub async fn domain(
    storage: &mut SdStorage,
    command: PodcastRequest,
) -> Result<runtime::Reply, Error> {
    match storage.request(Command::Domain(command), IO_BUDGET).await? {
        Reply::Domain(reply) => Ok(reply),
        _ => Err(Error::Io),
    }
}

pub async fn report_log_tail(storage: &mut SdStorage) -> Result<(), Error> {
    let tail = match storage.request(Command::Tail, IO_BUDGET).await? {
        Reply::Tail(tail) => tail,
        Reply::Offline => {
            warn_logs_offline();
            return Ok(());
        }
        _ => return Err(Error::Io),
    };
    esp_println::println!(
        "INFO - microSD previous theo.log: {} bytes; tail:\n{}",
        tail.size,
        core::str::from_utf8(&tail.bytes[..tail.len]).unwrap_or("<UTF-8 boundary>")
    );
    Ok(())
}

/// Distinct sectors traced per request by the `sd-check` read histogram.
#[cfg(feature = "sd-check")]
const TRACED_SECTORS: usize = 256;

#[cfg(feature = "sd-check")]
struct Measurements {
    stack_bottom: usize,
    min_sampled_headroom: Cell<usize>,
    reads: Cell<usize>,
    writes: Cell<usize>,
    syncs: Cell<usize>,
    io_us: Cell<u64>,
    max_io_us: Cell<u64>,
    read_sectors: RefCell<Vec<(u32, usize)>>,
    untracked_reads: Cell<usize>,
}

#[cfg(feature = "sd-check")]
impl Measurements {
    // Sample a synchronous callback's stack address without reading memory
    // owned by the running stack. This is sampled margin, not a high-water scan.
    #[inline(never)]
    fn sample(&self) {
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

struct CardIo {
    sectors: u64,
    client: SectorClient<'static>,
    #[cfg(feature = "sd-check")]
    measurements: Rc<Measurements>,
    /// The request's (hard) deadline: bounds and aborts in-flight sector RPCs.
    deadline: Rc<Cell<Instant>>,
    /// `deadline - OWNER_MARGIN_MS`: no new sector RPC starts after it.
    soft: Rc<Cell<Instant>>,
}

impl CardIo {
    /// Runs between sector RPCs, when none is in flight, so passing the soft
    /// deadline fails only the current scope: no late reply can exist and
    /// the bus stays live (t-121). A disabled bus fails everything.
    fn ensure_live(&self) -> Result<(), Error> {
        #[cfg(feature = "sd-check")]
        self.measurements.sample();
        if SECTORS.is_disabled() || Instant::now() >= self.soft.get() {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}

impl CardIo {
    fn request(&mut self, operation: Operation) -> Result<[u8; 512], Error> {
        self.ensure_live()?;
        #[cfg(feature = "sd-check")]
        let started = {
            let count = match &operation {
                Operation::Read(_) => &self.measurements.reads,
                Operation::Write(..) => &self.measurements.writes,
                Operation::Sync => &self.measurements.syncs,
            };
            count.set(count.get() + 1);
            if let Operation::Read(sector) = &operation {
                let mut trace = self.measurements.read_sectors.borrow_mut();
                if let Some((_, hits)) = trace.iter_mut().find(|(key, _)| key == sector) {
                    *hits += 1;
                } else if trace.len() < TRACED_SECTORS {
                    trace.push((*sector, 1));
                } else {
                    self.measurements
                        .untracked_reads
                        .set(self.measurements.untracked_reads.get() + 1);
                }
            }
            Instant::now()
        };
        let deadline = self.deadline.get();
        // Core 0's executor runs the async SD/I2C service while this core
        // waits. Its channels carry owned buffers only.
        let result = self.client.exchange_blocking(
            SectorRequest {
                operation,
                deadline_ticks: deadline.as_ticks(),
            },
            || SECTORS.is_disabled() || Instant::now() >= deadline,
        );
        #[cfg(feature = "sd-check")]
        {
            let elapsed = started.elapsed().as_micros();
            self.measurements
                .io_us
                .set(self.measurements.io_us.get() + elapsed);
            self.measurements
                .max_io_us
                .set(self.measurements.max_io_us.get().max(elapsed));
        }
        result
    }
}

impl SectorIo for CardIo {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        out.copy_from_slice(&self.request(Operation::Read(sector))?);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        self.request(Operation::Write(sector, *data)).map(|_| ())
    }
    fn sync(&mut self) -> Result<(), Error> {
        self.request(Operation::Sync).map(|_| ())
    }
    fn check(&mut self) -> Result<(), Error> {
        self.ensure_live()
    }
    /// Card/HAL errors and aborted exchanges disable the bus; a passed soft
    /// deadline does not.
    fn device_failed(&self) -> bool {
        SECTORS.is_disabled()
    }
}

#[embassy_executor::task]
async fn worker(geometry: Geometry, sectors: u64, stack_bottom: usize) {
    // Keep Embassy's task header/atomic wake state in internal memory. Only
    // the large, pinned domain future is placed externally; cache-sensitive
    // HAL and sector I/O tasks retain their existing internal storage.
    match theo_storage::future::try_pin_in(
        move || owner(geometry, sectors, stack_bottom),
        &esp_alloc::ExternalMemory,
        114000,
    ) {
        Ok(future) => {
            log::info!(
                "storage owner future: {} PSRAM bytes",
                core::mem::size_of_val(future.as_ref().get_ref())
            );
            future.await;
        }
        Err(_) => {
            SECTORS.disable();
            log::warn!("storage offline: PSRAM owner allocation failed");
        }
    }
}
async fn owner(geometry: Geometry, sectors: u64, _stack_bottom: usize) {
    #[cfg(feature = "sd-check")]
    let measurements = Rc::new(Measurements {
        stack_bottom: _stack_bottom,
        min_sampled_headroom: Cell::new(STACK_SIZE),
        reads: Cell::new(0),
        writes: Cell::new(0),
        syncs: Cell::new(0),
        io_us: Cell::new(0),
        max_io_us: Cell::new(0),
        read_sectors: RefCell::new(Vec::with_capacity(TRACED_SECTORS)),
        untracked_reads: Cell::new(0),
    });
    let deadline = Rc::new(Cell::new(Instant::now()));
    let soft = Rc::new(Cell::new(Instant::now()));
    let mut card = Some(CardIo {
        sectors,
        #[cfg(feature = "sd-check")]
        measurements: measurements.clone(),
        client: SECTORS.take_client().expect("one sector client"),
        deadline: deadline.clone(),
        soft: soft.clone(),
    });
    let mut volume = None;
    let mut podcasts = None;
    let mut scratch = allocator_api2::vec::Vec::new_in(&esp_alloc::ExternalMemory);
    if scratch.try_reserve_exact(4096).is_ok() {
        scratch.resize(4096, 0);
    }

    loop {
        let request = COMMANDS.receive().await;
        deadline.set(request.deadline);
        // Stop early enough for the error reply to beat the caller's deadline.
        soft.set(
            request
                .deadline
                .checked_sub(Duration::from_millis(OWNER_MARGIN_MS))
                .unwrap_or_else(Instant::now),
        );
        let kind = request.command.kind();
        #[cfg(feature = "sd-check")]
        let label = match &request.command {
            Command::Mount => "mount",
            Command::Domain(_) => "podcast",
            Command::Append(..) => "append",
            Command::Tail => "tail",
            Command::RotationStart => "rotation-start",
            Command::RotationFinish => "rotation-finish",
        };
        #[cfg(feature = "sd-check")]
        let started = {
            measurements.reads.set(0);
            measurements.writes.set(0);
            measurements.syncs.set(0);
            measurements.io_us.set(0);
            measurements.max_io_us.set(0);
            measurements.read_sectors.borrow_mut().clear();
            measurements.untracked_reads.set(0);
            Instant::now()
        };
        let result = if SECTORS.is_disabled() || Instant::now() >= soft.get() {
            Err(Error::Io)
        } else {
            match request.command {
                Command::Mount => match card.take() {
                    Some(card) => {
                        let allocated = !scratch.is_empty();
                        match apart(|| mount_owner(card, geometry, &mut podcasts, allocated))
                            .await
                            .unwrap_or(Err(Error::Io))
                        {
                            Ok(logs) => {
                                volume = logs;
                                Ok(Reply::Done)
                            }
                            Err(error) => Err(error),
                        }
                    }
                    None => Err(Error::Io),
                },
                Command::Domain(command) => match podcasts.as_mut() {
                    Some(owner) => {
                        match apart(|| owner.handle(Box::into_inner(command), &mut scratch))
                            .await
                            // No PSRAM for this request's state: refuse it, the runtime is intact.
                            .unwrap_or(Err(runtime::Error::Invalid))
                        {
                            Ok(reply) => Ok(Reply::Domain(reply)),
                            Err(runtime::Error::Storage) => Err(Error::Io),
                            Err(runtime::Error::Invalid) => {
                                Ok(Reply::Domain(runtime::Reply::Rejected))
                            }
                        }
                    }
                    None => Ok(Reply::Domain(runtime::Reply::Offline)),
                },
                Command::Append(bytes, target) => match volume.as_ref() {
                    Some(fs) => {
                        let mut buffer = LogBuffer::<4096>::new();
                        buffer.push(&bytes);
                        let (name, archive) = target.paths();
                        littlefs::append_log(fs.as_ref(), &mut buffer, name, archive)
                            .map(|()| Reply::Done)
                    }
                    None => Ok(Reply::Offline),
                },
                Command::Tail => match volume.as_ref() {
                    Some(fs) => littlefs::log_tail(fs.as_ref(), "/theo.log").map(Reply::Tail),
                    None => Ok(Reply::Offline),
                },
                #[cfg(feature = "sd-check")]
                Command::RotationStart => match volume.as_ref() {
                    Some(fs) => rotation_start(fs.as_ref()).map(|()| Reply::Done),
                    None => Ok(Reply::Offline),
                },
                #[cfg(feature = "sd-check")]
                Command::RotationFinish => match volume.as_ref() {
                    Some(fs) => {
                        let (name, archive) = LogTarget::Rotation.paths();
                        if fs.stat(name).map(|m| m.size) == Ok(4096)
                            && fs.stat(archive).map(|m| m.size as u64)
                                == Ok(theo_storage::LOG_FILE_LIMIT)
                        {
                            Ok(Reply::Done)
                        } else {
                            Err(Error::Io)
                        }
                    }
                    None => Ok(Reply::Offline),
                },
            }
        };
        let poisoned = SECTORS.is_disabled();
        let result = if result.is_ok() && !poisoned {
            result
        } else {
            // A scope failure is a completed reply: the caller's guard
            // completes and only a bus failure poisons the session.
            let effect = after_failure(failure_scope(poisoned, kind));
            if effect.disable_bus {
                SECTORS.disable();
                card = None;
            }
            if effect.drop_logs {
                // On a legacy whole-card mount the log volume holds the only
                // `CardIo`: dropping it drops the `SectorClient`, which
                // disables the bus, as losing the last live scope does on a
                // v2 card.
                volume = None;
            }
            if effect.drop_podcasts {
                podcasts = None;
            }
            match effect.reply {
                ReplyKind::Error => result,
                ReplyKind::LogsOffline => {
                    log::warn!("SD logs offline until reboot: {:?}", result.err());
                    Ok(Reply::Offline)
                }
                ReplyKind::PodcastsOffline => {
                    log::warn!("podcasts offline until reboot: {:?}", result.err());
                    Ok(Reply::Domain(runtime::Reply::Offline))
                }
            }
        };
        #[cfg(feature = "sd-check")]
        let (unique, hottest, hot_reads) = {
            let trace = measurements.read_sectors.borrow();
            let (sector, count) = trace
                .iter()
                .max_by_key(|(_, count)| count)
                .copied()
                .unwrap_or((0, 0));
            (trace.len(), sector, count)
        };
        #[cfg(feature = "sd-check")]
        esp_println::println!(
            "INFO - SD measurement: op={} elapsed_us={} sampled_stack_margin={} status={} reads={} writes={} syncs={} io_us={} max_io_us={} unique={} hottest={} hot_reads={} untracked={}",
            label,
            started.elapsed().as_micros(),
            measurements.min_sampled_headroom.get(),
            if result.is_ok() { "ok" } else { "error" },
            measurements.reads.get(),
            measurements.writes.get(),
            measurements.syncs.get(),
            measurements.io_us.get(),
            measurements.max_io_us.get(),
            unique,
            hottest,
            hot_reads,
            measurements.untracked_reads.get(),
        );
        REPLY.signal(result);
    }
}

#[cfg(feature = "sd-check")]
fn rotation_start<S: littlefs_rust::Storage>(fs: &Filesystem<S>) -> Result<(), Error> {
    let (name, archive) = LogTarget::Rotation.paths();
    for path in [name, archive] {
        match fs.stat(path) {
            Err(Error::NoEntry) => {}
            Ok(_) => return Err(Error::Exists),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Exercise the real rotation path with isolated, bounded batches.
#[cfg(feature = "sd-check")]
pub async fn test_rotation(storage: &mut SdStorage) -> Result<(), Error> {
    storage
        .request_done(Command::RotationStart, IO_BUDGET)
        .await?;
    let mut payload = alloc::vec![b'R'; 2048];
    payload[2047] = b'\n';
    for _ in 0..66 {
        storage
            .request_done(
                Command::Append(payload.clone(), LogTarget::Rotation),
                IO_BUDGET,
            )
            .await?;
        embassy_time::Timer::after_millis(1).await;
    }
    storage
        .request_done(Command::RotationFinish, IO_BUDGET)
        .await?;
    esp_println::println!(
        "INFO - SD rotation check passed: active=4096 archive=131072 (isolated sdtest files retained)"
    );
    Ok(())
}

type DbFlash = PageStore<AsyncRegion<CardIo>, &'static esp_alloc::ExternalMemory>;
type MediaStorage = SectorStorage<MediaIo, &'static esp_alloc::ExternalMemory>;
type MediaFs = Filesystem<MediaStorage>;
type UnitStore = UnitMedia<RegionIo<CardIo>, &'static esp_alloc::ExternalMemory>;
type PodcastOwner = Runtime<DbFlash, UnitStore>;
type Psram<T> = Box<T, &'static esp_alloc::ExternalMemory>;
/// Largest one-shot future the owner places in PSRAM (`handle`'s is ~76 KB).
const STEP_FUTURE_BYTES: usize = 96 * 1024;
enum MediaIo {
    Legacy(CardIo),
    Region(RegionIo<CardIo>),
}
impl SectorIo for MediaIo {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        match self {
            Self::Legacy(io) => io.read_sector(sector, out),
            Self::Region(io) => io.read_sector(sector, out),
        }
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        match self {
            Self::Legacy(io) => io.write_sector(sector, data),
            Self::Region(io) => io.write_sector(sector, data),
        }
    }
    fn sync(&mut self) -> Result<(), Error> {
        match self {
            Self::Legacy(io) => io.sync(),
            Self::Region(io) => io.sync(),
        }
    }
    fn check(&mut self) -> Result<(), Error> {
        match self {
            Self::Legacy(io) => io.check(),
            Self::Region(io) => io.check(),
        }
    }
    fn device_failed(&self) -> bool {
        match self {
            Self::Legacy(io) => io.device_failed(),
            Self::Region(io) => io.device_failed(),
        }
    }
}
/// Mount order: descriptors (bus scope), log LittleFS (log scope), catalogue
/// and media units (podcast scope). One scope failing leaves the other
/// mounted; the mount fails only on a descriptor error or a poisoned bus.
///
/// Core1's 48 KiB stack cannot hold the podcast state (t-121): the runtime
/// (33 KB) and the catalogue (10 KB) are built in PSRAM, never moved by
/// value, and each large step runs through [`apart`].
async fn mount_owner(
    mut card: CardIo,
    legacy: Geometry,
    podcasts: &mut Option<Psram<PodcastOwner>>,
    allocated: bool,
) -> Result<Option<Rc<MediaFs>>, Error> {
    let mut first = [0; 512];
    let mut second = [0; 512];
    card.read_sector(0, &mut first)?;
    card.read_sector(8, &mut second)?;
    // Actual capacity is retained, not rounded geometry, for descriptor checks.
    let layout = CardLayout::select(&first, &second, card.sectors).map_err(|_| Error::Corrupt)?;
    let (io, geometry, database) = match layout {
        None => (MediaIo::Legacy(card), Ok(legacy), None),
        Some(layout) => {
            let (db, log, media) = regions::split(card, layout);
            (
                MediaIo::Region(log),
                Geometry::new(layout.log().sectors() as u64),
                Some((db, media, layout)),
            )
        }
    };
    let legacy_card = database.is_none();
    let logs = match mount_logs(io, geometry, legacy_card) {
        Ok(fs) => Some(Rc::new(fs)),
        Err(error) => {
            log::warn!("SD logs offline: mount failed: {error:?}");
            None
        }
    };
    *podcasts = None;
    match database {
        Some((db, media, layout)) if allocated => open_podcasts(db, media, layout, podcasts).await,
        Some(_) => log::warn!("podcasts offline: PSRAM scratch allocation failed"),
        None => log::info!("podcasts offline: existing whole-card LittleFS preserved"),
    }
    if SECTORS.is_disabled() {
        return Err(Error::Io);
    }
    Ok(logs)
}

#[cfg_attr(
    not(feature = "sd-provision"),
    expect(
        unused_variables,
        reason = "only the sd-provision build formats, and only a legacy card"
    )
)]
fn mount_logs(
    io: MediaIo,
    geometry: Result<Geometry, Error>,
    legacy_card: bool,
) -> Result<MediaFs, Error> {
    let geometry = geometry?;
    let storage = SectorStorage::try_new_in(io, geometry, &esp_alloc::ExternalMemory)?;
    // Explicit provisioning remains legacy-only; normal boot never formats.
    #[cfg(feature = "sd-provision")]
    if option_env!("THEO_SD_PROVISION") == Some("format") && legacy_card {
        let (fs, _) = littlefs::provision_once(storage, geometry)?;
        littlefs::prepare_for_writes(&fs)?;
        return Ok(fs);
    }
    littlefs::mount_for_writes(storage, geometry)
}

/// Open the catalogue and media units; a failure leaves podcasts offline.
async fn open_podcasts(
    db: RegionIo<CardIo>,
    media: RegionIo<CardIo>,
    layout: CardLayout,
    slot: &mut Option<Psram<PodcastOwner>>,
) {
    let Ok(mut catalogue) = Box::try_new_in(None, &esp_alloc::ExternalMemory) else {
        log::warn!("podcasts offline: PSRAM catalogue allocation failed");
        return;
    };
    let opened = apart(|| async {
        let store = PageStore::open_in(
            AsyncRegion::new(db),
            layout.pages(),
            &esp_alloc::ExternalMemory,
        )
        .await;
        match store {
            Ok(store) => Catalogue::open_into(&mut catalogue, store)
                .await
                .map_err(|error| log::warn!("podcasts offline: catalogue open failed: {error:?}")),
            Err(PageError::Allocation) => {
                log::warn!("podcasts offline: PSRAM page store allocation failed");
                Err(())
            }
            Err(error) => {
                log::warn!("podcasts offline: page store open failed: {error:?}");
                Err(())
            }
        }
    })
    .await;
    let Some(open) = catalogue.as_mut() else {
        if opened.is_err() {
            log::warn!("podcasts offline: PSRAM open state allocation failed");
        }
        return;
    };
    let units = apart(|| {
        UnitMedia::open_in(
            open,
            media,
            layout.media_units(),
            &esp_alloc::ExternalMemory,
        )
    })
    .await;
    match units {
        Ok(Ok(units)) => install(slot, &mut catalogue, units),
        Ok(Err(UnitError::Alloc)) | Err(_) => {
            log::warn!("podcasts offline: PSRAM media unit allocation failed")
        }
        Ok(Err(error)) => log::warn!("podcasts offline: media units open failed: {error:?}"),
    }
}

/// Build the runtime in PSRAM; out of line so its temporaries stay off the
/// owner's frame.
#[inline(never)]
fn install(
    slot: &mut Option<Psram<PodcastOwner>>,
    catalogue: &mut Option<Catalogue<DbFlash>>,
    units: UnitStore,
) {
    let Some(catalogue) = catalogue.take() else {
        return;
    };
    match Box::try_new_uninit_in(&esp_alloc::ExternalMemory) {
        Ok(memory) => *slot = Some(Box::write(memory, Runtime::new(catalogue, units))),
        Err(_) => log::warn!("podcasts offline: PSRAM runtime allocation failed"),
    }
}

/// Await `make()`'s future from PSRAM, polled in a frame of its own: built
/// in place it is neither moved through core1's stack nor inlined into the
/// owner's frame. `Err` when PSRAM cannot hold it.
async fn apart<F: core::future::Future>(make: impl FnOnce() -> F) -> Result<F::Output, AllocError> {
    theo_storage::future::run_in(make, &esp_alloc::ExternalMemory, STEP_FUTURE_BYTES).await
}
