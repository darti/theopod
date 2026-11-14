# ADR-0023: Drive the microSD from the storage core

- Status: Accepted
- Date: 2026-11-14
- Related: t-155, t-153; amends ADR-0012 and ADR-0018 (SD HAL ownership)

## Context

Since ADR-0012 the synchronous storage owner (LittleFS, the redb catalogue,
`UnitMedia`) runs on core1, while core0 kept the async SPI3 device: SD chip
select is TCA9554 EXIO7, on the async I2C bus that touch and the IMU share,
and those async HAL drivers cannot leave core0. Every sector therefore went
to a core0 `serve` task through `SectorBus`; a 4 KB audio chunk took nine
requests (eight staged sectors and one CMD25 flush, t-153).

Measured on the board during podcast downloads (t-155), per 4 KB chunk, the
store took 28.4 ms. Chip-select toggles were about 12 I2C writes of ~670 us
(~8.6 ms); the cross-core requests about 2.7 ms (core0 picked each one up in
~250 us on average, up to 95 ms behind the UI or TLS). Keeping the card
selected after identification (it is the only device on SPI3) brought the
store to 22.4 ms and the download from 78 to 91 KB/s.

Running the owner on core0 instead is not an option: its work is blocking
(a cold redb commit is ~500 ms) and would stall the UI loop and its
watchdog (ADR-0009).

## Decision

- Core1 identifies the card itself, with SPI3 in blocking mode and a
  busy-wait delay, then keeps the device in the owner's `CardIo`. Each card
  operation is polled with `block_on` and completes on its first poll; its
  waits are bounded loops in the SPI-mode protocol and in `sdio`.
- During identification only, core1 asks core0 to set chip select through a
  one-slot signal; core0's `init` serves those requests on the shared I2C
  bus until core1 reports the result. Afterwards the card stays selected:
  commands end with the idle byte only, and core1 never waits for core0.
- `SectorBus`, the core0 `serve` task and its staging buffer are removed.
  Multi-block writes stage whole sectors in a PSRAM buffer on core1.
- Failure policy is unchanged: a card error, or an operation finishing after
  its request's deadline, disables storage until reboot; a passed soft
  deadline fails only the current scope. Deadlines are checked between
  operations, since a running operation cannot be cancelled.

## Consequences

- Board, podcast download: the store per 4 KB chunk dropped from 22.4 ms
  (card held, sectors still sent to core0) to about 11 ms (2.7-2.9 s per
  MiB); 28.4 ms before t-155. A temporary read-back of 6,144 multi-block
  sectors found no difference. The download is now bounded by the network
  read, which is not overlapped with storing (5.5 s or more per MiB).

- Sector I/O no longer depends on core0's scheduling, and core0 loses the
  `serve` task and the mailbox statics (internal RAM for its main stack).
- Core1 busy-waits while the card programs; it has no other work.
- A card that needs chip select released between commands would fail at
  the first command after identification; the measured card does not.
- Re-identifying the card needs core0 again: deselecting refuses once the
  card is held, so a mid-session re-acquire fails and disables storage.
- Identification ends by selecting the card while core0 still serves chip
  select; a held card never asks core0 again. (The first board build
  released it after the last identification command, then waited for a
  core0 that had stopped serving: every mount failed.)
