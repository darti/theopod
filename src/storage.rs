//! Bounded, recoverable persistent-log buffering.

use core::fmt;

#[cfg(test)]
pub trait LogSink {
    type Error;

    fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error>;
}

pub struct LogBuffer<const CAP: usize> {
    bytes: [u8; CAP],
    len: usize,
    dropped: usize,
}

impl<const CAP: usize> LogBuffer<CAP> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; CAP],
            len: 0,
            dropped: 0,
        }
    }

    pub fn push(&mut self, record: &[u8]) -> bool {
        if record.len() > CAP {
            self.dropped += 1;
            return false;
        }
        while CAP - self.len < record.len() {
            self.drop_oldest_record();
        }
        self.bytes[self.len..self.len + record.len()].copy_from_slice(record);
        self.len += record.len();
        true
    }

    pub fn push_fmt(&mut self, args: fmt::Arguments<'_>) -> bool {
        let start = self.len;
        let mut writer = RecordWriter { buffer: self };
        let result = fmt::write(&mut writer, args);
        if result.is_err() {
            self.len = start;
            self.dropped += 1;
            return false;
        }
        writer.finish()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[cfg(test)]
    pub fn flush<S: LogSink>(&mut self, sink: &mut S, max_bytes: usize) -> Result<usize, S::Error> {
        let offered = self.len.min(max_bytes);
        if offered == 0 {
            return Ok(0);
        }
        let written = sink.write(&self.bytes[..offered])?;
        let written = written.min(offered);
        self.bytes.copy_within(written..self.len, 0);
        self.len -= written;
        Ok(written)
    }

    pub fn pending(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub fn consume(&mut self, bytes: usize) {
        let bytes = bytes.min(self.len);
        self.bytes.copy_within(bytes..self.len, 0);
        self.len -= bytes;
    }

    fn drop_oldest_record(&mut self) {
        let end = self.bytes[..self.len]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(self.len, |index| index + 1);
        self.bytes.copy_within(end..self.len, 0);
        self.len -= end;
        self.dropped += 1;
    }
}

impl<const CAP: usize> Default for LogBuffer<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

struct RecordWriter<'a, const CAP: usize> {
    buffer: &'a mut LogBuffer<CAP>,
}

impl<const CAP: usize> fmt::Write for RecordWriter<'_, CAP> {
    fn write_str(&mut self, string: &str) -> fmt::Result {
        if self.buffer.len + string.len() > CAP {
            return Err(fmt::Error);
        }
        self.buffer.bytes[self.buffer.len..self.buffer.len + string.len()]
            .copy_from_slice(string.as_bytes());
        self.buffer.len += string.len();
        Ok(())
    }
}

impl<const CAP: usize> RecordWriter<'_, CAP> {
    fn finish(self) -> bool {
        if self.buffer.len == 0 || self.buffer.bytes[self.buffer.len - 1] != b'\n' {
            self.buffer.push(b"\n")
        } else {
            true
        }
    }
}

#[cfg(test)]
mod storage_tests;
