//! Board-local microSD SPI bring-up and the hadris block-device adapter.

use alloc::{boxed::Box, rc::Rc};
use core::{
    cell::Cell,
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use aligned::{A4, Aligned};
use embassy_time::Delay;
use embedded_hal_async::{delay::DelayNs, spi::SpiBus as AsyncSpiBus};
use esp_hal::{
    Async,
    peripherals::{GPIO1, GPIO2, GPIO3, SPI3},
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
};
use hadris_fat::embedded::async_::Fat;
use hadris_fs::OpenOptions;
use hadris_io::{Error as IoError, ErrorKind, ErrorType, SeekFrom};
use hadris_storage::async_::{BlockDevice, Stream, StreamDevice};
use hadris_storage::{BlockIndex, Partition};
use sdio::{
    BlockReadCommand, BlockWriteCommand, BusWidth, ByteReadCommand, ByteWriteCommand,
    ControlCommand, MmcBus, MmcError, Response, ResponseLen,
};
use theo_storage::LogBuffer;

const BLOCK_SIZE: usize = 512;
const CARD_FREQUENCY_HZ: u32 = 25_000_000;

/// SDCS is idle high; SPI asserts it low for each transaction.
pub const CARD_SELECT_LEVEL: embedded_hal::digital::PinState =
    embedded_hal::digital::PinState::High;

#[derive(Debug, Clone, Copy)]
pub enum SdStatus {
    Mounted,
    InitFailed(SdInitError),
    MountFailed,
}

pub const fn status_message(status: SdStatus) -> &'static str {
    match status {
        SdStatus::Mounted => "mounted",
        SdStatus::InitFailed(_) => "initialization failed",
        SdStatus::MountFailed => "FAT mount failed",
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SdInitError {
    Host,
    Card(sdio::MmcError),
}

pub const fn init_error_message(error: SdInitError) -> &'static str {
    match error {
        SdInitError::Host => "SD SPI host configuration failed",
        SdInitError::Card(error) => match error {
            sdio::MmcError::Timeout => "SD card timeout",
            sdio::MmcError::Crc => "SD card CRC error",
            sdio::MmcError::Busy => "SD card busy",
            sdio::MmcError::Io => "SD card I/O error",
            sdio::MmcError::BlockSize => "SD card block-size error",
            sdio::MmcError::Signaling => "SD card signaling error",
            sdio::MmcError::BusWidth => "SD card bus-width error",
            sdio::MmcError::Voltage => "SD card voltage error",
            sdio::MmcError::CardType => "unsupported SD card type",
            sdio::MmcError::Unsupported => "unsupported SD card feature",
            sdio::MmcError::Card(_) => "SD card protocol error",
            sdio::MmcError::Sdio(_) => "SDIO protocol error",
            sdio::MmcError::Other => "other SD card error",
            _ => "unknown SD card error",
        },
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SdError;

impl fmt::Display for SdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SD SPI operation failed")
    }
}

impl core::error::Error for SdError {}

type SpiBus = SdMmcBus;
type HalDevice = sdio::DefaultBlockDevice<sdio::sd::Card, SpiBus, Delay>;
type Scratch = Aligned<A4, [u8; BLOCK_SIZE]>;
type Transfer =
    Pin<Box<dyn Future<Output = (Result<(), SdError>, HalDevice, Box<Scratch>)> + 'static>>;

#[derive(Default)]
pub struct SdState {
    transfer: Option<Transfer>,
}

/// The V2 routes SDCS to TCA9554 EXIO7. Always await the shared I2C bus:
/// blocking a nested executor here would deadlock concurrent touch/IMU I/O.
struct ExpanderCs {
    expander: &'static super::Expander,
}

impl ExpanderCs {
    async fn set(&mut self, level: embedded_hal::digital::PinState) -> Result<(), MmcError> {
        self.expander
            .lock()
            .await
            .set_level(super::SD_CARD_SELECT, level)
            .await
            .map_err(|_| MmcError::Io)
    }
}

struct SdSpi(Spi<'static, Async>);

impl embedded_hal_async::spi::ErrorType for SdSpi {
    type Error = esp_hal::spi::Error;
}

impl embedded_hal_async::spi::SpiBus<u8> for SdSpi {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        // SD SPI requires 0xFF on MOSI while polling responses and reading
        // data. `SpiBus::read` leaves the dummy byte implementation-defined
        // (the ESP-HAL may transmit 0x00), so use an in-place transfer with
        // an explicit idle-byte fill.
        words.fill(0xFF);
        embedded_hal_async::spi::SpiBus::transfer_in_place(&mut self.0, words).await
    }

    async fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        embedded_hal_async::spi::SpiBus::write(&mut self.0, words).await
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        embedded_hal_async::spi::SpiBus::transfer(&mut self.0, read, write).await
    }

    async fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal_async::spi::SpiBus::transfer_in_place(&mut self.0, words).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_hal_async::spi::SpiBus::flush(&mut self.0).await
    }
}

