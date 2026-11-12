use super::*;
use crate::{
    catalogue::{AudioState, EpisodeId, EpisodeMetadata, MAX_RECORD_BYTES, SubscriptionId},
    media_test_support::{HostMedia, root_for},
    runtime::Runtime,
    test_support::Image,
};
use futures::executor::block_on;
use std::collections::VecDeque;
#[test]
fn connection_loss_during_handshake_retries_but_invalid_trust_stops() {
    // Regression (t-110): a transient socket failure must not require manual trust setup.
    assert_eq!(
        handshake_error(embedded_tls::TlsError::IoError),
        Error::Network
    );
    assert_eq!(
        handshake_error(embedded_tls::TlsError::ConnectionClosed),
        Error::Network
    );
    assert_eq!(
        handshake_error(embedded_tls::TlsError::InvalidCertificate),
        Error::Trust
    );
    assert_eq!(
        handshake_error(embedded_tls::TlsError::InvalidSignature),
        Error::Trust
    );
}
enum Transfer {
    Disconnect,
    Short,
    Complete,
}
struct Io {
    owner: Runtime<HostMedia>,
    responses: VecDeque<Transfer>,
    attempts: std::vec::Vec<u64>,
    scratch: [u8; MAX_RECORD_BYTES],
}
impl DownloadIo for Io {
    async fn command(&mut self, command: Command) -> Result<Reply, Error> {
        if let Command::DownloadStart(job) = &command {
            self.attempts.push(job.id.0);
        }
        self.owner
            .handle(command, &mut self.scratch)
            .await
            .map_err(|e| match e {
                crate::runtime::Error::Invalid => Error::Protocol,
                _ => Error::Storage,
            })
    }
    async fn transfer(&mut self, _: &str) -> Result<(), Error> {
        let bytes: &[u8] = match self.responses.pop_front().unwrap() {
            Transfer::Disconnect => return Err(Error::Network),
            Transfer::Short => b"123",
            Transfer::Complete => b"1234",
        };
        self.command(Command::DownloadHeaders {
            status: 200,
            length: None,
            partial: false,
        })
        .await?;
        self.command(Command::DownloadChunk(
            crate::runtime::Chunk::from_slice(bytes).unwrap(),
        ))
        .await?;
        Ok(())
    }
}
fn io(image: &Image, responses: &[Transfer]) -> Io {
    let mut cat = image.create();
    let mut scratch = [0; MAX_RECORD_BYTES];
    for guid in ["one", "two"] {
        cat.upsert(
            SubscriptionId(1),
            EpisodeMetadata {
                guid: Some(guid),
                title: guid,
                enclosure: "https://media.radiofrance-podcast.net/a.mp3",
                description: None,
                published: None,
                enclosure_bytes: Some(4),
                duration_seconds: None,
            },
            &mut scratch,
        )
        .unwrap();
    }
    Io {
        owner: Runtime::new(cat, HostMedia::new(root_for(image))),
        responses: responses
            .iter()
            .map(|r| match r {
                Transfer::Disconnect => Transfer::Disconnect,
                Transfer::Short => Transfer::Short,
                Transfer::Complete => Transfer::Complete,
            })
            .collect(),
        attempts: std::vec::Vec::new(),
        scratch,
    }
}
#[test]
fn failed_first_episode_does_not_block_later_queued_audio() {
    // Regression (t-110): production traversal advances after failure; recovery retains the failed job.
    block_on(async {
        let image = Image::new();
        let mut io = io(&image, &[Transfer::Disconnect, Transfer::Complete]);
        let mut after = None;
        assert_eq!(download_one(&mut io, &mut after).await, Err(Error::Network));
        io.command(Command::Abort).await.unwrap();
        assert_eq!(download_one(&mut io, &mut after).await, Ok(true));
        assert_eq!(io.attempts, [1, 2]);
        assert_eq!(download_one(&mut io, &mut after).await, Ok(false));
        assert_eq!(after, None);
        drop(io);
        let mut cat = image.reopen();
        let mut scratch = [0; MAX_RECORD_BYTES];
        assert_eq!(
            cat.get(EpisodeId(1), &mut scratch)
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Queued
        );
        assert_eq!(
            cat.get(EpisodeId(2), &mut scratch)
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Available
        );
    });
}
#[test]
fn short_framed_body_is_not_reported_as_success() {
    // Regression (t-110): RetryLater must reach the worker's backoff path, not its success loop.
    block_on(async {
        let image = Image::new();
        let mut io = io(&image, &[Transfer::Short]);
        assert_eq!(download_one(&mut io, &mut None).await, Err(Error::Network));
        drop(io);
        let mut cat = image.reopen();
        assert_eq!(
            cat.get(EpisodeId(1), &mut [0; MAX_RECORD_BYTES])
                .unwrap()
                .unwrap()
                .state
                .audio,
            AudioState::Queued
        );
    });
}
#[test]
fn stop_drops_the_pending_operation_in_the_production_cancellation_seam() {
    // Regression (t-110): stop must release a pending operation rather than leave it running.
    use std::cell::Cell;
    struct Guard<'a>(&'a Cell<bool>);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let dropped = Cell::new(false);
    let operation = async {
        let _guard = Guard(&dropped);
        futures::future::pending::<Result<(), Error>>().await
    };
    assert_eq!(
        block_on(cancellable(operation, futures::future::ready(()))),
        Err(Error::Stopped)
    );
    assert!(dropped.get());
}
#[test]
fn one_shutdown_deadline_spans_network_stop_and_pending_final_flush() {
    // Regression (t-110): finishing stop does not grant flush a second time budget or acknowledge pending writes.
    use futures::{channel::oneshot, future::join};
    use std::cell::Cell;
    let flush_started = Cell::new(false);
    let flush_dropped = Cell::new(false);
    struct Guard<'a>(&'a Cell<bool>);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let (stopped, stop_ack) = oneshot::channel::<()>();
    let (expired, deadline) = oneshot::channel::<()>();
    let flush = async {
        flush_started.set(true);
        let _guard = Guard(&flush_dropped);
        futures::future::pending::<()>().await;
    };
    let result = block_on(join(
        shutdown(
            async {
                let _ = stop_ack.await;
            },
            flush,
            async {
                let _ = deadline.await;
            },
        ),
        async {
            let _ = stopped.send(());
            embassy_futures::yield_now().await;
            let _ = expired.send(());
        },
    ))
    .0;
    assert!(!result);
    assert!(flush_started.get());
    assert!(flush_dropped.get());
}
#[test]
fn shutdown_reports_success_only_after_both_acknowledgments() {
    // Regression (t-110): the final-flush acknowledgment is required, not just network termination.
    use std::cell::Cell;
    let flushed = Cell::new(false);
    assert!(block_on(shutdown(
        async {},
        async {
            flushed.set(true);
        },
        futures::future::pending()
    )));
    assert!(flushed.get());
}

