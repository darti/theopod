//! ESP32-S3 data-cache maintenance for the PSRAM DMA investigation (t-4).
//!
//! - [`invalidate_lines`] (feature `frame-dump`): drops the cached copy of a PSRAM range, so the
//!   CPU next reads what the DMA reads.
//! - [`writeback_locked`] (feature `psram-writeback-locked`): writes a range
//!   back with interrupts disabled.
//!
//! esp-hal's `cache_writeback_addr` only suspends DCache autoload around
//! the ROM call. ESP-IDF also keeps interrupts away from the cache during
//! the write-back: on the S3, a line accessed while it is being written back
//! can hit a cache error (`esp_rom_cache_writeback_esp32s3.S`). Only core 0
//! runs, so disabling interrupts covers what ESP-IDF's cache freeze does.

/// Largest data cache line of the ESP32-S3; rounding to it never splits a line.
#[cfg(feature = "frame-dump")]
const CACHE_LINE: usize = 64;

unsafe extern "C" {
    #[cfg(feature = "frame-dump")]
    fn Cache_Invalidate_Addr(addr: u32, size: u32);
    #[cfg(feature = "psram-writeback-locked")]
    fn rom_Cache_WriteBack_Addr(addr: u32, size: u32);
    #[cfg(feature = "psram-writeback-locked")]
    fn Cache_Suspend_DCache_Autoload() -> u32;
    #[cfg(feature = "psram-writeback-locked")]
    fn Cache_Resume_DCache_Autoload(value: u32);
}

/// Invalidates the cache lines that lie entirely inside `range`.
///
/// Dirty lines are discarded, not written back: afterwards the CPU reads
/// PSRAM itself. Partial lines at either end are skipped, since they may
/// hold dirty data of neighbouring heap objects.
#[cfg(feature = "frame-dump")]
pub fn invalidate_lines(range: &[u8]) {
    let start = (range.as_ptr() as usize).next_multiple_of(CACHE_LINE);
    let end = (range.as_ptr() as usize + range.len()) & !(CACHE_LINE - 1);
    if end <= start {
        return;
    }
    // SAFETY: ROM routine; the range covers whole lines of `range` only, whose
    // owner accepts losing unwritten data.
    unsafe { Cache_Invalidate_Addr(start as u32, (end - start) as u32) };
}

/// Writes `range` back to PSRAM with interrupts and DCache autoload disabled.
#[cfg(feature = "psram-writeback-locked")]
#[esp_hal::ram]
pub fn writeback_locked(range: &[u8]) {
    critical_section::with(|_| {
        // SAFETY: ROM routines; write-back of a readable range has no effect
        // beyond syncing PSRAM, and autoload is restored as it was.
        unsafe {
            let autoload = Cache_Suspend_DCache_Autoload();
            rom_Cache_WriteBack_Addr(range.as_ptr() as u32, range.len() as u32);
            Cache_Resume_DCache_Autoload(autoload);
        }
    });
}
