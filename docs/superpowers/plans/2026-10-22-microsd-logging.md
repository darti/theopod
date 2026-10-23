# t-94 microSD logging Implementation Plan

Historical plan: the implementation uses SPI3 rather than SDMMC. Final
behavior and validation are recorded in ADR-0010 and `docs/sd-validation.md`;
task status is tracked in Oku t-94 through t-97.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Bring up the V2 microSD slot over SDMMC and provide bounded, recoverable FAT-backed persistent logging.

**Architecture:** Keep ESP-HAL pin/peripheral setup in `src/device/sd.rs`; keep buffering and log-sink policy in a host-testable `src/storage.rs`; connect them from `Board::init` without adding card I/O to the UI loop.

**Tech Stack:** Rust `no_std`, esp-hal 1.2, Embassy async, `hadris-fat` 3.0.0-rc.2, host unit tests.

**Spec:** `docs/superpowers/specs/2026-10-22-microsd-logging-design.md`

## Global Constraints

- Preserve the existing `no_std`/`no_main` firmware and ESP-HAL startup model.
- SD wiring is GPIO2 (clock), GPIO1 (command), and GPIO3 (data0), with
  TCA9554 EXIO7 as `SD_CS`.
- Use `hadris-fat` 3.0.0-rc.2 with `async` and `write` features.
- Missing cards and all runtime storage failures are recoverable.
- Bound storage work so it cannot violate the main watchdog budget.
- Regression tests belong in side-by-side `*_tests.rs` files with task tags.

## Review Focus

- No card at boot: initialization continues and the UI remains usable — test the service state/error path in the storage policy.
- Full buffer: oldest/newest record policy is explicit and bounded — test capacity behavior.
- Partial sink writes: unwritten bytes remain queued — test a sink that accepts fewer bytes.
- Sink failure: later flushes can retry without panic or data loss — test failure then recovery.
- Card removal/mount failure: filesystem setup returns a recoverable error — cover adapter/service initialization failure in the firmware check and policy tests.

### Task 1: Host-testable bounded log buffer

**Files:**
- Create: `src/storage.rs`
- Create: `src/storage_tests.rs`
- Modify: `src/main.rs` or module declarations as needed

**Interfaces:**
- Produces a fixed-capacity `LogBuffer` and a small `LogSink` trait whose flush operation can report partial progress or failure.

- [ ] Write failing regression tests for formatting, FIFO buffering, capacity, partial flush, and failure recovery, tagged `Regression (t-94)`.
- [ ] Run the focused host test and verify it fails because the types are absent.
- [ ] Implement the minimal fixed-capacity buffer/sink policy in `src/storage.rs`.
- [ ] Run the focused test and then the complete host suite.

### Task 2: SDMMC block-device adapter

**Files:**
- Create: `src/device/sd.rs`
- Modify: `src/device/mod.rs`
- Modify: `Cargo.toml` only if the resolved esp-hal feature/API requires it

**Interfaces:**
- Consumes `LogBuffer`/`LogSink` policy from Task 1.
- Produces an async hadris `BlockDevice` adapter over the ESP-HAL SDMMC slot and a recoverable initialization result.

- [ ] Add compile-oriented tests or pure helpers for the SD pin/card-select configuration where host compilation permits.
- [ ] Run the relevant host test and verify the new behavior is initially absent.
- [ ] Implement GPIO2 clock, GPIO1 command, GPIO3 data wiring, EXIO7 control,
  async slot conversion, and block read/write forwarding.
- [ ] Run `just check` and resolve target-specific API/type errors without changing the board wiring contract.

### Task 3: Mount and bounded firmware logging

**Files:**
- Modify: `src/device/mod.rs`
- Modify: `src/main.rs` or a new device task module
- Modify: `src/storage.rs` as required by the concrete filesystem API

**Interfaces:**
- Consumes the SD service from Task 2 and the bounded buffer from Task 1.
- Produces a startup mount attempt that returns a disabled/recoverable state on card or filesystem failure.

- [ ] Add a regression test for disabled storage on initialization/mount failure.
- [ ] Run it to verify it fails before the service is connected.
- [ ] Mount FAT with hadris, write bounded records through the sink, and ensure the UI path never awaits unbounded SD I/O.
- [ ] Run host tests and `just check`.

### Task 4: Documentation and full verification

**Files:**
- Modify: `crates/theo-drivers/ARCHITECTURE.md` only if a reusable abstraction is added there
- Modify: `README.md` if the user-visible storage behavior needs documenting
- Modify: `docs/adr/0010-microsd-over-sdmmc.md` with final implementation notes

- [ ] Run `just fmt`.
- [ ] Run `just test`, `just check`, and `just lint`.
- [ ] Review the diff for preservation of the pre-existing t-94 groundwork and confirm no hardware flashing/monitor command was run.

