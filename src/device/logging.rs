//! Nonblocking dual serial/SD producer; storage owns persistence and shutdown.

use core::cell::RefCell;
use critical_section::Mutex;
use theo_storage::{LogBuffer, log_enabled};

pub(super) const QUEUE_SIZE: usize = 4096;
const RECORD_SIZE: usize = 1024;
const FILTER: &str = match option_env!("ESP_LOG") {
    Some(value) => value,
    None => "off",
};
static QUEUE: Mutex<RefCell<LogBuffer<QUEUE_SIZE>>> = Mutex::new(RefCell::new(LogBuffer::new()));
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
        super::storage::request_flush();
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

pub(super) fn take_queue() -> LogBuffer<QUEUE_SIZE> {
    critical_section::with(|cs| core::mem::take(&mut *QUEUE.borrow(cs).borrow_mut()))
}
