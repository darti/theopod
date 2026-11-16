//! Sanitizer regression tests for opencore-aacdec (t-169, ADR-0027).
//!
//! The decoder (Apache-2.0, AOSP `android-4.0.4_r2.1`, patched by
//! `tools/opencore-aacdec/patches`) is built for the host with ASan and
//! UBSan by `build.rs`, together with the firmware's bench shim and the
//! libFuzzer driver (`harness/driver.cpp`).
//!
//! - [`decode`]: runs one input in the driver's layout, as `just aacdec-fuzz` does.
//! - [`Summary`]: what the decoder did with it.
//!
//! A memory error or undefined behaviour aborts the test process with the
//! sanitizer report; the tests in `decode_tests.rs` replay the crashes found
//! by fuzzing, each tagged `// Regression (t-N)` (ADR-0007).

mod decode;
#[cfg(test)]
mod decode_tests;

pub use decode::{Summary, decode};
