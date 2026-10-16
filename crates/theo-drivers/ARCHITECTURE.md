# theo-drivers

HAL-independent drivers for the Theo Pod hardware, plus HAL-free logic
(orientation, flush bands and retries, DMA arithmetic). `no_std`, depends
only on `embedded-hal` / `embedded-hal-async`, unit-tested on the host
(`just test`). See `docs/adr/0001` and `0002` for why. Dependencies: no
sibling crates; the firmware crate (`theo-pod`) depends on this one.

## Contents

- `panel.rs`: async `Panel` trait (exposes its framebuffer, sends ranges).
- `framebuffer.rs`, `dirty_rows.rs`: `flush_rows` sends a band of rows of a
  full-panel framebuffer as one full-width, even-row-aligned area;
  `flush_bands` sends each band of a `DirtyRows` (few disjoint bands).
- `retry.rs`, `stale_watch.rs`: `Transient` errors, the `FlushRetry` and
  `reset_due` policies; `StaleWatch`, the reboot policy for a broken panel.
- `qspi.rs`: async `QspiBus` transport (8-bit opcode, 24-bit address,
  single or quad data, `MAX_TRANSFER_LEN`); `FrameBus` adds the
  framebuffer it sends in place (`write_frame`).
- `chip_select.rs`: `ChipSelect`, released on drop (cancel-safe buses).
- `dma_layout.rs`, `memory_check.rs`: in-place PSRAM DMA arithmetic;
  `check_pattern`, a RAM self-test.
- `co5300.rs`: CO5300 AMOLED (init, sleep), a `Panel` on any `FrameBus`;
  checks geometry, frame size, areas and pixel counts, splits payloads.
- `tca9554.rs`, `panel_power.rs`: TCA9554 expander (registers read once,
  then cached; levels before direction); `PanelPower`, rail pulse/cut.
- `cst820.rs`: `Cst820` touch, reset by a `Tca9554` pin; interrupt-driven
  when idle, polled while touched; cancels a touch that a read, reset or
  `hold_in_reset` interrupts. `qmi8658.rs`: `Qmi8658`, accelerometer only.
- `geometry.rs`, `orientation.rs`: `PanelSize`/`TouchPoint`;
  `OrientationDetector` (gravity to `Rotation`), `AxisMap` (const-checked
  by `is_rotation`), the rotated view size and touch mapping.
- `button.rs`, `long_press.rs`: `DebouncedButton`, cancel-safe debounced
  press/release; `LongPress`, short vs long presses, and `LongPressButton`,
  a button raced against the long-press deadline (ADR-0008).

## Extension points

- Implement `QspiBus` and `FrameBus` to run the CO5300 on another HAL
  (ESP32-S3: `src/device/esp_qspi.rs`).
- Implement `Panel` to support another display controller.

## Tests

Side-by-side `*_tests.rs` files use the fakes in `test_support/`
(recording `FrameBus` and `Panel`, delays, one async I2C fake per chip).
Hardware-only: QSPI phases and DMA of `src/device/` (their arithmetic is
`dma_layout.rs`), CST820 timing, and the IMU axis map.
