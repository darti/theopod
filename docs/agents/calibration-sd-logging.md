# IMU calibration and SD logging

Read before touching IMU axes or the SD logger.

- `IMU_AXES` is measured, not documented: `just imu-calibrate` (needs a TTY)
  keeps the view upright and logs raw accelerometer axes once a second.
  Calibrate from positions defined against gravity (vertical with text
  upright, a quarter turn clockwise, flat); don't infer axes from how a
  tilted screen "points".
- SD logging uses a bounded queue and a separate async task. Core1 drives
  SPI3 itself; only identification asks core0 to set chip select on the
  shared I2C expander (touch/IMU also use that bus), then the card stays
  selected (ADR-0023). A failed storage operation disables storage until
  reboot. Never block on the expander otherwise (ADR-0005).
- The SD storage owner runs on core1's 48 KiB stack (`sd_worker.rs`).
  Awaiting a large future there builds it on that stack and inlines its
  poll into the owner's frame: await big one-shot work through `in_psram`
  (`theo_storage::future::run_in`) and build large values in PSRAM
  (t-121). Each request arm of `Runtime` runs through
  `theo_storage::future::apart` (its own frame), and large state is
  reset in place, never rebuilt as a stack temporary (`SyncSession::restart`,
  `RadioFranceFeed::reset`): a 53 KB request frame overflowed core1 on the
  first podcast write (t-133). Core0's main stack is the internal RAM the
  linker leaves after `.bss`: keep large task futures out of it (the
  network task's 64 KB state runs in PSRAM through `run_in`; as a static
  it left 47 KB for a 99 KB TLS/storage chain, t-133).
- Run `just frame-check` after changing the SD owner, the podcast runtime,
  the network task or what they call (release network build with the
  placeholder credentials, then each task's deepest call chain against its
  stack; `tools/frame-check.py`). Rebuild before flashing: it leaves a
  placeholder-credential image in `target/`.
- Run `just test-catalogue-bounds` after changing the catalogue, the redb
  backend or sync (release-only sector bounds, under a minute; kept out of
  `just test`, which runs debug).
- `just sd-provision-v3` erases the card; never run it without an explicit
  request.
- The UI loop feeds the main watchdog once per pass (ADR-0009): a pass must
  stay well under 5 s. Bound any new wait in it (`with_timeout`), as the
  flush and sleep-in are; a compile-time assert in `src/main.rs` checks the
  idle wait and `FLUSH_TIMEOUT`.
