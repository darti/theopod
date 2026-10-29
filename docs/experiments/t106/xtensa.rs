#![no_std]
extern crate alloc;
pub mod adapter;
pub mod page_store;
use embassy_crypto_rustcrypto as _;
use embedded_io_async::{Read, Write};
use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateRef, CertificateVerifyRef, TlsCipherSuite, TlsClock,
    TlsConfig, TlsConnection, TlsContext, TlsError, TlsVerifier,
};
use reqwless::request::{Method, Request, RequestBuilder};

pub struct StrictVerifier<'a, C: TlsClock>(
    pub embedded_tls::webpki::CertVerifier<'a, Aes128GcmSha256, C, 4096>,
);
impl<C: TlsClock> TlsVerifier<Aes128GcmSha256> for StrictVerifier<'_, C> {
    fn set_hostname_verification(&mut self, host: &str) -> Result<(), TlsError> {
        self.0.set_hostname_verification(host)
    }
    fn verify_certificate(
        &mut self,
        hash: &<Aes128GcmSha256 as TlsCipherSuite>::Hash,
        cert: CertificateRef,
    ) -> Result<(), TlsError> {
        if C::now().is_none() {
            return Err(TlsError::InvalidCertificate);
        }
        self.0.verify_certificate(hash, cert)
    }
    fn verify_signature(&mut self, signature: CertificateVerifyRef) -> Result<(), TlsError> {
        self.0.verify_signature(signature)
    }
}
#[derive(Debug)]
pub enum Error {
    Clock,
    Tls(TlsError),
    Http(reqwless::Error),
    Status(u16),
    Io,
}
pub async fn get<S: Read + Write, C: TlsClock>(
    socket: S,
    host: &str,
    path: &str,
    root: &[u8],
    rx: &mut [u8],
    tx: &mut [u8],
    headers: &mut [u8],
    chunk: &mut [u8],
) -> Result<usize, Error> {
    if C::now().is_none() {
        return Err(Error::Clock);
    }
    let config = TlsConfig::new().with_server_name(host);
    let verifier = StrictVerifier::<C>(embedded_tls::webpki::CertVerifier::new(Certificate::X509(
        root,
    )));
    let mut tls = TlsConnection::<_, Aes128GcmSha256>::new(socket, rx, tx);
    tls.open(TlsContext::new(&config, verifier))
        .await
        .map_err(Error::Tls)?;
    Request::get(path)
        .host(host)
        .headers(&[("Accept-Encoding", "identity"), ("Connection", "close")])
        .build()
        .write_header(&mut tls)
        .await
        .map_err(Error::Http)?;
    tls.flush().await.map_err(Error::Tls)?;
    let response = reqwless::response::Response::read(&mut tls, Method::GET, headers)
        .await
        .map_err(Error::Http)?;
    let status = response.status.0;
    if status != 200 {
        return Err(Error::Status(status));
    }
    let mut reader = response.body().reader();
    let mut count = 0;
    loop {
        let n = reader.read(chunk).await.map_err(|_| Error::Io)?;
        if n == 0 {
            break;
        }
        count += n;
    }
    Ok(count)
}
pub async fn dns_tcp_get<C: TlsClock>(
    stack: embassy_net::Stack<'_>,
    host: &str,
    path: &str,
    root: &[u8],
    tcp_rx: &mut [u8],
    tcp_tx: &mut [u8],
    rx: &mut [u8],
    tx: &mut [u8],
    headers: &mut [u8],
    chunk: &mut [u8],
) -> Result<usize, Error> {
    let ips = stack
        .dns_query(host, embassy_net::dns::DnsQueryType::A)
        .await
        .map_err(|_| Error::Io)?;
    let ip = ips.first().ok_or(Error::Io)?;
    let mut socket = embassy_net::tcp::TcpSocket::new(stack, tcp_rx, tcp_tx);
    socket.connect((*ip, 443)).await.map_err(|_| Error::Io)?;
    get::<_, C>(socket, host, path, root, rx, tx, headers, chunk).await
}

pub struct ProbeClock;
impl TlsClock for ProbeClock {
    fn now() -> Option<u64> {
        Some(0)
    }
}
#[unsafe(no_mangle)]
pub static T106_VERIFIER_BYTES: [u8; core::mem::size_of::<StrictVerifier<ProbeClock>>()] =
    [0; core::mem::size_of::<StrictVerifier<ProbeClock>>()];
#[unsafe(no_mangle)]
pub static T106_TLS_BYTES: [u8; core::mem::size_of::<
    TlsConnection<embassy_net::tcp::TcpSocket, Aes128GcmSha256>,
>()] = [0; core::mem::size_of::<TlsConnection<embassy_net::tcp::TcpSocket, Aes128GcmSha256>>()];
#[unsafe(no_mangle)]
pub static T106_STACK_BYTES: [u8; core::mem::size_of::<embassy_net::StackResources<4>>()] =
    [0; core::mem::size_of::<embassy_net::StackResources<4>>()];

pub struct ProbeFile;
impl page_store::SectorFile for ProbeFile {
    type Error = ();
    fn byte_len(&self) -> u64 {
        page_store::file_len(4096)
    }
    async fn read_sector(&mut self, _: u64, _: &mut [u8; 512]) -> Result<(), ()> {
        Err(())
    }
    async fn write_sector(&mut self, _: u64, _: &[u8; 512]) -> Result<(), ()> {
        Err(())
    }
    async fn sync(&mut self) -> Result<(), ()> {
        Err(())
    }
}
#[unsafe(no_mangle)]
pub static T106_DB_BYTES: [u8; core::mem::size_of::<adapter::Db<ProbeFile>>()] =
    [0; core::mem::size_of::<adapter::Db<ProbeFile>>()];

// Type-check both explicit PSRAM constructor paths without touching a device.
pub async fn t116_psram_open<F: page_store::SectorFile>(file: F, pages: usize) -> Result<page_store::PageStore<F, &'static esp_alloc::ExternalMemory>, page_store::PageError<F::Error>> {
    page_store::PageStore::open_in(file, pages, &esp_alloc::ExternalMemory).await
}
pub async fn t116_psram_create<F: page_store::SectorFile>(file: F, pages: usize) -> Result<page_store::PageStore<F, &'static esp_alloc::ExternalMemory>, page_store::PageError<F::Error>> {
    page_store::PageStore::create_in(file, pages, &esp_alloc::ExternalMemory).await
}
