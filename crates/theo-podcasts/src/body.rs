//! Response body pump: reads chunks and hands each to storage.
//!
//! [`pump`] overlaps the read of chunk N+1 with the store of chunk N when
//! asked (audio, t-160): storage runs on the other core, so the network
//! read and the SD write proceed in parallel. One store is in flight at a
//! time (ADR-0022), and a store already handed to the owner always runs to
//! its end: only the stop future cancels it, together with the read.
use crate::worker::{Error, cancellable};
use embassy_futures::{join::join, yield_now};

/// Source of a response body. Implement this to feed [`pump`].
#[expect(async_fn_in_trait, reason = "single-threaded executor, no Send bound")]
pub trait BodyRead {
    /// Reads into `buf`; `Ok(0)` is the end of the body.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error>;
}

/// Destination of body chunks. Implement this to receive [`pump`]'s chunks.
#[expect(async_fn_in_trait, reason = "single-threaded executor, no Send bound")]
pub trait ChunkSink {
    /// Stores all of `bytes`, however many owner requests that takes.
    async fn store(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

/// Reads the body to its end, storing every chunk before returning `Ok`.
///
/// `front` and `back` are the two chunk buffers (same length), each
/// filled across reads before it is stored; `back` is unused unless
/// `overlap`. While overlapping, the next read waits (yielding) until
/// `handed_off()` says the store in flight no longer needs this core to
/// reach storage. A store error wins over a concurrent read
/// error: `Storage` must reach the caller, who then skips `Abort`. Each
/// read/store step races a fresh `stop()` future, so one stop request
/// cancels both sides at once.
pub async fn pump<'b, F: core::future::Future<Output = ()>>(
    reader: &mut impl BodyRead,
    sink: &mut impl ChunkSink,
    mut front: &'b mut [u8],
    mut back: &'b mut [u8],
    overlap: bool,
    mut stop: impl FnMut() -> F,
    handed_off: impl Fn() -> bool,
) -> Result<(), Error> {
    // Bytes at the start of `back` still to be stored.
    let mut pending = 0;
    loop {
        let (stored, read) = cancellable(
            async {
                let store = async {
                    match pending {
                        0 => Ok(()),
                        n => sink.store(&back[..n]).await,
                    }
                };
                // Store first, and read only once the chunk has left this
                // core (`handed_off`): a read decrypting a buffered TLS
                // record completes within one poll and holds this core
                // meanwhile, so whatever relays the store here would wait
                // for it (t-160, t-163).
                let read = async {
                    while pending > 0 && !handed_off() {
                        yield_now().await;
                    }
                    fill(reader, front).await
                };
                Ok(join(store, read).await)
            },
            stop(),
        )
        .await?;
        stored?;
        let n = read?;
        if n == 0 {
            return Ok(());
        }
        if overlap {
            core::mem::swap(&mut front, &mut back);
            pending = n;
        } else {
            cancellable(sink.store(&front[..n]), stop()).await?;
        }
    }
}

/// Reads until `buf` is full or the body ends.
///
/// A TLS record decrypts in one slow read, then serves instant ones: a
/// step must span whole records for its store to overlap the next record.
async fn fill(reader: &mut impl BodyRead, buf: &mut [u8]) -> Result<usize, Error> {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]).await? {
            0 => break,
            read => n += read,
        }
    }
    Ok(n)
}

#[cfg(test)]
#[path = "body_tests.rs"]
mod tests;
