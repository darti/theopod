//! Theo Pod firmware entry point.
//!
//! - [`device`]: board bring-up (chip runtime, pins, concrete drivers).
//! - [`slint_backend`]: Slint platform and framebuffer rendering.
//!
//! `main` only talks to the board through the `device` module and the
//! [`theo_drivers::Panel`] trait, then runs the Slint event loop: render the
//! dirty part of the framebuffer, send its row bands to the panel by DMA,
//! resending the whole frame after a failed flush ([`FlushRetry`]), then
//! sleep until the next frame or Slint timer, or until an input changes:
//! the BOOT button is forwarded to Slint as the Return key, touches as
//! pointer events, and orientation changes rotate the rendering, the window
//! size and the touch coordinates together.
//!
//! Each input runs in its own task ([`watch_button`], [`watch_touch`],
//! [`watch_orientation`]) and reaches the loop through a channel
//! ([`BUTTON_EVENTS`], [`TOUCH_EVENTS`]) or a signal ([`ROTATION`]):
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
use embassy_futures::select::{Either4, select4};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use embassy_time::{Duration, Instant, Ticker, Timer};
use slint::{
    ComponentHandle, LogicalPosition, PhysicalSize,
    platform::{
        Key, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RenderingRotation, RepaintBufferType},
    },
};
use theo_drivers::{
    ButtonEvent, FlushRetry, OrientationDetector, Panel, PanelSize, Rotation, TouchEvent,
    TouchPoint, Transient, display_reset_due, flush_bands, reset_due,
};

use crate::{
    device::Board,
    slint_backend::{FrameBuffer, FrameRows, TheoPlatform},
};

slint::include_modules!();

/// Target interval between frame starts while animating.
///
/// Rendering and the panel transfer count against it, so a redraw slower
/// than this simply runs back to back.
const FRAME_PERIOD: Duration = Duration::from_millis(16);
/// Longest sleep when nothing is scheduled.
const MAX_IDLE: Duration = Duration::from_millis(1000);
/// First resend delay after a transient flush failure: one frame period.
///
/// It doubles per failure up to [`FLUSH_RETRY_MAX_MS`].
const FLUSH_RETRY_FIRST_MS: u32 = 16;
/// Longest resend delay after failed flushes.
const FLUSH_RETRY_MAX_MS: u32 = 1000;
/// How long the panel may stay stale before the device reboots (t-44).
const DISPLAY_RESET_AFTER: Duration = Duration::from_secs(10);
/// Reboots in a row for a broken display before giving up on rebooting.
const MAX_DISPLAY_RESETS: u32 = 3;
/// Uptime with a healthy display after which past reboots are forgotten.
const DISPLAY_HEALTHY_AFTER: Duration = Duration::from_secs(60);
/// Button changes queued while the loop renders or flushes.
const BUTTON_QUEUE: usize = 4;
/// Touch changes queued while the loop renders or flushes; a full queue
/// pauses the touch poll, it drops nothing.
const TOUCH_QUEUE: usize = 8;
/// Pause after a failed touch read, so a faulty controller toggling its
/// interrupt cannot keep the touch task busy.
const TOUCH_RETRY: Duration = Duration::from_millis(10);
/// Failed touch reads in a row before the first controller reset.
///
/// Later resets come after 16, 32 and 64 failures, then every
/// [`TOUCH_RESET_EVERY`] (see [`reset_due`]). Powers of two.
const TOUCH_RESET_AFTER: u32 = 8;
/// Failed touch reads between resets, once the interval stops doubling.
const TOUCH_RESET_EVERY: u32 = 64;
/// Longest pause between attempts to reset the touch controller.
const TOUCH_RESET_RETRY_MAX: Duration = Duration::from_secs(1);
/// Accelerometer sample period; a rotation needs
/// [`OrientationDetector::STABLE_SAMPLES`] samples in a row.
const ORIENTATION_PERIOD: Duration = Duration::from_millis(100);
/// View rotation at boot, until the accelerometer says otherwise.
const INITIAL_ROTATION: Rotation = Rotation::Deg0;

