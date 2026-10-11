//! Theo Pod firmware entry point.
//!
//! - [`device`]: board bring-up (chip runtime, pins, concrete drivers).
//! - [`slint_backend`]: Slint platform and framebuffer rendering.
//!
//! `main` only talks to the board through the `device` module and the
//! [`theo_drivers::Panel`] trait, then runs the Slint event loop: render the
//! dirty part of the framebuffer, send its rows to the panel by DMA, then
//! sleep until the next frame or Slint timer, or until the BOOT button
//! changes, which it forwards to Slint as the Return key.
//!
//! The button is debounced in its own task ([`watch_button`]) and reaches
//! the loop through [`BUTTON_EVENTS`]: the loop cancels its wait every frame,
//! which would otherwise restart the debounce forever.

#![no_std]
#![no_main]

extern crate alloc;

mod device;
mod slint_backend;

use alloc::boxed::Box;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Instant, Timer};
use slint::{
    ComponentHandle, PhysicalSize,
    platform::{
        Key, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};
use theo_drivers::{ButtonEvent, Panel, flush_rows};

use crate::{
    device::Board,
    slint_backend::{FrameBuffer, TheoPlatform},
};

slint::include_modules!();

/// Target interval between frame starts while animating.
///
/// Rendering and the panel transfer count against it, so a redraw slower
/// than this simply runs back to back.
const FRAME_PERIOD: Duration = Duration::from_millis(16);
/// Longest sleep when nothing is scheduled.
const MAX_IDLE: Duration = Duration::from_millis(1000);
/// Button changes queued while the loop renders or flushes.
const BUTTON_QUEUE: usize = 4;

/// Debounced BOOT button changes, from [`watch_button`] to the UI loop.
static BUTTON_EVENTS: Channel<CriticalSectionRawMutex, ButtonEvent, BUTTON_QUEUE> = Channel::new();

#[device::entry]
async fn main(spawner: Spawner) {
    let Board {
        mut display,
        button,
        i2c: _,
        frame,
    } = Board::init().await;
    spawner.spawn(watch_button(button).unwrap());
    let size = display.size();
    let mut frame = FrameBuffer::new(frame, size);

    // The framebuffer persists, so Slint only redraws what changed.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    window.set_size(PhysicalSize::new(u32::from(size.0), u32::from(size.1)));
    // Slint is built with `unsafe-single-threaded`: only this task may use it.
    slint::platform::set_platform(Box::new(TheoPlatform::new(window.clone()))).unwrap();
    let ui = MainWindow::new().unwrap();
    // Showing the window is what requests the first redraw.
    ui.show().unwrap();

    let mut resend_all = false;
    let mut first_frame_sent = false;
    // Dump the first frame, then the frame after each BOOT press.
    #[cfg(feature = "frame-dump")]
    let mut dump_pending = true;
    loop {
        let frame_start = Instant::now();
        slint::platform::update_timers_and_animations();
        let mut dirty = 0..0;
        window.draw_if_needed(|renderer| dirty = frame.render(renderer));
        if resend_all {
            dirty = frame.all_rows();
        }
        let sending = !dirty.is_empty();
        match flush_rows(&mut display, frame.as_bytes(), dirty).await {
            Ok(()) => {
                resend_all = false;
                if sending && !first_frame_sent {
                    first_frame_sent = true;
                    // Slint's allocations are in place once a frame is out.
                    device::log_heap_usage();
                }
                #[cfg(feature = "frame-dump")]
                if sending && dump_pending {
                    dump_pending = false;
                    device::dump_frame(size, frame.as_bytes());
                }
            }
            Err(error) => {
                // The framebuffer is still right; only the panel is stale.
                log::error!("frame flush failed, resending the full frame: {error}");
                resend_all = true;
            }
        }

        let wake_up = Timer::at(next_wake_up(&window, frame_start, resend_all));
        match select(wake_up, BUTTON_EVENTS.receive()).await {
            Either::First(()) => {}
            Either::Second(event) => {
                #[cfg(feature = "frame-dump")]
                if event == ButtonEvent::Pressed {
                    dump_pending = true;
                }
                dispatch_button(&window, event);
            }
        }
    }
}

/// Debounces the BOOT button and queues its changes for the UI loop.
#[embassy_executor::task]
async fn watch_button(mut button: device::Button) {
    loop {
        match button.changed().await {
            Ok(event) => BUTTON_EVENTS.send(event).await,
            Err(error) => log::error!("button read failed: {error:?}"),
        }
    }
}

/// Wakes one frame period after `frame_start` while animating or retrying,
/// otherwise at the next Slint timer (at most [`MAX_IDLE`] from now).
fn next_wake_up(window: &MinimalSoftwareWindow, frame_start: Instant, retry: bool) -> Instant {
    if retry || window.has_active_animations() {
        return frame_start + FRAME_PERIOD;
    }
    let idle = slint::platform::duration_until_next_timer_update()
        .and_then(|delay| u64::try_from(delay.as_micros()).ok())
        .map_or(MAX_IDLE, |micros| {
            Duration::from_micros(micros).min(MAX_IDLE)
        });
    Instant::now() + idle
}

/// Forwards a button change to Slint as a Return key press or release.
fn dispatch_button(window: &MinimalSoftwareWindow, event: ButtonEvent) {
    let text = Key::Return.into();
    let event = match event {
        ButtonEvent::Pressed => WindowEvent::KeyPressed { text },
        ButtonEvent::Released => WindowEvent::KeyReleased { text },
    };
    if let Err(error) = window.window().dispatch_event_with_result(event) {
        log::error!("button event dropped: {error}");
    }
}
