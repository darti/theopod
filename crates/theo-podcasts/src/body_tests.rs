use super::*;
use core::cell::RefCell;
use futures::executor::block_on;
use std::{format, string::String, vec, vec::Vec};

/// What happened, in order, across the reader and the sink.
type Log = RefCell<Vec<String>>;

struct Reader<'a> {
    log: &'a Log,
    body: Vec<u8>,
    at: usize,
    reads: usize,
    /// Fail the read with this index (1-based) instead of returning data.
    fail_at: Option<usize>,
    /// Most bytes one read returns, like a TLS record boundary.
    most: usize,
}
impl BodyRead for Reader<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.reads += 1;
        let k = self.reads;
        self.log.borrow_mut().push(format!("read {k} start"));
        embassy_futures::yield_now().await;
        if self.fail_at == Some(k) {
            self.log.borrow_mut().push(format!("read {k} failed"));
            return Err(Error::Network);
        }
        let n = buf.len().min(self.body.len() - self.at).min(self.most);
        buf[..n].copy_from_slice(&self.body[self.at..self.at + n]);
        self.at += n;
        self.log.borrow_mut().push(format!("read {k} end"));
        Ok(n)
    }
}

struct Sink<'a> {
    log: &'a Log,
    stored: Vec<u8>,
    sizes: Vec<usize>,
    stores: usize,
    /// Fail the store with this index (1-based).
    fail_at: Option<usize>,
}
impl ChunkSink for Sink<'_> {
    async fn store(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.stores += 1;
        let k = self.stores;
        self.log.borrow_mut().push(format!("store {k} start"));
        // Storage takes longer than the read: several polls.
        for _ in 0..3 {
            embassy_futures::yield_now().await;
        }
        if self.fail_at == Some(k) {
            self.log.borrow_mut().push(format!("store {k} failed"));
            return Err(Error::Storage);
        }
        self.stored.extend_from_slice(bytes);
        self.sizes.push(bytes.len());
        self.log.borrow_mut().push(format!("store {k} end"));
        Ok(())
    }
}

fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}
fn reader(log: &Log, len: usize) -> Reader<'_> {
    Reader {
        log,
        body: body(len),
        at: 0,
        reads: 0,
        fail_at: None,
        most: usize::MAX,
    }
}
fn sink(log: &Log) -> Sink<'_> {
    Sink {
        log,
        stored: Vec::new(),
        sizes: Vec::new(),
        stores: 0,
        fail_at: None,
    }
}
fn never() -> core::future::Pending<()> {
    core::future::pending()
}
fn position(log: &Log, event: &str) -> usize {
    log.borrow()
        .iter()
        .position(|e| e == event)
        .unwrap_or_else(|| panic!("{event} missing from {:?}", log.borrow()))
}

#[test]
fn pump_overlapped_reads_next_chunk_while_storing_previous() {
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 10), sink(&log));
    let (mut front, mut back) = ([0; 4], [0; 4]);
    block_on(pump(&mut r, &mut s, &mut front, &mut back, true, never)).unwrap();
    assert!(position(&log, "read 2 start") < position(&log, "store 1 end"));
    assert!(position(&log, "read 3 start") < position(&log, "store 2 end"));
}

#[test]
fn pump_overlapped_hands_store_over_before_reading() {
    // Regression (t-160): a TLS read that decrypts a buffered record finishes
    // within one poll, keeping core0 busy. Polled first, it delayed the store
    // request to core1 until it was done: no overlap on the board.
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 10), sink(&log));
    let (mut front, mut back) = ([0; 4], [0; 4]);
    block_on(pump(&mut r, &mut s, &mut front, &mut back, true, never)).unwrap();
    assert!(position(&log, "store 1 start") < position(&log, "read 2 start"));
    assert!(position(&log, "store 2 start") < position(&log, "read 3 start"));
}

#[test]
fn pump_fills_each_buffer_across_short_reads() {
    // Regression (t-160): a TLS record yields one slow read then three
    // instant ones; storing per read left three stores in four serial. A
    // full buffer per step overlaps a whole record's store with the next.
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 19), sink(&log));
    r.most = 3;
    let (mut front, mut back) = ([0; 8], [0; 8]);
    block_on(pump(&mut r, &mut s, &mut front, &mut back, true, never)).unwrap();
    assert_eq!(s.sizes, vec![8, 8, 3]);
    assert_eq!(s.stored, body(19));
}

#[test]
fn pump_overlapped_stores_every_byte_in_order() {
    // The swapped buffers must never hand the sink a chunk being read into.
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 4 * 5 + 3), sink(&log));
    let (mut front, mut back) = ([0; 4], [0; 4]);
    block_on(pump(&mut r, &mut s, &mut front, &mut back, true, never)).unwrap();
    assert_eq!(s.stored, body(23));
    assert_eq!(s.stores, 6);
}

#[test]
fn pump_overlapped_stores_last_chunk_before_returning() {
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 6), sink(&log));
    let (mut front, mut back) = ([0; 4], [0; 4]);
    block_on(pump(&mut r, &mut s, &mut front, &mut back, true, never)).unwrap();
    assert_eq!(log.borrow().last().map(String::as_str), Some("store 2 end"));
}

#[test]
fn pump_sequential_stores_each_chunk_before_next_read() {
    // Feed chunks are consumed in parts: no overlap.
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 10), sink(&log));
    let (mut front, mut back) = ([0; 4], [0; 4]);
    block_on(pump(&mut r, &mut s, &mut front, &mut back, false, never)).unwrap();
    assert!(position(&log, "store 1 end") < position(&log, "read 2 start"));
    assert!(position(&log, "store 2 end") < position(&log, "read 3 start"));
    assert_eq!(s.stored, body(10));
}

#[test]
fn pump_store_error_wins_over_concurrent_read_error() {
    // `Storage` disables the mount without `Abort`: a read error must not hide it.
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 10), sink(&log));
    r.fail_at = Some(2);
    s.fail_at = Some(1);
    let (mut front, mut back) = ([0; 4], [0; 4]);
    let result = block_on(pump(&mut r, &mut s, &mut front, &mut back, true, never));
    assert_eq!(result, Err(Error::Storage));
}

#[test]
fn pump_read_error_waits_for_store_in_flight() {
    // A store admitted by the owner is never cancelled by a failing read.
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 10), sink(&log));
    r.fail_at = Some(2);
    let (mut front, mut back) = ([0; 4], [0; 4]);
    let result = block_on(pump(&mut r, &mut s, &mut front, &mut back, true, never));
    assert_eq!(result, Err(Error::Network));
    assert!(position(&log, "read 2 failed") < position(&log, "store 1 end"));
    assert_eq!(s.stored, body(4));
}

#[test]
fn pump_stop_cancels_read_and_store_together() {
    let log = Log::default();
    let (mut r, mut s) = (reader(&log, 10), sink(&log));
    let (mut front, mut back) = ([0; 4], [0; 4]);
    let stop = || async {};
    let result = block_on(pump(&mut r, &mut s, &mut front, &mut back, true, stop));
    assert_eq!(result, Err(Error::Stopped));
    assert_eq!(s.stored, vec![]);
}
