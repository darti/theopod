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
    RUSTUP_TOOLCHAIN=stable cargo clippy -p theo-drivers -p theo-frame-dump -p frame-png --target {{host_target}} --all-targets -- -D warnings

build:
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo build --release

flash port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

monitor port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

run port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

# Like `run`, but flash, hard-reset, then monitor as separate espflash calls.
flash-monitor port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash reset --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

# Flash a build that dumps the framebuffer, recording the log from boot
# (Ctrl-C once `theo-frame-end` is printed).
flash-dump log="target/frame.log" port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features frame-dump --monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{log}}

# Flash a build that keeps the view upright and logs the raw accelerometer
# axes once a second, to measure `IMU_AXES` in `src/device/mod.rs`.
imu-calibrate port="":
    ESP_LOG=info LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --features imu-calibration --monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

# Record the serial log to a file, e.g. for dumps triggered by BOOT presses.
monitor-log log="target/frame.log" port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash monitor --non-interactive --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }} | tee {{log}}

# Convert the last framebuffer dump of a serial log into a PNG.
frame-png log="target/frame.log" out="target/frame.png":
    RUSTUP_TOOLCHAIN=stable cargo run -q -p frame-png --target {{host_target}} -- {{log}} {{out}}

# Run the HAL-independent unit tests on the host.
test:
    RUSTUP_TOOLCHAIN=stable cargo test -p theo-drivers -p theo-frame-dump -p frame-png --target {{host_target}}

fmt:
    cargo fmt --all

clean:
    cargo clean
