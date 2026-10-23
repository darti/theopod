use super::{LogBuffer, LogSink};
use core::fmt;

// Regression (t-94): persistent log buffering remains bounded and recoverable.

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

// Regression (t-94): formatted records are newline terminated.
#[test]
fn formats_records_with_a_newline() {
    let mut buffer = LogBuffer::<32>::new();

    assert!(buffer.push_fmt(format_args!("temperature={}", 23)));

    let mut sink = Sink::new(32);
    assert_eq!(buffer.flush(&mut sink, 32), Ok(15));
    assert_eq!(&sink.output[..sink.len], b"temperature=23\n");
}

// Regression (t-94): overflow drops the oldest complete lines.
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

// Regression (t-94): partial writes retain the remaining suffix.
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

// Regression (t-94): sink errors preserve the buffered bytes.
#[test]
fn failed_flush_preserves_all_bytes_for_retry() {
    let mut buffer = LogBuffer::<16>::new();
    assert!(buffer.push(b"retry\n"));
    let mut sink = Sink::new(16);
    sink.fail = true;

    assert_eq!(buffer.flush(&mut sink, 16), Err(()));
    assert_eq!(buffer.len(), 6);
}

// Regression (t-94): failed formatting never queues a partial record.
#[test]
fn oversized_formatted_record_is_not_partially_buffered() {
    let mut buffer = LogBuffer::<8>::new();

    assert!(!buffer.push_fmt(format_args!("{}", Split)));
    assert_eq!(buffer.len(), 0);
}

// Regression (t-94): formatting must evict old complete records atomically.
#[test]
fn formatted_record_evicts_oldest_and_keeps_newest() {
    let mut buffer = LogBuffer::<8>::new();
    assert!(buffer.push_fmt(format_args!("old")));
    assert!(buffer.push_fmt(format_args!("newer")));
    assert_eq!(buffer.pending(), b"newer\n");
    assert_eq!(buffer.dropped(), 1);
}

// Regression (t-94): a missing newline must not evict the record itself.
#[test]
fn newline_overflow_preserves_existing_records() {
    let mut buffer = LogBuffer::<4>::new();
    assert!(buffer.push_fmt(format_args!("ok")));
    assert!(!buffer.push_fmt(format_args!("1234")));
    assert_eq!(buffer.pending(), b"ok\n");
    assert_eq!(buffer.dropped(), 1);
}

// Regression (t-94): successful partial writes resume at exactly the next byte.
#[test]
fn sink_failure_then_retry_does_not_drop_or_duplicate_bytes() {
    let mut buffer = LogBuffer::<16>::new();
    buffer.push(b"abcdef\n");
    let mut sink = Sink::new(3);
    assert_eq!(buffer.flush(&mut sink, 16), Ok(3));
    sink.fail = true;
    assert_eq!(buffer.flush(&mut sink, 16), Err(()));
    assert_eq!(buffer.pending(), b"def\n");
    sink.fail = false;
    assert_eq!(buffer.flush(&mut sink, 16), Ok(3));
    assert_eq!(buffer.flush(&mut sink, 16), Ok(1));
    assert_eq!(&sink.output[..sink.len], b"abcdef\n");
}

// Regression (t-94): module directives obey longest-prefix matching.
#[test]
fn log_filter_preserves_global_and_module_levels() {
    use super::log_enabled;
    assert!(log_enabled("info", "theo_pod", 3));
    assert!(!log_enabled("info", "theo_pod", 4));
    assert!(!log_enabled("", "theo_pod", 1));
    assert!(log_enabled("info,theo_pod=debug", "theo_pod::device", 4));
    assert!(!log_enabled("theo=trace", "theo_pod", 5));
    assert!(!log_enabled(
        "theo_pod::device=off,theo_pod=trace,info",
        "theo_pod::device::sd",
        1
    ));
    assert!(log_enabled("off,theo_pod=debug", "theo_pod", 4));
}

// Regression (t-94): rotate before crossing the size limit, never an empty file.
#[test]
fn log_rotation_bounds_active_file() {
    use super::{LOG_FILE_LIMIT, rotation_needed};
    assert!(!rotation_needed(0, 1024));
    assert!(!rotation_needed(LOG_FILE_LIMIT - 1024, 1024));
    assert!(rotation_needed(LOG_FILE_LIMIT - 1023, 1024));
    assert!(rotation_needed(u64::MAX, 1024));
}

// Regression (t-94): FAT issues multi-sector requests; sector addresses must
// be computed before narrowing to u32 (cards exceed a 4 GiB byte offset).
#[test]
fn sector_requests_accept_multiple_blocks_and_large_offsets() {
    use super::sector_index;
    assert_eq!(sector_index(0, 122_265_600, 2048), Some(0));
    assert_eq!(
        sector_index(4_294_967_296, 122_265_600, 512),
        Some(8_388_608)
    );
    assert_eq!(sector_index(512, 2, 512), Some(1));
    assert_eq!(sector_index(512, 2, 1024), None);
    assert_eq!(sector_index(1, 2, 512), None);
    assert_eq!(sector_index(0, 2, 513), None);
    assert_eq!(sector_index(0, 2, 0), None);
}
