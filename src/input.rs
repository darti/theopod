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
//! ([`report_panel_asleep`], [`PANEL_SLEEP_TIMEOUT`] at most), has the
//! orientation then the touch task park their devices ([`IMU_PARK`],
//! [`TOUCH_PARK`]), then waits for the release and puts the chip in deep
//! sleep.

use core::cell::RefCell;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex},
    channel::Channel,
    signal::Signal,
};
use embassy_time::{Delay, Duration, Instant, Ticker, Timer, with_timeout};
use theo_drivers::{
    ButtonEvent, InputTask, InputWatch, LongPressButton, LongPressEvent, OrientationDetector,
    Rotation, TouchEvent, reset_due,
};

use crate::device;

/// Maximum interval without a completed input operation (t-83).
pub const INPUT_STALE_MS: u64 = 3000;
static INPUT_WATCH: Mutex<CriticalSectionRawMutex, RefCell<InputWatch>> =
    Mutex::new(RefCell::new(InputWatch::new(INPUT_STALE_MS)));

fn progress(task: InputTask) {
    INPUT_WATCH.lock(|watch| {
        watch
            .borrow_mut()
            .progress(task, Instant::now().as_millis())
    });
}

/// Checks before feeding MWDT0; a failure remains latched until reboot.
pub fn stalled_input() -> Option<InputTask> {
    INPUT_WATCH.lock(|watch| watch.borrow_mut().check(Instant::now().as_millis()))
}

/// Gives inputs a fresh grace interval after an executor-blocking frame dump.
#[cfg(feature = "frame-dump")]
pub fn resume_after_dump() {
    INPUT_WATCH.lock(|watch| watch.borrow_mut().rebase(Instant::now().as_millis()));
}

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
/// Longest wait for the UI loop to put the panel to sleep before a stop.
///
/// Room for one flush in flight and the sleep-in, each bounded by the
/// loop's `FLUSH_TIMEOUT` (`src/main.rs`, 500 ms), and a render. A loop that
/// keeps retrying flushes past that is the stall this bounds.
const PANEL_SLEEP_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest wait for an input task to park its device before deep sleep: at
/// most four I2C writes, each failing within the device module's bus
/// timeout (about 13 ms).
const PARK_TIMEOUT: Duration = Duration::from_millis(200);

/// BOOT button changes and stop requests, from [`watch_button`] to the UI
/// loop.
pub static BUTTON_EVENTS: Channel<CriticalSectionRawMutex, ButtonMessage, BUTTON_QUEUE> =
    Channel::new();
/// The panel is asleep, from the UI loop ([`report_panel_asleep`]) to
/// [`watch_button`].
static PANEL_ASLEEP: Signal<CriticalSectionRawMutex, ()> = Signal::new();
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
    /// BOOT was held: put the panel to sleep, then call
    /// [`report_panel_asleep`].
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
    INPUT_WATCH.lock(|watch| {
        let mut watch = watch.borrow_mut();
        let now = Instant::now().as_millis();
        watch.enable(InputTask::Button, now);
        watch.enable(InputTask::Touch, now);
        if imu.is_some() {
            watch.enable(InputTask::Orientation, now);
        }
    });
    spawner.spawn(watch_button(button, deep_sleep).unwrap());
    spawner.spawn(watch_touch(touch).unwrap());
    if let Some(imu) = imu {
        spawner.spawn(watch_orientation(imu).unwrap());
    } else {
        IMU_PARK.not_needed();
    }
}

/// Reports the panel asleep to [`watch_button`], then never returns.
///
/// The UI loop calls it last: nothing runs there until the chip sleeps.
pub async fn report_panel_asleep() -> ! {
    PANEL_ASLEEP.signal(());
    core::future::pending().await
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

/// Forwards BOOT changes to the UI loop; a long press stops the pod.
///
/// Changes are dropped, not queued, while the UI loop's queue is full. A
/// press held for [`LONG_PRESS_MS`] asks the UI loop to put the panel to
/// sleep and waits for that, [`PANEL_SLEEP_TIMEOUT`] at most, has the
/// orientation then the touch task park their devices, then waits for the
/// release and puts the chip in deep sleep until the next press. Its release
/// is not forwarded: it is not a click.
#[embassy_executor::task]
async fn watch_button(button: device::Button, deep_sleep: device::DeepSleep) {
    // `Delay` and `Instant` run on the same embassy time driver, as
    // `LongPressButton` requires.
    let now_ms: fn() -> u64 = || Instant::now().as_millis();
    let mut button = LongPressButton::new(button, LONG_PRESS_MS, Delay, now_ms);
    #[cfg(feature = "sd-check")]
    let automatic_stop_at = Instant::now() + Duration::from_secs(5);
    loop {
        #[cfg(feature = "sd-check")]
        let next = if device::diagnostic_sleep_requested() {
            match select(
                button.next_with_progress(|| progress(InputTask::Button)),
                Timer::at(automatic_stop_at),
            )
            .await
            {
                Either::First(next) => next,
                Either::Second(()) => {
                    log::info!("SD check: requesting automatic deep sleep");
                    break;
                }
            }
        } else {
            button
                .next_with_progress(|| progress(InputTask::Button))
                .await
        };
        #[cfg(not(feature = "sd-check"))]
        let next = button
            .next_with_progress(|| progress(InputTask::Button))
            .await;
        match next {
            // Never waits on the UI loop (t-54 review): a stalled loop with a
            // full queue would otherwise keep the long press from being seen.
            Ok(LongPressEvent::Changed(event)) => {
                if BUTTON_EVENTS
                    .try_send(ButtonMessage::Changed(event))
                    .is_err()
                {
                    log::warn!("button {event:?} dropped: the UI loop is not keeping up");
                }
            }
            Ok(LongPressEvent::Held) => {
                log::info!("BOOT held for {LONG_PRESS_MS} ms, stopping");
                break;
            }
            Err(error) => log::error!("button read failed: {error:?}"),
        }
    }
    INPUT_WATCH.lock(|watch| watch.borrow_mut().stop());
    // Bounded (t-54): a stalled UI loop, which may not even drain the
    // queue, must not keep the pod awake. The touch park below cuts the
    // panel rails anyway, so a panel left awake still goes dark.
    let sleep_panel = async {
        BUTTON_EVENTS.send(ButtonMessage::Stop).await;
        PANEL_ASLEEP.wait().await;
    };
    if with_timeout(PANEL_SLEEP_TIMEOUT, sleep_panel)
        .await
        .is_err()
    {
        log::error!("panel not asleep within {PANEL_SLEEP_TIMEOUT:?}, stopping anyway");
    }
    // The panel is asleep (or timed out) before its rails drop. Cutting them is the last
    // bus access: the touch controller may hang off those rails, and an
    // unpowered device can clamp the bus for the IMU.
    IMU_PARK.request().await;
    TOUCH_PARK.request().await;
    deep_sleep.until_pressed(button.into_button()).await
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
        match touch
            .changed_with_progress(|| progress(InputTask::Touch))
            .await
        {
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
    loop {
        let result = touch.reset().await;
        progress(InputTask::Touch);
        let Err(error) = result else {
            break;
        };
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
        let sample = imu.acceleration().await;
        progress(InputTask::Orientation);
        match sample {
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
