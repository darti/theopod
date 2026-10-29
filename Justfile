set shell := ["zsh", "-cu"]

chip := "esp32s3"

esp_toolchain := `dirname "$(rustup which rustc --toolchain esp)"`
libclang_path := `find "$(dirname "$(rustup which rustc --toolchain esp)")/../xtensa-esp32-elf-clang" -type d -path '*/esp-clang/lib' -print -quit`
host_target := `rustc +stable -vV | sed -n 's/^host: //p'`
gcc_path := `find "$(dirname "$(rustup which rustc --toolchain esp)")/../xtensa-esp-elf" -type d -path '*/xtensa-esp-elf/bin' -print -quit`

default:
    @just --list

install:
    RUSTUP_TOOLCHAIN=stable cargo install espup --locked
    espup install
    cargo install cargo-espflash --locked

check:
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo check

# Clippy on the firmware (ESP toolchain) and the host crates.
lint:
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy -- -D warnings
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy --features frame-dump -- -D warnings
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy --features imu-calibration -- -D warnings
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy --features watchdog-check -- -D warnings
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy --features watchdog-loop -- -D warnings
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy --features sleep-check -- -D warnings
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy --features sd-check -- -D warnings
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo clippy --features sd-provision -- -D warnings
    RUSTUP_TOOLCHAIN=stable cargo clippy -p theo-drivers -p theo-frame-dump -p theo-storage -p frame-png --target {{host_target}} --all-targets --features theo-storage/legacy-fat -- -D warnings

build:
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo build --release

flash port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

monitor port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

run port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

# Like `run`, but flash, hard-reset, then monitor as separate espflash calls.
flash-monitor port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash reset --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

# Flash a build that dumps the framebuffer, recording the log from boot
# (Ctrl-C once `theo-frame-end` is printed).
flash-dump log="target/frame.log" port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features frame-dump --monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{log}}

# Flash normal firmware and capture serial logs from boot without a TTY.
flash-log log="target/serial.log" port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{log}}

# Non-destructive SD diagnostics: full, readonly, removed, timeout, absent, rotate.
sd-check fault log="target/sd-check.log" port="":
    THEO_SD_FAULT={{quote(fault)}} ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features sd-check --monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{quote(log)}}

# Three real deep-sleep/timer-wake cycles; passive monitor preserves wake causes.
sd-sleep-check log="target/sd-sleep.log" port="":
    THEO_SD_FAULT=sleep ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features sd-check --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}
    python3 -I tools/passive-monitor.py {{quote(log)}} {{ if port != "" { quote(port) } else { "" } }}

# Flash a build that keeps the view upright and logs the raw accelerometer
# axes once a second, to measure `IMU_AXES` in `src/device/mod.rs`.
imu-calibrate port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features imu-calibration --monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

# Flash a build that hangs once to trip the watchdog, and record the log
# from boot: the next start must report the display reboot count it set.
watchdog-check log="target/watchdog.log" port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features watchdog-check --monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{log}}

# Flash a build that hangs at every start, then record the log with
# `monitor-passive`: the third watchdog reset in a row must stop into deep
# sleep (t-66); a BOOT press starts the loop again.
watchdog-loop log="target/watchdog-loop.log" port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features watchdog-loop --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}
    python3 -I tools/passive-monitor.py {{quote(log)}} {{ if port != "" { quote(port) } else { "" } }}

# Flash a build that counts deep sleeps in an always-on RTC register and
# logs the count and the raw reset state at each start (t-67), then record
# the log with `monitor-passive`: stop, wake, and read the start after it.
sleep-check log="target/sleep.log" port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features sleep-check --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}
    python3 -I tools/passive-monitor.py {{quote(log)}} {{ if port != "" { quote(port) } else { "" } }}

# Record the serial log without resetting the chip, across deep-sleep
# wakes (macOS, Ctrl-C to stop). Use it to see a wake: `monitor-log` resets
# the chip on connect, and the start then reports `ChipPowerOn` (t-67).
monitor-passive log="target/serial.log" port="":
    python3 -I tools/passive-monitor.py {{quote(log)}} {{ if port != "" { quote(port) } else { "" } }}

# Record the serial log to a file, e.g. for dumps triggered by BOOT
# presses. Connecting resets the chip.
monitor-log log="target/frame.log" port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{log}}

# Convert the last framebuffer dump of a serial log into a PNG.
frame-png log="target/frame.log" out="target/frame.png":
    RUSTUP_TOOLCHAIN=stable cargo run -q -p frame-png --target {{host_target}} -- {{log}} {{out}}

# Explicit destructive host provisioning; never called by boot/build/flash.
# For a raw device supply its exact 512-byte sector count. Unmount it first.
sd-format device sectors="":
    RUSTUP_TOOLCHAIN=stable cargo run -p theo-storage --example format_littlefs --target {{host_target}} -- --format {{quote(device)}} {{ if sectors != "" { "--sectors " + quote(sectors) } else { "" } }}

# Run the HAL-independent unit tests on the host.
test:
    RUSTUP_TOOLCHAIN=stable cargo test -p theo-drivers -p theo-frame-dump -p theo-storage -p frame-png --target {{host_target}} --features theo-storage/legacy-fat

# Run the driver tests under Miri, to catch undefined behaviour in their
# unsafe code (`rustup +nightly component add miri rust-src` once). Cargo
# runs from a private scratch directory: `.cargo/config.toml` (xtensa
# target, `build-std`) applies to the working directory and would break
# Miri.
miri:
    #!/usr/bin/env zsh
    set -eu
    nightly_rustc=$(rustup which rustc --toolchain nightly)
    nightly_bin=$(dirname "$nightly_rustc")
    scratch=$(mktemp -d)
    trap 'rm -rf "$scratch"' EXIT
    cd "$scratch"
    PATH="$nightly_bin:$PATH" cargo miri test --manifest-path "{{justfile_directory()}}/crates/theo-drivers/Cargo.toml" --target {{host_target}}

# Check dependencies for security advisories and license compatibility.
audit:
    RUSTUP_TOOLCHAIN=stable cargo deny check advisories licenses sources

# Run the same RustSec security gate as the release workflow.
security-audit:
    RUSTUP_TOOLCHAIN=stable cargo audit --file Cargo.lock

# Preview unreleased notes; release-plz owns the versioned CHANGELOG.md.
changelog:
    git-cliff --config cliff.toml --unreleased

fmt:
    cargo fmt --all

clean:
    cargo clean

# Explicitly provision the SD card inside the board, preserving an existing LittleFS.
sd-provision log="target/sd-provision.log" port="":
    THEO_SD_PROVISION=format ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features sd-provision --monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{quote(log)}}
