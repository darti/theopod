//! Selected Wi-Fi/DNS/strict TLS/streaming HTTP adapter (ADR-0016).
use allocator_api2::vec::Vec;
use core::{
    cell::Cell,
    future::Future,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};
use embassy_crypto_rustcrypto as _;
use embassy_futures::select::{Either, select};
use embassy_net::StackResources;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, with_timeout};
use embedded_io_async::Read;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateRef, CertificateVerifyRef, TlsCipherSuite, TlsClock,
    TlsConfig, TlsConnection, TlsContext, TlsError, TlsVerifier,
};
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ControllerConfig, Interface, WifiController,
    sta::StationConfig,
};
use reqwless::{
    request::{Method, Request, RequestBuilder},
    response::BodyReader,
};
use static_cell::StaticCell;
use theo_podcasts::{
    catalogue::MAX_URL_BYTES,
    download::DownloadOutcome,
    media::AUDIO_CHUNK,
    network_policy::{Purpose, Redirects, ResponseHeaders, Root, endpoint},
    runtime::{Command, Reply},
};

static RF_READY: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static STOP_WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static STOPPED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static SYNC: AtomicBool = AtomicBool::new(true);
static TRUST_CHANGED: AtomicBool = AtomicBool::new(false);
static SPACE: AtomicBool = AtomicBool::new(false);
static UTC: AtomicU32 = AtomicU32::new(0);
static UTC_AT: AtomicU32 = AtomicU32::new(0);
static STATUS: AtomicU8 = AtomicU8::new(0);
/// Coalesced triggers never admit simultaneous transfers.
pub fn sync() {
    SYNC.store(true, Ordering::Release);
    WAKE.signal(());
}
pub fn retry() {
    TRUST_CHANGED.store(true, Ordering::Release);
    SPACE.store(true, Ordering::Release);
    sync();
}
pub fn trusted_time(unix: u32) {
    TRUST_CHANGED.store(true, Ordering::Release);
    UTC_AT.store(Instant::now().as_secs() as u32, Ordering::Release);
    UTC.store(unix, Ordering::Release);
    sync();
}
pub fn status() -> &'static str {
    match STATUS.load(Ordering::Acquire) {
        1 => "waiting for current UTC",
        2 => "connecting",
        3 => "ready",
        4 => "synchronizing",
        5 => "downloading",
        6 => "waiting for space",
        7 => "TLS trust rejected; supply time and RETRY",
        8 => "storage unavailable until reboot",
        9 => "stopped",
        10 => "network retry scheduled",
        _ => "offline (local Wi-Fi configuration required)",
    }
}
pub async fn stop() {
    STOP.store(true, Ordering::Release);
    STOP_WAKE.signal(());
    WAKE.signal(());
    if RUNNING.load(Ordering::Acquire) {
        STOPPED.wait().await;
    }
}
struct Entropy;
impl embassy_crypto::driver::Rng for Entropy {
    fn fill_bytes(out: &mut [u8]) {
        assert!(RF_READY.load(Ordering::Acquire), "RF entropy is inactive");
        esp_hal::rng::Rng::new().read(out);
    }
}
embassy_crypto::rng_impl!(Entropy);
struct Clock;
impl TlsClock for Clock {
    fn now() -> Option<u64> {
        let unix = UTC.load(Ordering::Acquire);
        let elapsed =
            (Instant::now().as_secs() as u32).wrapping_sub(UTC_AT.load(Ordering::Acquire));
        // Trusted setup is per powered session, with a bounded24h lifetime.
        if unix == 0 || elapsed > 86400 {
            None
        } else {
            Some(unix as u64 + elapsed as u64)
        }
    }
}
struct StrictVerifier<'a>(embedded_tls::webpki::CertVerifier<'a, Aes128GcmSha256, Clock, 4096>);
impl TlsVerifier<Aes128GcmSha256> for StrictVerifier<'_> {
    fn set_hostname_verification(&mut self, host: &str) -> Result<(), TlsError> {
        self.0.set_hostname_verification(host)
    }
    fn verify_certificate(
        &mut self,
        hash: &<Aes128GcmSha256 as TlsCipherSuite>::Hash,
        cert: CertificateRef,
    ) -> Result<(), TlsError> {
        if Clock::now().is_none() {
            return Err(TlsError::InvalidCertificate);
        }
        self.0.verify_certificate(hash, cert)
    }
    fn verify_signature(&mut self, signature: CertificateVerifyRef) -> Result<(), TlsError> {
        self.0.verify_signature(signature)
    }
}
use theo_podcasts::worker::{self, Error};
async fn bounded<T>(seconds: u64, future: impl Future<Output = T>) -> Result<T, Error> {
    if STOP.load(Ordering::Acquire) {
        return Err(Error::Stopped);
    }
    worker::cancellable(
        async {
            with_timeout(Duration::from_secs(seconds), future)
                .await
                .map_err(|_| Error::Network)
        },
        STOP_WAKE.wait(),
    )
    .await
}
/// Logs where a transfer failed before passing its error on.
fn at(stage: &'static str) -> impl Fn(Error) -> Error {
    move |error| {
        log::warn!("network: {error:?} at {stage}");
        error
    }
}
async fn idle(seconds: u64) -> Result<(), Error> {
    match bounded(seconds, WAKE.wait()).await {
        Ok(()) | Err(Error::Network) => Ok(()),
        Err(e) => Err(e),
    }
}
async fn domain(command: Command) -> Result<Reply, Error> {
    if STOP.load(Ordering::Acquire) {
        return Err(Error::Stopped);
    }
    // Storage owns the complete queue/admission/I/O deadline. A shorter
    // network timer would cancel a healthy queued request and poison its mount.
    worker::cancellable(request(command), STOP_WAKE.wait()).await
}
/// One owner request, without STOP: the caller races it against STOP.
async fn request(command: Command) -> Result<Reply, Error> {
    match super::storage::command(command)
        .await
        .map_err(|_| Error::Storage)?
    {
        Reply::Offline => Err(Error::Storage),
        Reply::Rejected => Err(Error::Protocol),
        reply => Ok(reply),
    }
}
/// Body bytes per pump step: a whole TLS record (16 KiB of plaintext), so
/// storing one record overlaps decrypting the next (t-160).
const BODY_STEP_BYTES: usize = 4 * AUDIO_CHUNK;
/// TCP receive buffer, hence the advertised window (under 64 KiB: no scaling).
const TCP_RX_BYTES: usize = 32 * 1024;
struct Buffers {
    tcp_rx: Vec<u8, &'static esp_alloc::ExternalMemory>,
    tcp_tx: Vec<u8, &'static esp_alloc::ExternalMemory>,
    tls_rx: Vec<u8, &'static esp_alloc::ExternalMemory>,
    tls_tx: Vec<u8, &'static esp_alloc::ExternalMemory>,
    headers: Vec<u8, &'static esp_alloc::ExternalMemory>,
    chunk: Vec<u8, &'static esp_alloc::ExternalMemory>,
    /// The audio step being stored while `chunk` is filled (t-160).
    back: Vec<u8, &'static esp_alloc::ExternalMemory>,
}
fn buffer(n: usize) -> Result<Vec<u8, &'static esp_alloc::ExternalMemory>, Error> {
    let mut bytes = Vec::new_in(&esp_alloc::ExternalMemory);
    bytes.try_reserve_exact(n).map_err(|_| Error::Network)?;
    bytes.resize(n, 0);
    Ok(bytes)
}
impl Buffers {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            // The receive window: the server keeps sending while a chunk is
            // stored. At 4 KB each chunk waited a round trip (t-133).
            tcp_rx: buffer(TCP_RX_BYTES)?,
            tcp_tx: buffer(4096)?,
            tls_rx: buffer(16640)?,
            tls_tx: buffer(4096)?,
            headers: buffer(8192)?,
            chunk: buffer(BODY_STEP_BYTES)?,
            back: buffer(BODY_STEP_BYTES)?,
        })
    }
}
/// Every page/redirect has a fresh hostname-verifying TLS connection. Body
/// facts reach the owner only after validated framing; arbitrary EOF is rejected.
async fn transfer(
    stack: embassy_net::Stack<'_>,
    original: &str,
    purpose: Purpose,
    buffers: &mut Buffers,
) -> Result<(), Error> {
    let mut redirects = Redirects::new(original, purpose).map_err(|_| Error::Protocol)?;
    let mut url = heapless_url(original)?;
    loop {
        if Clock::now().is_none() {
            return Err(Error::Trust);
        }
        let requested = url.clone();
        let target = endpoint(&requested, purpose).map_err(|_| Error::Protocol)?;
        let ips = bounded(
            10,
            stack.dns_query(target.host, embassy_net::dns::DnsQueryType::A),
        )
        .await
        .map_err(at("dns"))?
        .map_err(|_| at("dns")(Error::Network))?;
        let ip = *ips.first().ok_or(Error::Network)?;
        let mut socket =
            embassy_net::tcp::TcpSocket::new(stack, &mut buffers.tcp_rx, &mut buffers.tcp_tx);
        socket.set_timeout(Some(Duration::from_secs(15)));
        bounded(10, socket.connect((ip, 443)))
            .await
            .map_err(at("connect"))?
            .map_err(|_| at("connect")(Error::Network))?;
        let root: &[u8] = match target.root {
            Root::GtsR4 => include_bytes!("../../docs/experiments/t106-runtime/gtsr4.der"),
            Root::IsrgX1 => include_bytes!("../../docs/experiments/t106-runtime/isrgrootx1.der"),
        };
        let config = TlsConfig::new().with_server_name(target.host);
        let verifier = StrictVerifier(embedded_tls::webpki::CertVerifier::new(Certificate::X509(
            root,
        )));
        let mut tls = TlsConnection::<_, Aes128GcmSha256>::new(
            socket,
            &mut buffers.tls_rx,
            &mut buffers.tls_tx,
        );
        bounded(20, tls.open(TlsContext::new(&config, verifier)))
            .await
            .map_err(at("handshake"))?
            .map_err(|e| {
                log::warn!("network: TLS {e:?}");
                worker::handshake_error(e)
            })?;
        let (etag, modified) = if purpose == Purpose::Feed && redirects.forward_conditionals() {
            match domain(Command::SyncConditionals).await? {
                Reply::Conditionals { etag, modified } => (etag, modified),
                _ => return Err(Error::Protocol),
            }
        } else {
            (heapless::String::new(), heapless::String::new())
        };
        let mut headers = heapless::Vec::<(&str, &str), 4>::new();
        let _ = headers.push(("Accept-Encoding", "identity"));
        let _ = headers.push(("Connection", "close"));
        if !etag.is_empty() {
            let _ = headers.push(("If-None-Match", etag.as_str()));
        }
        if !modified.is_empty() {
            let _ = headers.push(("If-Modified-Since", modified.as_str()));
        }
        bounded(
            15,
            Request::get(target.path)
                .host(target.host)
                .headers(&headers)
                .build()
                .write_header(&mut tls),
        )
        .await
        .map_err(at("request"))?
        .map_err(|_| at("request")(Error::Network))?;
        bounded(15, tls.flush())
            .await
            .map_err(at("flush"))?
            .map_err(|_| at("flush")(Error::Network))?;
        let response = bounded(
            15,
            reqwless::response::Response::read(&mut tls, Method::GET, &mut buffers.headers),
        )
        .await
        .map_err(at("response"))?
        .map_err(|e| {
            log::warn!("network: response {e:?}");
            Error::Protocol
        })?;
        let status = response.status.0;
        log::info!("network: {purpose:?} {} {status}", target.host);
        let facts = ResponseHeaders::parse(response.headers()).map_err(|_| Error::Protocol)?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            url = redirects
                .follow(&facts.location)
                .map_err(|_| Error::Protocol)?;
            continue;
        }
        if matches!(status, 429 | 503) {
            return Err(Error::Retry(facts.retry_after.unwrap_or(30)));
        }
        if status == 304 && purpose == Purpose::Feed {
            if !redirects.accepts_not_modified(!etag.is_empty() || !modified.is_empty()) {
                return Err(Error::Protocol);
            }
            domain(Command::SyncHeaders {
                status,
                etag: facts.etag,
                modified: facts.modified,
            })
            .await?;
            return Ok(());
        }
        if status != 200 {
            return Err(Error::Protocol);
        }
        if facts.length.is_none() && !facts.chunked {
            return Err(Error::Protocol);
        }
        match purpose {
            Purpose::Feed => {
                domain(Command::SyncHeaders {
                    status,
                    etag: facts.etag,
                    modified: facts.modified,
                })
                .await?;
            }
            Purpose::Audio => {
                domain(Command::DownloadHeaders {
                    status,
                    length: facts.length,
                    partial: facts.partial,
                })
                .await?;
            }
        }
        let mut body = Body {
            reader: response.body().reader(),
            purpose,
            absolute: Instant::now()
                + Duration::from_secs(if purpose == Purpose::Feed { 300 } else { 3600 }),
            total: 0,
            started: Instant::now(),
            timing: &Timing::default(),
        };
        let mut sink = Store {
            purpose,
            timing: body.timing,
        };
        // Audio stores chunk N on core1 while chunk N+1 is read and
        // decrypted here (t-160); a feed chunk is consumed in parts, so it
        // is stored before the next read.
        theo_podcasts::body::pump(
            &mut body,
            &mut sink,
            &mut buffers.chunk,
            &mut buffers.back,
            purpose == Purpose::Audio,
            || async {
                if !STOP.load(Ordering::Acquire) {
                    STOP_WAKE.wait().await;
                }
            },
        )
        .await?;
        let complete = match &body.reader {
            BodyReader::Empty => true,
            BodyReader::FixedLength(reader) => reader.remaining == 0,
            BodyReader::Chunked(reader) => reader.is_done(),
            BodyReader::ToEnd(_) => false,
        };
        return if complete {
            Ok(())
        } else {
            Err(Error::Protocol)
        };
    }
}
/// Time spent waiting for the network and for storage, per MiB.
#[derive(Default)]
struct Timing {
    read_us: Cell<u64>,
    store_us: Cell<u64>,
    reads: Cell<u32>,
    last_store: Cell<u64>,
    slowest: Cell<u64>,
    /// End of the previous MiB: wall time shows how much the two overlap.
    mark: Cell<Option<Instant>>,
}
/// A response body, read in bounded steps under the transfer's deadline.
struct Body<'t, B> {
    reader: BodyReader<B>,
    purpose: Purpose,
    absolute: Instant,
    total: usize,
    started: Instant,
    timing: &'t Timing,
}
impl<B: Read> theo_podcasts::body::BodyRead for Body<'_, B> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let purpose = self.purpose;
        if Instant::now() >= self.absolute {
            log::warn!("network: {purpose:?} transfer over its absolute deadline");
            return Err(Error::Network);
        }
        // reqwless0.14's fixed reader does not clamp the caller's slice
        // to remaining bytes, so enforce that limit before every read.
        let limit = match &self.reader {
            BodyReader::FixedLength(reader) => reader.remaining.min(buf.len()),
            _ => buf.len(),
        };
        let t = self.timing;
        let read = Instant::now();
        let n = with_timeout(Duration::from_secs(15), self.reader.read(&mut buf[..limit]))
            .await
            .map_err(|_| {
                log::warn!(
                    "network: body stalled after {} B in {} s, last store {} ms, slowest {} ms",
                    self.total,
                    self.started.elapsed().as_secs(),
                    t.last_store.get(),
                    t.slowest.get(),
                );
                Error::Network
            })?
            .map_err(|e| {
                log::warn!("network: body {e:?}");
                Error::Network
            })?;
        t.read_us.set(t.read_us.get() + read.elapsed().as_micros());
        t.reads.set(t.reads.get() + 1);
        let total = self.total + n;
        if total >> 20 != self.total >> 20 {
            let mark = t.mark.replace(Some(Instant::now())).unwrap_or(self.started);
            log::debug!(
                "network: {} MiB in {} s, last MiB: wall {} ms, read {} ms, store {} ms, {} reads, slowest store {} ms",
                total >> 20,
                self.started.elapsed().as_secs(),
                mark.elapsed().as_millis(),
                t.read_us.take() / 1000,
                t.store_us.take() / 1000,
                t.reads.take(),
                t.slowest.get(),
            );
        }
        self.total = total;
        Ok(n)
    }
}
/// Hands body bytes to the storage owner, one chunk request at a time.
struct Store<'t> {
    purpose: Purpose,
    timing: &'t Timing,
}
impl theo_podcasts::body::ChunkSink for Store<'_> {
    async fn store(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let (purpose, n) = (self.purpose, bytes.len());
        let store = Instant::now();
        let mut at = 0;
        // One commit per podcast request (ADR-0022): the owner consumes
        // a feed chunk in parts and makes space in steps, so resend
        // what it did not take. Each request is bounded by the
        // storage owner's IO budget; `pump` races them against STOP.
        while at < n {
            let piece = n.min(at + AUDIO_CHUNK) - at;
            let chunk = theo_podcasts::runtime::Chunk::from_slice(&bytes[at..at + piece])
                .map_err(|_| Error::Protocol)?;
            let command = match purpose {
                Purpose::Feed => Command::SyncChunk(chunk),
                Purpose::Audio => Command::DownloadChunk(chunk),
            };
            match (purpose, request(command).await?) {
                (Purpose::Feed, Reply::Consumed(used)) if used > 0 && used <= piece => {
                    at += used;
                }
                (Purpose::Audio, Reply::Done) => at += piece,
                (Purpose::Audio, Reply::Pending) => (),
                (Purpose::Audio, Reply::Downloaded(DownloadOutcome::WaitingForSpace)) => {
                    return Err(Error::Waiting);
                }
                (_, reply) => {
                    // `request` already turned Rejected/Offline into errors.
                    let kind = match reply {
                        Reply::Consumed(_) => "bad consumed count",
                        _ => "unexpected reply",
                    };
                    log::warn!("network: {purpose:?} chunk: {kind}");
                    return Err(Error::Protocol);
                }
            }
        }
        let t = self.timing;
        let elapsed = store.elapsed();
        t.store_us.set(t.store_us.get() + elapsed.as_micros());
        t.last_store.set(elapsed.as_millis());
        t.slowest.set(t.slowest.get().max(elapsed.as_millis()));
        Ok(())
    }
}
fn heapless_url(url: &str) -> Result<heapless::String<MAX_URL_BYTES>, Error> {
    url.try_into().map_err(|_| Error::Protocol)
}
async fn synchronize(stack: embassy_net::Stack<'_>, buffers: &mut Buffers) -> Result<(), Error> {
    let mut after = None;
    loop {
        let Reply::Subscription(subscription) = domain(Command::Subscription(after)).await? else {
            return Err(Error::Protocol);
        };
        let Some(subscription) = subscription else {
            return Ok(());
        };
        after = Some(subscription.id);
        let mut url = heapless_url(&subscription.feed)?;
        domain(Command::SyncStart(subscription.id, subscription.feed)).await?;
        loop {
            transfer(stack, &url, Purpose::Feed, buffers).await?;
            match domain(Command::SyncEnd).await? {
                Reply::Url(next) => url = next,
                Reply::Synced(progress) => {
                    log::info!(
                        "podcasts synchronized: {} entries, {} new",
                        progress.entries,
                        progress.created
                    );
                    break;
                }
                _ => return Err(Error::Protocol),
            }
        }
    }
}
struct AudioIo<'a, 's> {
    stack: embassy_net::Stack<'s>,
    buffers: &'a mut Buffers,
}
impl worker::DownloadIo for AudioIo<'_, '_> {
    async fn command(&mut self, command: Command) -> Result<Reply, Error> {
        domain(command).await
    }
    async fn transfer(&mut self, url: &str) -> Result<(), Error> {
        transfer(self.stack, url, Purpose::Audio, self.buffers).await
    }
}
/// Upper bound of the network future placed in PSRAM (measured 64 KB).
const NETWORK_FUTURE_BYTES: usize = 80 * 1024;
/// Size of the future `make` returns, without calling it.
fn future_bytes<F: Future>(_: &impl FnOnce() -> F) -> usize {
    core::mem::size_of::<F>()
}
#[embassy_executor::task]
pub async fn run(wifi: WIFI<'static>) {
    RUNNING.store(true, Ordering::Release);
    STOPPED.reset();
    // The 64 KB transfer state lives in PSRAM, polled in a frame of its own:
    // as this task's static storage it took that much internal RAM from the
    // main stack, which the TLS/storage call chain overflowed (t-133).
    let make = move || start(wifi);
    let bytes = future_bytes(&make);
    log::debug!("network: task future {bytes} B of {NETWORK_FUTURE_BYTES} B");
    let result =
        theo_storage::future::run_in(make, &esp_alloc::ExternalMemory, NETWORK_FUTURE_BYTES)
            .await
            .unwrap_or_else(|_| {
                log::error!("network: task future ({bytes} B) not placed in PSRAM");
                Err(Error::Network)
            });
    RF_READY.store(false, Ordering::Release);
    if let Err(error) = result {
        if matches!(error, Error::Storage) {
            STATUS.store(8, Ordering::Release);
        }
        log::info!("podcasts network stopped: {error:?}");
    }
    RUNNING.store(false, Ordering::Release);
    STOPPED.signal(());
}
async fn start(wifi: WIFI<'static>) -> Result<(), Error> {
    loop {
        if STOP.load(Ordering::Acquire) {
            return Err(Error::Stopped);
        }
        if matches!(domain(Command::Recover).await?, Reply::Recovered(true)) {
            break;
        }
        embassy_futures::yield_now().await;
    }
    let (Some(ssid), Some(password)) = (
        option_env!("THEO_WIFI_SSID"),
        option_env!("THEO_WIFI_PASSWORD"),
    ) else {
        return Ok(());
    };
    while Clock::now().is_none() {
        STATUS.store(1, Ordering::Release);
        bounded(3600, WAKE.wait()).await.or_else(|e| {
            if matches!(e, Error::Network) {
                Ok(())
            } else {
                Err(e)
            }
        })?;
    }
    let station = StationConfig::default()
        .with_ssid(ssid.try_into().map_err(|_| Error::Protocol)?)
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
            password.try_into().map_err(|_| Error::Protocol)?,
        ));
    let config = ControllerConfig::default()
        .with_static_rx_buf_num(6)
        .with_dynamic_rx_buf_num(8)
        .with_dynamic_tx_buf_num(4)
        .with_rx_ba_win(3)
        .with_rx_queue_size(2)
        .with_tx_queue_size(2)
        .with_initial_config(Config::Station(station));
    let mut controller = WifiController::new(wifi, config).map_err(|_| Error::Network)?;
    static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
    let (stack, mut runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        esp_hal::rng::Rng::new().random() as u64,
    );
    let mut buffers = Buffers::new()?;
    match select(runner.run(), work(&mut controller, stack, &mut buffers)).await {
        Either::First(never) => match never {},
        Either::Second(result) => result,
    }
}
async fn work(
    controller: &mut WifiController<'_>,
    stack: embassy_net::Stack<'_>,
    buffers: &mut Buffers,
) -> Result<(), Error> {
    let mut waiting = false;
    let mut failure = 0usize;
    let mut next_sync = Instant::now();
    let mut download_after = None;
    loop {
        if STOP.load(Ordering::Acquire) {
            STATUS.store(9, Ordering::Release);
            return Err(Error::Stopped);
        }
        if SPACE.swap(false, Ordering::AcqRel) {
            download_after = None;
            waiting = false;
        }
        if !controller.is_connected() {
            RF_READY.store(false, Ordering::Release);
            STATUS.store(2, Ordering::Release);
            match bounded(20, controller.connect_async()).await {
                Ok(Ok(_)) => (),
                Err(Error::Stopped) => return Err(Error::Stopped),
                _ => {
                    idle(30).await?;
                    continue;
                }
            }
            RF_READY.store(true, Ordering::Release);
            match bounded(20, stack.wait_config_up()).await {
                Ok(()) => (),
                Err(Error::Stopped) => return Err(Error::Stopped),
                _ => {
                    idle(30).await?;
                    continue;
                }
            }
            SYNC.store(true, Ordering::Release);
        }
        let result = if SYNC.swap(false, Ordering::AcqRel) || Instant::now() >= next_sync {
            STATUS.store(4, Ordering::Release);
            let result = synchronize(stack, buffers).await;
            if result.is_ok() {
                next_sync = Instant::now() + Duration::from_secs(6 * 3600);
            } else {
                SYNC.store(true, Ordering::Release);
            }
            result.map(|_| true)
        } else if !waiting {
            STATUS.store(5, Ordering::Release);
            worker::download_one(
                &mut AudioIo {
                    stack,
                    buffers: &mut *buffers,
                },
                &mut download_after,
            )
            .await
        } else {
            Ok(false)
        };
        match result {
            Ok(true) => {
                failure = 0;
                continue;
            }
            Ok(false) => {
                STATUS.store(if waiting { 6 } else { 3 }, Ordering::Release);
                idle(60).await?;
            }
            Err(Error::Storage) => {
                STATUS.store(8, Ordering::Release);
                return Err(Error::Storage);
            }
            Err(Error::Stopped) => {
                STATUS.store(9, Ordering::Release);
                return Err(Error::Stopped);
            }
            Err(error) => {
                log::warn!("network: {error:?}, retrying");
                // Ordinary network/protocol errors recover partials; storage
                // failures remain disabled and never become a fresh mount.
                domain(Command::Abort).await?;
                if matches!(error, Error::Waiting) {
                    waiting = true;
                    STATUS.store(6, Ordering::Release);
                    continue;
                }
                if matches!(error, Error::Trust) {
                    STATUS.store(7, Ordering::Release);
                    TRUST_CHANGED.store(false, Ordering::Release);
                    while !TRUST_CHANGED.swap(false, Ordering::AcqRel) {
                        idle(86400).await?;
                    }
                } else {
                    STATUS.store(10, Ordering::Release);
                    let delays = [30, 120, 600, 3600];
                    let retry = match error {
                        Error::Retry(seconds) => seconds as u64,
                        _ => 0,
                    };
                    let seconds = delays[failure.min(3)].max(retry)
                        + esp_hal::rng::Rng::new().random() as u64 % 6;
                    failure = failure.saturating_add(1);
                    match bounded(seconds, WAKE.wait()).await {
                        Ok(()) | Err(Error::Network) => (),
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
}
