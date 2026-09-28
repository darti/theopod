#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{delay::Delay, main};

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let _peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();

    esp_println::println!("Theo Pod — ESP32-S3 hello world");
    esp_println::println!("Firmware initialisée, écran/tactile/audio à venir.");

    loop {
        esp_println::println!("Hello world !");
        delay.delay_millis(1_000);
    }
}
