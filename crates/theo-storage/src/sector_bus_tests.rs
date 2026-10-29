// Regression guard: actual cross-thread sector handoff and cancelled RPCs.
extern crate std;
use crate::{
    littlefs::{Geometry, SectorIo, SectorStorage},
    sector_bus::{Operation, Request, SectorBus, SectorClient},
};
use alloc::{boxed::Box, vec};
use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll},
};
use futures::{executor::block_on, task::noop_waker_ref};
use littlefs_rust::{Error, Filesystem};

struct Proxy(SectorClient<'static>);

// Regression (t-103): synchronous core callbacks must not register an Embassy timer.
#[test]
fn blocking_sector_deadline_cancels_the_inflight_exchange() {
    let bus = SectorBus::new();
    let mut client = bus.take_client().unwrap();
    let mut polls = 0;
    let result = client.exchange_blocking(
        Request {
            operation: Operation::Read(0),
            deadline_ticks: 1,
        },
        || {
            polls += 1;
            polls >= 3
        },
    );
    assert_eq!(result, Err(Error::Io));
    assert!(bus.is_disabled());
    assert_eq!(polls, 3);
    let bus = SectorBus::new();
    let mut client = bus.take_client().unwrap();
    assert_eq!(
        client.exchange_blocking(
            Request {
                operation: Operation::Sync,
                deadline_ticks: 0
            },
            || true
        ),
        Err(Error::Io)
    );
    assert!(bus.is_disabled());
}
impl Proxy {
    fn call(&mut self, operation: Operation) -> Result<[u8; 512], Error> {
        block_on(self.0.exchange(Request {
            operation,
            deadline_ticks: 0,
        }))
    }
}
impl SectorIo for Proxy {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        out.copy_from_slice(&self.call(Operation::Read(sector))?);
        Ok(())
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        self.call(Operation::Write(sector, *data)).map(|_| ())
    }
    fn sync(&mut self) -> Result<(), Error> {
        self.call(Operation::Sync).map(|_| ())
    }
}

// Regression (t-101): filesystem and async device run on separate threads.
#[test]
fn filesystem_and_async_device_run_on_separate_threads() {
    let bus = Box::leak(Box::new(SectorBus::new()));
    let client = bus.take_client().unwrap();
    assert!(bus.take_client().is_none());
    let (done, finished) = futures::channel::oneshot::channel();
    let worker = std::thread::spawn(move || {
        let geometry = Geometry::new(512).unwrap();
        let mut adapter = SectorStorage::new(Proxy(client), geometry);
        Filesystem::format(&mut adapter, &geometry.config()).unwrap();
        let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
        fs.write_file("/episode", &[42; 8192]).unwrap();
        let adapter = fs.unmount().unwrap();
        let fs = Filesystem::mount(adapter, geometry.config()).ok().unwrap();
        assert_eq!(fs.read_to_vec("/episode").unwrap(), [42; 8192]);
        drop(fs.unmount().unwrap());
        done.send(()).unwrap();
    });
    let mut image = vec![0xff; 4096 * 64];
    block_on(async {
        use futures::{FutureExt, select_biased};
        let mut finished = finished.fuse();
        loop {
            select_biased! {
                result = finished => { result.unwrap(); break; },
                request = bus.receive().fuse() => {
                    let mut bytes = [0; 512];
                    match request.operation {
                        Operation::Read(sector) => bytes.copy_from_slice(&image[sector as usize * 512..][..512]),
                        Operation::Write(sector, data) => image[sector as usize * 512..][..512].copy_from_slice(&data),
                        Operation::Sync => {},
                    }
                    bus.respond(Ok(bytes));
                }
            }
        }
    });
    worker.join().unwrap();
    assert!(bus.is_disabled());
}

// Regression (t-101): cancelling inflight rpc rejects late reply and next request.
#[test]
fn cancelling_inflight_rpc_rejects_late_reply_and_next_request() {
    let bus = SectorBus::new();
    let mut client = bus.take_client().unwrap();
    let mut cx = Context::from_waker(noop_waker_ref());
    {
        let mut request = pin!(client.exchange(Request {
            operation: Operation::Read(7),
            deadline_ticks: 123
        }));
        assert!(request.as_mut().poll(&mut cx).is_pending());
        let queued = block_on(bus.receive());
        assert_eq!(queued.deadline_ticks, 123);
        assert!(matches!(queued.operation, Operation::Read(7)));
        // Dropping this future simulates with_deadline cancelling the wait.
    }
    assert!(bus.is_disabled());
    bus.respond(Ok([99; 512]));
    let mut next = pin!(client.exchange(Request {
        operation: Operation::Read(8),
        deadline_ticks: 456
    }));
    assert_eq!(next.as_mut().poll(&mut cx), Poll::Ready(Err(Error::Io)));
    assert!(pin!(bus.receive()).as_mut().poll(&mut cx).is_pending());
}
