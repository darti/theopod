//! Nonblocking dual serial/SD logger and its single-owner storage task.

use core::cell::RefCell;
use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Timer, with_timeout};
use theo_storage::{LogBuffer, log_enabled};

use super::sd::{self, SdStorage};

const QUEUE_SIZE: usize = 4096;
const RECORD_SIZE: usize = 1024;
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const FILTER: &str = match option_env!("ESP_LOG") {
    Some(value) => value,
    None => "off",
};
static QUEUE: Mutex<RefCell<LogBuffer<QUEUE_SIZE>>> = Mutex::new(RefCell::new(LogBuffer::new()));
static FLUSH: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static FLUSHED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static LOGGER: Logger = Logger;

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        log_enabled(FILTER, metadata.target(), metadata.level() as u8)
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        esp_println::println!("{} - {}", record.level(), record.args());
        let mut formatted = LogBuffer::<RECORD_SIZE>::new();
        if formatted.push_fmt(format_args!(
            "{} [{}] {}",
            record.level(),
            record.target(),
            record.args()
        )) {
            critical_section::with(|cs| {
                QUEUE.borrow(cs).borrow_mut().push(formatted.pending());
            });
        } else {
            critical_section::with(|cs| QUEUE.borrow(cs).borrow_mut().record_dropped());
        }
    }

    fn flush(&self) {
        FLUSH.signal(());
    }
}

/// Called once, before HAL startup and before other tasks or core 1 exist.
pub(super) fn init() {
    // SAFETY: Board::init is the sole startup caller. No other task/core
    // can install or access the global logger during this initialization.
    unsafe {
        log::set_logger_racy(&LOGGER).expect("logger initialized once");
        log::set_max_level_racy(log::LevelFilter::Trace);
    }
}

fn take_queue() -> LogBuffer<QUEUE_SIZE> {
    critical_section::with(|cs| core::mem::take(&mut *QUEUE.borrow(cs).borrow_mut()))
}

pub fn spawn(spawner: Spawner, volume: Option<SdStorage>) {
    spawner.spawn(persist(volume).expect("storage task spawned once"));
}

/// Best effort before sleep: bounds both an in-flight batch and the final
/// queue flush. This never makes stopping depend on working storage.
pub async fn flush_before_sleep() {
    FLUSHED.reset();
    FLUSH.signal(());
    if with_timeout(Duration::from_millis(1200), FLUSHED.wait())
        .await
        .is_err()
    {
        esp_println::println!("WARN - microSD final flush timed out; sleeping anyway");
    }
}

#[embassy_executor::task]
async fn persist(mut volume: Option<SdStorage>) {
    #[cfg(feature = "sd-check")]
    if option_env!("THEO_SD_FAULT") == Some("rotate")
        && let Some(storage) = volume.as_mut()
    {
        let result = sd::test_rotation(storage).await;
        esp_println::println!("INFO - SD rotation check result: {result:?}");
        if result.is_err() {
            volume = None;
        }
    }
    let mut pending = LogBuffer::<QUEUE_SIZE>::new();
    loop {
        let requested = matches!(
            select(Timer::after_millis(1000), FLUSH.wait()).await,
            Either::Second(())
        );
        if let Some(storage) = volume.as_mut() {
            if pending.is_empty() {
                pending = take_queue();
            }
            let dropped = pending.dropped();
            if dropped > 0 {
                esp_println::println!("WARN - microSD queue dropped {dropped} oldest records");
            }
            if !pending.is_empty() {
                let count = pending.len();
                match with_timeout(IO_TIMEOUT, sd::append_log(storage, &mut pending)).await {
                    Ok(Ok(())) => {
                        esp_println::println!("INFO - microSD synced {count} log bytes");
                    }
                    result => {
                        // Cancelling may leave filesystem metadata or a command in an
                        // unknown state. Never reuse that mount this boot.
                        volume = None;
                        log::error!(
                            "microSD logging disabled until reboot: {result:?}; {} pending bytes retained",
                            pending.len()
                        );
                    }
                }
            }
        }
        if requested {
            FLUSHED.signal(());
        }
    }
}
