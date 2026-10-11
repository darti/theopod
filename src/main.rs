//! Theo Pod firmware entry point.
//!
//! - [`device`]: board bring-up (chip runtime, pins, concrete drivers).
//! - [`slint_backend`]: Slint platform and frame streaming to any panel.
//!
//! `main` only talks to the board through `device::{entry, Board}` and the
//! [`theo_drivers::Panel`] trait, then runs the Slint event loop.

#![no_std]
#![no_main]

extern crate alloc;

mod device;
mod slint_backend;

use alloc::boxed::Box;

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use slint::{
    ComponentHandle, PhysicalSize,
    platform::{
        WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};
use theo_drivers::Panel;

use crate::{
    device::Board,
    slint_backend::{LineBuffer, TheoPlatform, render_frame},
};

slint::include_modules!();

/// Target interval between frame starts while animating.
///
/// Rendering and the blocking panel transfer count against it, so a full
/// redraw slower than this simply runs back to back.
const FRAME_PERIOD: Duration = Duration::from_millis(16);
/// Longest sleep when nothing is scheduled.
const MAX_IDLE: Duration = Duration::from_millis(1000);

#[device::entry]
async fn main(_spawner: Spawner) {
    let Board { mut display } = Board::init().await;
    let (width, height) = display.size();

    // Full repaints deliver whole rows in order, as `render_frame` requires.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(PhysicalSize::new(u32::from(width), u32::from(height)));
    // Slint is built with `unsafe-single-threaded`: only this task may use it.
    slint::platform::set_platform(Box::new(TheoPlatform::new(window.clone()))).unwrap();
    let ui = MainWindow::new().unwrap();
    // Showing the window is what requests the first redraw.
    ui.show().unwrap();

    let mut buffer = LineBuffer::default();
    loop {
        let frame_start = Instant::now();
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer| {
            if let Err(error) = render_frame(renderer, &mut display, &mut buffer) {
                log::error!("frame dropped, retrying: {error}");
                // Slint already considers this frame drawn; ask for it again
                // so a partial frame does not stay on screen. The retry waits
                // for the next wake-up, which paces repeated failures.
                window.window().request_redraw();
            }
        });
        Timer::at(next_wake_up(&window, frame_start)).await;
    }
}

/// Wakes one frame period after `frame_start` while animating, otherwise at
/// the next Slint timer (at most [`MAX_IDLE`] from now).
fn next_wake_up(window: &MinimalSoftwareWindow, frame_start: Instant) -> Instant {
    if window.has_active_animations() {
        return frame_start + FRAME_PERIOD;
    }
    let idle = slint::platform::duration_until_next_timer_update()
        .and_then(|delay| u64::try_from(delay.as_micros()).ok())
        .map_or(MAX_IDLE, |micros| {
            Duration::from_micros(micros).min(MAX_IDLE)
        });
    Instant::now() + idle
}
