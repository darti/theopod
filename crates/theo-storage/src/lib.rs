//! Bounded, recoverable persistent-log buffering.

#![no_std]

extern crate alloc;

pub mod card_layout;
#[cfg(feature = "legacy-fat")]
pub mod fat_page_file;
pub mod littlefs;
pub mod page_store;
pub mod sector_bus;

use core::fmt;

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
        let mut record = Self::new();
        let mut writer = RecordWriter {
            buffer: &mut record,
        };
        let result = fmt::write(&mut writer, args);
        if result.is_err() || !writer.finish() {
            self.dropped += 1;
            return false;
        }
        self.push(record.pending())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn record_dropped(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

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
            if self.buffer.len == CAP {
                return false;
            }
            self.buffer.bytes[self.buffer.len] = b'\n';
            self.buffer.len += 1;
            true
        } else {
            true
        }
    }
}

#[cfg(test)]
mod storage_tests;

#[cfg(test)]
mod littlefs_tests;

/// Match ESP_LOG's global and longest module-prefix filters. Levels use
/// log's ordering: error=1 through trace=5; zero disables a target.
pub fn log_enabled(filter: &str, target: &str, level: u8) -> bool {
    let mut limit = 0;
    let mut specificity = 0;
    for directive in filter.split(',') {
        let directive = directive.trim();
        let (module, value) = directive.split_once('=').unwrap_or(("", directive));
        let module = module.trim();
        if !(module.is_empty()
            || target == module
            || target
                .strip_prefix(module)
                .is_some_and(|rest| rest.starts_with("::")))
        {
            continue;
        }
        let value = match value.trim() {
            "off" => 0,
            "error" => 1,
            "warn" => 2,
            "info" => 3,
            "debug" => 4,
            "trace" => 5,
            _ => continue,
        };
        if module.len() >= specificity {
            specificity = module.len();
            limit = value;
        }
    }
    level <= limit
}

pub const LOG_FILE_LIMIT: u64 = 128 * 1024;

pub fn rotation_needed(current: u64, incoming: usize) -> bool {
    current > 0 && current.saturating_add(incoming as u64) > LOG_FILE_LIMIT
}

/// Validate a sector-aligned request while allowing short (one-sector)
/// progress through a larger read/write buffer.
pub fn sector_index(position: u64, blocks: u64, len: usize) -> Option<u32> {
    if !position.is_multiple_of(512) || len == 0 || !len.is_multiple_of(512) {
        return None;
    }
    let block = position / 512;
    if block.checked_add((len / 512) as u64)? > blocks {
        return None;
    }
    u32::try_from(block).ok()
}

#[cfg(test)]
#[path = "page_store_rollover_tests.rs"]
mod page_store_rollover_tests;
