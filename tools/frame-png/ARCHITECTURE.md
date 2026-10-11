# frame-png

Host tool: `frame-png <log> <out.png>` decodes the last framebuffer dump of a
serial log (`theo-frame-dump` format) and writes it as an RGB PNG. Built for
the host only; run it through `just frame-png`.

## Contents

- `main.rs`: argument handling and `convert` (read log, parse, encode PNG).
- `main_tests.rs`: end-to-end conversion and error messages.

## Dependencies

- `theo-frame-dump` (parsing), `png` (encoding).
