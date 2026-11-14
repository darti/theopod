//! TLS AES-128-GCM on the ESP32-S3 AES peripheral (t-154).
//!
//! Registered as embassy-crypto's global `Aes128Gcm` driver, which
//! embedded-tls' `Aes128GcmSha256` suite calls for every record. The S3's
//! AES block has no GCM mode: [`theo_drivers::gcm`] asks it for two ECB
//! blocks (hash key, tag mask) and the CTR keystream of the whole record,
//! run by DMA in place in pieces of [`MAX_DMA_BYTES`] (esp-hal's work
//! queue; it handles PSRAM buffers), and computes GHASH in software.
//! Measured at 80 MHz per 16 KiB record (ADR-0024): software GCM 78 ms,
//! now 36 ms, of which GHASH 33 ms and the DMA CTR 2 ms; driving the
//! peripheral one block at a time through its registers cost 20 µs a block.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_crypto::{Error, driver::InOutBuf};
use esp_hal::{
    aes::{
        AesContext, Operation,
        cipher_modes::{Ctr, Ecb},
        dma::{AesDmaBackend, AesDmaWorkQueueDriver},
    },
    peripherals::{AES, DMA_CH1},
};
use static_cell::StaticCell;
use theo_drivers::gcm::{self, GcmCipher, GcmError};

static BACKEND: StaticCell<AesDmaBackend<'static>> = StaticCell::new();
/// The running work-queue driver: AES operations complete only while it lives.
static DRIVER: StaticCell<AesDmaWorkQueueDriver<'static, 'static>> = StaticCell::new();
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Hands the AES peripheral and a DMA channel to the TLS cipher.
///
/// Call once, before any connection: until then records fail with
/// [`Error::HardwareError`] (the work queue has no driver to run them).
pub fn install(aes: AES<'static>, dma: DMA_CH1<'static>) {
    let backend = BACKEND.init(AesDmaBackend::new(aes, dma));
    DRIVER.init(backend.start());
    RUNNING.store(true, Ordering::Release);
}

/// Largest piece handed to one AES DMA operation.
///
/// esp-hal 1.2.2 links an operation's input to a single DMA descriptor
/// (at most 4095 bytes) and panics on `InsufficientDescriptors` beyond:
/// a 16 KiB record did. 4032 is a whole number of AES blocks and of
/// 64-byte PSRAM DMA lines. Its AES DMA also loses the CTR counter after
/// an operation (the next piece, or the partial last block it finishes on
/// the CPU, used a stale counter, measured on the board): every piece
/// starts from a fresh context with an explicit counter and holds whole
/// blocks only ([`gcm::ctr_in_pieces`]).
const MAX_DMA_BYTES: usize = 63 * 64;

/// The AES peripheral under one record's key.
struct Engine<'a>(&'a [u8; 16]);

impl Engine<'_> {
    /// Runs `mode` over `buf` in place and waits for the DMA.
    fn run(&self, mode: impl Into<esp_hal::aes::CipherState>, buf: &mut [u8]) -> Result<(), Error> {
        if !RUNNING.load(Ordering::Acquire) {
            return Err(Error::HardwareError);
        }
        AesContext::new(mode, Operation::Encrypt, *self.0)
            .process_in_place(buf)
            .map_err(|_| Error::InvalidInput)?
            .wait_blocking();
        Ok(())
    }
}

impl GcmCipher for Engine<'_> {
    type Error = Error;

    fn encrypt_blocks(&mut self, blocks: &mut [[u8; 16]]) -> Result<(), Error> {
        for piece in blocks.as_flattened_mut().chunks_mut(MAX_DMA_BYTES) {
            self.run(Ecb, piece)?;
        }
        Ok(())
    }

    fn apply_ctr(&mut self, counter: &[u8; 16], buf: &mut [u8]) -> Result<(), Error> {
        gcm::ctr_in_pieces(counter, buf, MAX_DMA_BYTES, |counter, piece| {
            self.run(Ctr::new(*counter), piece)
        })
    }
}

struct Driver;

impl embassy_crypto::driver::Aes128Gcm for Driver {
    type Context = [u8; 16];

    fn init(key: &[u8; 16]) -> Self::Context {
        *key
    }

    fn encrypt(
        key: &Self::Context,
        nonce: &[u8; 12],
        aad: &[u8],
        buffer: InOutBuf<'_, '_, u8>,
        tag: &mut [u8; 16],
    ) -> Result<(), Error> {
        let buf = buffer.into_out_with_copied_in();
        *tag = gcm::seal(&mut Engine(key), nonce, aad, buf)?;
        Ok(())
    }

    fn decrypt(
        key: &Self::Context,
        nonce: &[u8; 12],
        aad: &[u8],
        buffer: InOutBuf<'_, '_, u8>,
        tag: &[u8; 16],
    ) -> Result<(), Error> {
        let buf = buffer.into_out_with_copied_in();
        gcm::open(&mut Engine(key), nonce, aad, buf, tag).map_err(|e| match e {
            GcmError::Cipher(e) => e,
            GcmError::Tag => Error::InvalidSignature,
        })
    }
}

embassy_crypto::aes128_gcm_impl!(Driver);
