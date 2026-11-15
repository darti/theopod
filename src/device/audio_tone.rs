//! ES8311 + I2S test tone (`just audio-bench`, t-164, ADR-0026).
//!
//! Wiring from Waveshare's BSP (`esp32_s3_touch_amoled_1_8.h` l.39–44) and
//! `pin_config.h` (l.20–24, l.31): MCLK GPIO16, BCLK GPIO9, WS GPIO45,
//! DOUT GPIO8 (to the codec's DSDIN), NS4150B amplifier enable GPIO46
//! (active high). The ES8311 answers at 0x18 on the shared I2C bus. Its init
//! follows Waveshare's `es8311.c` (`es8311_init`, 48 kHz / MCLK 12.288 MHz
//! coefficients, slave, 16-bit I2S). Plays 1 kHz for 3 s at about −12 dBFS
//! through I2S0 + GDMA and logs what the DMA consumed.

use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::i2c::I2c as _;
use esp_hal::{
    dma_tx_stream_buffer,
    gpio::{Level, Output, OutputConfig},
    i2s::master::{Channels, DataFormat, I2s, TdmConfig},
    peripherals::{DMA_CH2, GPIO8, GPIO9, GPIO16, GPIO45, GPIO46, I2S0},
    time::Rate,
};

use super::{I2cBus, I2cDevice};

const ES8311: u8 = 0x18;
const RATE_HZ: u32 = 48_000;
const PLAY: Duration = Duration::from_secs(3);
/// One 1 kHz period at 48 kHz, amplitude 8000 (about −12 dBFS).
const SINE: [i16; 48] = [
    0, 1044, 2071, 3061, 4000, 4870, 5657, 6347, 6928, 7391, 7727, 7932, 8000, 7932, 7727, 7391,
    6928, 6347, 5657, 4870, 4000, 3061, 2071, 1044, 0, -1044, -2071, -3061, -4000, -4870, -5657,
    -6347, -6928, -7391, -7727, -7932, -8000, -7932, -7727, -7391, -6928, -6347, -5657, -4870,
    -4000, -3061, -2071, -1044,
];

/// Peripherals the tone needs, taken from `Board::init`.
pub struct ToneHw {
    pub i2c: &'static I2cBus,
    pub i2s: I2S0<'static>,
    pub dma: DMA_CH2<'static>,
    pub mclk: GPIO16<'static>,
    pub bclk: GPIO9<'static>,
    pub ws: GPIO45<'static>,
    pub dout: GPIO8<'static>,
    pub amp: GPIO46<'static>,
}

// SAFETY: stored and taken on core0 only (`Board::init`, then `main`), so
// the async I2C bus reference never crosses to the other core.
unsafe impl Send for ToneHw {}

static TONE: embassy_sync::blocking_mutex::Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    core::cell::RefCell<Option<ToneHw>>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(None));

/// Keeps the tone peripherals until `main` spawns [`run`].
pub fn store(hw: ToneHw) {
    TONE.lock(|tone| *tone.borrow_mut() = Some(hw));
}

/// Takes the peripherals stored by [`store`].
pub fn take() -> Option<ToneHw> {
    TONE.lock(|tone| tone.borrow_mut().take())
}

struct Codec(I2cDevice);

