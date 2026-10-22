use super::{LogBuffer, LogSink};
use core::fmt;

struct Split;

impl fmt::Display for Split {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("1234")?;
        f.write_str("56789")
    }
}

struct Sink {
    output: [u8; 32],
    len: usize,
    limit: usize,
    fail: bool,
}

impl Sink {
    fn new(limit: usize) -> Self {
        Self {
            output: [0; 32],
            len: 0,
            limit,
            fail: false,
        }
    }
}

impl LogSink for Sink {
    type Error = ();

    fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error> {
        if self.fail {
            return Err(());
        }
        let count = bytes.len().min(self.limit);
        self.output[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
        Ok(count)
    }
}

#[test]
fn formats_records_with_a_newline() {
    let mut buffer = LogBuffer::<32>::new();

    assert!(buffer.push_fmt(format_args!("temperature={}", 23)));

    let mut sink = Sink::new(32);
    assert_eq!(buffer.flush(&mut sink, 32), Ok(15));
    assert_eq!(&sink.output[..sink.len], b"temperature=23\n");
}

#[test]
fn drops_oldest_complete_record_when_full() {
    let mut buffer = LogBuffer::<7>::new();
    assert!(buffer.push(b"one\n"));
    assert!(buffer.push(b"two\n"));

    let mut sink = Sink::new(32);
    assert_eq!(buffer.flush(&mut sink, 32), Ok(4));
    assert_eq!(&sink.output[..sink.len], b"two\n");
    assert_eq!(buffer.dropped(), 1);
}

#[test]
fn partial_flush_keeps_unwritten_bytes_queued() {
    let mut buffer = LogBuffer::<16>::new();
    assert!(buffer.push(b"abcd\n"));
    assert!(buffer.push(b"efgh\n"));
    let mut sink = Sink::new(3);

    assert_eq!(buffer.flush(&mut sink, 16), Ok(3));
    assert_eq!(buffer.len(), 7);
    assert_eq!(buffer.flush(&mut sink, 16), Ok(3));
    assert_eq!(buffer.len(), 4);
}

#[test]
fn failed_flush_preserves_all_bytes_for_retry() {
    let mut buffer = LogBuffer::<16>::new();
    assert!(buffer.push(b"retry\n"));
    let mut sink = Sink::new(16);
    sink.fail = true;

    assert_eq!(buffer.flush(&mut sink, 16), Err(()));
    assert_eq!(buffer.len(), 6);
}

#[test]
fn oversized_formatted_record_is_not_partially_buffered() {
    let mut buffer = LogBuffer::<8>::new();

    assert!(!buffer.push_fmt(format_args!("{}", Split)));
    assert_eq!(buffer.len(), 0);
}