/// Debounced BOOT button changes, from [`watch_button`] to the UI loop.
static BUTTON_EVENTS: Channel<CriticalSectionRawMutex, ButtonEvent, BUTTON_QUEUE> = Channel::new();
/// Touch presses, moves, releases and cancellations, from [`watch_touch`]
/// to the UI loop.
static TOUCH_EVENTS: Channel<CriticalSectionRawMutex, TouchEvent, TOUCH_QUEUE> = Channel::new();
/// Latest view rotation, from [`watch_orientation`] to the UI loop.
static ROTATION: Signal<CriticalSectionRawMutex, Rotation> = Signal::new();

#[device::entry]
async fn main(spawner: Spawner) {
    let Board {
        mut display,
        button,
        touch,
        imu,
        frame,
    } = Board::init().await;
    spawner.spawn(watch_button(button).unwrap());
    spawner.spawn(watch_touch(touch).unwrap());
    if let Some(imu) = imu {
        spawner.spawn(watch_orientation(imu).unwrap());
    }
    let size = display.size();
    let panel = PanelSize {
        width: size.0,
        height: size.1,
    };
    let mut frame = FrameBuffer::new(frame, size);

    // The framebuffer persists, so Slint only redraws what changed.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    window.set_size(PhysicalSize::new(u32::from(size.0), u32::from(size.1)));
    // Slint is built with `unsafe-single-threaded`: only this task may use it.
    slint::platform::set_platform(Box::new(TheoPlatform::new(window.clone()))).unwrap();
    let ui = MainWindow::new().unwrap();
    // Showing the window is what requests the first redraw.
    ui.show().unwrap();

    // After a failed flush the panel is stale: the next flush sends every
    // row, at the latest at `resend_at` for a transient failure (ADR-0004).
    let mut flush_retry = FlushRetry::new(FLUSH_RETRY_FIRST_MS, FLUSH_RETRY_MAX_MS);
    let mut resend_at: Option<Instant> = None;
    let mut first_frame_sent = false;
    let mut display_watch = DisplayWatch::new();
    let mut rotation = INITIAL_ROTATION;
    // Set when `rotation` changed and the next frame must redraw everything.
    let mut rotate_pending = false;
    // Rotation signalled while a finger was down: applied once it lifts, so
    // a press and its release are mapped the same way.
    let mut deferred_rotation: Option<Rotation> = None;
    let mut finger_down = false;
    // Dump the first frame, then the frame after each BOOT press.
    #[cfg(feature = "frame-dump")]
    let mut dump_pending = true;
    loop {
        let frame_start = Instant::now();
        slint::platform::update_timers_and_animations();
        let mut dirty = FrameRows::new(size.1);
        let drawn = window.draw_if_needed(|renderer| {
            if rotate_pending {
                renderer.set_rendering_rotation(rendering_rotation(rotation));
                // Every pixel moves. Changing the buffer type empties Slint's
                // partial-rendering cache, so the render below redraws the
                // whole frame, once.
                renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
                renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            }
            dirty = frame.render(renderer);
        });
        if drawn {
            rotate_pending = false;
        }
        if flush_retry.is_stale() {
            // Hold flushes until the resend is due, so redraws (an animation)
            // cannot bypass the backoff. After a permanent failure nothing is
            // scheduled: a redraw schedules one, at most once a second.
            if !dirty.is_empty() && resend_at.is_none() {
                resend_at = Some(Instant::now() + retry_delay(flush_retry.max_ms()));
            }
            let resend_due = resend_at.is_some_and(|at| Instant::now() >= at);
            dirty = if resend_due {
                frame.all_rows()
            } else {
                FrameRows::new(size.1)
            };
        }
        if !dirty.is_empty() {
            // Cancel-safe, but awaited outside `select`: a cancelled flush
            // would leave its rows stale on the panel (t-3).
            match flush_bands(&mut display, frame.as_bytes(), &dirty).await {
                Ok(()) => {
                    resend_at = None;
                    if let Some(failures) = flush_retry.succeeded() {
                        log::info!("frame flush recovered after {failures} failed attempts");
                    }
                    if !first_frame_sent {
                        first_frame_sent = true;
                        // Slint's allocations are in place once a frame is out.
                        device::log_heap_usage();
                    }
                    #[cfg(feature = "frame-dump")]
                    if dump_pending {
                        dump_pending = false;
                        device::dump_frame(size, frame.as_bytes());
                    }
                }
                Err(error) => {
                    // The framebuffer is still right; only the panel is stale.
                    let retry = flush_retry.failed(error.is_transient());
                    if retry.log {
                        match retry.after_ms {
                            Some(_) => log::error!("frame flush failed, retrying: {error}"),
                            None => log::error!(
                                "frame flush failed, not retrying until the next redraw: {error}"
                            ),
                        }
                    }
                    resend_at = retry.after_ms.map(|ms| Instant::now() + retry_delay(ms));
                }
            }
        }

        display_watch.check(flush_retry.is_stale());

        let wake_up = Timer::at(next_wake_up(&window, frame_start, resend_at));
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
        let mut on_touch = |event| {
            finger_down = matches!(event, TouchEvent::Pressed(_) | TouchEvent::Moved(_));
            dispatch_touch(&window, event, rotation, panel);
        };
        match select4(
            wake_up,
            BUTTON_EVENTS.receive(),
            TOUCH_EVENTS.receive(),
            ROTATION.wait(),
        )
        .await
        {
            Either4::First(()) => {}
            Either4::Second(event) => on_button(event),
            Either4::Third(event) => on_touch(event),
            Either4::Fourth(new_rotation) => deferred_rotation = Some(new_rotation),
        }
        // Catch up with everything queued during the frame before drawing
        // the next one, in the rotation those touches were made in.
        while let Ok(event) = BUTTON_EVENTS.try_receive() {
            on_button(event);
        }
        while let Ok(event) = TOUCH_EVENTS.try_receive() {
            on_touch(event);
        }
        if let Some(new_rotation) = deferred_rotation.filter(|_| !finger_down) {
            deferred_rotation = None;
            rotation = new_rotation;
            rotate_pending = true;
            let view = rotation.view_size(panel);
            window.set_size(PhysicalSize::new(
                u32::from(view.width),
                u32::from(view.height),
            ));
            window.request_redraw();
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
/// pauses the task for [`TOUCH_RETRY`]. Failures in a row reset the
/// controller ([`TOUCH_RESET_AFTER`]); a failed reset is retried until it
/// succeeds, so the controller is never left held in reset. An idle
/// controller is read once a second, so one that went silent fails too.
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
                if reset_due(failures, TOUCH_RESET_AFTER, TOUCH_RESET_EVERY) {
                    log::warn!("resetting the touch controller after {failures} failed reads");
                    reset_touch(&mut touch).await;
                }
                Timer::after(TOUCH_RETRY).await;
            }
        }
    }
}

