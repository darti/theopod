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
//! Holding BOOT for [`LONG_PRESS_MS`] stops the pod: the loop puts the
//! panel to sleep, [`watch_button`] has the orientation then the touch task
//! park their devices ([`IMU_PARK`], [`TOUCH_PARK`]), then waits for the
//! release and puts the chip in deep sleep. The next BOOT press wakes it,
//! which restarts the firmware; that press never reaches Slint.
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
use embassy_futures::select::{Either, Either4, select, select4};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use embassy_time::{Delay, Duration, Instant, Ticker, TimeoutError, Timer, with_timeout};
use slint::{
    ComponentHandle, LogicalPosition, PhysicalSize,
    platform::{
        Key, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RenderingRotation, RepaintBufferType},
    },
};
use theo_drivers::{
    ButtonEvent, FlushRetry, LongPress, OrientationDetector, Panel, PanelSize, Rotation,
    StaleWatch, TouchEvent, TouchPoint, Transient, WatchAction, flush_bands, reset_due,
};

use crate::{
    device::Board,
    slint_backend::{FrameRenderer, FrameRows, TheoPlatform},
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
/// How long BOOT is held to stop the pod, from the debounced press.
const LONG_PRESS_MS: u32 = 2000;
/// Longest a flush may take before it is taken for hung (t-44 review).
///
/// A full frame takes about 17 ms on the bus.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(500);
/// How long the panel may stay stale before the device reboots (t-44).
const DISPLAY_RESET_AFTER: Duration = Duration::from_secs(10);
/// Reboots in a row for a broken display before giving up on rebooting.
const MAX_DISPLAY_RESETS: u32 = 3;
/// Uninterrupted healthy display after which past reboots are forgotten.
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
/// Longest wait for an input task to park its device before deep sleep: at
/// most four I2C writes, each failing within the device module's bus
/// timeout (about 13 ms).
const PARK_TIMEOUT: Duration = Duration::from_millis(200);

/// BOOT button changes and stop requests, from [`watch_button`] to the UI
/// loop.
static BUTTON_EVENTS: Channel<CriticalSectionRawMutex, ButtonMessage, BUTTON_QUEUE> =
    Channel::new();
/// The panel is asleep, from the UI loop to [`watch_button`].
static PANEL_ASLEEP: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Touch presses, moves, releases and cancellations, from [`watch_touch`]
/// to the UI loop.
static TOUCH_EVENTS: Channel<CriticalSectionRawMutex, TouchEvent, TOUCH_QUEUE> = Channel::new();
/// Latest view rotation, from [`watch_orientation`] to the UI loop.
static ROTATION: Signal<CriticalSectionRawMutex, Rotation> = Signal::new();
/// Stop handshake between [`watch_button`] and [`watch_touch`].
static TOUCH_PARK: Park = Park::new("touch controller and panel rails");
/// Stop handshake between [`watch_button`] and [`watch_orientation`].
static IMU_PARK: Park = Park::new("IMU");

/// Stop handshake: [`watch_button`] asks an input task to park its device,
/// which then stops using the I2C bus for good (t-50).
///
/// The task drops its loop wherever it is, even mid-transfer (esp-hal
/// clears the bus when a transfer is dropped), parks its device and
/// reports it, failed or not.
struct Park {
    /// What is parked, for the logs.
    device: &'static str,
    requested: Signal<CriticalSectionRawMutex, ()>,
    parked: Signal<CriticalSectionRawMutex, ()>,
}

impl Park {
    const fn new(device: &'static str) -> Self {
        Self {
            device,
            requested: Signal::new(),
            parked: Signal::new(),
        }
    }

    /// Asks the task to park and waits for it, [`PARK_TIMEOUT`] at most.
    ///
    /// Best effort, like the panel's sleep-in: a task that does not answer
    /// is logged, and the chip sleeps anyway.
    async fn request(&self) {
        self.requested.signal(());
        if with_timeout(PARK_TIMEOUT, self.parked.wait())
            .await
            .is_err()
        {
            let device = self.device;
            log::error!("{device} not parked within {PARK_TIMEOUT:?}, sleeping anyway");
        }
    }

    /// Marks the device parked, for a task that never started: the stop
    /// then does not wait for it.
    fn not_needed(&self) {
        self.parked.signal(());
    }

    /// Waits until [`watch_button`] asks for parking.
    async fn requested(&self) {
        self.requested.wait().await;
    }

    /// Reports the device parked (or given up on); the task then idles.
    async fn done(&self) -> ! {
        self.parked.signal(());
        core::future::pending().await
    }
}

/// What [`watch_button`] tells the UI loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonMessage {
    /// A press or release for Slint.
    Changed(ButtonEvent),
    /// BOOT was held: put the panel to sleep, then stop.
    Stop,
}