impl SdSpi {
    fn set_hz(&mut self, hz: u32) {
        let _ = self
            .0
            .apply_config(&SpiConfig::default().with_frequency(Rate::from_hz(hz)));
    }
}

/// SD SPI command bus. This is local because `sdio 0.5.1` decodes SPI R1/R7
/// responses as if the leading R1 byte were part of the native 32-bit word.
struct SdMmcBus {
    spi: SdSpi,
    cs: ExpanderCs,
    delay: Delay,
    blocks: Rc<Cell<u64>>,
}

impl SdMmcBus {
    fn new(spi: SdSpi, cs: ExpanderCs, delay: Delay, blocks: Rc<Cell<u64>>) -> Self {
        Self {
            spi,
            cs,
            delay,
            blocks,
        }
    }

    async fn select(&mut self) -> Result<(), MmcError> {
        self.cs.set(embedded_hal::digital::PinState::Low).await
    }

    async fn deselect(&mut self) -> Result<(), MmcError> {
        self.cs.set(embedded_hal::digital::PinState::High).await?;
        self.spi.write(&[0xFF]).await.map_err(|_| MmcError::Io)
    }

    async fn send_header<C: sdio::Command>(&mut self, cmd: &C) -> Result<(), MmcError> {
        self.select().await?;
        let mut header = [
            0x40 | (cmd.index() & 0x3F),
            (cmd.arg() >> 24) as u8,
            (cmd.arg() >> 16) as u8,
            (cmd.arg() >> 8) as u8,
            cmd.arg() as u8,
            0x01,
        ];
        header[5] = (crc7(&header[..5]) << 1) | 1;
        self.spi.write(&header).await.map_err(|_| MmcError::Io)
    }

    async fn read_r1(&mut self) -> Result<u8, MmcError> {
        let mut byte = [0xFF];
        for _ in 0..8 {
            self.spi.read(&mut byte).await.map_err(|_| MmcError::Io)?;
            if byte[0] != 0xFF {
                return Ok(byte[0]);
            }
        }
        Err(MmcError::Timeout)
    }

    async fn read_response<R: Response>(&mut self, index: u8, r1: u8) -> Result<R, MmcError> {
        let mut words = [0u32; 4];
        match R::LEN {
            ResponseLen::Zero => {}
            ResponseLen::R48 => {
                words[0] = if matches!(index, 8 | 58) {
                    let mut payload = [0xFFu8; 4];
                    self.spi
                        .read(&mut payload)
                        .await
                        .map_err(|_| MmcError::Io)?;
                    u32::from_be_bytes(payload)
                } else if index == 13 {
                    // SPI CMD13 returns R1 plus one status byte. Translate
                    // a clean status to the native ready/transfer bits used
                    // by sdio's post-write R1b readiness polling.
                    let mut status = [0xFF];
                    self.spi.read(&mut status).await.map_err(|_| MmcError::Io)?;
                    if r1 != 0 || status[0] != 0 {
                        return Err(MmcError::Other);
                    }
                    (1 << 8) | (4 << 9)
                } else {
                    r1 as u32
                };
            }
            ResponseLen::R136 => {
                let mut payload = [0xFFu8; 16];
                // SPI CID/CSD registers are data blocks following a 0xFE
                // token, not native-mode inline 136-bit responses.
                self.read_block(&mut payload).await?;
                // sdio::R2 stores the least significant word first.
                for (word, bytes) in words.iter_mut().rev().zip(payload.chunks_exact(4)) {
                    *word = u32::from_be_bytes(bytes.try_into().unwrap());
                }
            }
        }
        Ok(R::from_words(&words))
    }

