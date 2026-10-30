//! Link-only feasibility code. This disposable image must never be flashed.
use allocator_api2::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use embassy_net::StackResources;
use esp_hal::{peripherals::WIFI, ram};
use esp_radio::wifi::{ControllerConfig, Interface, WifiController};
use static_cell::StaticCell;
use theo_storage::{card_layout::CardLayout, littlefs::SectorIo, page_store::PageStore};
#[path = "t106_regions.rs"]
mod regions;
#[path = "t106_transport.rs"]
mod transport;
static READY: AtomicBool = AtomicBool::new(false);
static UTC: AtomicU32 = AtomicU32::new(0);
struct Entropy;
impl embassy_crypto::driver::Rng for Entropy {
    fn fill_bytes(out: &mut [u8]) {
        assert!(READY.load(Ordering::Acquire), "RF entropy not active");
        esp_hal::rng::Rng::new().read(out);
    }
}
embassy_crypto::rng_impl!(Entropy);
struct EntropySession;
impl Drop for EntropySession {
    fn drop(&mut self) {
        READY.store(false, Ordering::Release);
    }
}
struct Clock;
impl embedded_tls::TlsClock for Clock {
    fn now() -> Option<u64> {
        let t = UTC.load(Ordering::Acquire);
        if t == 0 { None } else { Some(t as u64) }
    }
}
fn buffer(n: usize) -> Vec<u8, &'static esp_alloc::ExternalMemory> {
    let mut bytes = Vec::new_in(&esp_alloc::ExternalMemory);
    bytes.try_reserve_exact(n).unwrap();
    bytes.resize(n, 0);
    bytes
}
struct ProbeCard;
impl SectorIo for ProbeCard {
    fn read_sector(&mut self, _: u32, _: &mut [u8; 512]) -> Result<(), littlefs_rust::Error> {
        core::hint::black_box(Err(littlefs_rust::Error::Io))
    }
    fn write_sector(&mut self, _: u32, _: &[u8; 512]) -> Result<(), littlefs_rust::Error> {
        core::hint::black_box(Err(littlefs_rust::Error::Io))
    }
    fn sync(&mut self) -> Result<(), littlefs_rust::Error> {
        core::hint::black_box(Err(littlefs_rust::Error::Io))
    }
}
async fn database() {
    let layout = CardLayout::new(256 * 1024, 4096).unwrap();
    let (db, _media) = regions::split(ProbeCard, layout);
    let file = regions::AsyncRegion::new(db);
    if let Ok(store) = PageStore::open_in(file, 4096, &esp_alloc::ExternalMemory).await {
        let db = ekv::Database::<_, embassy_sync_06::blocking_mutex::raw::NoopRawMutex>::new(
            store,
            ekv::Config::default(),
        );
        if db.mount().await.is_ok() {
            let mut tx = db.write_transaction().await;
            if tx.write(b"read", b"true").await.is_ok() {
                let _ = tx.commit().await;
            }
        }
    }
}
pub async fn run(wifi: WIFI<'static>) {
    // In addition to the unchanged ordinary64KiB heap, reserve a separate
    // reclaimed64KiB region. Linker evidence must verify its actual placement.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size:64*1024);
    embassy_futures::join::join(database(), network(wifi)).await;
}

async fn network(wifi: WIFI<'static>) {
    let config = ControllerConfig::default()
        .with_static_rx_buf_num(6)
        .with_dynamic_rx_buf_num(8)
        .with_dynamic_tx_buf_num(4)
        .with_rx_ba_win(3)
        .with_rx_queue_size(2)
        .with_tx_queue_size(2);
    let Ok(mut controller) = WifiController::new(wifi, config) else {
        return;
    };
    if controller.connect_async().await.is_err() {
        return;
    }
    READY.store(true, Ordering::Release);
    let _entropy = EntropySession;
    static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
    let (stack, mut runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        esp_hal::rng::Rng::new().random() as u64,
    );
    let (mut tcp_rx, mut tcp_tx) = (buffer(4096), buffer(4096));
    let (mut tls_rx, mut tls_tx) = (buffer(16640), buffer(4096));
    let (mut headers, mut chunk) = (buffer(8192), buffer(4096));
    let request = transport::dns_tcp_get::<Clock>(
        stack,
        "radio-france-rss.aerion.workers.dev",
        "/",
        include_bytes!("../../gtsr4.der"),
        &mut tcp_rx,
        &mut tcp_tx,
        &mut tls_rx,
        &mut tls_tx,
        &mut headers,
        &mut chunk,
    );
    let _ = embassy_futures::select::select(runner.run(), request).await;
}
