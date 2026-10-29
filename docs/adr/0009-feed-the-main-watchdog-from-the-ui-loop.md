# ADR-0009: Feed the main watchdog from the UI loop

- Status: Accepted
- Date: 2026-10-16
- Amended: 2026-10-16, why the start cause is logged late (t-67)
- Amended: 2026-10-29, bounded input task progress before feeding (t-83)
- Amended: 2026-10-17, watchdog reboots capped by a stop (t-66), I2C bus
  freed at every start (t-84), start cause logged after 500 ms (t-76)

## Context

The display reboots the chip when the panel stays stale (t-44,
`StaleWatch`), and every flush is bounded (`FLUSH_TIMEOUT`). Nothing
reboots it if the UI task or the executor itself stops: a deadlock or a
busy loop leaves a frozen pod (t-57). `esp_hal::init` disables every
watchdog.

The ESP32-S3 has two candidates. The main watchdog of timer group 0
(MWDT0) runs on the APB clock and resets the digital core
(`CoreMwdt0`, ROM `TG0WDT_SYS_RST`). The RTC watchdog (RWDT) runs on the
RTC slow clock and, as esp-hal arms it, resets the digital core and the
RTC module (`SysRtcWdt`). The display reboot count lives in RTC fast RAM
and must survive the reset, or the cap of three display reboots stops
holding.

## Decision

- Arm MWDT0 (the `wdt` of the `TimerGroup` already created for the
  scheduler) with a 5 s timeout (`WATCHDOG_TIMEOUT_MS`) as soon as it
  exists, early in `Board::init`, as `device::Watchdog`
  (`src/device/restart.rs`): the display bring-up waits on DMA with no
  timeout of its own. `Board::init` feeds it once at the end. esp-hal's
  `Wdt::enable` sets stage 0 to `ResetSystem`, which on the ESP32-S3 is
  the `CoreMwdt0` core reset (esp-hal's doc for that action mentions RTC
  peripherals; the board check below is what counts).
- The UI loop feeds it once per pass. A pass is a render (unbounded,
  short), a flush (500 ms at most), an idle wait (1 s at most) and, on a
  stop, a sleep-in (500 ms at most); `src/main.rs` asserts at compile time
  that the bounded parts fit in half the timeout.
- A frame dump (feature `frame-dump`) runs with the watchdog paused
  (`Watchdog::paused`): it takes seconds on the serial port.
- It keeps feeding once `StaleWatch` gives up: the loop still runs then,
  and the watchdog only catches a loop that stopped, so the display cap
  holds.
- The loop disarms it just before reporting the panel asleep for a stop:
  the button task then waits for the BOOT release for any time. MWDT0's
  clock stops in deep sleep, and the wake resets the chip anyway.
- The UI loop logs why the chip started, the reset reason or the
  deep-sleep wake cause (t-51), on the first pass at least 500 ms after
  start-up (`START_CAUSE_LOG_AFTER`, t-76), so up to a pass later, or
  first thing on a stop that comes before (the stop never returns). The
  USB serial port returns about 0.15 s after a wake (t-67), so a
  `just monitor-passive` already running sees the line; one opened by
  hand later does not. The delay was 3 s while the line was thought lost
  for a second after a wake: `espflash monitor` was resetting the chip on
  connect. Measured on a deep-sleep wake only: a cold plug-in, which the
  host enumerates from scratch, may still miss the line.
  `reset_reason` and `wakeup_cause` are free functions in esp-hal 1.2, and
  `Rtc` takes `RTC_TIMER`, so `DeepSleep` keeps `LPWR`.

## Consequences

- Positive: a hung UI task or executor reboots within 5 s, and the reset
  reason is logged at the next start.
- Positive: checked on the board with `just watchdog-check`: the count
  set before a hang is still there after the MWDT0 reset, which logs
  `PASS`.