/// Resets the touch controller, retrying with a growing pause until it
/// works: a reset that fails half-way can hold the controller in reset.
async fn reset_touch(touch: &mut device::Touch) {
    let mut pause = TOUCH_RETRY;
    let mut attempts: u32 = 0;
    while let Err(error) = touch.reset().await {
        if attempts == 0 {
            log::error!("touch controller reset failed, retrying: {error}");
        }
        attempts = attempts.saturating_add(1);
        Timer::after(pause).await;
        pause = (pause * 2).min(TOUCH_RESET_RETRY_MAX);
    }
    if attempts > 0 {
        log::info!("touch controller reset after {attempts} failed attempts");
    }
}

/// Samples the accelerometer and signals view rotation changes.
///
/// Logs each change with the sample on the panel's axes. With the
/// `imu-calibration` feature, keeps the view upright and logs the raw
/// sensor axes once a second instead, to measure [`device::IMU_AXES`].
/// Read failures are logged once per streak.
#[embassy_executor::task]
async fn watch_orientation(mut imu: device::Imu) {
    /// Samples between two calibration logs: one second.
    const CALIBRATION_LOG_EVERY: u64 = 1000 / ORIENTATION_PERIOD.as_millis();
    let mut detector = OrientationDetector::new(INITIAL_ROTATION);
    let mut first_sample = true;
    let mut samples: u64 = 0;
    // A ticker keeps the period exact whatever each read costs.
    let mut ticker = Ticker::every(ORIENTATION_PERIOD);
    // Consecutive failed reads.
    let mut failures: u32 = 0;
    loop {
        ticker.next().await;
        match imu.acceleration().await {
            Ok(sample) => {
                if failures > 0 {
                    log::info!("IMU reads recovered after {failures} failures");
                    failures = 0;
                }
                if cfg!(feature = "imu-calibration") {
                    if samples.is_multiple_of(CALIBRATION_LOG_EVERY) {
                        log::info!("raw IMU sample: {sample:?}");
                    }
                    samples = samples.wrapping_add(1);
                    continue;
                }
                let up = device::IMU_AXES.apply(sample);
                if core::mem::take(&mut first_sample) {
                    log::info!("first IMU sample on panel axes: {up:?}");
                }
                if let Some(rotation) = detector.update(up) {
                    log::info!("view rotation {rotation:?} for {up:?}");
                    ROTATION.signal(rotation);
                }
            }
            Err(error) => {
                if failures == 0 {
                    log::error!("IMU read failed: {error}");
                }
                failures = failures.saturating_add(1);
            }
        }
    }
}