    async fn command<'a, C: ControlCommand + 'a>(
        &mut self,
        cmd: C,
    ) -> Result<C::Resp<'a>, MmcError> {
        self.send_header(&cmd).await?;
        let r1 = self.read_r1().await?;
        // In SD SPI mode ACMD41 returns only R1. The generic sdio acquisition
        // path expects an OCR, so expose only its busy/ready bit here; the
        // actual OCR is read by the SPI fallback's following CMD58.
        if cmd.index() == 41 {
            self.deselect().await?;
            let mut words = [0u32; 4];
            words[0] = if r1 & 1 == 0 { 1 << 31 } else { 0 };
            return Ok(C::Resp::from_words(&words));
        }
        let response = self.read_response::<C::Resp<'a>>(cmd.index(), r1).await?;
        self.deselect().await?;
        if cmd.index() == 58 {
            // sdio 0.5.1 returns from SPI acquisition immediately after
            // CMD58, leaving Card::csd at its default (four blocks).
            // Read the real CSD here while the initialized bus is available.
            self.send_header(&sdio::common::send_csd(0)).await?;
            let status = self.read_r1().await?;
            if status != 0 {
                self.deselect().await?;
                return Err(MmcError::Other);
            }
            let csd = self.read_response::<sdio::R2>(9, status).await?;
            self.deselect().await?;
            let csd: sdio::common::CSD<sdio::sd::SD> = csd.into();
            let blocks = csd.card_size() / BLOCK_SIZE as u64;
            if blocks == 0 || blocks > u32::MAX as u64 {
                return Err(MmcError::Unsupported);
            }
            self.blocks.set(blocks);
        }
        Ok(response)
    }

    async fn wait_not_busy(&mut self) -> Result<(), MmcError> {
        let mut byte = [0xFF];
        for _ in 0..65_536 {
            self.spi.read(&mut byte).await.map_err(|_| MmcError::Io)?;
            if byte[0] == 0xFF {
                return Ok(());
            }
        }
        Err(MmcError::Busy)
    }

    async fn read_block(&mut self, block: &mut [u8]) -> Result<(), MmcError> {
        let mut token = [0xFF];
        for _ in 0..65_536 {
            self.spi.read(&mut token).await.map_err(|_| MmcError::Io)?;
            if token[0] == 0xFE {
                self.spi.read(block).await.map_err(|_| MmcError::Io)?;
                let mut crc = [0xFF; 2];
                self.spi.read(&mut crc).await.map_err(|_| MmcError::Io)?;
                return Ok(());
            }
        }
        Err(MmcError::Timeout)
    }

    async fn write_block(&mut self, block: &[u8]) -> Result<(), MmcError> {
        self.spi.write(&[0xFE]).await.map_err(|_| MmcError::Io)?;
        self.spi.write(block).await.map_err(|_| MmcError::Io)?;
        self.spi
            .write(&[0xFF, 0xFF])
            .await
            .map_err(|_| MmcError::Io)?;
        let mut response = [0xFF];
        self.spi
            .read(&mut response)
            .await
            .map_err(|_| MmcError::Io)?;
        if response[0] & 0x1F != 0x05 {
            return Err(MmcError::Crc);
        }
        self.wait_not_busy().await
    }
}

fn crc7(data: &[u8]) -> u8 {
    let mut crc = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc ^= 0x89;
            }
            crc <<= 1;
        }
    }
    crc >> 1
}

impl MmcBus for SdMmcBus {
    async fn send_command<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ControlCommand + 'a,
    {
        self.command(cmd).await
    }

    async fn read_blocks<'a, C>(
        &mut self,
        mut cmd: C,
        auto_stop: bool,
    ) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockReadCommand + 'a,
    {
        if auto_stop {
            return Err(MmcError::Unsupported);
        }
        self.send_header(&cmd).await?;
        let r1 = self.read_r1().await?;
        if r1 & 0x7E != 0 {
            return Err(MmcError::Other);
        }
        let size = cmd.block_size().len();
        for block in cmd.buf().chunks_exact_mut(size) {
            self.read_block(block).await?;
        }
        self.deselect().await?;
        Ok(C::Resp::from_words(&[r1 as u32, 0, 0, 0]))
    }

    async fn write_blocks<'a, C>(
        &mut self,
        cmd: C,
        auto_stop: bool,
    ) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockWriteCommand + 'a,
    {
        if auto_stop {
            return Err(MmcError::Unsupported);
        }
        self.send_header(&cmd).await?;
        let r1 = self.read_r1().await?;
        if r1 & 0x7E != 0 {
            return Err(MmcError::Other);
        }
        let size = cmd.block_size().len();
        for block in cmd.buf().chunks_exact(size) {
            self.write_block(block).await?;
        }
        self.deselect().await?;
        Ok(C::Resp::from_words(&[r1 as u32, 0, 0, 0]))
    }

    async fn read_bytes<'a, C>(&mut self, mut cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteReadCommand + 'a,
    {
        self.send_header(&cmd).await?;
        let r1 = self.read_r1().await?;
        self.read_block(cmd.buf()).await?;
        self.deselect().await?;
        Ok(C::Resp::from_words(&[r1 as u32, 0, 0, 0]))
    }

    async fn write_bytes<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteWriteCommand + 'a,
    {
        self.send_header(&cmd).await?;
        let r1 = self.read_r1().await?;
        self.write_block(cmd.buf()).await?;
        self.deselect().await?;
        Ok(C::Resp::from_words(&[r1 as u32, 0, 0, 0]))
    }

    async fn init_idle(&mut self, hz: u32) -> Result<(), MmcError> {
        self.spi.set_hz(hz);
        self.cs.set(embedded_hal::digital::PinState::High).await?;
        self.spi
            .write(&[0xFF; 10])
            .await
            .map_err(|_| MmcError::Io)?;
        self.delay.delay_us(1000).await;
        Ok(())
    }

    fn set_bus(&mut self, _width: BusWidth, hz: u32) -> Result<(), MmcError> {
        self.spi.set_hz(hz);
        Ok(())
    }

    fn supports_frequency(&self) -> u32 {
        25_000_000
    }
}

