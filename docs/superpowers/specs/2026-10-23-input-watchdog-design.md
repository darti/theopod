# t-83: Watch input task progress with MWDT0

Status: approved by user on 2026-10-29 ("fait tout"); implemented and software-verified; see docs/input-watchdog-validation.md.

## Intent

The UI loop currently feeds MWDT0 every pass even if the button, touch or
orientation task has stopped progressing. Extend that check to the input
tasks while preserving normal idle input, recoverable device errors, and
the existing long-press stop and reboot cap (ADR-0008 and ADR-0009).

The user approved this design, including its three-second threshold, on
2026-10-29. The sections below retain the implementation requirements.

## Current behavior and constraints

- `src/input.rs` owns all three tasks and the parking handshake. The IMU
  task is only spawned when an IMU was initialized.
- `LongPressButton::next()` and `Cst820::changed()` may never return while
  idle. The touch driver already probes once a second and polls a held
  finger every 10 ms, including when its position does not change.
- The orientation loop reads every 100 ms. Sensor errors are recoverable;
  the touch reset retry delay grows to one second.
- The button queue never blocks the button task. The touch queue applies
  backpressure, and a running UI loop drains it every pass.
- MWDT0 is armed early in bring-up with a five-second timeout. The UI
  disarms it before reporting the panel asleep. BOOT release after that
  may take any time.
- Serial frame dumps pause MWDT0 and can block the executor for seconds.
- Keep board details in `src/device/`, drivers HAL independent, no heap
  allocation or new dependencies for monitoring. Keep existing driver
  APIs and their cancellation behavior compatible.

## Approaches

1. **Count returned input events.** Small change, but a stationary finger
   or an untouched button would cause false resets. Rejected.
2. **Run a periodic heartbeat beside each whole input future.** Avoids
   idle resets but also reports healthy while that future is stuck on an
   I2C transfer or mutex. Rejected for sensor tasks.
3. **Report progress at actual bounded input steps.** Recommended. Expose
   optional progress hooks in the driver loops, then check timestamps
   from the UI. This takes more driver tests, but distinguishes normal
   inactivity from a task that never completes its next operation.

## Recommended design

### Pure progress policy

Add a small HAL-independent, host-tested policy in `theo-drivers` for
three named input tasks. Each entry has an enabled state and a last
progress timestamp. Keep synchronization and Embassy time in the firmware;
the policy itself takes millisecond timestamps from its caller.

Enable button and touch before spawning them, with the current time as
their initial checkpoint. Enable orientation only if it is spawned. The
agent proposes a three-second stale threshold: above the one-second touch
probe and reset retry delays, with margin for the UI's bounded waits.
Check the exact boundary consistently and test it.

Once a required task becomes stale, latch that failure until reboot. The
UI logs its name once and stops feeding MWDT0, even if late progress
subsequently arrives. Continue processing the UI and stop requests while
waiting for the hardware reset. Never clear the existing watchdog reboot
count after an input failure has been latched.

### Driver checkpoints

Keep existing public methods as compatibility wrappers. Add opt-in
progress variants that take a synchronous callback, without depending on
Embassy, logging, or board code.

- **Button:** bound only the idle GPIO level wait with a proposed 250 ms
  delay, then sample the actual pin. Report progress after a completed
  sample, including when the pin is unchanged. If the level changed,
  finish its existing debounce before proceeding. Do not periodically
  cancel/recreate the whole debounce or long-press future. Forward this
  hook through `LongPressButton`; retain the absolute hold deadline and
  the existing release-versus-deadline ordering. Existing callers do not
  need a periodic idle wake unless they opt in.
- **Touch:** report progress after each register read, even if it reports
  no finger or an unmoved finger. A returned read error also proves the
  task is running. Never report progress from an independent timer while
  a bus read or mutex acquisition is still pending.
- **Orientation:** report progress after each completed acceleration
  attempt, success or error, before the calibration branch can continue.
- **Touch recovery:** report progress after each completed reset attempt,
  including failures, before its bounded retry delay. A hung reset
  operation must stop progress.

Do not drop queued touch events or restart controller operations to make
heartbeats. The existing full-queue wait remains covered: a healthy UI
drains it well before the proposed threshold. Test and document that
timing relationship in the firmware.

### UI, stop, and diagnostics

Before each normal feed, check enabled input timestamps under a short
critical section. Preserve unconditional bring-up feeds before input tasks
exist. Preserve the display retry cap: a failed display does not itself
make input tasks unhealthy.

Once a normal long press enters the bounded stop handshake, suspend input
progress checks for that stop. Tasks may then park and never report again;
the UI still disarms MWDT0 at the existing point. If the UI never completes
the stop, retain the existing MWDT0 behavior. Do not introduce any new
unbounded waits before disarming it.

After a known frame-dump pause, rebase the enabled task timestamps to the
resume time, giving them one normal grace interval to run again. Do not
clear an already latched fault. Confine this exception to the diagnostic
pause; regular stalls never receive a grace reset.

The detection delay is separate from MWDT0's five-second timeout: failure
is observed after the stale threshold plus at most one UI pass, and the
reset occurs within five seconds of the final feed. Do not claim detection
and reset always happen within five seconds of the input stall.

## Validation

Use side-by-side `*_tests.rs` regression tests tagged `Regression (t-83)`:

- Enabled tasks, absent IMU, initial grace, exact stale boundary, each task
  independently stuck, late recovery after a latched failure, stop state,
  and diagnostic rebase without clearing a latched failure.
- Button idle across multiple checkpoints; press/release debounce across
  checkpoint times; long hold still stops at its absolute deadline; wake
  press synchronization; ready release wins the deadline tie.
- Touch idle and unmoved-finger reads advance progress without inventing
  events; returned errors advance progress; a pending bus future does not.
- Recovery retries remain live on returned errors; a pending operation
  does not acquire health from a parallel timer.

Run `just fmt`, `just check`, `just test`, `just lint`, and `just build`.
Update ADR-0009 and the driver map for the implemented interfaces. No
dependency changes or new unsafe code are planned.

Hardware fault injection, current measurement (t-59), and the pending
counter/stop board validation (t-93 under t-85) remain separate work.
Flashing and serial monitoring require an explicit request under AGENTS.md.
Do not mark board behavior verified using host tests or a target build.

## Left to implementation

Exact type and method names, storage representation, and test fixtures are
routine implementation choices. Before implementation, mirror the agreed
work into Oku subtasks and write a plan. Complete t-83 only with a precise
record of checks run and any remaining hardware validation limits.
