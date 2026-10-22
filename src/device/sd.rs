//! Board-local microSD SPI bring-up and the hadris block-device adapter.

use alloc::{boxed::Box, rc::Rc};
use core::{
    cell::Cell,
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::storage::LogBuffer;
use aligned::{A4, Aligned};
use embassy_time::Delay;
use embedded_hal::digital::OutputPin;
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
        SdInitError::Host => "SDHOST configuration failed",
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
        f.write_str("SDMMC operation failed")
    }
}

impl core::error::Error for SdError {}

type SpiBus = SdMmcBus;
type HalDevice = sdio::DefaultBlockDevice<sdio::sd::Card, SpiBus, Delay>;
type Scratch = Aligned<A4, [u8; BLOCK_SIZE]>;
type Transfer =
    Pin<Box<dyn Future<Output = (Result<(), SdError>, &'static mut Scratch)> + 'static>>;

#[derive(Default)]
pub struct SdState {
    transfer: Option<Transfer>,
}

#[derive(Debug, Clone, Copy)]
struct CsError;

impl embedded_hal::digital::Error for CsError {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}

/// The V2 routes SDCS to TCA9554 EXIO7 rather than an ESP GPIO. The SDIO
/// crate's SPI bus uses the synchronous `OutputPin` trait, so bridge the
/// already-async shared expander access with Embassy's small local executor.
struct ExpanderCs {
    expander: &'static super::Expander,
}

impl embedded_hal::digital::ErrorType for ExpanderCs {
    type Error = CsError;
}

impl OutputPin for ExpanderCs {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        embassy_futures::block_on(async {
            let mut expander = self.expander.lock().await;
            expander
                .set_level(super::SD_CARD_SELECT, embedded_hal::digital::PinState::Low)
                .await
                .map_err(|_| CsError)
        })
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        embassy_futures::block_on(async {
            let mut expander = self.expander.lock().await;
            expander
                .set_level(super::SD_CARD_SELECT, embedded_hal::digital::PinState::High)
                .await
                .map_err(|_| CsError)
        })
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
        self.cs.set_low().map_err(|_| MmcError::Io)
    }

    async fn deselect(&mut self) -> Result<(), MmcError> {
        self.cs.set_high().map_err(|_| MmcError::Io)?;
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
        self.cs.set_high().map_err(|_| MmcError::Io)?;
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
    device: *mut HalDevice,
    blocks: u64,
    position: u64,
}

// The stream is only exposed through the single mutable board service. Its
// transfer state serializes every use of the leaked HAL device.
unsafe impl Send for SdStream {}

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
        if !self.position.is_multiple_of(BLOCK_SIZE as u64) || buf.len() != BLOCK_SIZE {
            return Poll::Ready(Err(ErrorKind::InvalidInput.into()));
        }
        let mut copy = [0u8; BLOCK_SIZE];
        copy.copy_from_slice(buf);
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
        if !self.position.is_multiple_of(BLOCK_SIZE as u64)
            || buf.len() != BLOCK_SIZE
            || self.position / BLOCK_SIZE as u64 >= self.blocks
        {
            return Poll::Ready(Err(ErrorKind::InvalidInput.into()));
        }
        if state.transfer.is_none() {
            let scratch = Box::leak(Box::new(Aligned::<A4, [u8; BLOCK_SIZE]>([0; BLOCK_SIZE])));
            if write {
                scratch.copy_from_slice(buf);
            }
            let device = unsafe { &mut *self.device };
            let block = (self.position / BLOCK_SIZE as u64) as u32;
            let transfer: Transfer = if write {
                Box::pin(async move {
                    let result = block_device_driver::BlockDevice::write(
                        device,
                        block,
                        core::slice::from_ref(&*scratch),
                    )
                    .await
                    .map_err(|error| {
                        log::error!("microSD block write error: {error:?}");
                        SdError
                    });
                    (result, scratch)
                })
            } else {
                Box::pin(async move {
                    let result = block_device_driver::BlockDevice::read(
                        device,
                        block,
                        core::slice::from_mut(scratch),
                    )
                    .await
                    .map_err(|error| {
                        log::error!("microSD block read error: {error:?}");
                        SdError
                    });
                    (result, scratch)
                })
            };
            state.transfer = Some(transfer);
        }
        match state.transfer.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready((result, scratch)) => {
                state.transfer = None;
                if let Err(error) = result {
                    return Poll::Ready(Err(IoError::device(error, "SD block transfer failed")));
                }
                if !write {
                    buf.copy_from_slice(&scratch[..]);
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
    let device = Box::leak(Box::new(device));
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
pub async fn append_log(volume: &mut SdStorage, buffer: &mut LogBuffer<1024>) -> Result<(), ()> {
    while !buffer.is_empty() {
        let file = volume
            .open(
                volume.root(),
                "theo.log",
                OpenOptions::new().write().create().append(),
            )
            .await
            .map_err(|_| ())?;
        let written = volume
            .write(&file, buffer.pending())
            .await
            .map_err(|_| ())?;
        volume.close(file).await.map_err(|_| ())?;
        if written == 0 {
            return Err(());
        }
        buffer.consume(written);
    }
    volume.sync().await.map_err(|_| ())
}

/// Reopens the boot log and verifies the appended record after sync.
pub async fn verify_boot_log(volume: &mut SdStorage) -> Result<(), ()> {
    let file = volume
        .open(volume.root(), "theo.log", OpenOptions::new().read())
        .await
        .map_err(|_| ())?;
    let result = async {
        let start = volume.seek(&file, SeekFrom::End(-5)).map_err(|_| ())?;
        let mut tail = [0u8; 5];
        let read = volume.read(&file, &mut tail).await.map_err(|_| ())?;
        if read != tail.len() || &tail != b"boot\n" {
            return Err(());
        }
        log::info!(
            "microSD theo.log: {} bytes, boot record read back",
            start + read as u64
        );
        Ok(())
    }
    .await;
    volume.close(file).await.map_err(|_| ())?;
    result
}
