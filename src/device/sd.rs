//! Board-local microSD SPI protocol; LittleFS runs on its dedicated core.

use alloc::rc::Rc;
use core::cell::Cell;

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
        SdStatus::MountFailed => "LittleFS mount failed",
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

type SpiBus = SdMmcBus;
type HalDevice = sdio::DefaultBlockDevice<sdio::sd::Card, SpiBus, Delay>;
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

/// Initializes the microSD SPI slot and identifies the card.
async fn init_card(
    spi3: SPI3<'static>,
    clk: GPIO2<'static>,
    mosi: GPIO1<'static>,
    miso: GPIO3<'static>,
    expander: &'static super::Expander,
) -> Result<(HalDevice, u64), SdInitError> {
    // Exercise the same bounded identification path as an absent/unresponsive
    // card without requiring physical removal or altering the card contents.
    #[cfg(feature = "sd-check")]
    if option_env!("THEO_SD_FAULT") == Some("absent") {
        core::future::pending::<()>().await;
    }
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
    Ok((device, blocks))
}

#[path = "sd_worker.rs"]
mod worker;
#[cfg(feature = "sd-check")]
pub use worker::test_rotation;
pub use worker::{SdStorage, append_log, init, mount, report_log_tail};

#[path = "sd_io.rs"]
mod io;
