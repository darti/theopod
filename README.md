# Theo Pod

Rust `no_std` firmware skeleton for the ESP32-S3-Touch-ePaper-1.54.

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

The firmware currently writes `Hello world !` to the serial output. The exact
board pin assignments will be documented before the e-paper, touch, and audio
drivers are added.
