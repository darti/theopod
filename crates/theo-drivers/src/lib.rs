//! HAL-independent drivers for the Theo Pod hardware.
//!
//! Every driver is generic over `embedded-hal` traits or [`QspiBus`], so it
//! builds for any target and is unit-tested on the host.
//!
//! - [`Panel`]: pixel-stream abstraction consumed by renderers.
//! - [`FrameStreamer`], [`FrameError`]: full-frame line streaming to a [`Panel`].
//! - [`QspiBus`], [`Lanes`]: Quad-SPI transport for display controllers.
//! - [`Co5300`], [`PanelGeometry`], [`Co5300Error`]: CO5300 AMOLED controller.
//! - [`Tca9554`]: TCA9554 I2C I/O expander.

#![cfg_attr(not(test), no_std)]

pub mod co5300;
pub mod frame;
pub mod panel;
pub mod qspi;
pub mod tca9554;

#[cfg(test)]
mod co5300_tests;
#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod tca9554_tests;
#[cfg(test)]
mod test_support;

pub use co5300::{Co5300, Error as Co5300Error, PanelGeometry};
pub use frame::{FrameError, FrameStreamer};
pub use panel::Panel;
pub use qspi::{Lanes, QspiBus};
pub use tca9554::Tca9554;
