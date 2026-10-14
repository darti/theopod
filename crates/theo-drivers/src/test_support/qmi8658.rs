//! [`FakeQmi8658`]: async I2C bus modelling a QMI8658's registers.

use embedded_hal_async::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

/// QMI8658 registers, as seen by [`FakeQmi8658`].
const QMI_WHO_AM_I: u8 = 0x00;
const QMI_CTRL1: u8 = 0x02;
/// CTRL1 bit enabling register auto-increment.
const QMI_CTRL1_AUTO_INCREMENT: u8 = 0x40;
const QMI_ACCEL_X_LOW: u8 = 0x35;
const QMI_RESET_RESULT: u8 = 0x4D;

/// Async I2C bus emulating one QMI8658: register file, reads that only
/// auto-increment once CTRL1 enables it (as on the chip), and a soft reset
/// that completes after `reset_polls` reads of the reset-result register
/// (never, when `None`).
///
/// `writes` records every register write as `(register, value)`. A bus at
/// another address NACKs; with `failing` set every transaction fails, and
/// with `failing_register` set writes to that register fail.
#[derive(Debug)]
pub struct FakeQmi8658 {
    pub address: u8,
    pub registers: [u8; 0x80],
    pub writes: Vec<(u8, u8)>,
    pub reset_polls: Option<usize>,
    pub failing: bool,
    pub failing_register: Option<u8>,
}

impl FakeQmi8658 {
    /// Creates a QMI8658 at `address` whose reset completes on first poll.
    pub fn new(address: u8) -> Self {
        let mut registers = [0; 0x80];
        registers[usize::from(QMI_WHO_AM_I)] = 0x05;
        Self {
            address,
            registers,
            writes: Vec::new(),
            reset_polls: Some(1),
            failing: false,
            failing_register: None,
        }
    }

    /// Sets the raw X, Y and Z acceleration registers (little-endian).
    pub fn set_acceleration(&mut self, x: i16, y: i16, z: i16) {
        let start = usize::from(QMI_ACCEL_X_LOW);
        for (index, value) in [x, y, z].into_iter().enumerate() {
            let offset = start + 2 * index;
            self.registers[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }

    fn read(&mut self, register: u8) -> u8 {
        if register == QMI_RESET_RESULT {
            return match &mut self.reset_polls {
                Some(0) => 0x80,
                Some(polls) => {
                    *polls -= 1;
                    if *polls == 0 { 0x80 } else { 0x00 }
                }
                None => 0x00,
            };
        }
        self.registers[usize::from(register)]
    }

    /// Register after `register` in a burst: the next one with auto-increment
    /// on (wrapping in the 128-register map), the same one otherwise.
    fn next(&self, register: u8) -> u8 {
        if self.registers[usize::from(QMI_CTRL1)] & QMI_CTRL1_AUTO_INCREMENT == 0 {
            register
        } else {
            register.wrapping_add(1) & 0x7F
        }
    }
}

impl ErrorType for FakeQmi8658 {
    type Error = ErrorKind;
}

impl I2c for FakeQmi8658 {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        if self.failing {
            return Err(ErrorKind::Bus);
        }
        if address != self.address {
            return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address));
        }
        let mut pointer = 0;
        for operation in operations {
            match operation {
                Operation::Write(bytes) => {
                    let Some((&register, data)) = bytes.split_first() else {
                        continue;
                    };
                    pointer = register & 0x7F;
                    for &value in data {
                        if self.failing_register == Some(pointer) {
                            return Err(ErrorKind::Bus);
                        }
                        self.registers[usize::from(pointer)] = value;
                        self.writes.push((pointer, value));
                        pointer = self.next(pointer);
                    }
                }
                Operation::Read(buffer) => {
                    for byte in buffer.iter_mut() {
                        *byte = self.read(pointer);
                        pointer = self.next(pointer);
                    }
                }
            }
        }
        Ok(())
    }
}