/// Reboots the device when the panel stays stale (t-44).
///
/// A display no flush can repair (a lost SPI master, a permanent error)
/// is recovered by a reboot, as at boot; after [`MAX_DISPLAY_RESETS`] in a
/// row the cause survives reboots, so the device keeps running instead.
/// The count is forgotten once the display stays healthy.
struct DisplayWatch {
    /// When the panel last became stale.
    stale_since: Option<Instant>,
    /// Whether giving up on reboots was logged.
    gave_up: bool,
}

impl DisplayWatch {
    fn new() -> Self {
        let resets = device::display_resets();
        if resets > 0 {
            log::warn!("rebooted {resets} times in a row for a broken display");
        }
        Self {
            stale_since: None,
            gave_up: false,
        }
    }

    /// Checks the panel after a frame; reboots if it stayed stale too long.
    fn check(&mut self, stale: bool) {
        let now = Instant::now();
        if !stale {
            self.stale_since = None;
            let uptime = Duration::from_micros(now.as_micros());
            if uptime >= DISPLAY_HEALTHY_AFTER && device::display_resets() > 0 {
                device::clear_display_resets();
                log::info!("display healthy: reboot count cleared");
            }
            return;
        }
        let stale_for = now - *self.stale_since.get_or_insert(now);
        let resets = device::display_resets();
        if display_reset_due(
            stale_for.as_millis(),
            DISPLAY_RESET_AFTER.as_millis(),
            resets,
            MAX_DISPLAY_RESETS,
        ) {
            log::error!("display stale for {} ms, rebooting", stale_for.as_millis());
            device::reset_for_display();
        }
        if stale_for >= DISPLAY_RESET_AFTER && !core::mem::replace(&mut self.gave_up, true) {
            log::error!("display still broken after {resets} reboots; not rebooting again");
        }
    }
}

/// A [`FlushRetry`] delay as a [`Duration`].
fn retry_delay(ms: u32) -> Duration {
    Duration::from_millis(u64::from(ms))
}

/// Slint's name for `rotation`: both turn the view clockwise.
fn rendering_rotation(rotation: Rotation) -> RenderingRotation {
    match rotation {
        Rotation::Deg0 => RenderingRotation::NoRotation,
        Rotation::Deg90 => RenderingRotation::Rotate90,
        Rotation::Deg180 => RenderingRotation::Rotate180,
        Rotation::Deg270 => RenderingRotation::Rotate270,
    }
}

/// When the UI loop wakes next.
///
/// One frame period after `frame_start` while animating, otherwise at the
/// next Slint timer (at most [`MAX_IDLE`] from now); earlier if a failed
/// flush is due for a resend at `resend_at`.
fn next_wake_up(
    window: &MinimalSoftwareWindow,
    frame_start: Instant,
    resend_at: Option<Instant>,
) -> Instant {
    let wake_up = if window.has_active_animations() {
        frame_start + FRAME_PERIOD
    } else {
        let idle = slint::platform::duration_until_next_timer_update()
            .and_then(|delay| u64::try_from(delay.as_micros()).ok())
            .map_or(MAX_IDLE, |micros| {
                Duration::from_micros(micros).min(MAX_IDLE)
            });
        Instant::now() + idle
    };
    resend_at.map_or(wake_up, |resend_at| wake_up.min(resend_at))
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
///
/// Points arrive in panel pixels and are mapped into the view drawn with
/// `rotation` on `panel`: Slint takes input in un-rotated view coordinates.
fn dispatch_touch(
    window: &MinimalSoftwareWindow,
    event: TouchEvent,
    rotation: Rotation,
    panel: PanelSize,
) {
    // View pixels are logical pixels: the window keeps Slint's default
    // scale factor of 1.
    let position = |point: TouchPoint| {
        let view = rotation.panel_to_view(point, panel);
        LogicalPosition::new(f32::from(view.x), f32::from(view.y))
    };
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
