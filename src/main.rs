#![no_std]
#![no_main]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc};
use core::time::Duration;

use esp_alloc::EspHeap;
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use slint::{
    PhysicalSize,
    platform::{Platform, WindowAdapter, software_renderer::MinimalSoftwareWindow},
};

esp_bootloader_esp_idf::esp_app_desc!();

slint::include_modules!();

const HEAP_SIZE: usize = 64 * 1024;
static mut HEAP_MEMORY: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
#[global_allocator]
static HEAP: EspHeap = EspHeap::empty();

struct TheoPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for TheoPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        Duration::ZERO
    }
}

#[main]
fn main() -> ! {
    unsafe {
        HEAP.init(HEAP_MEMORY.as_mut_ptr(), HEAP_SIZE);
    }

    esp_println::logger::init_logger_from_env();
    let _peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();

    let window = MinimalSoftwareWindow::new(Default::default());
    window.set_size(PhysicalSize::new(368, 448));
    slint::platform::set_platform(Box::new(TheoPlatform { window })).unwrap();

    let _ui = MainWindow::new().unwrap();
    esp_println::println!("Slint: Hello world !");
    esp_println::println!("AMOLED rendering is waiting for pin mapping.");

    loop {
        slint::platform::update_timers_and_animations();
        delay.delay_millis(1_000);
    }
}
