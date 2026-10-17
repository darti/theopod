//! HAL-independent drivers for the Theo Pod hardware.
//!
//! Every driver is generic over `embedded-hal` traits or [`QspiBus`], so it
//! builds for any target and is unit-tested on the host.
//!
//! - [`DebouncedButton`], [`ButtonEvent`]: debounced push button on any input pin.
//! - [`LongPress`]: tells short presses from long ones;
//!   [`LongPressButton`], [`LongPressEvent`]: a button that reports them.
//! - [`Panel`]: panel owning its framebuffer, sent to it by byte ranges.
//! - [`flush_rows`], [`flush_bands`], [`DirtyRows`], [`FlushError`]: sends
//!   dirty bands of framebuffer rows to a [`Panel`].
//! - [`FlushRetry`], [`Retry`], [`Transient`]: when to resend the frame
//!   after failed flushes; [`reset_due`]: when to reset a failing device.
//! - [`StaleWatch`], [`WatchAction`], [`encode_resets`], [`decode_resets`]:
//!   when a display that stays broken calls for a reboot.
//! - [`RebootCap`], [`StartDecision`], [`StartAction`]: when repeated watchdog
//!   reboots call for a stop instead.
//! - [`QspiBus`], [`Lanes`]: Quad-SPI transport for display controllers;
//!   [`FrameBus`]: one that holds the framebuffer it sends in place.
//! - [`ChipSelect`]: chip select released even when a transfer is cancelled.
//! - [`dma_layout`]: descriptor and payload arithmetic of in-place DMA
//!   transfers ([`descriptor_chunks`], [`DmaChunk`], [`frame_payload`],
//!   [`FramePayloadError`]).
//! - [`Co5300`], [`PanelGeometry`], [`Co5300Error`]: CO5300 AMOLED controller.
//! - [`Cst820`], [`TouchEvent`], [`Cst820Error`], [`Cst820InitError`]:
//!   CST820 capacitive touch controller, interrupt-driven.
//! - [`check_pattern`], [`check_pattern_with`], [`PatternMismatch`]: pattern
//!   test of a RAM region.
//! - [`PanelSize`], [`TouchPoint`]: pixel sizes and positions of the panel
//!   or of a view drawn on it.
//! - [`Qmi8658`], [`Acceleration`], [`Qmi8658Error`]: QMI8658 inertial
//!   sensor, accelerometer only.
//! - [`OrientationDetector`], [`Rotation`], [`AxisMap`], [`Axis`],
//!   [`PanelAcceleration`]: view rotation from gravity, and the matching
//!   touch transform.
//! - [`clear_i2c_bus`], [`BusClear`], [`BusClearError`], [`MAX_CLOCK_PULSES`]:
//!   frees an I2C bus a slave keeps busy after a reset.
//! - [`Tca9554`], [`ExpanderPin`]: TCA9554 async I2C I/O expander with per-pin access.
//! - [`PanelPower`]: panel rail/reset power-up and power-down through the
//!   expander.

#![cfg_attr(not(test), no_std)]

pub mod bus_clear;
pub mod button;
pub mod chip_select;
pub mod co5300;
pub mod cst820;
pub mod dirty_rows;
pub mod dma_layout;
pub mod framebuffer;
pub mod geometry;
pub mod long_press;
pub mod memory_check;
pub mod orientation;
pub mod panel;
pub mod panel_power;
pub mod qmi8658;
pub mod qspi;
pub mod reboot_cap;
pub mod retry;
pub mod stale_watch;
pub mod tca9554;

#[cfg(test)]
mod bus_clear_tests;
#[cfg(test)]
mod button_tests;
#[cfg(test)]
mod chip_select_tests;
#[cfg(test)]
mod co5300_tests;
#[cfg(test)]
mod cst820_tests;
#[cfg(test)]
mod dirty_rows_tests;
#[cfg(test)]
mod dma_layout_tests;
#[cfg(test)]
mod framebuffer_tests;
#[cfg(test)]
mod long_press_tests;
#[cfg(test)]
mod memory_check_tests;
#[cfg(test)]
mod orientation_tests;
#[cfg(test)]
mod panel_power_tests;
#[cfg(test)]
mod qmi8658_tests;
#[cfg(test)]
mod reboot_cap_tests;
#[cfg(test)]
mod retry_tests;
#[cfg(test)]
mod stale_watch_tests;
#[cfg(test)]
mod tca9554_tests;
#[cfg(test)]
mod test_support;

pub use bus_clear::{BusClear, BusClearError, MAX_CLOCK_PULSES, clear_i2c_bus};
pub use button::{ButtonEvent, DebouncedButton};
pub use chip_select::ChipSelect;
pub use co5300::{Co5300, Error as Co5300Error, PanelGeometry};
pub use cst820::{Cst820, Error as Cst820Error, InitError as Cst820InitError, TouchEvent};
pub use dirty_rows::DirtyRows;
pub use dma_layout::{DmaChunk, FramePayloadError, descriptor_chunks, frame_payload};
pub use framebuffer::{FlushError, flush_bands, flush_rows};
pub use geometry::{PanelSize, TouchPoint};
pub use long_press::{LongPress, LongPressButton, LongPressEvent};
pub use memory_check::{PatternMismatch, check_pattern, check_pattern_with};
pub use orientation::{Axis, AxisMap, OrientationDetector, PanelAcceleration, Rotation};
pub use panel::Panel;
pub use panel_power::PanelPower;
pub use qmi8658::{Acceleration, Error as Qmi8658Error, Qmi8658};
pub use qspi::{FrameBus, Lanes, QspiBus};
pub use reboot_cap::{RebootCap, StartAction, StartDecision};
pub use retry::{FlushRetry, Retry, Transient, reset_due};
pub use stale_watch::{StaleWatch, WatchAction, decode_resets, encode_resets};
pub use tca9554::{Pin as ExpanderPin, Tca9554};
