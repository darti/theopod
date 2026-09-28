set shell := ["zsh", "-cu"]

chip := "esp32s3"

esp_toolchain := `dirname "$(rustup which rustc --toolchain esp)"`
libclang_path := `find "$(dirname "$(rustup which rustc --toolchain esp)")/../xtensa-esp32-elf-clang" -type d -path '*/esp-clang/lib' -print -quit`
gcc_path := `find "$(dirname "$(rustup which rustc --toolchain esp)")/../xtensa-esp-elf" -type d -path '*/xtensa-esp-elf/bin' -print -quit`

default:
    @just --list

install:
    RUSTUP_TOOLCHAIN=stable cargo install espup --locked
    espup install
    cargo install cargo-espflash --locked

check:
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo check

build:
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo build --release

flash port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

monitor port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

run port="":
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTUP_TOOLCHAIN=esp cargo espflash flash --release --monitor --chip {{chip}} {{ if port != "" { "--port " + port } else { "" } }}

fmt:
    cargo fmt --all

clean:
    cargo clean
