# theo-drivers

HAL-independent drivers for the Theo Pod hardware. The crate is `no_std`,
depends only on `embedded-hal` / `embedded-hal-async` (plus `bytemuck` for
safe pixel-to-byte views), and is unit-tested on the host (`just test`).

## Contents

- `panel.rs`: `Panel` trait, the pixel-stream abstraction the Slint backend
  renders into (open an area, then stream big-endian RGB565 rows).
- `frame.rs`: `FrameStreamer`, which turns a line renderer's output into one
  full-panel area per frame (line order, full width, big-endian RGB565).
- `qspi.rs`: `QspiBus` trait (8-bit opcode, 24-bit address, single or quad
  data). `embedded-hal` has no Quad-SPI trait, hence this local one.
- `co5300.rs`: CO5300 AMOLED controller. Implements `Panel` over any
  `QspiBus`; validates geometry, 2x2 area alignment, and pixel counts.
- `tca9554.rs`: TCA9554 I2C I/O expander over `embedded_hal::i2c::I2c`.

## Extension points

- Implement `QspiBus` to run the CO5300 on another HAL (the ESP32-S3
  implementation lives in the firmware crate, `src/device/esp_qspi.rs`).
- Implement `Panel` to support another display controller.

## Dependencies

- No sibling crates. The firmware crate (`theo-pod`) depends on this one.

## Tests

Side-by-side `*_tests.rs` files use recording `QspiBus` and `Panel` fakes
from `test_support.rs`. Regression tests cover the driver's opcode and lane
choices, the CO5300 2x2 alignment rule, and full-frame streaming.

Not covered on the host: the single-line command/address phases are set by
the `QspiBus` implementation (`src/device/esp_qspi.rs`) and are only verified
on hardware.
