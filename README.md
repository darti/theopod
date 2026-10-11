# Theo Pod

Rust `no_std` firmware targeting the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.

## Hardware

The V2 board has a 368 × 448 AMOLED display driven by the CO5300 and a CST820
touch controller, using QSPI for the display and I²C for touch. Its other
onboard hardware includes an AXP2101 PMU, PCF85063 RTC, QMI8658 IMU, ES8311
audio codec, and microSD card slot.

This firmware targets the V2 hardware. The display is implemented: the
CO5300 is driven over QSPI and its rails are cycled through the TCA9554 I/O
expander. Slint renders into a framebuffer in PSRAM and only the dirty rows
are sent to the panel by async DMA. The BOOT button (GPIO0) is forwarded to
the UI as the Return key. The pin assignments live in `src/device/mod.rs`.
Touch, audio, and the other peripherals are not implemented yet. Refer to the
[Waveshare ESP32-S3-Touch-AMOLED-1.8 documentation](https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.8)
for the V2 pinout and hardware resources.

## Layout

- `src/main.rs`: Slint event loop and the BOOT button task; talks to the
  board only through the `device` module and `theo-drivers` types.
- `src/device/`: everything specific to the ESP32-S3 and this board (chip
  runtime, heaps and PSRAM, logger, pins, panel geometry, esp-hal QSPI DMA
  transport, BOOT button, shared async I2C bus).
- `src/slint_backend.rs`: Slint platform and framebuffer rendering.
- `crates/theo-drivers/`: HAL-independent drivers (CO5300, TCA9554, panel
  power-up, button, `Panel`, `QspiBus`, framebuffer flushing), unit-tested on
  the host. See its `ARCHITECTURE.md`.
- `crates/theo-frame-dump/`: `no_std` text format for dumping the framebuffer
  over the serial log (firmware writer, host parser).
- `tools/frame-png/`: host tool turning a dump in a serial log into a PNG.
- `docs/adr/`: architecture decision records.

## Setup

```sh
just install
```

The Cargo configuration in `.cargo/config.toml` automatically provides the
paths required by the ESP toolchain. No `export-esp.sh` is needed.

## Build and flash

```sh
just check
just test                          # host unit tests (drivers, frame dump, frame-png)
just lint                          # clippy on the firmware and host crates
just build
just flash                         # automatically detect the port
just flash /dev/cu.usbmodemXXXX   # specify the port
just monitor /dev/cu.usbmodemXXXX
just run /dev/cu.usbmodemXXXX     # flash and start the serial monitor
just flash-monitor                 # flash, reset, then start the monitor
```

The firmware renders the Slint UI from `ui/main.slint` on the AMOLED display.
It logs heap usage (internal RAM and PSRAM) at start-up and after the first
frame; `log` output only appears when the firmware is built with `ESP_LOG`
set (for example `ESP_LOG=info just run`).

## Seeing what the firmware renders

```sh
just flash-dump      # flash with the `frame-dump` feature, record the log from boot
                     # (Ctrl-C after `theo-frame-end`)
just frame-png       # target/frame.log -> target/frame.png
just monitor-log     # record later dumps (one after each BOOT press)
```
