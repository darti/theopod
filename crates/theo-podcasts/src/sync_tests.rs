use super::*;
use crate::{
    catalogue::*,
    test_support::{HostFile, Image},
};
use futures::executor::block_on;
use std::{string::String as StdString, vec::Vec};
use theo_storage::page_store::PageStore;
const ROOT: &str = "https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b";
const ID: SubscriptionId = SubscriptionId(1);
async fn import(
    cat: &mut Catalogue<PageStore<HostFile>>,
    pages: &[&[u8]],
    prior: Option<Checkpoint>,
    validators: Validators<'_>,
) -> Result<Progress, SyncError<theo_storage::page_store::PageError<std::io::Error>>> {
    let mut session = SyncSession::new(ID, ROOT, prior).unwrap();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for (page, body) in pages.iter().enumerate() {
        session.response(
            200,
            if page == 0 {
                validators
            } else {
                Validators::default()
            },
        )?;
        for chunk in body.chunks(257) {
            session.push(cat, chunk, &mut scratch).await?;
        }
        assert_eq!(session.end_page()?, page + 1 < pages.len());
    }
    session.complete(cat, &mut scratch).await
}
fn simple(items: &str) -> StdString {
    std::format!("<rss><channel>{items}</channel></rss>")
}
fn item(guid: &str, title: &str) -> StdString {
    std::format!(
        "<item><title>{title}</title><guid>{guid}</guid><enclosure url='https://cdn.example/{guid}'/></item>"
    )
}
#[test]
fn complete_history_replay_queues_once_and_keeps_read_state() {
    // Regression (t-108): full paginated import queues each identity once across reboot.
    block_on(async {
        let image = Image::with_pages(256);
        // Large history uses the immediate-durability file model. Small sync
        // tests retain real sync_all barriers, including failure and reopen.
        image.sync_to_disk.set(false);
        let mut cat = image.create().await;
        let pages = [
            include_bytes!("../fixtures/radio-france-page0.xml").as_slice(),
            include_bytes!("../fixtures/radio-france-page1.xml").as_slice(),
        ];
        let p = import(&mut cat, &pages, None, Validators::default())
            .await
            .unwrap();
        assert_eq!(
            p,
            Progress {
                entries: 134,
                created: 129,
                queued: 129
            }
        );
        let mut scratch = [0; MAX_RECORD_BYTES];
        cat.mark_read(EpisodeId(1), &mut scratch).await.unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let p = import(&mut cat, &pages, None, Validators::default())
            .await
            .unwrap();
        assert_eq!(
            p,
            Progress {
                entries: 134,
                created: 0,
                queued: 0
            }
        );
        assert!(
            cat.get(EpisodeId(1), &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .state
                .read
        );
        let body = simple(
            &(item("e0c4bd07-bf68-4880-8c69-b28cefbc87bf", "changed") + &item("new-guid", "new")),
        );
        let p = import(
            &mut cat,
            &[body.as_bytes()],
            None,
            Validators {
                etag: Some("new"),
                last_modified: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            p,
            Progress {
                entries: 2,
                created: 1,
                queued: 1
            }
        );
        let mut ids = Vec::new();
        let mut after = None;
        loop {
            let page = cat
                .scan(after, &mut scratch, |e| ids.push(e.id))
                .await
                .unwrap();
            if !page.has_more {
                break;
            }
            after = page.resume_after;
        }
        assert_eq!(ids.len(), 130);
        let e = cat.get(EpisodeId(1), &mut scratch).await.unwrap().unwrap();
        assert!(e.state.read);
        assert_eq!(e.metadata.title, "changed");
    });
}
#[test]
fn validators_commit_only_after_all_pages_and_304_is_a_noop() {
    // Regression (t-108): root validators alone cannot certify unvisited history pages.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let body = simple(&item("a", "old"));
        import(
            &mut cat,
            &[body.as_bytes()],
            None,
            Validators {
                etag: Some("old"),
                last_modified: Some("date"),
            },
        )
        .await
        .unwrap();
        drop(cat);
        let mut cat = image.reopen().await;
        let old = cat
            .sync_checkpoint(ID, &mut scratch)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.etag, "old");
        assert_eq!(old.pages, 1);
        let writes = image.writes.get();
        let mut unchanged = SyncSession::new(ID, ROOT, Some(old.clone())).unwrap();
        assert_eq!(unchanged.conditional().etag, Some("old"));
        unchanged
            .response::<std::io::Error>(304, Validators::default())
            .unwrap();
        assert!(!unchanged.end_page::<std::io::Error>().unwrap());
        assert_eq!(
            unchanged.complete(&mut cat, &mut scratch).await.unwrap(),
            Progress::default()
        );
        assert_eq!(image.writes.get(), writes);
        let mut interrupted = SyncSession::new(ID, ROOT, Some(old.clone())).unwrap();
        interrupted
            .response::<std::io::Error>(
                200,
                Validators {
                    etag: Some("new"),
                    last_modified: None,
                },
            )
            .unwrap();
        interrupted
            .push(&mut cat, b"<rss><channel>", &mut scratch)
            .await
            .unwrap();
        assert!(interrupted.end_page::<std::io::Error>().is_err());
        assert!(interrupted.complete(&mut cat, &mut scratch).await.is_err());
        assert_eq!(
            cat.sync_checkpoint(ID, &mut scratch).await.unwrap(),
            Some(old.clone())
        );
        let page = std::format!(
            "<rss xmlns:atom='http://www.w3.org/2005/Atom'><channel><atom:link rel='next' href='{ROOT}?page=1'/>{}</channel></rss>",
            item("b", "new")
        );
        let mut interrupted = SyncSession::new(ID, ROOT, Some(old.clone())).unwrap();
        interrupted
            .response::<std::io::Error>(
                200,
                Validators {
                    etag: Some("new"),
                    last_modified: None,
                },
            )
            .unwrap();
        interrupted
            .push(&mut cat, page.as_bytes(), &mut scratch)
            .await
            .unwrap();
        assert!(interrupted.end_page::<std::io::Error>().unwrap());
        assert!(interrupted.complete(&mut cat, &mut scratch).await.is_err());
        assert_eq!(
            cat.sync_checkpoint(ID, &mut scratch).await.unwrap(),
            Some(old)
        );
        import(
            &mut cat,
            &[page.as_bytes(), simple("").as_bytes()],
            None,
            Validators {
                etag: Some("multi"),
                last_modified: None,
            },
        )
        .await
        .unwrap();
        let checkpoint = cat
            .sync_checkpoint(ID, &mut scratch)
            .await
            .unwrap()
            .unwrap();
        let mut session = SyncSession::new(ID, ROOT, Some(checkpoint)).unwrap();
        assert!(session.conditional().etag.is_none());
        assert!(
            session
                .response::<std::io::Error>(304, Validators::default())
                .is_err()
        );
    });
}
#[test]
fn failed_final_commit_is_replayable_after_reopen() {
    // Regression (t-108): a failed completion cannot expose new validators or duplicate jobs.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let old_body = simple(&item("a", "old"));
        import(
            &mut cat,
            &[old_body.as_bytes()],
            None,
            Validators {
                etag: Some("old"),
                last_modified: None,
            },
        )
        .await
        .unwrap();
        let old = cat
            .sync_checkpoint(ID, &mut scratch)
            .await
            .unwrap()
            .unwrap();
        let new_body = simple(&(item("a", "changed") + &item("b", "new")));
        let mut session = SyncSession::new(ID, ROOT, Some(old.clone())).unwrap();
        session
            .response::<std::io::Error>(
                200,
                Validators {
                    etag: Some("new"),
                    last_modified: None,
                },
            )
            .unwrap();
        session
            .push(&mut cat, new_body.as_bytes(), &mut scratch)
            .await
            .unwrap();
        assert!(!session.end_page::<std::io::Error>().unwrap());
        image.fail.set(true);
        assert!(session.complete(&mut cat, &mut scratch).await.is_err());
        assert!(matches!(
            cat.sync_checkpoint(ID, &mut scratch).await,
            Err(Error::Disabled)
        ));
        drop(cat);
        image.fail.set(false);
        let mut cat = image.reopen().await;
        assert_eq!(
            cat.sync_checkpoint(ID, &mut scratch).await.unwrap(),
            Some(old)
        );
        let p = import(
            &mut cat,
            &[new_body.as_bytes()],
            None,
            Validators::default(),
        )
        .await
        .unwrap();
        assert_eq!(p.created, 0);
        assert_eq!(p.queued, 0);
        assert!(
            cat.sync_checkpoint(ID, &mut scratch)
                .await
                .unwrap()
                .unwrap()
                .etag
                .is_empty()
        );
    });
}
#[test]
fn malformed_response_poisoning_cannot_be_bypassed() {
    // Regression (t-108): callers cannot turn malformed/oversized/unsolicited responses into success.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        for status in [206, 301, 404, 500, 304] {
            let mut session = SyncSession::new(ID, ROOT, None).unwrap();
            assert!(
                session
                    .response::<std::io::Error>(status, Validators::default())
                    .is_err()
            );
            assert!(session.complete(&mut cat, &mut scratch).await.is_err());
        }
        let mut session = SyncSession::new(ID, ROOT, None).unwrap();
        session
            .response::<std::io::Error>(200, Validators::default())
            .unwrap();
        let bad = simple(&item("a", "a")).replace("</rss>", "</bad>");
        assert!(
            session
                .push(&mut cat, bad.as_bytes(), &mut scratch)
                .await
                .is_err()
        );
        assert!(session.end_page::<std::io::Error>().is_err());
        assert!(session.complete(&mut cat, &mut scratch).await.is_err());
        assert!(
            cat.sync_checkpoint(ID, &mut scratch)
                .await
                .unwrap()
                .is_none()
        );
    });
}
#[test]
fn cancelled_upsert_poisoning_requires_reopen_and_replay() {
    // Regression (t-108): cancellation cannot skip an item and still publish validators.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        let mut session = SyncSession::new(ID, ROOT, None).unwrap();
        session
            .response::<std::io::Error>(200, Validators::default())
            .unwrap();
        image.hang.set(true);
        let body = simple(&item("a", "a"));
        {
            let operation = session.push(&mut cat, body.as_bytes(), &mut scratch);
            futures::pin_mut!(operation);
            assert!(futures::poll!(operation.as_mut()).is_pending());
        }
        assert!(session.end_page::<std::io::Error>().is_err());
        assert!(session.complete(&mut cat, &mut scratch).await.is_err());
        assert!(matches!(
            cat.sync_checkpoint(ID, &mut scratch).await,
            Err(Error::Disabled)
        ));
        drop(cat);
        image.hang.set(false);
        let mut cat = image.reopen().await;
        assert!(
            cat.sync_checkpoint(ID, &mut scratch)
                .await
                .unwrap()
                .is_none()
        );
        let progress = import(&mut cat, &[body.as_bytes()], None, Validators::default())
            .await
            .unwrap();
        assert_eq!(progress.created, 1);
        assert_eq!(progress.queued, 1);
    });
}
#[test]
fn header_and_chunk_limits_fail_without_completing() {
    // Regression (t-108): oversized transport input and unsafe validators cannot be saved.
    block_on(async {
        let image = Image::new();
        let mut cat = image.create().await;
        let mut scratch = [0; MAX_RECORD_BYTES];
        for etag in ["x\r\ny".into(), "x".repeat(513)] {
            let mut session = SyncSession::new(ID, ROOT, None).unwrap();
            assert!(
                session
                    .response::<std::io::Error>(
                        200,
                        Validators {
                            etag: Some(&etag),
                            last_modified: None
                        }
                    )
                    .is_err()
            );
            assert!(session.complete(&mut cat, &mut scratch).await.is_err());
        }
        let mut session = SyncSession::new(ID, ROOT, None).unwrap();
        session
            .response::<std::io::Error>(200, Validators::default())
            .unwrap();
        assert!(
            session
                .push(&mut cat, &[b' '; MAX_CHUNK_BYTES + 1], &mut scratch)
                .await
                .is_err()
        );
        assert!(session.complete(&mut cat, &mut scratch).await.is_err());
    });
}
