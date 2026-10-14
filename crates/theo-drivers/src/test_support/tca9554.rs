//! [`FakeTca9554`]: async I2C bus modelling a TCA9554's registers.

use embedded_hal_async::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

use super::YieldOnce;

/// TCA9554 register indices, as seen by [`FakeTca9554`].
pub const TCA_INPUT: u8 = 0x00;
pub const TCA_OUTPUT: u8 = 0x01;
pub const TCA_CONFIG: u8 = 0x03;

/// Async I2C bus emulating one TCA9554: register pointer, reads and writes.
///
/// `writes` records every register write as `(register, value)`; `inputs`
/// is what the input port reads back. A bus at another address NACKs.
#[derive(Debug)]
pub struct FakeTca9554 {
    pub address: u8,
    pub registers: [u8; 4],
    pub inputs: u8,
    pub writes: Vec<(u8, u8)>,
    pointer: u8,
    fail_at: Option<usize>,
    fail_writes_only: bool,
    yielding: bool,
}

impl FakeTca9554 {
    /// Creates an expander at `address` with power-on register values.
    pub fn new(address: u8) -> Self {
        Self {
            address,
            // Input, output (all high), polarity (none), config (all inputs).
            registers: [0x00, 0xFF, 0x00, 0xFF],
            inputs: 0x00,
            writes: Vec::new(),
            pointer: 0,
            fail_at: None,
            fail_writes_only: false,
            yielding: false,
        }
    }

    /// Makes every transaction fail once `fail_at` writes are recorded.
    pub fn failing(mut self, fail_at: usize) -> Self {
        self.fail_at = Some(fail_at);
        self
    }

    /// Like [`Self::failing`], but register reads keep succeeding.
    pub fn failing_writes(mut self, fail_at: usize) -> Self {
        self.fail_writes_only = true;
        self.failing(fail_at)
    }

    /// Makes every transaction return `Pending` once before running.
    pub fn yielding(mut self) -> Self {
        self.yielding = true;
        self
    }

    /// Pins driven low after each recorded write, replayed from `initial`.
    ///
    /// A pin is driven low when it is an output (config bit clear) whose
    /// output bit is clear.
    pub fn driven_low_history(&self, initial: [u8; 4]) -> Vec<u8> {
        let mut registers = initial;
        self.writes
            .iter()
            .map(|&(register, value)| {
                registers[usize::from(register)] = value;
                !registers[usize::from(TCA_CONFIG)] & !registers[usize::from(TCA_OUTPUT)]
            })
            .collect()
    }
}

impl ErrorType for FakeTca9554 {
    type Error = ErrorKind;
}

impl I2c for FakeTca9554 {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        if self.yielding {
            YieldOnce(false).await;
        }
        // A write carries data after the register pointer.
        let writes_data = operations
            .iter()
            .any(|operation| matches!(operation, Operation::Write(bytes) if bytes.len() > 1));
        if self.fail_at == Some(self.writes.len()) && (writes_data || !self.fail_writes_only) {
            return Err(ErrorKind::Bus);
        }
        if address != self.address {
            return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address));
        }
        for operation in operations {
            match operation {
                Operation::Write(bytes) => {
                    let Some((&pointer, data)) = bytes.split_first() else {
                        continue;
                    };
                    if pointer > TCA_CONFIG {
                        return Err(ErrorKind::Other);
                    }
                    self.pointer = pointer;
                    // The TCA9554 does not auto-increment: extra bytes rewrite it.
                    for &value in data {
                        self.registers[usize::from(pointer)] = value;
                        self.writes.push((pointer, value));
                    }
                }
                Operation::Read(buffer) => {
                    let value = if self.pointer == TCA_INPUT {
                        self.inputs
                    } else {
                        self.registers[usize::from(self.pointer)]
                    };
                    buffer.fill(value);
                }
            }
        }
        Ok(())
    }
}
