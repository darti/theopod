# Board wiring

Read before touching wiring, pins or I2C.

- Display pin assignments, panel geometry, the BOOT button, PSRAM mode and
  frequency, and the TCA9554 power-up values are defined in
  `src/device/mod.rs`, the single source of truth for board wiring; the
  power-up sequence itself is `PanelPower` in `crates/theo-drivers`.
- The CST820 touch wiring (I2C 0x15, reset on TCA9554 EXIO2, interrupt on
  GPIO21), the QMI8658 address and axis map (`IMU_AXES`, measured on the
  board) and the shared async I2C bus (ADR-0005) are there too.
- Do not assume the audio pin assignments: they are not yet documented or
  implemented.
- Undocumented board wiring: check Waveshare's sources
  (`waveshareteam/ESP32-S3-Touch-AMOLED-1.8`: `pin_config.h`,
  `board_variant.c`; BSP in `waveshareteam/Waveshare-ESP32-components`) and
  cite them in `src/device/mod.rs`.
- Never block on the shared I2C expander to select the SD card: touch/IMU
  also use that bus (ADR-0005).
