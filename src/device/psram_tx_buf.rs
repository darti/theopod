//! DMA transmit buffer that reads PSRAM in 64-byte bursts.
//!
//! - [`PsramTxBuf`]: descriptor chain over a borrowed PSRAM region.
//! - [`BURST`]: PSRAM burst length, and the alignment a region must have.
//! - [`CHUNK`]: bytes per descriptor, for `dma_descriptors_chunk_size!`.
//!
//! esp-hal's `half_duplex_write_async(&[u8])` reads PSRAM in 16-byte bursts
//! through 4095-byte descriptors, so every descriptor after the first starts
//! unaligned. The DMA then fell behind the SPI, which ran dry and showed
//! bands on the panel (t-4). This buffer uses 64-byte bursts and descriptors
//! that each start a whole burst (ADR-0004).

use core::ptr;

use esp_hal::dma::{
    BurstConfig, DmaDescriptor, DmaTxBuffer, ExternalBurstConfig, InternalBurstConfig, Preparation,
};
use theo_drivers::descriptor_chunks;

/// PSRAM burst length; regions sent must start on a multiple of it.
pub const BURST: usize = ExternalBurstConfig::Size64 as usize;

/// Burst configuration of framebuffer transfers.
const BURST_CONFIG: BurstConfig = BurstConfig {
    external_memory: ExternalBurstConfig::Size64,
    internal_memory: InternalBurstConfig::DEFAULT,
};

/// Bytes per descriptor: the largest multiple of [`BURST`] a descriptor holds.
pub const CHUNK: usize = BURST_CONFIG.max_compatible_chunk_size();
// Host tests use this value: keep `crates/theo-drivers/src/dma_layout_tests.rs`
// in sync.
const _: () = assert!(CHUNK == 4032);

/// DMA transmit buffer over a PSRAM region, set by [`PsramTxBuf::set_source`].
///
/// It holds a raw pointer to the region, valid only under the contract of
/// [`PsramTxBuf::set_source`]. esp-hal's SPI master hands the buffer back
/// without calling `from_view`, so its owner calls
/// [`PsramTxBuf::clear_source`] after every transfer, completed or failed.
#[derive(Debug)]
pub struct PsramTxBuf {
    /// Descriptor chain, in internal RAM.
    descriptors: &'static mut [DmaDescriptor],
    source: *const u8,
    len: usize,
}

// SAFETY: the raw `source` pointer is only dereferenced by the DMA during a
// transfer, under the `set_source` contract; moving the buffer between
// contexts does not touch it.
unsafe impl Send for PsramTxBuf {}

impl PsramTxBuf {
    /// Wraps descriptors from `dma_descriptors_chunk_size!(.., CHUNK)`.
    pub fn new(descriptors: &'static mut [DmaDescriptor]) -> Self {
        Self {
            descriptors,
            source: ptr::null(),
            len: 0,
        }
    }

    /// Longest region the descriptors can chain.
    pub fn capacity(&self) -> usize {
        self.descriptors.len() * CHUNK
    }

    /// Selects the PSRAM region the next transfer sends.
    ///
    /// # Panics
    ///
    /// Panics if `source` is empty, longer than [`Self::capacity`], or not
    /// [`BURST`]-aligned: callers check these first.
    ///
    /// # Safety
    ///
    /// `source` must stay valid and unmodified until the transfer using this
    /// buffer has completed, or has been dropped (which waits for the DMA to
    /// stop). If the transfer can be leaked, `source` must stay allocated
    /// for the rest of the program: the DMA keeps reading it.
    pub unsafe fn set_source(&mut self, source: &[u8]) {
        assert!(
            !source.is_empty()
                && source.len() <= self.capacity()
                && (source.as_ptr() as usize).is_multiple_of(BURST),
            "PSRAM source must be non-empty, fit the descriptors and be burst-aligned"
        );
        self.source = source.as_ptr();
        self.len = source.len();
    }

    /// Forgets the region, so `prepare` refuses to start without a new one.
    pub fn clear_source(&mut self) {
        self.source = ptr::null();
        self.len = 0;
    }
}

// SAFETY: `prepare` links only descriptors this buffer owns (in internal
// RAM, valid for 'static), over the region the `set_source` contract keeps
// valid until the transfer ends. The owner clears the region after each
// transfer (`clear_source`), so `prepare` cannot reuse a stale one.
unsafe impl DmaTxBuffer for PsramTxBuf {
    type View = Self;
    type Final = Self;

    fn prepare(&mut self) -> Preparation {
        assert!(self.len != 0, "PsramTxBuf prepared without a source");
        assert!(
            self.len <= self.capacity(),
            "source exceeds the descriptors"
        );
        // All descriptor accesses go through `head`, which also links them.
        let head = self.descriptors.as_mut_ptr();
        // Host-tested: chunks tile the source, each starting a whole burst.
        for (index, chunk) in descriptor_chunks(self.len, CHUNK).enumerate() {
            let (len, last) = (chunk.len, chunk.last);
            // SAFETY: `len <= capacity` makes `index < descriptors.len()`, and
            // `head` is the only pointer used to reach the descriptors here.
            let descriptor = unsafe { &mut *head.add(index) };
            let buffer = self.source.wrapping_add(chunk.offset);
            // The DMA reads PSRAM itself: flush what the CPU left in the cache,
            // one descriptor at a time to bound the time with interrupts off.
            writeback(buffer as usize, len);
            // Each descriptor starts a whole burst, for speed: the hardware
            // does not require TX alignment. The DMA only reads through it.
            descriptor.buffer = buffer.cast_mut();
            descriptor.set_size(len);
            descriptor.set_length(len);
            descriptor.reset_for_tx(last);
            descriptor.next = if last {
                ptr::null_mut()
            } else {
                head.wrapping_add(index + 1)
            };
        }
        Preparation {
            start: head,
            accesses_psram: true,
            burst_transfer: BURST_CONFIG,
            check_owner: None,
            auto_write_back: false,
        }
    }

    fn into_view(self) -> Self {
        self
    }

    fn from_view(mut view: Self) -> Self {
        view.clear_source();
        view
    }
}

// Provided by esp-rom-sys's ROM linker scripts (through esp-hal): a rename
// there shows up as a link error. Re-check on every esp-hal upgrade.
unsafe extern "C" {
    fn rom_Cache_WriteBack_Addr(addr: u32, size: u32);
    fn Cache_Suspend_DCache_Autoload() -> u32;
    fn Cache_Resume_DCache_Autoload(value: u32);
}

/// Writes `len` bytes from `start` back to PSRAM, interrupts and autoload off.
///
/// Mirrors esp-hal 1.2.2's private `soc::esp32s3::cache_writeback_addr`,
/// plus ESP-IDF's guard for the S3 errata: a line accessed during its
/// write-back can be lost (`esp_rom_cache_writeback_esp32s3.S`). Disabling
/// interrupts stands in for ESP-IDF's cache freeze only while the app core
/// stays off (ADR-0004).
#[esp_hal::ram]
fn writeback(start: usize, len: usize) {
    critical_section::with(|_| {
        // SAFETY: ROM routines; writing back a readable range only syncs PSRAM,
        // and autoload is restored as it was. Addresses and lengths fit u32:
        // usize is 32 bits on the ESP32-S3.
        unsafe {
            let autoload = Cache_Suspend_DCache_Autoload();
            rom_Cache_WriteBack_Addr(start as u32, len as u32);
            Cache_Resume_DCache_Autoload(autoload);
        }
    });
}
