//! Fallible placement of large cooperative futures; task headers stay internal.
use allocator_api2::{
    alloc::{AllocError, Allocator},
    boxed::Box,
};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
/// Reserve storage before constructing the future. The closure gives the
/// optimizer an in-place return destination instead of moving a live future.
/// The allocation is pinned; executor task headers and atomic wake state remain
/// outside it. The caller must verify large-future initialization stack use.
#[inline(always)]
pub fn try_pin_in<F: Future, A: Allocator + 'static>(
    make: impl FnOnce() -> F,
    allocator: A,
    max_bytes: usize,
) -> Result<Pin<Box<F, A>>, AllocError> {
    if core::mem::size_of::<F>() > max_bytes {
        return Err(AllocError);
    }
    let allocation = Box::<F, A>::try_new_uninit_in(allocator)?;
    Ok(Box::into_pin(Box::write(allocation, make())))
}
/// Await a future placed by [`try_pin_in`], polled in a frame of its own.
///
/// Awaiting a large future directly builds it on the caller's stack before
/// moving it into the caller's state, and inlines its poll temporaries into
/// the caller's frame. Here neither happens: the caller's state holds one
/// pointer, and the poll frame exists only while it runs. Keep the output
/// small: it is still returned through the caller's frame.
pub async fn run_in<F: Future, A: Allocator + 'static>(
    make: impl FnOnce() -> F,
    allocator: A,
    max_bytes: usize,
) -> Result<F::Output, AllocError> {
    let mut future = try_pin_in(make, allocator, max_bytes)?;
    Ok(core::future::poll_fn(|cx| poll_apart(future.as_mut(), cx)).await)
}
/// Await `future` in place, polled in a frame of its own.
///
/// The future's state stays where the caller's state is (no allocation);
/// only its poll temporaries move out of the caller's frame. Use it for each
/// arm of a large request dispatcher: inlined together, the arms' temporaries
/// share one frame of their combined size (53 KB on core1, t-133).
pub async fn apart<F: Future>(future: F) -> F::Output {
    let mut future = core::pin::pin!(future);
    core::future::poll_fn(|cx| poll_apart(future.as_mut(), cx)).await
}
#[inline(never)]
fn poll_apart<F: Future>(future: Pin<&mut F>, cx: &mut Context<'_>) -> Poll<F::Output> {
    future.poll(cx)
}
#[cfg(test)]
#[path = "future_tests.rs"]
mod tests;
