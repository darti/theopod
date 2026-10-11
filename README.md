# Theo Pod

Rust `no_std` firmware targeting the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.

## Hardware

The V2 board has a 368 × 448 AMOLED display driven by the CO5300 and a CST820
touch controller, using QSPI for the display and I²C for touch. Its other
onboard hardware includes an AXP2101 PMU, PCF85063 RTC, QMI8658 IMU, ES8311
audio codec, and microSD card slot.

This firmware targets the V2 hardware. The display is implemented: the
CO5300 is driven over QSPI and its rails are cycled through the TCA9554 I/O
expander. The pin assignments live in `src/device/mod.rs`. Touch, audio, and
the other peripherals are not implemented yet. Refer to the
[Waveshare ESP32-S3-Touch-AMOLED-1.8 documentation](https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.8)
for the V2 pinout and hardware resources.

## Layout

- `src/main.rs`: Slint event loop; talks to the board only through
  `device::{entry, Board}` and the `theo_drivers::Panel` trait.
- `src/device/`: everything specific to the ESP32-S3 and this board (chip
  runtime, heap, logger, pins, panel geometry, esp-hal QSPI transport).
- `src/slint_backend.rs`: Slint platform and frame streaming to any `Panel`.
- `crates/theo-drivers/`: HAL-independent drivers (CO5300, TCA9554, `Panel`,
  `QspiBus`), unit-tested on the host. See its `ARCHITECTURE.md`.

## Setup

```sh
just install
```

The Cargo configuration in `.cargo/config.toml` automatically provides the
paths required by the ESP toolchain. No `export-esp.sh` is needed.

## Build and flash

```sh
just check
just test                          # host unit tests for crates/theo-drivers
just build
just flash                         # automatically detect the port
just flash /dev/cu.usbmodemXXXX   # specify the port
just monitor /dev/cu.usbmodemXXXX
just run /dev/cu.usbmodemXXXX     # flash and start the serial monitor
just flash-monitor                 # flash, reset, then start the monitor
```

The firmware renders the Slint UI from `ui/main.slint` on the AMOLED display.
