//! Theo Pod firmware entry point.
//!
//! - [`device`]: board bring-up (chip runtime, pins, concrete drivers).
//! - [`slint_backend`]: Slint platform and framebuffer rendering.
//!
//! `main` only talks to the board through the `device` module and the
//! [`theo_drivers::Panel`] trait, then runs the Slint event loop: render the
//! dirty part of the framebuffer, send its rows to the panel by DMA, then
//! sleep until the next frame or Slint timer, or until an input changes:
//! the BOOT button is forwarded to Slint as the Return key, touches as
//! pointer events.
//!
//! Each input runs in its own task ([`watch_button`], [`watch_touch`]) and
//! reaches the loop through a channel ([`BUTTON_EVENTS`], [`TOUCH_EVENTS`]):
//! the loop cancels its wait every frame, which would otherwise restart the
//! button debounce, or a touch poll, forever. Each wake-up dispatches every
//! queued event before the next frame, so a drag never trails the finger.

#![no_std]
#![no_main]

extern crate alloc;

mod device;
mod slint_backend;

use alloc::boxed::Box;

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, select3};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Instant, Timer};
use slint::{
    ComponentHandle, LogicalPosition, PhysicalSize,
    platform::{
        Key, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};
use theo_drivers::{ButtonEvent, Panel, TouchEvent, TouchPoint, flush_rows};

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
/// Touch changes queued while the loop renders or flushes; a full queue
/// pauses the touch poll, it drops nothing.
const TOUCH_QUEUE: usize = 8;
/// Pause after a failed touch read, so a faulty controller toggling its
/// interrupt cannot keep the touch task busy.
const TOUCH_RETRY: Duration = Duration::from_millis(10);

/// Debounced BOOT button changes, from [`watch_button`] to the UI loop.
static BUTTON_EVENTS: Channel<CriticalSectionRawMutex, ButtonEvent, BUTTON_QUEUE> = Channel::new();
/// Touch presses, moves, releases and cancellations, from [`watch_touch`]
/// to the UI loop.
static TOUCH_EVENTS: Channel<CriticalSectionRawMutex, TouchEvent, TOUCH_QUEUE> = Channel::new();

#[device::entry]
async fn main(spawner: Spawner) {
    let Board {
        mut display,
        button,
        touch,
        frame,
    } = Board::init().await;
    spawner.spawn(watch_button(button).unwrap());
    spawner.spawn(watch_touch(touch).unwrap());
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

    // Consecutive failed flushes; while non-zero, every frame is resent.
    let mut failed_flushes: u32 = 0;
    let mut first_frame_sent = false;
    // Dump the first frame, then the frame after each BOOT press.
    #[cfg(feature = "frame-dump")]
    let mut dump_pending = true;
    loop {
        let frame_start = Instant::now();
        slint::platform::update_timers_and_animations();
        let mut dirty = 0..0;
        window.draw_if_needed(|renderer| dirty = frame.render(renderer));
        if failed_flushes > 0 {
            dirty = frame.all_rows();
        }
        let sending = !dirty.is_empty();
        match flush_rows(&mut display, frame.as_bytes(), dirty).await {
            Ok(()) => {
                if failed_flushes > 0 {
                    log::info!("frame flush recovered after {failed_flushes} failed attempts");
                }
                failed_flushes = 0;
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
                // Retry every frame period, logging once per failure streak.
                if failed_flushes == 0 {
                    log::error!("frame flush failed, resending the full frame: {error}");
                }
                failed_flushes = failed_flushes.saturating_add(1);
            }
        }

        let wake_up = Timer::at(next_wake_up(&window, frame_start, failed_flushes > 0));
        #[cfg_attr(
            not(feature = "frame-dump"),
            expect(unused_mut, reason = "only frame dumps update state")
        )]
        let mut on_button = |event| {
            #[cfg(feature = "frame-dump")]
            if event == ButtonEvent::Pressed {
                dump_pending = true;
            }
            dispatch_button(&window, event);
        };
        match select3(wake_up, BUTTON_EVENTS.receive(), TOUCH_EVENTS.receive()).await {
            Either3::First(()) => {}
            Either3::Second(event) => on_button(event),
            Either3::Third(event) => dispatch_touch(&window, event),
        }
        // Catch up with everything queued during the frame before drawing
        // the next one.
        while let Ok(event) = BUTTON_EVENTS.try_receive() {
            on_button(event);
        }
        while let Ok(event) = TOUCH_EVENTS.try_receive() {
            dispatch_touch(&window, event);
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

/// Reports touches to the UI loop.
///
/// A failed read mid-touch reaches the loop as [`TouchEvent::Cancelled`]
/// (from the driver). Failures are logged once per streak, and each one
/// pauses the task for [`TOUCH_RETRY`].
#[embassy_executor::task]
async fn watch_touch(mut touch: device::Touch) {
    // Consecutive failed reads.
    let mut failures: u32 = 0;
    loop {
        match touch.changed().await {
            Ok(event) => {
                // A cancellation follows the failure it reports.
                if failures > 0 && !matches!(event, TouchEvent::Cancelled(_)) {
                    log::info!("touch reads recovered after {failures} failures");
                    failures = 0;
                }
                TOUCH_EVENTS.send(event).await;
            }
            Err(error) => {
                if failures == 0 {
                    log::error!("touch read failed: {error}");
                }
                failures = failures.saturating_add(1);
                Timer::after(TOUCH_RETRY).await;
            }
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

/// Forwards a touch change to Slint as left-button pointer events.
///
/// A release is followed by an exit: a lifted finger hovers nowhere. A
/// cancellation is an exit alone, which Slint never takes for a click.
fn dispatch_touch(window: &MinimalSoftwareWindow, event: TouchEvent) {
    // Panel pixels are logical pixels: the window keeps Slint's default
    // scale factor of 1.
    let position = |point: TouchPoint| LogicalPosition::new(f32::from(point.x), f32::from(point.y));
    let button = PointerEventButton::Left;
    let (event, exit) = match event {
        TouchEvent::Pressed(point) => (
            WindowEvent::PointerPressed {
                position: position(point),
                button,
            },
            false,
        ),
        TouchEvent::Moved(point) => (
            WindowEvent::PointerMoved {
                position: position(point),
            },
            false,
        ),
        TouchEvent::Released(point) => (
            WindowEvent::PointerReleased {
                position: position(point),
                button,
            },
            true,
        ),
        TouchEvent::Cancelled(_) => (WindowEvent::PointerExited, false),
    };
    let events = core::iter::once(event).chain(exit.then_some(WindowEvent::PointerExited));
    for event in events {
        if let Err(error) = window.window().dispatch_event_with_result(event) {
            log::error!("touch event dropped: {error}");
        }
    }
}