#[device::entry]
async fn main(spawner: Spawner) {
    let Board {
        mut display,
        button,
        deep_sleep,
        touch,
        imu,
    } = Board::init().await;
    spawner.spawn(watch_button(button, deep_sleep).unwrap());
    spawner.spawn(watch_touch(touch).unwrap());
    if let Some(imu) = imu {
        spawner.spawn(watch_orientation(imu).unwrap());
    } else {
        IMU_PARK.not_needed();
    }
    let size = display.size();
    let panel = PanelSize {
        width: size.0,
        height: size.1,
    };
    let frame_renderer = FrameRenderer::new(size);

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
    let mut stop = false;
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
            dirty = frame_renderer.render(renderer, display.frame_mut());
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
                frame_renderer.all_rows()
            } else {
                FrameRows::new(size.1)
            };
        }
        if !dirty.is_empty() {
            // Not raced against inputs: a cancelled flush leaves its rows
            // stale (t-3). Only a hung one is cancelled, after
            // [`FLUSH_TIMEOUT`]; the flush is cancel-safe, so the bus stays
            // usable, and the hang counts as a transient failure.
            let flushed = with_timeout(FLUSH_TIMEOUT, flush_bands(&mut display, &dirty)).await;
            match flushed {
                Err(TimeoutError) => {
                    let retry = flush_retry.failed(true);
                    if retry.log {
                        log::error!("frame flush timed out after {FLUSH_TIMEOUT:?}, retrying");
                    }
                    resend_at = retry.after_ms.map(|ms| Instant::now() + retry_delay(ms));
                }
                Ok(Ok(())) => {
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
                        device::dump_frame(size, display.frame());
                    }
                }
                Ok(Err(error)) => {
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
        let mut on_button = |message| match message {
            ButtonMessage::Changed(event) => {
                #[cfg(feature = "frame-dump")]
                if event == ButtonEvent::Pressed {
                    dump_pending = true;
                }
                dispatch_button(&window, event);
            }
            ButtonMessage::Stop => stop = true,
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
        if stop {
            // No flush is in flight here: flushes are awaited above. Best
            // effort: a panel that ignores sleep-in would ignore a retry
            // too, and staying awake would not darken it either. Bounded
            // like a flush, so a hung bus cannot keep the pod from stopping.
            match with_timeout(FLUSH_TIMEOUT, display.sleep(&mut Delay)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::error!("panel sleep failed: {error}"),
                Err(TimeoutError) => {
                    log::error!("panel sleep timed out after {FLUSH_TIMEOUT:?}");
                }
            }
            PANEL_ASLEEP.signal(());
            // `watch_button` puts the chip to sleep; nothing to draw until then.
            core::future::pending::<()>().await;
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

/// Debounces the BOOT button and queues its changes for the UI loop, until
/// it is held for [`LONG_PRESS_MS`]; then stops the pod.
///
/// A long press asks the UI loop to put the panel to sleep, waits for that,
/// has the orientation then the touch task park their devices, then waits
/// for the release and puts the chip in deep sleep until the next press.
/// Its release is not forwarded: it is not a click.
#[embassy_executor::task]
async fn watch_button(mut button: device::Button, deep_sleep: device::DeepSleep) {
    let mut press = LongPress::new(LONG_PRESS_MS);
    loop {
        let changed = match press.deadline_ms() {
            // Cancelling `changed` keeps the debounced state (pressed). A
            // release still in its debounce at the deadline loses: the
            // stop window is at most one debounce delay short.
            Some(at) => match select(button.changed(), Timer::at(Instant::from_millis(at))).await {
                Either::First(changed) => changed,
                Either::Second(()) => {
                    press.held();
                    break;
                }
            },
            None => button.changed().await,
        };
        match changed {
            Ok(event) => {
                if let Some(event) = press.changed(event, Instant::now().as_millis()) {
                    BUTTON_EVENTS.send(ButtonMessage::Changed(event)).await;
                }
            }
            Err(error) => log::error!("button read failed: {error:?}"),
        }
    }
    log::info!("BOOT held for {LONG_PRESS_MS} ms, stopping");
    BUTTON_EVENTS.send(ButtonMessage::Stop).await;
    PANEL_ASLEEP.wait().await;
    // The panel is asleep before its rails drop. Cutting them is the last
    // bus access: the touch controller may hang off those rails, and an
    // unpowered device can clamp the bus for the IMU.
    IMU_PARK.request().await;
    TOUCH_PARK.request().await;
    deep_sleep.until_pressed(button).await
}

/// Reports touches to the UI loop.
///
/// A failed read mid-touch reaches the loop as [`TouchEvent::Cancelled`]
/// (from the driver). Failures are logged once per streak, and each one
/// pauses the task for [`TOUCH_RETRY`]. Failures in a row reset the
/// controller ([`TOUCH_RESET_AFTER`]); a failed reset is retried until it
/// succeeds, so the controller is never left held in reset. An idle
/// controller is read once a second, so one that went silent fails too.
///
/// When the pod stops, holds the controller in reset and cuts the panel
/// rails ([`TOUCH_PARK`]).
#[embassy_executor::task]
async fn watch_touch(mut touch: device::Touch) {
    match select(report_touches(&mut touch), TOUCH_PARK.requested()).await {
        Either::First(never) => never,
        Either::Second(()) => {}
    }
    if let Err(error) = touch.park().await {
        log::error!("touch controller and panel rails not parked: {error:?}");
    }
    TOUCH_PARK.done().await
}

/// The touch loop of [`watch_touch`], until it is dropped.
async fn report_touches(touch: &mut device::Touch) -> ! {
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
                    reset_touch(touch).await;
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
/// Read failures are logged once per streak. When the pod stops, powers the
/// sensor down ([`IMU_PARK`]).
#[embassy_executor::task]
async fn watch_orientation(mut imu: device::Imu) {
    match select(report_orientation(&mut imu), IMU_PARK.requested()).await {
        Either::First(never) => never,
        Either::Second(()) => {}
    }
    if let Err(error) = imu.power_down().await {
        log::error!("IMU not powered down: {error}");
    }
    IMU_PARK.done().await
}

/// The sampling loop of [`watch_orientation`], until it is dropped.
async fn report_orientation(imu: &mut device::Imu) -> ! {
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
/// Runs the device side of [`StaleWatch`]: rebooting, logging, and the
/// reboot count kept across resets.
struct DisplayWatch {
    watch: StaleWatch,
}

impl DisplayWatch {
    fn new() -> Self {
        let resets = device::display_resets();
        if resets > 0 {
            log::warn!("rebooted {resets} times in a row for a broken display");
        }
        Self {
            watch: StaleWatch::new(
                DISPLAY_RESET_AFTER.as_millis(),
                DISPLAY_HEALTHY_AFTER.as_millis(),
                MAX_DISPLAY_RESETS,
            ),
        }
    }

    /// Checks the panel after a frame; reboots if it stayed stale too long.
    fn check(&mut self, stale: bool) {
        let resets = device::display_resets();
        match self.watch.check(Instant::now().as_millis(), stale, resets) {
            WatchAction::None => {}
            WatchAction::Reboot => {
                log::error!("display stale for {DISPLAY_RESET_AFTER:?}, rebooting");
                device::reset_for_display();
            }
            WatchAction::GiveUp => {
                log::error!("display still broken after {resets} reboots; not rebooting again");
            }
            WatchAction::ClearResets => {
                device::clear_display_resets();
                log::info!("display healthy: reboot count cleared");
            }
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
///
/// A long press only sends the press: UI actions on Return must fire on
/// release, or every stop would trigger them (ADR-0008).
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
