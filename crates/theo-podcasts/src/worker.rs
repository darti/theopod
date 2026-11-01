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
            match io.command(Command::DownloadStart(job)).await? {
                Reply::Ready => (),
                Reply::Downloaded(crate::download::DownloadOutcome::WaitingForSpace) => {
                    return Err(Error::Waiting);
                }
                _ => return Ok(true),
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
