use super::*;
use allocator_api2::alloc::{AllocError, Allocator, Global};
use core::{alloc::Layout, cell::Cell, ptr::NonNull};
struct Reject;
// SAFETY: allocation always fails; no memory is ever handed to the caller.
unsafe impl Allocator for Reject {
    fn allocate(&self, _: Layout) -> Result<NonNull<[u8]>, AllocError> {
        Err(AllocError)
    }
    unsafe fn deallocate(&self, _: NonNull<u8>, _: Layout) {
        unreachable!()
    }
}
#[test]
fn failed_future_placement_does_not_construct_or_start_storage() {
    // Regression (t-110): a PSRAM reservation failure must leave services offline before construction.
    let constructed = Cell::new(false);
    let placed = try_pin_in(
        || {
            constructed.set(true);
            async {}
        },
        Reject,
        4096,
    );
    assert!(placed.is_err());
    assert!(!constructed.get());
}
#[test]
fn allocated_future_retains_its_local_address_across_yields() {
    // Regression (t-110): polling a placed future must not move its borrowed local state.
    futures::executor::block_on(
        try_pin_in(
            || async {
                let bytes = [1u8; 4096];
                let address = bytes.as_ptr();
                embassy_futures::yield_now().await;
                assert_eq!(address, bytes.as_ptr());
            },
            Global,
            8192,
        )
        .unwrap(),
    );
}
