# theo-drivers

HAL-independent drivers for the Theo Pod hardware, plus HAL-free input
logic (orientation) and shared geometry. The crate is `no_std`,
depends only on `embedded-hal` / `embedded-hal-async`, and is unit-tested on the host (`just test`).
See `docs/adr/0001` and `0002` for why.

## Contents

- `panel.rs`: async `Panel` trait (open an area, stream big-endian RGB565).
- `framebuffer.rs`: `flush_rows`, which sends dirty rows of a full-panel
  framebuffer as one full-width, even-row-aligned area.
- `qspi.rs`: async `QspiBus` trait (8-bit opcode, 24-bit address, single or
  quad data, `MAX_TRANSFER_LEN`). `embedded-hal` has no Quad-SPI trait.
- `co5300.rs`: CO5300 AMOLED controller. Implements `Panel` over any
  `QspiBus`; validates geometry, 2x2 alignment and pixel counts, and splits
  pixel payloads into bus-sized transfers.
- `tca9554.rs`: TCA9554 I/O expander over async I2C, per pin (`Pin`) or
  by mask; every change is a read-modify-write, outputs latch levels
  before direction.
- `panel_power.rs`: `PanelPower`, the panel rail/reset pulse through the
  expander. Board values live in `src/device/`.
- `cst820.rs`: `Cst820` touch controller over async I2C; pulses its reset
  through a `Tca9554` pin, waits for the interrupt edge while idle and
  polls while touched; rejects reports off the panel, and cancels (rather
  than releases) a touch whose read fails.
- `qmi8658.rs`: `Qmi8658` IMU over async I2C, accelerometer only.
- `geometry.rs`, `orientation.rs`: `PanelSize`/`TouchPoint`;
  `OrientationDetector` (gravity to view `Rotation`, dead band), `AxisMap`,
  and the rotated view size and touch mapping.
- `button.rs`: `DebouncedButton`, cancel-safe debounced press/release.

## Extension points

- Implement `QspiBus` to run the CO5300 on another HAL (the ESP32-S3
  implementation lives in the firmware crate, `src/device/esp_qspi.rs`).
- Implement `Panel` to support another display controller.

## Dependencies

- No sibling crates. The firmware crate (`theo-pod`) depends on this one.

## Tests

Side-by-side `*_tests.rs` files use the fakes in `test_support/`: recording
`QspiBus` and `Panel`, `AddressBus`, `RecordingDelay`, and one async I2C
fake per chip (`FakeTca9554`, `FakeCst820`, `FakeQmi8658`). Hardware-only:
the QSPI phases and DMA of `src/device/esp_qspi.rs` / `psram_tx_buf.rs`,
CST820 timing, and the IMU axis map (`src/device/mod.rs`).
