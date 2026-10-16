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
the UI as the Return key; holding it for 2 s stops the pod (panel asleep
and unpowered, touch and IMU parked, ESP32-S3 in deep sleep) and the next
press starts it again (ADR-0008).
Touches on the CST820 (I²C 0x15, interrupt on GPIO21) reach the UI as
pointer events. The QMI8658 accelerometer (I²C 0x6B or 0x6A)
turns the view in four orientations (ADR-0006). The pin assignments live in
`src/device/mod.rs`. Audio and the other peripherals are not implemented yet. Refer to the
[Waveshare ESP32-S3-Touch-AMOLED-1.8 documentation](https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.8)
for the V2 pinout and hardware resources.

## Layout

- `src/main.rs`: Slint event loop; talks to the board only through the
  `device` module and `theo-drivers` types.
- `src/input.rs`: the BOOT button (and its long-press stop), touch and
  orientation tasks, and the channels and signals between them and the
  event loop.
- `src/device/`: everything specific to the ESP32-S3 and this board (chip
  runtime, heaps and PSRAM, logger, pins, panel geometry, esp-hal QSPI DMA
  transport, BOOT button and deep sleep, shared async I2C bus, touch
  controller, IMU and its measured axis map); `restart.rs` holds the main
  watchdog, the start-cause log and the display reboot count.
- `src/slint_backend.rs`: Slint platform and framebuffer rendering.
- `crates/theo-drivers/`: HAL-independent drivers (CO5300, TCA9554, CST820,
  QMI8658, panel power-up/down, button and long press, `Panel`,
  `QspiBus`/`FrameBus`, framebuffer flushing) and the view-orientation logic,
  unit-tested on the host. See its `ARCHITECTURE.md`.
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
just miri                          # driver tests under Miri (nightly + miri component)
just audit                         # advisories, licenses, sources (cargo install cargo-deny)
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

## License

The source code is licensed under either of
[Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT),
at your option.

The firmware image also links [Slint](https://slint.dev), which is licensed
under GPL-3.0-only, its royalty-free license, or a commercial license. The
royalty-free license does not cover embedded systems, so a distributed
firmware image must comply with the GPL-3.0 (this source is compatible with
it) unless a Slint commercial license applies.
