# ADR-0008: Stop with a BOOT long press into deep sleep

- Status: Accepted
- Date: 2026-10-15
- Amended: 2026-10-16, host-tested long-press race (t-52), bounded wait
  for the panel's sleep-in (t-54), start cause logged (t-51), watchdog
  disarmed before the stop (t-57)

## Context

The pod should be started and stopped from the BOOT button (t-49), which
already acts as the Return key for the UI. The board has no other button
the firmware can read: the AXP2101 power key is not wired to the ESP32-S3
in this firmware. BOOT is GPIO0, an RTC pad of the ESP32-S3, so it can
wake the chip from deep sleep. It is also a strapping pin, but strapping
pins are only sampled at power-on and similar resets, not when the chip
leaves deep sleep.

## Decision

- Holding BOOT for 2 s (`LONG_PRESS_MS`) stops the pod; shorter presses
  still reach Slint as Return. `LongPress` (`theo-drivers`) keeps the
  time stamps: every press is forwarded at once, and its release only if
  the press did not become a long press. A stop never clicks as long as
  UI actions on Return fire on release, as `ui/main.slint` does: an action
  on press would fire on every stop.
- The button task owns the stop: it races `DebouncedButton::changed`
  against the long-press deadline (`LongPressButton`, host-tested since
  t-52: cancelling `changed` keeps the debounced state, and the deadline
  is absolute, so restarting the wait does not restart the hold), asks the
  UI loop to put the CO5300 to sleep (display off, sleep-in), then hands
  the button to `DeepSleep::until_pressed` (`src/device/`). It waits for
  the release, since sleeping with the button down would wake at once,
  arms GPIO0 as a low-level wakeup on its low-power path and enters deep
  sleep. If GPIO0 cannot be armed, the chip restarts rather than halt with
  the panel dark.
- Putting the panel to sleep is best effort: a failure is logged and the
  chip sleeps anyway. A panel that ignores sleep-in would ignore a retry,
  and keeping the chip awake would not darken it. Like a flush, it gives
  up after `FLUSH_TIMEOUT`, so a hung bus cannot keep the pod awake. The
  button task waits for it `PANEL_SLEEP_TIMEOUT` (2 s) at most, sending
  the stop request included, then stops anyway (t-54): a stalled UI loop
  must not keep the pod awake either. The cost is a panel that skips its
  sleep-in: it goes dark only when the touch task cuts its rails, and a
  loop that resumes late may send sleep-in to an unpowered panel, which
  fails and is logged. If the timeout cancels the stop request itself, a
  loop that resumes renders into the unpowered panel; its failed flushes
  can only reboot the chip if BOOT is still held long enough: the main
  watchdog (ADR-0009) fires 5 s into a stall, the display one (t-44)
  after 10 s. Button changes never wait for the UI loop: they are dropped
  while its queue is full, so a stalled loop cannot hide a long press
  either. A stop also clears the display reboot count (t-44): deep sleep
  powers down the RTC fast RAM that keeps it.
- The sleep API (`LowPower`, `WakeupConfig`) is esp-hal `unstable`
  (ADR-0003): recheck it when leaving the `~1.2` pin.
- The wake resets the chip, so the firmware starts again from
  `Board::init`: "start" is a normal boot. `DebouncedButton::sync_state`
  takes the level at start-up as the button state, so a press still held
  then only reports its release, which `LongPress` drops: the press that
  woke the pod neither clicks nor stops it again. The level, not the wake
  cause, decides (t-51): a press held across any reset is just as stale,
  and one released before the start needs no special case.

## Consequences

- Stopping keeps no state: the UI starts afresh after each wake.
- Each start logs its cause (t-51). On this board, the start after a BOOT
  wake reports `ChipPowerOn` with no wakeup source, not `CoreDeepSleep`:
  the chip may not reach deep sleep, or loses power on the way out (t-67).
- The UI loop disarms the main watchdog (ADR-0009) before it reports the
  panel asleep, so the wait for the BOOT release cannot trip it.
- After the panel's sleep-in, the button task has the orientation task,
  then the touch task, park their devices (t-50): the QMI8658's sensors
  and oscillator stop, then the touch task holds the CST820 in reset
  (EXIO2 low) and cuts the panel rails (EXIO0 reset and EXIO1
  `DSI_PWR_EN` low), holding the shared expander's lock across both
  (ADR-0005). The rail cut comes last, as an unpowered device may clamp the bus. The
  expander stays powered in deep sleep, so its pins hold until the next
  boot powers the panel up again; `Qmi8658::init` starts the oscillator
  before its soft reset, as the sensor stays powered down across the
  sleep. Only the GPIO0 wake path stays active.
- Each task drops its loop to park, possibly mid-transfer. esp-hal 1.2
  clears the bus when a transfer is dropped (`I2cFuture`'s `Drop` resets
  the controller and clocks the bus free), whichever device held SDA;
  recheck this when leaving the `~1.2` pin. A task that parks in time no
  longer uses the bus, so the sleep cuts no transfer of its own.
- Parking is best effort, like the sleep-in: a failure is logged, and a
  task that does not answer within `PARK_TIMEOUT` (200 ms) is logged and
  slept over. Such a task may still be mid-transfer when the chip
  sleeps, as before t-50.
- The stop-then-wake behaviour was checked on the board (t-55); the
  sleep current is not measured yet (t-59).
- Plugging the board in still boots it directly; BOOT only matters once
  the firmware has stopped.
