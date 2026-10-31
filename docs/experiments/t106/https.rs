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
