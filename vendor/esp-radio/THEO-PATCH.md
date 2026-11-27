# Temporary esp-radio correction (t-172)

Decision record: ADR-0027 (task t-172, 2026-11-15).

- Upstream: crates.io `esp-radio` 1.0.0-beta.1, from esp-hal commit
  `f769fcd27056b0837febe4209f3bae78ed764561` (`esp-radio/`).
- Upstream `.crate` sha256 (crates.io index `cksum`):
  `50247f4e91000fc87661e77254126c74acc4535f822ee247f7778807202dc938`.
- The copy is byte for byte the published crate, except for the files
  listed below. `.cargo_vcs_info.json` and `.cargo-ok` are dropped. The
  crate is licensed MIT OR Apache-2.0, like this repository, whose
  `LICENSE-*` files apply.
- As a path dependency, the copy makes the root `Cargo.lock` list every
  chip's optional `esp-wifi-sys-*` crate. Only the ESP32-S3 one is built.

## Defect

The Wi-Fi driver lets a frame go to the blob only while its in-flight
count (`WIFI_TX_INFLIGHT`) is below `tx_queue_size`. Each frame increments
the count, and each tx-done callback decrements it. On the board, the blob
sometimes skipped the callback for one of our frames. Some callbacks also
come for frames the driver did not send (the handshake). The count drifted,
and at the limit `can_send()` stayed false for good. That stopped TX, and
RX too, because `rx_token()` also needs a TX token. Association, RSSI, link
and DHCP state stayed up, while DNS and TCP failed until a chip reset.
Nothing reset the count, not even dropping the controller.

Instrumented run, `tx_queue_size` 2: during a download the count read 0
or 1. At the `ConnectionReset` (about 4 MiB in) it read 2. From then on,
sent and done frames stayed equal (3939 each) and no frame moved. 1.0.0-beta.2
has the same code.

## Change

- `src/wifi/tx_gate.rs` (new) holds the count. When it has held frames for
  200 ms (`HEAL_MS`) without a done callback, it assumes the count leaked
  and resets it to zero. A frame genuinely still in the blob then costs at
  most one extra queued frame, which `esp_wifi_internal_tx` rejects when
  its buffers are full (the driver already handles that error). Any send
  below the limit clears a hold start that a racing callback left behind.
- `src/wifi/tx_gate.rs` also holds `HealLog` (t-179), which rate-limits
  the heal warning: the first heal is logged at once, later ones are
  counted and logged with the first heal after `REPORT_MS` (60 s), as
  "N heal(s) since the last warning". Racing callers are safe (CAS on the
  window start). Heals after the last warning are only reported by the next
  heal. A download once logged 54 warnings in 16 min.
- `src/wifi/tx_gate_tests.rs` (new): host tests, run by `just test`
  through `rustc --test` (the crate itself does not build on the host).
- `src/wifi/mod.rs`: `WIFI_TX_INFLIGHT` is replaced by the gate, and
  `can_send()` asks it and logs a rate-limited warning when it heals.

## Drift check

`just vendor-check` (`tools/check-vendor-esp-radio.sh`, CI job "Vendor
check", t-178) downloads the upstream version above, checks its sha256,
and fails if any file differs other than the paths that start a bullet of
the "Change" section, this note and the dropped `.cargo_vcs_info.json`. It
also fails if the root `Cargo.toml` no longer patches esp-radio to this
directory. Start a "Change" bullet with each new patched file, or the
check fails. On pull requests the allowed paths come from this note on
the base branch, so a new patched file needs its bullet merged first. It
checks which files differ, not their content: review
edits to the listed files, and to this note, like any other code.

## Removal

Remove the `[patch.crates-io]` entry and this directory once upstream
esp-radio no longer lets a missing tx-done callback block TX for good. Then
re-run the board download test from t-172. Do not update this copy in
place to a newer upstream version: vendor that version afresh and port the
change.
