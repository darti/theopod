use super::*;
use crate::test_support::Image;
const FEED: &str = "https://rss-rf.aerion.me/rss/00000000-0000-0000-0000-000000000001";
#[test]
fn follow_is_persistent_idempotent_and_independent_of_sync() {
    // Regression (t-110): interrupted first synchronization must not forget a follow or allocate another ID.
    let image = Image::new();
    let mut cat = image.create();
    let id = cat.follow(FEED).unwrap();
    assert_eq!(id, SubscriptionId(1));
    drop(cat);
    let mut cat = image.reopen();
    assert_eq!(cat.follow(FEED).unwrap(), id);
    let sub = cat.next_subscription(None).unwrap().unwrap();
    assert_eq!(sub.id, id);
    assert_eq!(sub.feed.as_str(), FEED);
    assert!(cat.next_subscription(Some(id)).unwrap().is_none());
    assert_eq!(
        cat.follow("https://rss-rf.aerion.me/rss/00000000-0000-0000-0000-000000000002")
            .unwrap(),
        SubscriptionId(2)
    );
}
#[test]
fn follow_rejects_other_sources_and_history_pages_without_consuming_ids() {
    // Regression (t-110): only a Radio France history root is a subscription.
    let image = Image::new();
    let mut cat = image.create();
    for url in [
        "https://example.com/rss",
        "https://rss-rf.aerion.me/rss/00000000-0000-0000-0000-000000000001?page=1",
    ] {
        assert!(cat.follow(url).is_err());
    }
    assert_eq!(cat.follow(FEED).unwrap(), SubscriptionId(1));
}
