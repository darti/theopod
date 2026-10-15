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
- Negative: the accelerometer is polled at 10 Hz for as long as the
  firmware runs. It runs in its low-power mode at 21 Hz (t-41; first
  62.5 Hz in normal mode), which still gives each poll a fresh sample;
  readings on the board kept their scale, with about ±50 LSB of noise
  (0.003 g). Waking on the QMI8658's motion interrupt instead would need
  its INT pins, which Waveshare neither documents nor uses (only
  `TP_INT` 21 in `pin_config.h`).
- Neutral: the view only turns once a finger lifts.
- Neutral: booting flat or in landscape shows portrait first, then turns
  about 300 ms later.

## Alternatives considered

- CO5300 row/column exchange: rejected for now (and tried for t-40, see
  below). It changes the drawing
  window and the 2x2 alignment rules the driver enforces, and touch would
  still need the same mapping in software.
- Two orientations only (portrait and landscape): rejected, the user asked
  for all four.

## Update (t-40): hardware rotation tried and dropped

In landscape a wide but short change spans many panel rows, and
`flush_rows` sends whole rows. A landscape update of the button (240 x 72
view pixels) costs about 176 KB, 9 ms on the bus: within a 16 ms frame,
and the UI has no animation. t-40 tried the real fix anyway: lay the
framebuffer out as the 448 x 368 view and let the CO5300 turn it with its
row/column exchange (MADCTL MV, plus MY), so a wide change covers few
framebuffer rows (two landscape rows are 1,792 bytes, 28 x 64, so the
DMA alignment holds). Espressif's `esp_lcd_co5300` 2.1.0 uses MV for
`swap_xy`, so the controller has the bit.

On the board, with a test pattern (green corner square, red vertical and
blue horizontal bands), the panel drew lines of 368 pixels along the
view's 448-pixel rows, whichever address window (CASET or RASET) carried
the long axis: rows sheared by about 80 pixels each (448 - 368), a strip
about 80 pixels wide stayed unwritten, and the vertical axis came out
mirrored. In row/column exchange the controller caps a written line at
the panel's physical width, so a landscape row cannot be one line.
Sending the frame column-major instead would bring back exactly the row
cost t-40 meant to remove. Rotation stays in software for all four
orientations; the experiment's patch is not kept.
