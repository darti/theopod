//! [`FakeCst820`]: async I2C bus replaying scripted CST820 touch reports.

use std::collections::VecDeque;

use embedded_hal_async::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

use crate::cst820::REGISTER_FINGERS;

/// Async I2C bus emulating a CST820's report registers.
///
/// Each read of the finger-count register pops one scripted report (finger count,
/// then X and Y as high/low byte pairs) or error; with no report left it
/// panics. `writes` records every register write as `(register, value)`,
/// `transactions` counts every transaction, failed or not.
#[derive(Debug)]
pub struct FakeCst820 {
    pub address: u8,
    pub reports: VecDeque<Result<[u8; 5], ErrorKind>>,
    pub writes: Vec<(u8, u8)>,
    pub transactions: usize,
}

impl FakeCst820 {
    /// Creates a controller at `address` replaying `reports` in order.
    pub fn new(address: u8, reports: impl IntoIterator<Item = Result<[u8; 5], ErrorKind>>) -> Self {
        Self {
            address,
            reports: reports.into_iter().collect(),
            writes: Vec::new(),
            transactions: 0,
        }
    }

    /// Report of one finger at (`x`, `y`); `event` fills the X high nibble.
    pub fn touch(x: u16, y: u16, event: u8) -> Result<[u8; 5], ErrorKind> {
        assert!(
            event < 4 && x <= 0xFFF,
            "event or X does not fit the report"
        );
        let [x_high, x_low] = x.to_be_bytes();
        let [y_high, y_low] = y.to_be_bytes();
        Ok([1, (event << 6) | x_high, x_low, y_high, y_low])
    }

    /// Report of no finger on the panel.
    pub fn no_touch() -> Result<[u8; 5], ErrorKind> {
        Ok([0; 5])
    }
}

impl ErrorType for FakeCst820 {
    type Error = ErrorKind;
}

impl I2c for FakeCst820 {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.transactions += 1;
        if address != self.address {
            return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address));
        }
        let mut pointer = None;
        for operation in operations {
            match operation {
                Operation::Write(bytes) => {
                    let Some((&register, data)) = bytes.split_first() else {
                        continue;
                    };
                    pointer = Some(register);
                    if let Some(&value) = data.first() {
                        self.writes.push((register, value));
                    }
                }
                Operation::Read(buffer) => {
                    assert_eq!(pointer, Some(REGISTER_FINGERS), "unexpected CST820 read");
                    let report = self
                        .reports
                        .pop_front()
                        .expect("no scripted CST820 report left")?;
                    let len = buffer.len().min(report.len());
                    buffer[..len].copy_from_slice(&report[..len]);
                }
            }
        }
        Ok(())
    }
}