- Changed: a panic once the watchdog is armed (esp-backtrace prints it,
  then spins) ends in a MWDT0 reset 5 s later, instead of a halt; the
  next start reports `CoreMwdt0`. Only a panic before `TimerGroup::new`
  still halts.
- Capped (t-66): a hang or panic that repeats after every boot no longer
  reboots every 5 s forever. `WATCHDOG_RESETS`, in RTC fast RAM like the
  display count, counts `CoreMwdt0` starts since the UI loop last ran
  healthy (`RebootCap`, host-tested in `theo-drivers`): other starts
  neither count nor clear it (a display reboot keeps it), and power-on or
  deep sleep read it as zero. `Board::init` counts right after arming the
  watchdog, so a hang anywhere in the bring-up is capped. At the third,
  it stops before the panel bring-up: it parks the IMU, the touch
  controller and the panel rails like a normal stop, minus the panel's
  sleep-in (QSPI is not up yet), then clears the count, disarms the
  watchdog and sleeps until BOOT (`DeepSleep::enter`, the sleep path
  every stop shares). The count stays at the cap while it parks: if
  parking hangs, the next watchdog start sleeps without parking, and the
  rails may then stay on through the sleep. Every start frees the I2C bus
  first (ADR-0005, t-84), so a device left mid-transfer by the reset does
  not keep the bus from the parking. The UI loop clears the count
  once it has run 60 s, so a hang that comes back later than that is not
  capped. Checked on the board with `just watchdog-loop` (hangs at every
  start): three resets, then deep sleep. A stop with BOOT still held
  wakes at once and starts a fresh round. Watchdog resets still do not
  count toward the display cap.
- Checked on the board (t-66): BOOT held through a MWDT0 reset does not
  re-sample the GPIO0 strapping pin; the chip boots the firmware
  (`SPI_FAST_FLASH_BOOT`), never download mode.
- Extended 2026-10-29 (t-83): the UI feeds MWDT0 only while all enabled
  input tasks have completed an operation within 3000 ms (inclusive stale
  boundary). Button checks the actual GPIO at least every 250 ms at idle;
  touch reports every completed read, including stationary/absent fingers
  and errors; orientation reports every completed sample attempt. Touch
  reset attempts report completion too. A pending I2C operation or shared
  mutex never gets health from an independent timer. An absent IMU is not
  enabled. The first stale input is logged once and latched until reboot;
  late progress cannot resume feeding or clear watchdog reboot counters.
  A long press suspends new checks before the bounded stop handshake;
  the existing watchdog disarm and BOOT release behavior stays unchanged.
  Known blocking frame dumps rebase the input timestamps after resume;
  dumps are skipped after a latched fault. Detection takes 3 s plus at
  most one UI pass; reset follows within 5 s of the last hardware feed,
  not necessarily within 5 s of the original input stall.
- Negative: nothing watches the stop once the watchdog is disarmed.
- Neutral: a loop stalled for 5 s reboots the chip, stop or not. The
  `PANEL_SLEEP_TIMEOUT` path of ADR-0008 (the loop never reports the panel
  asleep, so it never disarms the watchdog) therefore ends in deep sleep
  only if BOOT is released before the watchdog fires; otherwise in a
  reboot, which also recovers the pod.
- Neutral: MWDT0 shares the APB clock with the code it watches; a clock
  failure would stop both. The RWDT, set to reset the core only, is the
  alternative if that ever matters.

## Alternatives considered

- RWDT as esp-hal arms it: rejected, its system reset also resets the RTC
  module, which the reboot count relies on.
- Stop feeding once `StaleWatch` gives up: rejected, the watchdog would
  then reboot the chip past the display cap.
- Count watchdog resets toward the display cap: rejected (t-66), the two
  causes differ; watchdog reboots have their own cap and fall back to deep
  sleep.
- Stop at the cap after the bring-up, with the debounced button: rejected
  (t-66), a hang during the bring-up would never reach the check.
