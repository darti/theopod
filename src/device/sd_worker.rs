//! Core 1 owns the card and everything stored on it: it identifies the card
//! over blocking SPI3, then runs LittleFS, the catalogue and the media units
//! without waiting for core 0 (ADR-0023). Only identification asks core 0 to
//! drive chip select over the shared I2C bus; core 0 sends owned requests and
//! receives owned replies.

use aligned::{A4, Aligned};
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
    catalogue::{Catalogue, Error as CatalogueError},
    provision::CardId,
    runtime::{self, Runtime},
    unit_media::{UnitError, UnitMedia},
};
use theo_storage::{
    LogBuffer,
    littlefs::{self, Geometry, LogTail, RequestGuard, SectorIo, SectorStorage},
};
use theo_storage::{
    card_layout::{CardLayout, LayoutError},
    redb_backend::{BackendError, RegionBackend},
    regions::{self, RegionIo},
    scope::{OWNER_MARGIN_MS, ReplyKind, RequestKind, after_failure, failure_scope},
};

use super::{HalDevice, SdInitError};

const IO_BUDGET: Duration = Duration::from_millis(500);
/// Budget of one podcast (`Domain`) request: at most one catalogue commit
/// (ADR-0022). User-approved 2026-11-12: cold-cache redb commits measured
/// ~250 commands (2 ms each), at the 500 ms `IO_BUDGET`; 1 s leaves room.
/// Log append, tail, rotation and mount keep their budgets.
pub(in crate::device) const PODCAST_BUDGET: Duration = Duration::from_secs(1);
/// Budget of the mount request: descriptors, log mount and catalogue open.
const MOUNT_BUDGET: Duration = Duration::from_secs(2);
// The owner's margin must leave it time to work within every request budget.
const _: () = assert!(
    0 < OWNER_MARGIN_MS
        && OWNER_MARGIN_MS < IO_BUDGET.as_millis()
        && OWNER_MARGIN_MS < PODCAST_BUDGET.as_millis()
        && OWNER_MARGIN_MS < MOUNT_BUDGET.as_millis()
);
/// Set once the card or a request failed: storage stays offline until
/// reboot (ADR-0010). Shared by core 0's requests and core 1's owner.
static DISABLED: AtomicBool = AtomicBool::new(false);
fn is_disabled() -> bool {
    DISABLED.load(Ordering::Acquire)
}
/// Identification's outcome, from core 1 to `init` on core 0.
static IDENTIFIED: Signal<CriticalSectionRawMutex, Result<(), SdInitError>> = Signal::new();
/// Identification's own budget; `init`'s caller allows 3 s in all.
const IDENTIFY_BUDGET: Duration = Duration::from_millis(2500);
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
        disable();
    }
}

