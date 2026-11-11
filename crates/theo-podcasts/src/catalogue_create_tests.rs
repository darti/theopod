use super::*;
use crate::test_support::Image;
use futures::executor::block_on;
use theo_storage::page_store::PageStore;

#[test]
fn create_into_fills_the_slot_with_a_reopenable_catalogue() {
    // Regression (t-133): on-device provisioning creates the catalogue in
    // place; a by-value catalogue overflowed core1's stack.
    block_on(async {
        let image = Image::new();
        let mut slot = None;
        Catalogue::create_into(
            &mut slot,
            PageStore::create(image.file(), 32).await.unwrap(),
        )
        .await
        .unwrap();
        assert!(slot.is_some());
        drop(slot);
        let mut cat = image.reopen().await;
        assert!(cat.next_subscription(None).await.unwrap().is_none());
    });
}

#[test]
fn create_into_failure_leaves_the_slot_empty() {
    // Regression (t-133): a failed in-place create must not leave a
    // catalogue that looks usable.
    block_on(async {
        let image = Image::new();
        let store = PageStore::create(image.file(), 32).await.unwrap();
        image.fail.set(true);
        let mut slot = None;
        assert!(matches!(
            Catalogue::create_into(&mut slot, store).await,
            Err(Error::Storage(_))
        ));
        assert!(slot.is_none());
    });
}
