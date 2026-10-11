# theo-drivers

HAL-independent drivers for the Theo Pod hardware. The crate is `no_std`,
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
- `button.rs`: `DebouncedButton`, cancel-safe debounced press/release.

## Extension points

- Implement `QspiBus` to run the CO5300 on another HAL (the ESP32-S3
  implementation lives in the firmware crate, `src/device/esp_qspi.rs`).
- Implement `Panel` to support another display controller.

## Dependencies

- No sibling crates. The firmware crate (`theo-pod`) depends on this one.

## Tests

Side-by-side `*_tests.rs` files use fakes from `test_support.rs`: recording
`QspiBus` and `Panel`, `AddressBus` (where pixel payloads start, at a real
transfer size), and `FakeTca9554`, an async I2C bus that models the chip's
registers, replays which pins are driven low, and can fail or suspend
transactions. Not covered on the host: the single-line command/address
phases and DMA behaviour of `src/device/esp_qspi.rs` and
`src/device/psram_tx_buf.rs`, only verified on hardware.
