# theo-drivers

HAL-independent drivers for the Theo Pod hardware, plus HAL-free logic
(orientation, flush bands and retries, DMA arithmetic). `no_std`, depends
only on `embedded-hal` / `embedded-hal-async`, unit-tested on the host
(`just test`). See `docs/adr/0001` and `0002` for why.

## Contents

- `panel.rs`: async `Panel` trait (open an area, stream big-endian RGB565).
- `framebuffer.rs`, `dirty_rows.rs`: `flush_rows` sends a band of rows of a
  full-panel framebuffer as one full-width, even-row-aligned area;
  `flush_bands` sends each band of a `DirtyRows` (few disjoint bands).
- `retry.rs`: `Transient` errors and `FlushRetry`, the resend policy.
- `qspi.rs`: async `QspiBus` trait (8-bit opcode, 24-bit address, single or
  quad data, `MAX_TRANSFER_LEN`). `embedded-hal` has no Quad-SPI trait.
- `chip_select.rs`: `ChipSelect`, released on drop (cancel-safe buses).
- `dma_layout.rs`, `memory_check.rs`: descriptor chunking and payload
  checks of the in-place PSRAM DMA; `check_pattern`, a RAM self-test.
- `co5300.rs`: CO5300 AMOLED controller. Implements `Panel` over any
  `QspiBus`; validates geometry, 2x2 alignment and pixel counts, and splits
  pixel payloads into bus-sized transfers.
- `tca9554.rs`, `panel_power.rs`: TCA9554 expander (read-modify-writes,
  levels latched before direction); `PanelPower`, the panel rail pulse.
- `cst820.rs`: `Cst820` touch controller over async I2C; pulses its reset
  through a `Tca9554` pin, waits for the interrupt edge while idle and
  polls while touched; rejects reports off the panel, and cancels (rather
  than releases) a touch whose read fails or that a reset interrupts.
- `qmi8658.rs`: `Qmi8658` IMU over async I2C, accelerometer only.
- `geometry.rs`, `orientation.rs`: `PanelSize`/`TouchPoint`;
  `OrientationDetector` (gravity to `Rotation`), `AxisMap` (const-checked
  by `is_rotation`), the rotated view size and touch mapping.
- `button.rs`: `DebouncedButton`, cancel-safe debounced press/release.

## Extension points

- Implement `QspiBus` to run the CO5300 on another HAL (the ESP32-S3
  implementation lives in the firmware crate, `src/device/esp_qspi.rs`).
- Implement `Panel` to support another display controller.

## Dependencies

- No sibling crates. The firmware crate (`theo-pod`) depends on this one.

## Tests

Side-by-side `*_tests.rs` files use the fakes in `test_support/`
(recording `QspiBus` and `Panel`, delays, one async I2C fake per chip).
Hardware-only: QSPI phases and DMA of `src/device/` (their arithmetic is
`dma_layout.rs`), CST820 timing, and the IMU axis map.