/// A stream over a 512-byte SD card.
pub struct SdStream {
    device: Option<HalDevice>,
    blocks: u64,
    position: u64,
}

impl ErrorType for SdStream {
    type Error = SdError;
}

impl Stream for SdStream {
    type State = SdState;

    fn writable(&self) -> bool {
        true
    }

    fn poll_read(
        &mut self,
        state: &mut Self::State,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, IoError<Self::Error>>> {
        self.poll_transfer(state, cx, buf, false)
    }

    fn poll_seek(
        &mut self,
        _: &mut Self::State,
        _: &mut Context<'_>,
        pos: SeekFrom,
    ) -> Poll<Result<u64, IoError<Self::Error>>> {
        let next = match pos.resolve(self.position, self.blocks * BLOCK_SIZE as u64) {
            Some(next) => next,
            None => return Poll::Ready(Err(ErrorKind::InvalidInput.into())),
        };
        self.position = next;
        Poll::Ready(Ok(next))
    }

    fn poll_write(
        &mut self,
        state: &mut Self::State,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, IoError<Self::Error>>> {
        #[cfg(feature = "sd-check")]
        match option_env!("THEO_SD_FAULT") {
            Some("readonly") => return Poll::Ready(Err(ErrorKind::ReadOnly.into())),
            Some("full") => return Poll::Ready(Err(ErrorKind::NoSpace.into())),
            Some("removed") => {
                return Poll::Ready(Err(IoError::device(SdError, "injected card removal")));
            }
            Some("timeout") => return Poll::Pending,
            _ => {}
        }
        if theo_storage::sector_index(self.position, self.blocks, buf.len()).is_none() {
            return Poll::Ready(Err(ErrorKind::InvalidInput.into()));
        }
        let mut copy = [0u8; BLOCK_SIZE];
        copy.copy_from_slice(&buf[..BLOCK_SIZE]);
        self.poll_transfer(state, cx, &mut copy, true)
    }

    fn poll_flush(
        &mut self,
        _: &mut Self::State,
        _: &mut Context<'_>,
    ) -> Poll<Result<(), IoError<Self::Error>>> {
        Poll::Ready(Ok(()))
    }

    fn cancel(&mut self, state: &mut Self::State) {
        state.transfer = None;
    }
}

impl SdStream {
    fn poll_transfer(
        &mut self,
        state: &mut SdState,
        cx: &mut Context<'_>,
        buf: &mut [u8],
        write: bool,
    ) -> Poll<Result<usize, IoError<SdError>>> {
        let Some(block) = theo_storage::sector_index(self.position, self.blocks, buf.len()) else {
            return Poll::Ready(Err(ErrorKind::InvalidInput.into()));
        };
        if state.transfer.is_none() {
            let mut scratch = Box::new(Aligned::<A4, [u8; BLOCK_SIZE]>([0; BLOCK_SIZE]));
            if write {
                scratch.copy_from_slice(&buf[..BLOCK_SIZE]);
            }
            let Some(mut device) = self.device.take() else {
                return Poll::Ready(Err(IoError::device(
                    SdError,
                    "SD transfer cancelled; reboot to retry",
                )));
            };
            let transfer: Transfer = if write {
                Box::pin(async move {
                    let result = block_device_driver::BlockDevice::write(
                        &mut device,
                        block,
                        core::slice::from_ref(&*scratch),
                    )
                    .await
                    .map_err(|error| {
                        log::error!("microSD block write error: {error:?}");
                        SdError
                    });
                    (result, device, scratch)
                })
            } else {
                Box::pin(async move {
                    let result = block_device_driver::BlockDevice::read(
                        &mut device,
                        block,
                        core::slice::from_mut(&mut *scratch),
                    )
                    .await
                    .map_err(|error| {
                        log::error!("microSD block read error: {error:?}");
                        SdError
                    });
                    (result, device, scratch)
                })
            };
            state.transfer = Some(transfer);
        }
        match state.transfer.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready((result, device, scratch)) => {
                state.transfer = None;
                self.device = Some(device);
                if let Err(error) = result {
                    return Poll::Ready(Err(IoError::device(error, "SD block transfer failed")));
                }
                if !write {
                    buf[..BLOCK_SIZE].copy_from_slice(&scratch[..]);
                }
                self.position += BLOCK_SIZE as u64;
                Poll::Ready(Ok(BLOCK_SIZE))
            }
        }
    }
}

/// A FAT-compatible SD block device.
pub type SdBlockDevice = StreamDevice<SdStream>;
pub type SdStorage = Fat<'static, Partition<SdBlockDevice>, 4>;

/// Initializes the microSD SPI slot and identifies the card.
pub async fn init(
    spi3: SPI3<'static>,
    clk: GPIO2<'static>,
    mosi: GPIO1<'static>,
    miso: GPIO3<'static>,
    expander: &'static super::Expander,
) -> Result<SdBlockDevice, SdInitError> {
    let spi = Spi::new(
        spi3,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(CARD_FREQUENCY_HZ / 1_000_000))
            .with_mode(Mode::_0),
    )
    .map_err(|_| SdInitError::Host)?
    .with_sck(clk)
    .with_mosi(mosi)
    .with_miso(miso)
    .into_async();
    let capacity = Rc::new(Cell::new(0));
    let bus = SdMmcBus::new(SdSpi(spi), ExpanderCs { expander }, Delay, capacity.clone());
    let device: HalDevice = sdio::DefaultBlockDevice::new(bus, Delay, CARD_FREQUENCY_HZ)
        .await
        .map_err(SdInitError::Card)?;
    let blocks = capacity.get();
    log::info!(
        "microSD capacity: {} blocks ({} MiB)",
        blocks,
        blocks / 2048
    );
    let device = Some(device);
    Ok(StreamDevice::with_block_count(
        SdStream {
            device,
            blocks,
            position: 0,
        },
        hadris_storage::BlockSize::new(BLOCK_SIZE as u32).unwrap(),
        blocks,
    ))
}