// Regression (t-133): each podcast request makes at most one commit, so the
// worker repeats `DownloadStart` while the owner answers `Pending`.
#[test]
fn download_start_is_repeated_while_pending() {
    struct Scripted {
        replies: VecDeque<Reply>,
        starts: usize,
    }
    impl DownloadIo for Scripted {
        async fn command(&mut self, command: Command) -> Result<Reply, Error> {
            if let Command::DownloadStart(_) = command {
                self.starts += 1;
            }
            self.replies.pop_front().ok_or(Error::Protocol)
        }
        async fn transfer(&mut self, _: &str) -> Result<(), Error> {
            Ok(())
        }
    }
    let job = crate::download::DownloadJob {
        id: EpisodeId(7),
        enclosure: "https://cdn.example/a".try_into().unwrap(),
        expected_bytes: None,
    };
    let mut io = Scripted {
        replies: VecDeque::from([
            Reply::Job {
                job: Some(job),
                after: None,
                done: false,
            },
            Reply::Pending,
            Reply::Pending,
            Reply::Ready,
            Reply::Downloaded(crate::download::DownloadOutcome::Complete),
        ]),
        starts: 0,
    };
    let mut after = None;
    assert_eq!(block_on(download_one(&mut io, &mut after)), Ok(true));
    assert_eq!(io.starts, 3);
    assert!(io.replies.is_empty());
}
