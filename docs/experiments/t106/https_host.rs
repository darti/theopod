use embassy_crypto_rand as _;
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::TlsClock;
use theo_t106_probe::get;
struct Clock;
impl TlsClock for Clock {
    fn now() -> Option<u64> {
        Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
    }
}
struct NoTime;
impl TlsClock for NoTime {
    fn now() -> Option<u64> {
        None
    }
}
#[tokio::main]
async fn main() {
    let host = "radio-france-rss.aerion.workers.dev";
    let path = "/search/?query=espions";
    let root = include_bytes!(concat!(env!("THEO_T106_DIR"), "/gtsr4.der"));
    for case in 0..4 {
        let stream = tokio::net::TcpStream::connect((host, 443)).await.unwrap();
        let mut rx = vec![0; 16640];
        let mut tx = vec![0; 4096];
        let mut headers = vec![0; 8192];
        let mut chunk = vec![0; 4096];
        let result = if case == 3 {
            get::<_, NoTime>(
                FromTokio::new(stream),
                host,
                path,
                root,
                &mut rx,
                &mut tx,
                &mut headers,
                &mut chunk,
            )
            .await
        } else {
            get::<_, Clock>(
                FromTokio::new(stream),
                if case == 1 { "example.com" } else { host },
                path,
                if case == 2 { &[] } else { root },
                &mut rx,
                &mut tx,
                &mut headers,
                &mut chunk,
            )
            .await
        };
        println!("case {case}: {result:?}");
        if case == 0 {
            assert!(matches!(
                result,
                Ok(_) | Err(theo_t106_probe::Error::Status(_))
            ));
        } else {
            assert!(matches!(
                result,
                Err(theo_t106_probe::Error::Tls(_)) | Err(theo_t106_probe::Error::Clock)
            ));
        }
    }
}
