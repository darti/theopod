# LittleFS SD migration

The user selected the public pure Rust `littlefs-rust` wrapper from git main
and requested implementation. Replace runtime FAT mounting/logging, preserving
no_std, UI watchdog progress, shared async I2C ownership, bounded logging and
sleep flush. Normal boot never formats a card. Initial provisioning is an
explicit host utility using exactly the firmware geometry.

## Approach

Use the otherwise unused second CPU core for synchronous filesystem work.
It owns the LittleFS mount and a sector proxy. Core 0 keeps the non-Send async
SD HAL and shared I2C/expander handles. The proxy exchanges owned sector
messages with an async service on core 0, leaving that executor free to release
shared bus mutexes. Cross-core
commands and replies own their buffers and use CriticalSectionRawMutex. No HAL
objects or LittleFS references cross cores. The two channels each have capacity
one; a unique sector client prevents concurrent RPCs. No unsafe Send implementations.

An inline block_on on core 0 is rejected because it deadlocks shared async
I2C. Porting LittleFS to async is a separate upstream project. A dedicated core
costs a bounded internal stack and occupies core 1, but avoids either change.
The user delegated implementation choices by requesting execution; this
architecture is the implementer's choice.

## Contract

- Whole-card LittleFS at byte zero, 4096-byte blocks, 512-byte read/program,
  4096-byte cache, 128-byte lookahead; discard incomplete trailing block.
- Adapter checks geometry/bounds before I/O, writes whole SD sectors, erases
  all eight sectors to FF, and syncs the underlying device. SD writes must
  wait until card busy clears; this is not proof of physical durability.
- One request at a time. Each has an absolute deadline. Timeout, cancellation
  or failure poisons the storage until reboot; callbacks check poison and
  deadline before touching media. No subsequent commands reuse that mount.
- Boot identification 3 s, mount 2 s, previous tail 500 ms, append 500 ms;
  shutdown still waits at most 1.2 s. UI never waits synchronously on storage.
- Preserve log rotation at 128 KiB and bounded pending bytes on failure.
- Host format tool requires explicit destructive flag and existing sized
  file/device; opening the wrong filesystem at boot performs no writes.
- Keep main dependency and lock revision; no implicit fork override. Known
  upstream aliasing bugs remain until PR #12 merges, recorded in ADR-0012.
- EKV/PageStore remain available, but no random-access database file is added
  to LittleFS. Podcast/network work remains outside this filesystem migration.

## Validation

Host tests use the actual safe wrapper over a sector image, reopen written
logs, rotate, read tails, reject invalid ranges, and inject sector/sync failures.
Cancellation/deadline state must be host-testable. Firmware check/lint/release
build verify core startup and Send bounds. Run full host suite and dependency
audit. No hardware flashing, card formatting or power-loss claim in this work.
