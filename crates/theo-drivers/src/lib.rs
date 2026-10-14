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
//! - [`Cst820`], [`TouchEvent`], [`Cst820Error`], [`Cst820InitError`]:
//!   CST820 capacitive touch controller, interrupt-driven.
//! - [`PanelSize`], [`TouchPoint`]: pixel sizes and positions of the panel
//!   or of a view drawn on it.
//! - [`Qmi8658`], [`Acceleration`], [`Qmi8658Error`]: QMI8658 inertial
//!   sensor, accelerometer only.
//! - [`OrientationDetector`], [`Rotation`], [`AxisMap`], [`Axis`],
//!   [`PanelAcceleration`]: view rotation from gravity, and the matching
//!   touch transform.
//! - [`Tca9554`], [`ExpanderPin`]: TCA9554 async I2C I/O expander with per-pin access.
//! - [`PanelPower`]: panel rail/reset power-up through the expander.

#![cfg_attr(not(test), no_std)]

pub mod button;
pub mod co5300;
pub mod cst820;
pub mod framebuffer;
pub mod geometry;
pub mod orientation;
pub mod panel;
pub mod panel_power;
pub mod qmi8658;
pub mod qspi;
pub mod tca9554;

#[cfg(test)]
mod button_tests;
#[cfg(test)]
mod co5300_tests;
#[cfg(test)]
mod cst820_tests;
#[cfg(test)]
mod framebuffer_tests;
#[cfg(test)]
mod orientation_tests;
#[cfg(test)]
mod panel_power_tests;
#[cfg(test)]
mod qmi8658_tests;
#[cfg(test)]
mod tca9554_tests;
#[cfg(test)]
mod test_support;

pub use button::{ButtonEvent, DebouncedButton};
pub use co5300::{Co5300, Error as Co5300Error, PanelGeometry};
pub use cst820::{Cst820, Error as Cst820Error, InitError as Cst820InitError, TouchEvent};
pub use framebuffer::{FlushError, flush_rows};
pub use geometry::{PanelSize, TouchPoint};
pub use orientation::{Axis, AxisMap, OrientationDetector, PanelAcceleration, Rotation};
pub use panel::Panel;
pub use panel_power::PanelPower;
pub use qmi8658::{Acceleration, Error as Qmi8658Error, Qmi8658};
pub use qspi::{Lanes, QspiBus};
pub use tca9554::{Pin as ExpanderPin, Tca9554};