/// Mounts a card and keeps its FAT volume alive for the board lifetime.
pub async fn mount(device: SdBlockDevice) -> Result<SdStorage, ErrorKind> {
    log::info!("microSD FAT mount: reading boot sector");
    let mut device = device;
    let device_blocks = device.block_count();
    let mut mbr = [0u8; BLOCK_SIZE];
    BlockDevice::read_blocks(&mut device, BlockIndex::new(0), &mut mbr)
        .await
        .map_err(|error| error.kind())?;

    let bytes_per_sector = u16::from_le_bytes([mbr[11], mbr[12]]);
    let partition = if bytes_per_sector == BLOCK_SIZE as u16 {
        Partition::new(device, 0, device_blocks * BLOCK_SIZE as u64)
    } else if mbr[510] == 0x55 && mbr[511] == 0xAA {
        let entry = (0..4)
            .map(|index| &mbr[446 + index * 16..462 + index * 16])
            .find(|entry| matches!(entry[4], 0x0b | 0x0c | 0x1b | 0x1c));
        let Some(entry) = entry else {
            return Err(ErrorKind::NotRecognized);
        };
        let start = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64;
        let blocks = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as u64;
        if blocks == 0 {
            return Err(ErrorKind::NotRecognized);
        }
        log::info!(
            "microSD MBR FAT32 partition type={:#04x} start={} blocks={} device_blocks={}",
            entry[4],
            start,
            blocks,
            device_blocks
        );
        Partition::new(
            device,
            start * BLOCK_SIZE as u64,
            blocks * BLOCK_SIZE as u64,
        )
    } else {
        return Err(ErrorKind::NotRecognized);
    };
    let owner = Box::leak(Box::new(hadris_fat::embedded::MountToken::new()));
    match Fat::mount(partition, owner).await {
        Ok(volume) => Ok(volume),
        Err(error) => {
            log::error!("microSD FAT mount error: {error:?}");
            Err(error.kind())
        }
    }
}

