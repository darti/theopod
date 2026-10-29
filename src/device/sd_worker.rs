//! Core 1 owns LittleFS; core 0 keeps the async SD HAL device. Only owned messages cross
//! cores. Its synchronous callbacks may wait for async I2C while core 0 keeps
//! running touch/IMU tasks that can release that bus (ADR-0012).

use alloc::{rc::Rc, vec::Vec};
use core::cell::Cell;
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
use theo_storage::{
    LogBuffer,
    littlefs::{self, Geometry, LogTail, RequestGuard, SectorIo, SectorStorage},
};

use super::{SdInitError, io};
use theo_storage::sector_bus::{Operation, Request as SectorRequest, SectorBus, SectorClient};

const IO_BUDGET: Duration = Duration::from_millis(500);
pub(super) static SECTORS: SectorBus = SectorBus::new();
static COMMANDS: Channel<CriticalSectionRawMutex, Request, 1> = Channel::new();
static REPLY: Signal<CriticalSectionRawMutex, Result<Reply, Error>> = Signal::new();
const STACK_SIZE: usize = 32768;
static STACK: ConstStaticCell<Stack<STACK_SIZE>> = ConstStaticCell::new(Stack::new());

enum Command {
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
struct Request {
    command: Command,
    deadline: Instant,
}
// Fixed inline replies keep the one-slot mailbox bounded without allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Reply {
    Done,
    Tail(LogTail),
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
                spawner.spawn(worker(geometry, stack_bottom).expect("SD owner once"));
            });
    });
    guard.complete().map_err(|_| SdInitError::Host)?;
    Ok(SdStorage { _private: () })
}

pub async fn mount(mut storage: SdStorage) -> Result<SdStorage, Error> {
    storage
        .request(Command::Mount, Duration::from_secs(2))
        .await?;
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
    storage
        .request(
            Command::Append(buffer.pending().to_vec(), LogTarget::Live),
            IO_BUDGET,
        )
        .await?;
    buffer.consume(buffer.len());
    Ok(())
}

pub async fn report_log_tail(storage: &mut SdStorage) -> Result<(), Error> {
    let Reply::Tail(tail) = storage.request(Command::Tail, IO_BUDGET).await? else {
        return Err(Error::Io);
    };
    esp_println::println!(
        "INFO - microSD previous theo.log: {} bytes; tail:\n{}",
        tail.size,
        core::str::from_utf8(&tail.bytes[..tail.len]).unwrap_or("<UTF-8 boundary>")
    );
    Ok(())
}

#[cfg(feature = "sd-check")]
struct Measurements {
    stack_bottom: usize,
    min_sampled_headroom: Cell<usize>,
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
    client: SectorClient<'static>,
    #[cfg(feature = "sd-check")]
    measurements: Rc<Measurements>,
    deadline: Rc<Cell<Instant>>,
}

