use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, Operation};
use speculoos::prelude::*;

use crate::tca9554::Tca9554;

/// I2C bus recording every write as `(address, bytes)`, optionally failing.
#[derive(Debug, Default)]
struct RecordingI2c {
    writes: Vec<(u8, Vec<u8>)>,
    fail: bool,
}

impl ErrorType for RecordingI2c {
    type Error = ErrorKind;
}

impl I2c for RecordingI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        if self.fail {
            return Err(ErrorKind::Bus);
        }
        for operation in operations {
            match operation {
                Operation::Write(bytes) => self.writes.push((address, bytes.to_vec())),
                Operation::Read(_) => return Err(ErrorKind::Other),
            }
        }
        Ok(())
    }
}

#[test]
fn set_outputs_clears_config_bits_of_output_pins() {
    let mut expander = Tca9554::new(RecordingI2c::default(), 0x20);

    expander.set_outputs(0x07).unwrap();

    assert_that!(expander.release().writes).is_equal_to(vec![(0x20, vec![0x03, 0xF8])]);
}

#[test]
fn write_levels_writes_output_register() {
    let mut expander = Tca9554::new(RecordingI2c::default(), 0x21);

    expander.write_levels(0x05).unwrap();

    assert_that!(expander.release().writes).is_equal_to(vec![(0x21, vec![0x01, 0x05])]);
}

#[test]
fn write_levels_bus_failure_returns_i2c_error() {
    let i2c = RecordingI2c {
        fail: true,
        ..RecordingI2c::default()
    };
    let mut expander = Tca9554::new(i2c, 0x20);

    let result = expander.write_levels(0x05);

    assert_that!(result.err()).is_equal_to(Some(ErrorKind::Bus));
}

#[test]
fn new_borrowed_bus_drives_caller_bus() {
    let mut i2c = RecordingI2c::default();

    Tca9554::new(&mut i2c, 0x20).write_levels(0x07).unwrap();

    assert_that!(i2c.writes).is_equal_to(vec![(0x20, vec![0x01, 0x07])]);
}
