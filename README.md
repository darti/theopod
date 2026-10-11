# Theo Pod

Rust `no_std` firmware targeting the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.

## Hardware

The V2 board has a 368 × 448 AMOLED display driven by the CO5300 and a CST820
touch controller, using QSPI for the display and I²C for touch. Its other
onboard hardware includes an AXP2101 PMU, PCF85063 RTC, QMI8658 IMU, ES8311
audio codec, and microSD card slot.

This firmware targets the V2 hardware. Display, touch, and peripheral drivers
are not yet implemented, and no pin assignments are configured. Refer to the
[Waveshare ESP32-S3-Touch-AMOLED-1.8 documentation](https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.8)
for the V2 pinout and hardware resources.

## Setup

```sh
just install
```

The Cargo configuration in `.cargo/config.toml` automatically provides the
paths required by the ESP toolchain. No `export-esp.sh` is needed.

## Build and flash

```sh
just check
just build
just flash                         # automatically detect the port
just flash /dev/cu.usbmodemXXXX   # specify the port
just monitor /dev/cu.usbmodemXXXX
just run /dev/cu.usbmodemXXXX     # flash and start the serial monitor
```

The firmware currently writes `Hello world!` to the serial output.
