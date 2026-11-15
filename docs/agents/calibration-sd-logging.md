# IMU calibration and SD logging

Read before touching IMU axes or the SD logger.

- `IMU_AXES` is measured, not documented: `just imu-calibrate` (needs a TTY)
  keeps the view upright and logs raw accelerometer axes once a second.
  Calibrate from positions defined against gravity (vertical with text
  upright, a quarter turn clockwise, flat); don't infer axes from how a
  tilted screen "points".
- SD logging uses a bounded queue and a separate async task. Never block on
  the shared I2C expander to select the card (ADR-0005). A cancelled or
  failed storage operation disables that mount until reboot.
- The UI loop feeds the main watchdog once per pass (ADR-0009): a pass must
  stay well under 5 s. Bound any new wait in it (`with_timeout`), as the
  flush and sleep-in are; a compile-time assert in `src/main.rs` checks the
  idle wait and `FLUSH_TIMEOUT`.