impl SdStorage {
    async fn request(&mut self, command: Command, budget: Duration) -> Result<Reply, Error> {
        let guard = RequestGuard::begin(&DISABLED)?;
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

/// Start core 1, which identifies the card then runs the storage owner.
///
/// Core 0 drives chip select over the shared I2C bus while core 1
/// identifies; afterwards the card stays selected and core 1 drives SPI3
/// alone (ADR-0023).
pub async fn init(
    core: (CPU_CTRL<'static>, FROM_CPU_INTR1<'static>),
    spi: SPI3<'static>,
    clk: GPIO2<'static>,
    mosi: GPIO1<'static>,
    miso: GPIO3<'static>,
    expander: &'static crate::device::Expander,
) -> Result<SdStorage, SdInitError> {
    let guard = RequestGuard::begin(&DISABLED).map_err(|_| SdInitError::Host)?;
    let (cpu, interrupt) = core;
    let stack = STACK.take();
    let stack_bottom = stack.bottom() as usize;
    esp_rtos::start_second_core(cpu, interrupt, stack, move || {
        let deadline = Instant::now() + IDENTIFY_BUDGET;
        let identified = super::identify(spi, clk, mosi, miso, deadline).and_then(
            |(device, sectors, card_id)| {
                Geometry::new(sectors)
                    .map(|geometry| (device, geometry, sectors, card_id))
                    .map_err(|_| SdInitError::Card(sdio::MmcError::BlockSize))
            },
        );
        let (device, geometry, sectors, card_id) = match identified {
            Ok(owner) => owner,
            Err(error) => {
                IDENTIFIED.signal(Err(error));
                return;
            }
        };
        // Core 0 gave up (its caller's timeout): stay offline.
        if is_disabled() {
            return;
        }
        IDENTIFIED.signal(Ok(()));
        static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
        EXECUTOR
            .init(esp_rtos::embassy::Executor::new())
            .run(|spawner| {
                spawner.spawn(
                    worker(device, geometry, sectors, card_id, stack_bottom)
                        .expect("SD owner once"),
                );
            });
    });
    let deadline = Instant::now() + IDENTIFY_BUDGET + Duration::from_millis(OWNER_MARGIN_MS);
    let outcome = with_deadline(
        deadline,
        embassy_futures::select::select(super::serve_chip_select(expander), IDENTIFIED.wait()),
    )
    .await;
    match outcome {
        Ok(embassy_futures::select::Either::Second(Ok(()))) => {}
        Ok(embassy_futures::select::Either::Second(Err(error))) => return Err(error),
        Ok(embassy_futures::select::Either::First(never)) => match never {},
        Err(_) => return Err(SdInitError::Card(sdio::MmcError::Timeout)),
    }
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
    DISABLED.store(true, Ordering::Release);
}

/// Whether this build provisions layout v3 (ADR-0021).
///
/// Core0 must then send no request: the run holds the owner for minutes,
/// and a timed-out request would poison the bus mid-run.
pub const fn provisions_v3() -> bool {
    #[cfg(feature = "sd-provision")]
    return matches!(provision::REQUEST, provision::Request::V3(_));
    #[cfg(not(feature = "sd-provision"))]
    false
}
pub fn guard() -> Result<RequestGuard<'static>, Error> {
    RequestGuard::begin(&DISABLED)
}

pub async fn domain(
    storage: &mut SdStorage,
    command: PodcastRequest,
) -> Result<runtime::Reply, Error> {
    match storage
        .request(Command::Domain(command), PODCAST_BUDGET)
        .await?
    {
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

#[embassy_executor::task]
async fn worker(
    device: HalDevice,
    geometry: Geometry,
    sectors: u64,
    card_id: Option<CardId>,
    stack_bottom: usize,
) {
    // Keep Embassy's task header/atomic wake state in internal memory. Only
    // the large, pinned domain future is placed externally; cache-sensitive
    // HAL and sector I/O tasks retain their existing internal storage.
    match theo_storage::future::try_pin_in(
        move || owner(device, geometry, sectors, card_id, stack_bottom),
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
            disable();
            log::warn!("storage offline: PSRAM owner allocation failed");
        }
    }
}
async fn owner(
    device: HalDevice,
    geometry: Geometry,
    sectors: u64,
    card_id: Option<CardId>,
    _stack_bottom: usize,
) {
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
    let Ok(staged) = Box::try_new_in(
        [Aligned::<A4, _>([0u8; 512]); STAGED_SECTORS],
        &esp_alloc::ExternalMemory,
    ) else {
        disable();
        log::warn!("storage offline: PSRAM staging allocation failed");
        return;
    };
    let mut card = Some(CardIo {
        sectors,
        device,
        staged,
        #[cfg(feature = "sd-check")]
        measurements: measurements.clone(),
        deadline: deadline.clone(),
        soft: soft.clone(),
    });
    let mut volume = None;
    let mut podcasts = None;
    let mut scratch = allocator_api2::vec::Vec::new_in(&esp_alloc::ExternalMemory);
    if scratch.try_reserve_exact(4096).is_ok() {
        scratch.resize(4096, 0);
    }
    // Explicit provisioning build only: runs before any request is served.
    #[cfg(feature = "sd-provision")]
    if let Some(io) = card.as_mut()
        && provision::run(io, card_id).await
    {
        // The card was being erased: keep it offline for this boot; the
        // normal firmware mounts the new layout.
        card = None;
    }
    #[cfg(not(feature = "sd-provision"))]
    let _ = card_id;

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
        let result = if is_disabled() || Instant::now() >= soft.get() {
            Err(Error::Io)
        } else {
            match request.command {
                Command::Mount => match card.take() {
                    Some(card) => {
                        let allocated = !scratch.is_empty();
                        match in_psram(|| mount_owner(card, geometry, &mut podcasts, allocated))
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
                        match in_psram(|| owner.handle(Box::into_inner(command), &mut scratch))
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
        let poisoned = is_disabled();
        let result = if result.is_ok() && !poisoned {
            result
        } else {
            // A scope failure is a completed reply: the caller's guard
            // completes and only a bus failure poisons the session.
            let effect = after_failure(failure_scope(poisoned, kind));
            if effect.disable_bus {
                disable();
                card = None;
            }
            if effect.drop_logs {
                // On a legacy whole-card mount the log volume holds the only
                // `CardIo`: dropping it disables the card, as losing the last live scope does on a
                // region card.
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

type MediaStorage = SectorStorage<MediaIo, &'static esp_alloc::ExternalMemory>;
type MediaFs = Filesystem<MediaStorage>;
type UnitStore = UnitMedia<RegionIo<CardIo>, &'static esp_alloc::ExternalMemory>;
type PodcastOwner = Runtime<UnitStore>;
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
/// is built in PSRAM, never moved by value, and each large step runs
/// through [`in_psram`].
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
    let layout = match CardLayout::select(&first, &second, card.sectors) {
        Ok(layout) => layout,
        Err(error) => {
            match error {
                LayoutError::Obsolete => log::warn!(
                    "microSD layout is older than v3: re-provision with `just sd-provision-v3`"
                ),
                LayoutError::Version => {
                    log::warn!("microSD layout version is unknown to this firmware")
                }
                _ => log::warn!("microSD layout refused: {error:?}"),
            }
            return Err(Error::Corrupt);
        }
    };
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
    if is_disabled() {
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
    if provision::REQUEST == provision::Request::Format && legacy_card {
        let (fs, _) = littlefs::provision_once(storage, geometry)?;
        littlefs::prepare_for_writes(&fs)?;
        return Ok(fs);
    }
    littlefs::mount_for_writes(storage, geometry)
}

/// Open the catalogue and media units; a failure leaves podcasts offline.
///
/// The catalogue (redb, its page cache in PSRAM) and the unit table open
/// synchronously, each through [`in_psram`] so their frames stay off the
/// owner's; the runtime is then built in PSRAM by [`install`].
async fn open_podcasts(
    db: RegionIo<CardIo>,
    media: RegionIo<CardIo>,
    layout: CardLayout,
    slot: &mut Option<Psram<PodcastOwner>>,
) {
    let started = Instant::now();
    let Ok(opened) = in_psram(|| async { open_catalogue(db, layout) }).await else {
        log::warn!("podcasts offline: PSRAM open state allocation failed");
        return;
    };
    let Some(mut catalogue) = opened else {
        return;
    };
    let units = in_psram(|| async {
        UnitMedia::open_in(
            &mut catalogue,
            media,
            layout.media_units(),
            &esp_alloc::ExternalMemory,
        )
    })
    .await;
    match units {
        Ok(Ok(units)) => {
            if install(slot, catalogue, units) {
                log::info!(
                    "podcasts online: catalogue opened in {} ms",
                    started.elapsed().as_millis()
                );
            }
        }
        Ok(Err(UnitError::Alloc)) | Err(_) => {
            log::warn!("podcasts offline: PSRAM media unit allocation failed")
        }
        Ok(Err(error)) => log::warn!("podcasts offline: media units open failed: {error:?}"),
    }
}

/// A corrupt catalogue is never repaired at boot (ADR-0015); the explicit
/// provisioning build accepts such a v3 card.
const CORRUPT_HINT: &str = "Corrupt; re-provision (erases the card) with `just sd-provision-v3`";

/// Open the redb catalogue on the database region; `None` (logged) leaves
/// podcasts offline.
#[inline(never)]
fn open_catalogue(db: RegionIo<CardIo>, layout: CardLayout) -> Option<Catalogue> {
    let sectors = layout.database().sectors();
    // SAFETY: the storage owner creates, uses and drops the catalogue and
    // this backend on core1 only; `PodcastOwner` holds `Rc`s, so it is
    // `!Send` and the compiler keeps the database on this core.
    let backend = match unsafe { RegionBackend::new(db, sectors) } {
        Ok(backend) => backend,
        Err(BackendError::Corrupt) => {
            log::warn!("podcasts offline: database region unusable: {CORRUPT_HINT}");
            return None;
        }
        Err(error) => {
            log::warn!("podcasts offline: database region unusable: {error:?}");
            return None;
        }
    };
    match Catalogue::open(backend) {
        Ok(catalogue) => Some(catalogue),
        Err(CatalogueError::Corrupt) => {
            log::warn!("podcasts offline: catalogue open failed: {CORRUPT_HINT}");
            None
        }
        Err(error) => {
            log::warn!("podcasts offline: catalogue open failed: {error:?}");
            None
        }
    }
}

/// Build the runtime in PSRAM; out of line so its temporaries stay off the
/// owner's frame. Whether the runtime was installed.
#[inline(never)]
fn install(slot: &mut Option<Psram<PodcastOwner>>, catalogue: Catalogue, units: UnitStore) -> bool {
    match Box::try_new_uninit_in(&esp_alloc::ExternalMemory) {
        Ok(memory) => {
            *slot = Some(Box::write(memory, Runtime::new(catalogue, units)));
            true
        }
        Err(_) => {
            log::warn!("podcasts offline: PSRAM runtime allocation failed");
            false
        }
    }
}

/// Await `make()`'s future from PSRAM, polled in a frame of its own: built
/// in place it is neither moved through core1's stack nor inlined into the
/// owner's frame. `Err` when PSRAM cannot hold it.
async fn in_psram<F: core::future::Future>(
    make: impl FnOnce() -> F,
) -> Result<F::Output, AllocError> {
    theo_storage::future::run_in(make, &esp_alloc::ExternalMemory, STEP_FUTURE_BYTES).await
}

#[path = "sd_card_io.rs"]
mod card_io;
use card_io::{CardIo, STAGED_SECTORS};
#[cfg(feature = "sd-check")]
use card_io::{Measurements, TRACED_SECTORS};

#[cfg(feature = "sd-provision")]
#[path = "sd_provision.rs"]
mod provision;
