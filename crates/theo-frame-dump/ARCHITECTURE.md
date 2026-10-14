# theo-frame-dump

Text format for dumping a framebuffer through a serial log, so a host can
see exactly what the firmware rendered. `no_std`; the host-side parser sits
behind the `parse` feature, the only part that needs `alloc`. Host-tested
with `just test`.

## Contents

- `lib.rs`: format markers and the item list. One dump is a
  `theo-frame-begin <w> <h>` line, one `theo-frame-row <y> <hex>` line per
  row, and `theo-frame-end`; markers may follow any log prefix.
- `write.rs`: firmware side. `write_frame` writes a big-endian RGB565 frame
  to any `core::fmt::Write` (the firmware uses `esp_println::Printer`)
  without allocating.
- `parse.rs` (feature `parse`): host side. `parse_frame` returns the last
  complete, valid dump of a log as a `Frame`; `Frame::to_rgb8` expands it
  for image encoders.
  Header sizes are capped so a corrupted log cannot exhaust memory.

## Extension points

- None; change the format in `write.rs` and `parse.rs` together and keep
  the round-trip tests passing.

## Dependencies

- No sibling crates. Used by the firmware (`frame-dump` feature, through
  `src/device/mod.rs`, without `parse`) and by `tools/frame-png` (with
  `parse`).

## Tests

`write_tests.rs` and `parse_tests.rs`, including round trips, corrupted and
truncated logs, and oversized or empty headers.
