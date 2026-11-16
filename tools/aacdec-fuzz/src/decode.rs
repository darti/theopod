//! Safe entry to the fuzz driver.

use core::ffi::c_int;
use std::sync::{Mutex, PoisonError};

/// The bench shim keeps the decoder's external structure in a static (one
/// decoder at a time), so concurrent tests would share it.
static SHIM: Mutex<()> = Mutex::new(());

/// What the decoder did with one input.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// `theo_opencore_init`'s error code; 0 when the config was accepted.
    pub init: c_int,
    /// Number of `theo_opencore_decode` calls.
    pub frames: c_int,
    /// Calls that returned `MP4AUDEC_SUCCESS`.
    pub decoded: c_int,
}

unsafe extern "C" {
    fn theo_fuzz_one(data: *const u8, size: usize) -> Summary;
}

/// Decodes one input in the driver's layout (`harness/driver.cpp`).
///
/// Byte 0 selects raw access units after an AudioSpecificConfig (even) or
/// an ADTS stream (odd). Calls are serialised: the shim is single-instance.
#[must_use]
pub fn decode(input: &[u8]) -> Summary {
    let _shim = SHIM.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: the driver reads `input.len()` bytes from the slice and owns
    // every other buffer; it keeps no pointer once it returns, and `SHIM`
    // keeps other threads out of the shim's static state meanwhile.
    unsafe { theo_fuzz_one(input.as_ptr(), input.len()) }
}
