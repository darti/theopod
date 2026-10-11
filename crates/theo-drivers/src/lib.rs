//! HAL-independent drivers for the Theo Pod hardware.
//!
//! Every driver is generic over `embedded-hal` traits or [`QspiBus`], so it
//! builds for any target and is unit-tested on the host.
//!
//! - [`DebouncedButton`], [`ButtonEvent`]: debounced push button on any input pin.
//! - [`Panel`]: pixel-stream abstraction consumed by renderers.
//! - [`flush_rows`], [`FlushError`]: sends dirty framebuffer rows to a [`Panel`].
//! - [`QspiBus`], [`Lanes`]: Quad-SPI transport for display controllers.
//! - [`Co5300`], [`PanelGeometry`], [`Co5300Error`]: CO5300 AMOLED controller.
//! - [`Tca9554`], [`ExpanderPin`]: TCA9554 I2C I/O expander with per-pin access.
//! - [`PanelPower`]: panel rail/reset power-up through the expander.

#![cfg_attr(not(test), no_std)]

pub mod button;
pub mod co5300;
pub mod framebuffer;
pub mod panel;
pub mod panel_power;
pub mod qspi;
pub mod tca9554;

#[cfg(test)]
mod button_tests;
#[cfg(test)]
mod co5300_tests;
#[cfg(test)]
mod framebuffer_tests;
#[cfg(test)]
mod panel_power_tests;
#[cfg(test)]
mod tca9554_tests;
#[cfg(test)]
mod test_support;

pub use button::{ButtonEvent, DebouncedButton};
pub use co5300::{Co5300, Error as Co5300Error, PanelGeometry};
pub use framebuffer::{FlushError, flush_rows};
pub use panel::Panel;
pub use panel_power::PanelPower;
pub use qspi::{Lanes, QspiBus};
pub use tca9554::{Pin as ExpanderPin, Tca9554};
