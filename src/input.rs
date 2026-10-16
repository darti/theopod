//! Input tasks: the BOOT button, touch and orientation.
//!
//! [`spawn`] starts one task per input ([`watch_button`], [`watch_touch`],
//! [`watch_orientation`]). Each reaches the UI loop through a channel
//! ([`BUTTON_EVENTS`], [`TOUCH_EVENTS`]) or a signal ([`ROTATION`]): the
//! loop cancels its wait every frame, which would otherwise restart the
//! button debounce, or a touch poll, forever.
//!
//! Holding BOOT for [`LONG_PRESS_MS`] stops the pod: [`watch_button`] sends
//! [`ButtonMessage::Stop`], waits for the loop to put the panel to sleep
//! ([`PANEL_ASLEEP`]), has the orientation then the touch task park their
//! devices ([`IMU_PARK`], [`TOUCH_PARK`]), then waits for the release and
//! puts the chip in deep sleep.

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use theo_drivers::{ButtonEvent, LongPress, OrientationDetector, Rotation, TouchEvent, reset_due};

use crate::device;

/// How long BOOT is held to stop the pod, from the debounced press.
pub const LONG_PRESS_MS: u32 = 2000;
/// View rotation at boot, for both the UI loop and the orientation
/// detector, until the accelerometer says otherwise.
pub const INITIAL_ROTATION: Rotation = Rotation::Deg0;
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
/// Longest wait for an input task to park its device before deep sleep: at
/// most four I2C writes, each failing within the device module's bus
/// timeout (about 13 ms).
const PARK_TIMEOUT: Duration = Duration::from_millis(200);

/// BOOT button changes and stop requests, from [`watch_button`] to the UI
/// loop.
pub static BUTTON_EVENTS: Channel<CriticalSectionRawMutex, ButtonMessage, BUTTON_QUEUE> =
    Channel::new();
/// The panel is asleep, from the UI loop to [`watch_button`].
pub static PANEL_ASLEEP: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Touch presses, moves, releases and cancellations, from [`watch_touch`]
/// to the UI loop.
pub static TOUCH_EVENTS: Channel<CriticalSectionRawMutex, TouchEvent, TOUCH_QUEUE> = Channel::new();
/// Latest view rotation, from [`watch_orientation`] to the UI loop.
pub static ROTATION: Signal<CriticalSectionRawMutex, Rotation> = Signal::new();
/// Stop handshake between [`watch_button`] and [`watch_touch`].
static TOUCH_PARK: Park = Park::new("touch controller and panel rails");
/// Stop handshake between [`watch_button`] and [`watch_orientation`].
static IMU_PARK: Park = Park::new("IMU");

/// What [`watch_button`] tells the UI loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonMessage {
    /// A press or release for Slint.
    Changed(ButtonEvent),
    /// BOOT was held: put the panel to sleep, signal [`PANEL_ASLEEP`], then
    /// stop.
    Stop,
}

/// Starts the input tasks.
///
/// Without an IMU the view stays at [`INITIAL_ROTATION`], and the stop does
/// not wait for an IMU to park.
pub fn spawn(
    spawner: Spawner,
    button: device::Button,
    deep_sleep: device::DeepSleep,
    touch: device::Touch,
    imu: Option<device::Imu>,
) {
    spawner.spawn(watch_button(button, deep_sleep).unwrap());
    spawner.spawn(watch_touch(touch).unwrap());
    if let Some(imu) = imu {
        spawner.spawn(watch_orientation(imu).unwrap());
    } else {
        IMU_PARK.not_needed();
    }
}

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
