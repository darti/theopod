//! Fallible placement of large cooperative futures; task headers stay internal.
use allocator_api2::{
    alloc::{AllocError, Allocator},
    boxed::Box,
};
use core::{future::Future, pin::Pin};
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
#[cfg(test)]
#[path = "future_tests.rs"]
mod tests;
