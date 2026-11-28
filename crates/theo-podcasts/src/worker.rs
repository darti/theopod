//! Transport-independent download orchestration used by the firmware worker.
use crate::{
    catalogue::EpisodeId,
    runtime::{Command, Reply},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Stopped,
    Network,
    Trust,
    Storage,
    Protocol,
    Waiting,
    Retry(u32),
}
#[allow(async_fn_in_trait)]
pub trait DownloadIo {
    async fn command(&mut self, command: Command) -> Result<Reply, Error>;
    async fn transfer(&mut self, url: &str) -> Result<(), Error>;
}
pub fn handshake_error(error: embedded_tls::TlsError) -> Error {
    match error {
        embedded_tls::TlsError::Io(_)
        | embedded_tls::TlsError::IoError
        | embedded_tls::TlsError::ConnectionClosed => Error::Network,
        _ => Error::Trust,
    }
}
pub async fn cancellable<T>(
    operation: impl core::future::Future<Output = Result<T, Error>>,
    stop: impl core::future::Future<Output = ()>,
) -> Result<T, Error> {
    match embassy_futures::select::select(operation, stop).await {
        embassy_futures::select::Either::First(result) => result,
        embassy_futures::select::Either::Second(()) => Err(Error::Stopped),
    }
}

/// `Pending` replies accepted for one request before giving up (t-150).
///
/// Each `Pending` is one committed step: an eviction takes two, at most one
/// per catalogue episode, plus a few discards. A request past this bound
/// is not progressing, and resending it would spin the network task. It
/// bounds spinning, not latency: legitimate steps up to it can take
/// minutes, past the TLS idle timeout, and the transfer then fails as usual.
pub const MAX_PENDING_STEPS: u32 = {
    assert!(crate::catalogue::MAX_EPISODES < (u32::MAX as u64 - 8) / 2);
    2 * crate::catalogue::MAX_EPISODES as u32 + 8
};

pub async fn download_one<I: DownloadIo>(
    io: &mut I,
    after: &mut Option<EpisodeId>,
) -> Result<bool, Error> {
    loop {
        let Reply::Job {
            job,
            after: next,
            done,
        } = io.command(Command::NextJob(*after)).await?
        else {
            return Err(Error::Protocol);
        };
        if let Some(job) = job {
            // Advance before external I/O: a failing enclosure must not block
            // healthy later episodes. At end-of-pass the cursor resets, so a
            // retained failed job remains eligible on the next retry pass.
            *after = Some(job.id);
            let url = job.enclosure.clone();
            // Each start request makes one bounded step; repeat it.
            let mut pending = 0;
            loop {
                match io.command(Command::DownloadStart(job.clone())).await? {
                    Reply::Pending if pending < MAX_PENDING_STEPS => pending += 1,
                    Reply::Pending => return Err(Error::Protocol),
                    Reply::Ready => break,
                    Reply::Downloaded(crate::download::DownloadOutcome::WaitingForSpace) => {
                        return Err(Error::Waiting);
                    }
                    _ => return Ok(true),
                }
            }
            io.transfer(&url).await?;
            return match io.command(Command::DownloadEnd(true)).await? {
                Reply::Downloaded(crate::download::DownloadOutcome::Complete) => Ok(true),
                Reply::Downloaded(crate::download::DownloadOutcome::WaitingForSpace) => {
                    Err(Error::Waiting)
                }
                Reply::Downloaded(crate::download::DownloadOutcome::RetryLater) => {
                    Err(Error::Network)
                }
                _ => Err(Error::Protocol),
            };
        }
        if done {
            *after = None;
            return Ok(false);
        }
        if next == *after {
            return Err(Error::Protocol);
        }
        *after = next;
    }
}

/// One externally supplied deadline spans cancellation acknowledgment and final
/// persistence together. Never grant a fresh deadline when advancing stages.
pub async fn shutdown(
    stop: impl core::future::Future<Output = ()>,
    flush: impl core::future::Future<Output = ()>,
    deadline: impl core::future::Future<Output = ()>,
) -> bool {
    cancellable(
        async {
            stop.await;
            flush.await;
            Ok(())
        },
        deadline,
    )
    .await
    .is_ok()
}
#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
