//! Single frontend owner for storage requests, log persistence and shutdown.
use super::{
    logging::{QUEUE_SIZE, take_queue},
    sd::{self, SdStorage},
};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_sync::{channel::Channel, mutex::Mutex};
use embassy_time::{Duration, Timer, with_timeout};
use theo_podcasts::{
    runtime::{Command, Reply},
    service::{Scheduler, Work},
};
use theo_storage::LogBuffer;
static REQUESTS: Channel<CriticalSectionRawMutex, sd::PodcastRequest, 1> = Channel::new();
static RESPONSE: Signal<CriticalSectionRawMutex, Result<Reply, ()>> = Signal::new();
static CLIENT: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());
static STOPPED: AtomicBool = AtomicBool::new(false);
/// All clients share this one serialized owned-message exchange. Cancelling
/// after admission invalidates the shared sector owner rather than reusing a late reply.
pub async fn command(command: Command) -> Result<Reply, ()> {
    with_timeout(Duration::from_millis(2500), async {
        let _client = CLIENT.lock().await;
        if STOPPED.load(Ordering::Acquire) {
            return Err(());
        }
        let command = allocator_api2::boxed::Box::try_new_in(command, &esp_alloc::ExternalMemory)
            .map_err(|_| ())?;
        let guard = sd::guard().map_err(|_| ())?;
        RESPONSE.reset();
        REQUESTS.send(command).await;
        let result = RESPONSE.wait().await;
        if result.is_ok() {
            guard.complete().map_err(|_| ())?;
        }
        result
    })
    .await
    .map_err(|_| ())?
}
const IO_TIMEOUT: Duration = Duration::from_millis(500);
/// Bound on the stop before sleep: an in-flight podcast request
/// (`PODCAST_BUDGET`) then the final log append (`IO_TIMEOUT`). User-approved
/// 2026-11-12 (t-133): 2 s, up from 1.2 s once podcast requests got 1 s.
const SLEEP_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const _: () = assert!(
    sd::PODCAST_BUDGET.as_millis() + IO_TIMEOUT.as_millis() <= SLEEP_FLUSH_TIMEOUT.as_millis()
        && SLEEP_FLUSH_TIMEOUT.as_millis() < super::WATCHDOG_TIMEOUT_MS / 2
);
static FLUSH: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static FINAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static FLUSHED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub(super) fn request_flush() {
    FLUSH.signal(());
}
pub fn spawn(spawner: Spawner, volume: Option<SdStorage>) {
    spawner.spawn(persist(volume).expect("storage task spawned once"));
}

/// Best effort before sleep: bounds both an in-flight batch and the final
/// queue flush. This never makes stopping depend on working storage.
pub async fn flush_before_sleep() {
    STOPPED.store(true, Ordering::Release);
    let completed = theo_podcasts::worker::shutdown(
        super::network::stop(),
        async {
            FLUSHED.reset();
            FINAL.signal(());
            FLUSHED.wait().await;
        },
        Timer::after(SLEEP_FLUSH_TIMEOUT),
    )
    .await;
    if !completed {
        sd::disable();
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
    let mut scheduler = Scheduler::default();
    let mut data = None;
    loop {
        if data.is_none() {
            data = REQUESTS.try_receive().ok();
        }
        if pending.is_empty() {
            pending = take_queue();
        }
        let final_flush = FINAL.try_take().is_some();
        // Ordinary flushes wake the fair log queue; only shutdown gets
        // control priority, so repeated logger flushes cannot starve data.
        let _ = FLUSH.try_take();
        match scheduler.next(
            final_flush,
            !pending.is_empty() && volume.is_some(),
            data.is_some(),
        ) {
            Some(Work::Control) => {
                if let Some(storage) = volume.as_mut() {
                    let result =
                        with_timeout(IO_TIMEOUT, sd::append_log(storage, &mut pending)).await;
                    if !matches!(result, Ok(Ok(()))) {
                        volume = None;
                    }
                    if let Some(storage) = volume.as_mut() {
                        pending = take_queue();
                        let result =
                            with_timeout(IO_TIMEOUT, sd::append_log(storage, &mut pending)).await;
                        if !matches!(result, Ok(Ok(()))) {
                            volume = None;
                        }
                    }
                }
                if final_flush {
                    FLUSHED.signal(());
                }
            }
            Some(Work::Log) => {
                let dropped = pending.dropped();
                if dropped > 0 {
                    esp_println::println!("WARN - microSD queue dropped {dropped} oldest records");
                }
                if let Some(storage) = volume.as_mut() {
                    let result =
                        with_timeout(IO_TIMEOUT, sd::append_log(storage, &mut pending)).await;
                    if !matches!(result, Ok(Ok(()))) {
                        volume = None;
                        log::error!("microSD disabled until reboot: {result:?}");
                    }
                }
            }
            Some(Work::Data) => {
                let command = data.take().expect("admitted command");
                let result = if let Some(storage) = volume.as_mut() {
                    match sd::domain(storage, command).await {
                        Ok(reply) => Ok(reply),
                        Err(_) => {
                            volume = None;
                            Err(())
                        }
                    }
                } else {
                    Ok(Reply::Offline)
                };
                RESPONSE.signal(result);
            }
            None => {
                match select(
                    select(
                        Timer::after_millis(1000),
                        select(FLUSH.wait(), FINAL.wait()),
                    ),
                    REQUESTS.receive(),
                )
                .await
                {
                    Either::Second(command) => data = Some(command),
                    Either::First(Either::Second(Either::First(()))) => FLUSH.signal(()),
                    Either::First(Either::Second(Either::Second(()))) => FINAL.signal(()),
                    Either::First(Either::First(())) => (),
                }
            }
        }
    }
}
