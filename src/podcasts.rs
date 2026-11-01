//! Minimal local command/status hooks; playback and a subscription UI are future work.
use crate::device::{network, storage};
use embassy_executor::Spawner;
use embassy_time::Timer;
use esp_hal::{
    peripherals::{USB_DEVICE, WIFI},
    usb::usb_serial_jtag::UsbSerialJtag,
};
use theo_podcasts::{
    catalogue::EpisodeId,
    runtime::{Command, Reply},
    setup::{SetupCommand, SetupInput},
};
pub fn spawn(spawner: Spawner, wifi: WIFI<'static>, usb: USB_DEVICE<'static>) {
    spawner.spawn(network::run(wifi).expect("network once"));
    spawner.spawn(setup(usb).expect("USB setup once"));
}
pub async fn follow(feed: heapless::String<256>) -> Result<(), ()> {
    if !matches!(
        storage::command(Command::Follow(feed)).await?,
        Reply::Followed(_)
    ) {
        return Err(());
    }
    network::sync();
    Ok(())
}
pub async fn mark_read(id: EpisodeId) -> Result<(), ()> {
    if !matches!(storage::command(Command::MarkRead(id)).await?, Reply::Done) {
        return Err(());
    }
    network::retry();
    Ok(())
}
#[embassy_executor::task]
async fn setup(usb: USB_DEVICE<'static>) {
    // Receive only. Serial log transmission stays with the existing logger.
    let (mut rx, _tx) = UsbSerialJtag::new(usb).split();
    let mut input = SetupInput::default();
    loop {
        // Bound input processing even if the host floods the USB FIFO.
        for _ in 0..64 {
            let Ok(byte) = rx.read_byte() else {
                break;
            };
            if let Some(command) = input.push(byte) {
                let result = match command {
                    Ok(SetupCommand::Time(unix)) => {
                        network::trusted_time(unix);
                        Ok(())
                    }
                    Ok(SetupCommand::Follow(feed)) => follow(feed).await,
                    Ok(SetupCommand::Read(id)) => mark_read(id).await,
                    Ok(SetupCommand::Sync) => {
                        network::sync();
                        Ok(())
                    }
                    Ok(SetupCommand::Retry) => {
                        network::retry();
                        Ok(())
                    }
                    Ok(SetupCommand::Status) => {
                        log::info!("podcasts: {}", network::status());
                        Ok(())
                    }
                    Err(_) => Err(()),
                };
                if result.is_err() {
                    log::warn!("podcast setup command rejected");
                }
            }
        }
        Timer::after_millis(20).await;
    }
}