impl CardIo {
    fn check(&self) -> Result<(), Error> {
        #[cfg(feature = "sd-check")]
        self.measurements.sample();
        if SECTORS.is_disabled() || Instant::now() >= self.deadline.get() {
            SECTORS.disable();
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}

impl CardIo {
    fn request(&mut self, operation: Operation) -> Result<[u8; 512], Error> {
        self.check()?;
        let deadline = self.deadline.get();
        // Core 0's executor runs the async SD/I2C service while this core
        // waits. Its channels carry owned buffers only.
        self.client.exchange_blocking(
            SectorRequest {
                operation,
                deadline_ticks: deadline.as_ticks(),
            },
            || SECTORS.is_disabled() || Instant::now() >= deadline,
        )
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
}

#[embassy_executor::task]
async fn worker(geometry: Geometry, _stack_bottom: usize) {
    #[cfg(feature = "sd-check")]
    let measurements = Rc::new(Measurements {
        stack_bottom: _stack_bottom,
        min_sampled_headroom: Cell::new(STACK_SIZE),
    });
    let deadline = Rc::new(Cell::new(Instant::now()));
    let mut card = Some(SectorStorage::new(
        CardIo {
            #[cfg(feature = "sd-check")]
            measurements: measurements.clone(),
            client: SECTORS.take_client().expect("one sector client"),
            deadline: deadline.clone(),
        },
        geometry,
    ));
    let mut volume = None;
    loop {
        let request = COMMANDS.receive().await;
        deadline.set(request.deadline);
        #[cfg(feature = "sd-check")]
        let label = match &request.command {
            Command::Mount => "mount",
            Command::Append(..) => "append",
            Command::Tail => "tail",
            Command::RotationStart => "rotation-start",
            Command::RotationFinish => "rotation-finish",
        };
        #[cfg(feature = "sd-check")]
        let started = Instant::now();
        let result = if SECTORS.is_disabled() || Instant::now() >= request.deadline {
            Err(Error::Io)
        } else {
            match request.command {
                Command::Mount => match card.take() {
                    Some(card) => match mount_card(card, geometry) {
                        Ok(fs) => {
                            volume = Some(fs);
                            Ok(Reply::Done)
                        }
                        Err((error, _)) => Err(error),
                    },
                    None => Err(Error::Io),
                },
                Command::Append(bytes, target) => match volume.as_ref() {
                    Some(fs) => {
                        let mut buffer = LogBuffer::<4096>::new();
                        buffer.push(&bytes);
                        let (name, archive) = target.paths();
                        littlefs::append_log(fs, &mut buffer, name, archive).map(|()| Reply::Done)
                    }
                    None => Err(Error::Io),
                },
                Command::Tail => match volume.as_ref() {
                    Some(fs) => littlefs::log_tail(fs, "/theo.log").map(Reply::Tail),
                    None => Err(Error::Io),
                },
                #[cfg(feature = "sd-check")]
                Command::RotationStart => match volume.as_ref() {
                    Some(fs) => rotation_start(fs).map(|()| Reply::Done),
                    None => Err(Error::Io),
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
                    None => Err(Error::Io),
                },
            }
        };
        if result.is_err() || SECTORS.is_disabled() {
            SECTORS.disable();
            volume = None;
            card = None;
        }
        #[cfg(feature = "sd-check")]
        esp_println::println!(
            "INFO - SD measurement: op={} elapsed_us={} sampled_stack_margin={} status={}",
            label,
            started.elapsed().as_micros(),
            measurements.min_sampled_headroom.get(),
            if result.is_ok() { "ok" } else { "error" }
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
    storage.request(Command::RotationStart, IO_BUDGET).await?;
    let mut payload = alloc::vec![b'R'; 2048];
    payload[2047] = b'\n';
    for _ in 0..66 {
        storage
            .request(
                Command::Append(payload.clone(), LogTarget::Rotation),
                IO_BUDGET,
            )
            .await?;
        embassy_time::Timer::after_millis(1).await;
    }
    storage.request(Command::RotationFinish, IO_BUDGET).await?;
    esp_println::println!(
        "INFO - SD rotation check passed: active=4096 archive=131072 (isolated sdtest files retained)"
    );
    Ok(())
}

fn mount_card(
    card: SectorStorage<CardIo>,
    geometry: Geometry,
) -> Result<Filesystem<SectorStorage<CardIo>>, (Error, ())> {
    #[cfg(feature = "sd-provision")]
    if option_env!("THEO_SD_PROVISION") == Some("format") {
        return littlefs::provision_once(card, geometry)
            .map(|(fs, formatted)| {
                esp_println::println!(
                    "INFO - microSD provisioning: {}",
                    if formatted {
                        "formatted LittleFS"
                    } else {
                        "existing LittleFS preserved"
                    }
                );
                fs
            })
            .map_err(|error| (error, ()));
    }
    Filesystem::mount(card, geometry.config()).map_err(|(error, _)| (error, ()))
}
