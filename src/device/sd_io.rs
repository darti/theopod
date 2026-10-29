//! Core 0 keeps the non-Send async HAL. The LittleFS owner on core 1 sends
//! owned sectors; no borrowed buffers or peripheral pointers cross cores.

use aligned::{A4, Aligned};
use embassy_time::{Instant, with_deadline};
use littlefs_rust::Error;

use super::{HalDevice, worker::SECTORS};
use theo_storage::sector_bus::Operation;

#[embassy_executor::task]
pub(super) async fn serve(mut device: HalDevice) {
    loop {
        let request = SECTORS.receive().await;
        let result = if SECTORS.is_disabled()
            || Instant::now() >= Instant::from_ticks(request.deadline_ticks)
        {
            Err(Error::Io)
        } else {
            with_deadline(Instant::from_ticks(request.deadline_ticks), async {
                let mut scratch = Aligned::<A4, _>([0; 512]);
                match request.operation {
                    Operation::Read(sector) => {
                        block_device_driver::BlockDevice::read(
                            &mut device,
                            sector,
                            core::slice::from_mut(&mut scratch),
                        )
                        .await
                        .map_err(|_| Error::Io)?;
                    }
                    Operation::Write(sector, bytes) => {
                        #[cfg(feature = "sd-check")]
                        match option_env!("THEO_SD_FAULT") {
                            Some("full") => return Err(Error::NoSpace),
                            Some("readonly" | "removed") => return Err(Error::Io),
                            Some("timeout") => core::future::pending::<()>().await,
                            _ => {}
                        }
                        scratch.copy_from_slice(&bytes);
                        block_device_driver::BlockDevice::write(
                            &mut device,
                            sector,
                            core::slice::from_ref(&scratch),
                        )
                        .await
                        .map_err(|_| Error::Io)?;
                    }
                    // Each SPI write already waits until the card releases
                    // busy, deselects it and sends idle clocks. No writes are
                    // queued or cached in this layer.
                    Operation::Sync => {}
                }
                Ok(*scratch)
            })
            .await
            .unwrap_or(Err(Error::Io))
        };
        if result.is_err() {
            SECTORS.disable();
            SECTORS.respond(result);
            // A cancelled HAL operation cannot be resumed safely. Dropping
            // the whole device also prevents subsequent queued requests.
            return;
        }
        SECTORS.respond(result);
    }
}
