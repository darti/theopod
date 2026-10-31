use embassy_crypto_rand as _;
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::TlsClock;
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
#[tokio::main]
async fn main() {
    let host = "media.radiofrance-podcast.net";
    for case in 0..2 {
        let result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let stream = tokio::net::TcpStream::connect((host, 443)).await.unwrap();
            let root: &[u8] = if case == 0 {
                include_bytes!("../isrgrootx1.der")
            } else {
                include_bytes!("../gtsr4.der")
            };
            theo_t106_probe::get_range::<_, Clock>(
                FromTokio::new(stream),
                host,
                include_str!("../audio.path"),
                root,
                &mut vec![0; 16640],
                &mut vec![0; 4096],
                &mut vec![0; 8192],
                &mut vec![0; 1024],
            )
            .await
        })
        .await;
        println!("CDN case={case}: {result:?}");
        if case == 0 {
            assert!(matches!(result, Ok(Ok(1024))));
        } else {
            assert!(matches!(result, Ok(Err(theo_t106_probe::Error::Tls(_)))));
        }
    }
}