/// Appends the bounded buffer to the persistent log and makes it durable.
pub async fn append_log<const N: usize>(
    volume: &mut SdStorage,
    buffer: &mut LogBuffer<N>,
) -> Result<(), ErrorKind> {
    append_named(volume, buffer, "theo.log", "theo.1.log").await
}

async fn append_named<const N: usize>(
    volume: &mut SdStorage,
    buffer: &mut LogBuffer<N>,
    name: &str,
    archive: &str,
) -> Result<(), ErrorKind> {
    let size = match volume.metadata(volume.root(), name).await {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == ErrorKind::NotFound => 0,
        Err(error) => return Err(error.kind()),
    };
    if theo_storage::rotation_needed(size, buffer.len()) {
        match volume.remove_file(volume.root(), archive).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.kind()),
        }
        volume
            .rename(volume.root(), name, volume.root(), archive)
            .await
            .map_err(|error| error.kind())?;
    }
    while !buffer.is_empty() {
        let file = volume
            .open(
                volume.root(),
                name,
                OpenOptions::new().write().create().append(),
            )
            .await
            .map_err(|error| error.kind())?;
        let result = volume.write(&file, buffer.pending()).await;
        let closed = volume.close(file).await;
        let written = result.map_err(|error| error.kind())?;
        closed.map_err(|error| error.kind())?;
        if written == 0 {
            return Err(ErrorKind::NoSpace);
        }
        volume.sync().await.map_err(|error| error.kind())?;
        buffer.consume(written);
    }
    volume.sync().await.map_err(|error| error.kind())
}

/// Exercise the real rotation path on separate files, preserving live logs.
#[cfg(feature = "sd-check")]
pub async fn test_rotation(volume: &mut SdStorage) -> Result<(), ErrorKind> {
    for name in ["sdtest2.log", "sdtest2.1"] {
        match volume.metadata(volume.root(), name).await {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            _ => return Err(ErrorKind::AlreadyExists),
        }
    }
    let mut payload = [b'R'; 2048];
    payload[2047] = b'\n';
    for _ in 0..66 {
        let mut buffer = LogBuffer::<2048>::new();
        buffer.push(&payload);
        match embassy_time::with_timeout(
            embassy_time::Duration::from_millis(500),
            append_named(volume, &mut buffer, "sdtest2.log", "sdtest2.1"),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => return Err(ErrorKind::Io),
        }
        embassy_time::Timer::after_millis(1).await;
    }
    let active = volume
        .metadata(volume.root(), "sdtest2.log")
        .await
        .map_err(|error| error.kind())?
        .len();
    let archive = volume
        .metadata(volume.root(), "sdtest2.1")
        .await
        .map_err(|error| error.kind())?
        .len();
    if active != 4096 || archive != theo_storage::LOG_FILE_LIMIT {
        return Err(ErrorKind::Io);
    }
    esp_println::println!(
        "INFO - SD rotation check passed: active={active} archive={archive} (isolated sdtest files retained)"
    );
    Ok(())
}

/// Read the prior log before this boot writes anything. Its tail proves
/// persistence across reset/wake without creating a serial-to-SD feedback loop.
pub async fn report_log_tail(volume: &mut SdStorage) -> Result<(), ()> {
    let size = match volume.metadata(volume.root(), "theo.log").await {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            esp_println::println!("INFO - microSD previous theo.log: absent (first boot)");
            return Ok(());
        }
        Err(_) => return Err(()),
    };
    let file = volume
        .open(volume.root(), "theo.log", OpenOptions::new().read())
        .await
        .map_err(|_| ())?;
    let result = async {
        let mut tail = [0u8; 384];
        let count = size.min(tail.len() as u64) as usize;
        volume
            .seek(&file, SeekFrom::End(-(count as i64)))
            .map_err(|_| ())?;
        let read = volume
            .read(&file, &mut tail[..count])
            .await
            .map_err(|_| ())?;
        if read != count {
            return Err(());
        }
        esp_println::println!(
            "INFO - microSD previous theo.log: {size} bytes; tail:\n{}",
            core::str::from_utf8(&tail[..read]).unwrap_or("<UTF-8 boundary>")
        );
        Ok(())
    }
    .await;
    volume.close(file).await.map_err(|_| ())?;
    result
}
