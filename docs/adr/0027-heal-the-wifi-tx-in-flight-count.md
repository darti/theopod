# ADR-0027: Heal the Wi-Fi TX in-flight count and disable AMPDU RX

- Status: Accepted
- Date: 2026-11-15, updated 2026-11-16 (second cause: AMPDU RX)

## Context

The t-172 board test downloaded about 2-4 MiB of an episode, then the body
stalled or was reset. After that, every network operation failed, while the
station stayed associated (RSSI about -60 dBm, link and DHCP up, no
disconnect event). Only a reset recovered.

An instrumented esp-radio 1.0.0-beta.1 showed the cause. The driver sends a
frame only while its in-flight count is below `tx_queue_size` (2, chosen in
t-161). The count goes up per frame sent and down per tx-done callback. The
blob sometimes skipped the callback for one of our frames, and the count
drifted. At the limit, `can_send()` stayed false for good: no TX, and no
RX either, because the RX token comes paired with a TX token. Nothing
resets the count, not even dropping the controller. 1.0.0-beta.2, the
latest release, has the same code.

Raising `tx_queue_size` only delays the failure: the leaks accumulate over
uptime. An application watchdog that reboots on a dead data path would
recover, but would also cut every download in progress.

## Decision

- Vendor esp-radio 1.0.0-beta.1 in `vendor/esp-radio`, patched through
  `[patch.crates-io]`, as for LittleFS (`vendor/littlefs`). Provenance,
  the exact changes and the removal criteria are in
  `vendor/esp-radio/THEO-PATCH.md`.
- The in-flight count moves to a small, dependency-free gate
  (`src/wifi/tx_gate.rs`). When it has held frames for 200 ms without a
  tx-done callback, it resets the count and logs a warning. A callback
  normally arrives within milliseconds, failed frames included.
- The gate is unit-tested on the host by `just test` (`rustc --test`; the
  crate itself only builds for the ESP target).
- esp-radio's `log-04` feature is enabled so that the heal warning, and
  the driver's other warnings, reach the firmware log.
- `src/device/network.rs` logs connect and DHCP failures with the
  controller state (connected, RSSI, link, DHCP, free internal heap),
  watches for disconnects during a job, and adds that state to retry and
  stall logs.

## Consequences

- One leaked callback costs a 200 ms TX pause instead of the whole
  network until reset.
- If a frame is genuinely still in the blob after 200 ms, the driver may
  queue one more than `tx_queue_size`; the blob rejects it if its own
  buffers are full, which the driver already handles. 200 ms is a judgment,
  not a measured worst case: in the 16-minute board run below the heal
  fired 54 times while sent and done frames stayed within a few of each
  other, so many heals are slow callbacks rather than leaks. Each costs at
  most one extra queued frame; frequent heals are the signal to revisit it.
- A single leaked callback below the limit is not healed: with
  `tx_queue_size` 2, the pipe then runs one frame deep until a second leak
  holds the gate and the heal resets the count.
- The gate's wiring into the driver is verified only on the board.
- The firmware carries a fork of esp-radio. Upgrading esp-radio means
  vendoring the new version and porting the change, or removing the patch
  once upstream fixes the accounting.
- Known remaining leak source: `esp_wifi_send_data` returns early when the
  station is not connected, after the in-flight count was raised, without
  lowering it. The heal covers it; it is not patched.

## Second cause: AMPDU RX holds every RX buffer

With the heal in place, downloads still froze after about 30 s: no frame
was received any more (beacon timeout, then scans finding no access point
at all), and nothing recovered until reset. Instrumenting the blob's
allocations and OS queues showed the driver's RX queue and the blob's only
queue empty, the blob task alive, and live ~1.6 KB blob buffers at 48: the
16 static RX buffers plus all 32 dynamic ones, never freed. The blob's
AMPDU RX reorder path kept them, so the radio had no buffer left to
receive into.

Decision: disable AMPDU RX (`with_ampdu_rx_enable(false)` in
`src/device/network.rs`). With that single change the board downloaded two
full ~74 MiB episodes and started a third (16 min, no stall); dynamic RX
buffers returned to the 16 static ones after each burst and the internal
heap to about 76 KB. Throughput is about 187 KB/s, against about 195 KB/s
with AMPDU in t-161.

This setting is a controller argument in board code that only runs on the
ESP target, with the blob as the failing component: there is no host test
for it. The board run above is its verification.
