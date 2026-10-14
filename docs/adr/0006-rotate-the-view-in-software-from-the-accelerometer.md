# ADR-0006: Rotate the view in software from the accelerometer

- Status: Accepted
- Date: 2026-10-11

## Context

The screen should follow how the device is held, in all four orientations
(t-32). The board has a QMI8658 IMU on the shared I2C bus (ADR-0005). The
368 x 448 panel is not square, so a quarter turn also changes the layout
size, and touch points (CST820, panel pixels) must follow the same turn as
the picture.

Two places can rotate the picture: the CO5300 (memory access control,
row/column exchange and mirroring) or Slint's software renderer
(`SoftwareRenderer::set_rendering_rotation`).

## Decision

- Slint rotates while rendering into the PSRAM framebuffer, which keeps
  the panel's native layout. The CO5300 setup, `flush_rows` and the DMA
  path (ADR-0004) are unchanged: Slint reports the dirty region in
  framebuffer coordinates. A rotation change redraws one full frame:
  switching the repaint buffer type back and forth empties Slint's
  partial-rendering cache, so the next render covers the whole window
  (checked on the host).
- One `Rotation` value drives the renderer, the window size
  (`Rotation::view_size`) and touch input (`Rotation::panel_to_view`). The
  mapping lives in `theo-drivers`; its tests invert a transcription of
  Slint 1.18's rotation transform for every pixel of the four rotations,
  checked against Slint on the host and on the board. The touch driver
  keeps reporting panel pixels.
- A rotation signalled while a finger is down waits until it lifts, and
  touches already queued are dispatched in the rotation they were made in:
  a press and its release always use the same mapping.
- `OrientationDetector` turns gravity into a rotation: it ignores samples
  in free fall or lying flat (tilt under 30°), needs one axis to exceed
  the other by 3:2 (about 56°, a dead band around the diagonals), and
  three samples in a row (300 ms at 10 Hz).
- The accelerometer axes are mapped to the panel by `IMU_AXES` in
  `src/device/mod.rs`. Waveshare does not document it: it is measured on
  the board with the `imu-calibration` feature (`just imu-calibrate`: view
  kept upright, raw axes logged once a second), from positions defined
  against gravity. A compile-time assert (`AxisMap::is_rotation`, t-42)
  rejects a map that repeats an axis or mirrors space.
- The IMU is optional: if no QMI8658 answers at 0x6B or 0x6A, the view
  stays upright. Its identity is checked before any write, so probing an
  address held by another chip leaves it untouched.

## Consequences

- Positive: no change to the panel driver or the DMA alignment rules
  (ADR-0004); every rotation reuses the same flush path.
- Positive: the detector and the touch mapping are host-tested.
- Negative: a rotation costs one full redraw and full-frame flush.
- Negative: in landscape, a wide but short change (a label, a progress
  bar) spans many panel rows, and `flush_rows` sends whole rows: partial
  updates cost more than in portrait. Column windows in `flush_rows`, or
  the CO5300 alternative below, would fix it if animations stutter.
- Negative: the UI must lay out at both 368 x 448 and 448 x 368.
- Negative: the accelerometer runs at 62.5 Hz and is polled at 10 Hz for
  as long as the firmware runs; power management will want its low-power
  mode or its motion interrupt.
- Neutral: the view only turns once a finger lifts.
- Neutral: booting flat or in landscape shows portrait first, then turns
  about 300 ms later.

## Alternatives considered

- CO5300 row/column exchange: rejected for now. It changes the drawing
  window and the 2x2 alignment rules the driver enforces, and touch would
  still need the same mapping in software.
- Two orientations only (portrait and landscape): rejected, the user asked
  for all four.
