# Input watchdog and remaining backlog validation

Date: 2026-10-29. Approved scope: t-83, t-93/t-85, t-59.

## t-83 software

UI checks input task progress before feeding MWDT0. Enabled inputs become
stale at 3000 ms without a completed operation. GPIO idle sampling probes
every 250 ms; touch reads, reset attempts and orientation sample attempts
checkpoint on completion, including returned errors. Pending bus operations
do not report progress. Faults latch until reboot and prevent counter
clearing. Stop suspends new checks; frame dumps rebase timestamps without
clearing faults.

Verification: `just fmt`, `just check`, `just test` (344 tests: 308 driver,
22 frame-dump, 11 storage, 3 frame-png), `just lint` (all recipe firmware
features plus host crates), and `just build` passed with `RUSTC_WRAPPER=`.
No dependency or unsafe changes. Fresh independent source review reported
no material findings.

Tests cover initial grace, exact stale boundary, each input stuck, absent
IMU, latch persistence after late progress, stop and diagnostic rebase,
idle GPIO samples/errors, uninterrupted debounce, release/deadline ordering,
idle/stationary touch reads, read errors and pending I2C without progress.

## t-93 / t-85 board

Explicit authorization obtained after automatic review requested a more
specific hardware permission.

- `target/t93-watchdog.log`: display reboot count set to 1; MWDT0 reset;
  `watchdog check: PASS, MWDT0 reset kept display reboot count 1`.
- `target/t93-watchdog-loop.log`: watchdog counts 1 then 2 (continuing the
  prior check), then `3 watchdog resets in a row: stopping`, followed by
  `entering deep sleep until BOOT is pressed` and USB disappearance.
  No parking error was logged. This verifies the early stop cap with the
  shared parking path; it does not measure rail voltage or supply current.
- `target/t93-sleep.log`: three automatic stops through the normal input
  stop handshake, pre-sleep SD flush, timer wakes with RTC sleep counts
  1, 2, 3 and raw reset cause 0x05. No parking or input watchdog error.
- Firmware without diagnostic features restored with `just flash`.
  `target/t93-normal.log` is a passive recording of the normal firmware.
  More than 60 seconds of idle operation produced no input fault or reset;
  the UI logged both watchdog and display reboot counters cleared after
  their healthy intervals. Manual BOOT hold logs `BOOT held for 2000 ms,
  stopping`, then deep sleep after release; the next brief press wakes with
  `WakeupReason(EnumSet(Ext1))`. No parking/stop timeout or input fault is
  present. User confirmed the screen switched off/on and touch/rotation
  both work. Subsequent rotation changes and SD syncs are recorded.
  The board remains running normal firmware; passive monitor stopped.

No input-task fault injection on hardware has been claimed; hung-operation
and latch behavior are covered by host regression tests.

## t-59 current measurement

Blocked: user confirmed on 2026-10-29 that no measurement instrument is
available. Serial logs and RTC counters cannot measure supply current.
Compare panel sleep-in only (188478a) against parked peripherals (e57da75
or current equivalent): same supply/measurement point, stable deep sleep,
external USB power/backfeeding accounted for. Record actual values and
conditions in ADR-0008 when an instrument is available. No estimated
numbers substitute for physical measurements.

## Backlog synchronization

Created t-98 under t-83 to track implementation. User explicitly authorized
local Oku updates after automatic review; t-59 now records the lack of an
instrument. t-83/t-98 completed with software verification and explicit hardware
limits. t-93/t-85 acceptance criteria now passed. t-59 remains open.