impl Codec {
    async fn write(&mut self, reg: u8, value: u8) -> Result<(), &'static str> {
        self.0
            .write(ES8311, &[reg, value])
            .await
            .map_err(|_| "i2c write")
    }

    async fn read(&mut self, reg: u8) -> Result<u8, &'static str> {
        let mut value = [0];
        self.0
            .write_read(ES8311, &[reg], &mut value)
            .await
            .map_err(|_| "i2c read")?;
        Ok(value[0])
    }

    async fn update(&mut self, reg: u8, keep: u8, set: u8) -> Result<(), &'static str> {
        let value = self.read(reg).await? & keep | set;
        self.write(reg, value).await
    }

    /// `es8311_init` for 48 kHz, MCLK 256 x Fs from the pin, slave, 16-bit.
    async fn init(&mut self) -> Result<(), &'static str> {
        for value in [0x1f, 0x00, 0x80] {
            self.write(0x00, value).await?;
        }
        self.write(0x01, 0x3f).await?; // all clocks on, MCLK from the pin
        // Coefficients {12288000, 48000}: pre_div 1, mult 0, adc/dac_div 1,
        // fs_mode 0, lrck 0x00ff, bclk_div 4, osr 0x10.
        self.update(0x02, 0x07, 0).await?;
        self.write(0x03, 0x10).await?;
        self.write(0x04, 0x10).await?;
        self.write(0x05, 0x00).await?;
        self.update(0x06, 0xc0, 4 - 1).await?; // SCLK not inverted
        self.update(0x07, 0xc0, 0x00).await?;
        self.write(0x08, 0xff).await?;
        self.update(0x00, 0xbf, 0).await?; // slave serial port
        self.write(0x09, 3 << 2).await?; // SDP in: I2S, 16-bit
        self.write(0x0a, 3 << 2).await?; // SDP out: I2S, 16-bit
        self.write(0x0d, 0x01).await?; // analog power up
        self.write(0x0e, 0x02).await?; // PGA + ADC modulator
        self.write(0x12, 0x00).await?; // DAC power up
        self.write(0x13, 0x10).await?; // output to the HP driver
        self.write(0x1c, 0x6a).await?; // ADC EQ bypass, DC cancel
        self.write(0x37, 0x08).await?; // DAC EQ bypass
        self.write(0x32, 0x98).await?; // volume 60 %: 60 * 256 / 100 - 1
        self.update(0x31, !0x60, 0).await // unmute
    }
}

/// Initializes the codec, plays the tone, then mutes and powers the amp off.
#[embassy_executor::task]
pub async fn run(hw: ToneHw) {
    Timer::after_secs(1).await;
    let mut codec = Codec(I2cDevice::new(hw.i2c));
    match codec.read(0xfd).await {
        Ok(id) => log::info!("audio-tone: ES8311 at {ES8311:#04x}, chip id reg 0xfd = {id:#04x}"),
        Err(error) => {
            log::error!("audio-tone: no ES8311 at {ES8311:#04x}: {error}");
            return;
        }
    }
    let i2s = match I2s::new(
        hw.i2s,
        hw.dma,
        TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(RATE_HZ))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(Channels::STEREO),
    ) {
        Ok(i2s) => i2s.with_mclk(hw.mclk).into_async(),
        Err(error) => {
            log::error!("audio-tone: I2S config: {error:?}");
            return;
        }
    };
    // Start I2S first, as Waveshare does, so MCLK runs while the codec is
    // programmed; the 170 ms of initial silence covers the I2C writes.
    let tx = i2s
        .i2s_tx
        .with_bclk(hw.bclk)
        .with_ws(hw.ws)
        .with_dout(hw.dout)
        .build();
    let buffer = dma_tx_stream_buffer!(8 * 4092, 4092);
    let mut transfer = match tx.write(buffer) {
        Ok(transfer) => transfer,
        Err((error, ..)) => {
            log::error!("audio-tone: DMA start: {error:?}");
            return;
        }
    };
    if let Err(error) = codec.init().await {
        log::error!("audio-tone: ES8311 init: {error}");
        return;
    }
    let mut amp = Output::new(hw.amp, Level::High, OutputConfig::default());
    let mut phase = 0usize;
    let mut pushed = 0usize;
    let start = Instant::now();
    while start.elapsed() < PLAY {
        if let Err(error) = transfer.wait_for_available_async().await {
            log::error!("audio-tone: DMA error after {pushed} B: {error:?}");
            break;
        }
        pushed += transfer.push_with(|bytes| {
            let frames = bytes.len() / 4;
            for frame in bytes.chunks_exact_mut(4).take(frames) {
                let sample = SINE[phase].to_le_bytes();
                frame[..2].copy_from_slice(&sample);
                frame[2..].copy_from_slice(&sample);
                phase = (phase + 1) % SINE.len();
            }
            frames * 4
        });
    }
    let elapsed_ms = start.elapsed().as_millis();
    let _ = codec.update(0x31, 0xff, 0x60).await; // mute
    amp.set_low();
    drop(transfer);
    // At 48 kHz stereo 16-bit the DMA drains 192,000 B/s.
    log::info!(
        "audio-tone: pushed {pushed} B in {elapsed_ms} ms = {} B/s (expected {}); amp off",
        pushed as u64 * 1000 / elapsed_ms.max(1),
        RATE_HZ * 4
    );
}
