use super::*;
const FEED: &str = "https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b";
#[test]
fn authorities_select_bundled_roots_without_broadening_subscription_source() {
    // Regression (t-110): trusted roots do not authorize arbitrary subscription/audio hosts.
    let e = endpoint(FEED, Purpose::Feed).unwrap();
    assert_eq!(e.host, "rss-rf.aerion.me");
    assert_eq!(e.root, Root::GtsR4);
    let e = endpoint(
        "https://media.radiofrance-podcast.net/a?x=1",
        Purpose::Audio,
    )
    .unwrap();
    assert_eq!(e.root, Root::IsrgX1);
    assert_eq!(e.path, "/a?x=1");
    for bad in [
        "http://media.radiofrance-podcast.net/a",
        "https://media.radiofrance-podcast.net.evil.example/a",
        "https://media.radiofrance-podcast.net@evil.example/a",
        "https://media.radiofrance-podcast.net/a#fragment",
        "https://media.radiofrance-podcast.net:443/a",
        "https://evil.example/a",
        "https://media.radiofrance-podcast.net/a\r\nHost: evil",
    ] {
        assert!(endpoint(bad, Purpose::Audio).is_err(), "{bad}");
    }
    assert!(endpoint("https://media.radiofrance-podcast.net/a", Purpose::Feed).is_err());
}
#[test]
fn redirects_reject_loops_downgrades_foreign_shows_and_excess_hops() {
    // Regression (t-110): each HTTPS hop retains feed identity and has bounded loop detection.
    let mut r = Redirects::new(FEED, Purpose::Feed).unwrap();
    assert!(
        r.follow("https://rss-rf.aerion.me/rss/aaaaaaaa-f1f7-4725-8380-e428893eb93b")
            .is_err()
    );
    let mut r = Redirects::new(FEED, Purpose::Feed).unwrap();
    assert!(r.follow("?page=1").is_err());
    let mut r = Redirects::new(
        "https://media.radiofrance-podcast.net/start",
        Purpose::Audio,
    )
    .unwrap();
    assert_eq!(
        r.follow("/next").unwrap(),
        "https://media.radiofrance-podcast.net/next"
    );
    assert!(r.follow("/start").is_err());
    let mut r = Redirects::new(
        "https://media.radiofrance-podcast.net/start",
        Purpose::Audio,
    )
    .unwrap();
    for i in 0..5 {
        assert!(r.follow(&std::format!("/hop{i}")).is_ok());
    }
    assert_eq!(r.follow("/too-many"), Err(Error::Limit));
    let mut r = Redirects::new(
        "https://media.radiofrance-podcast.net/start",
        Purpose::Audio,
    )
    .unwrap();
    assert!(r.follow("http://media.radiofrance-podcast.net/x").is_err());
}
#[test]
fn response_framing_rejects_ambiguous_or_compressed_bodies() {
    // Regression (t-110): successful socket EOF must never certify a truncated/ambiguous HTTP body.
    assert!(
        ResponseHeaders::parse([
            ("content-length", b"4".as_slice()),
            ("Content-Length", b"5")
        ])
        .is_err()
    );
    assert!(
        ResponseHeaders::parse([
            ("content-length", b"4".as_slice()),
            ("Transfer-Encoding", b"chunked")
        ])
        .is_err()
    );
    assert!(ResponseHeaders::parse([("Content-Encoding", b"gzip".as_slice())]).is_err());
    let parsed = ResponseHeaders::parse([
        ("Content-Length", b"4".as_slice()),
        ("ETag", b"tag"),
        ("Retry-After", b"120"),
    ])
    .unwrap();
    assert_eq!(parsed.length, Some(4));
    assert_eq!(parsed.retry_after, Some(120));
}
#[test]
fn redirected_unconditional_response_cannot_reuse_prior_not_modified() {
    // Regression (t-110): a redirect strips validators, so its unsolicited304 cannot certify the original feed.
    let mut redirects = Redirects::new(FEED, Purpose::Feed).unwrap();
    assert!(redirects.accepts_not_modified(true));
    assert!(!redirects.accepts_not_modified(false));
    redirects.follow("?page=0").unwrap();
    assert!(!redirects.accepts_not_modified(true));
}
