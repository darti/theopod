# ADR-0011: Adapt EKV to FAT sector storage with copy-on-write pages

- Status: Accepted direction; storage transport implemented, firmware service integration pending.
- Date: 2026-10-29

## Context

Radio France subscriptions need a persistent local catalogue and durable read
states that survive audio reclamation. The firmware is no_std, the microSD is
FAT32, and all SD I/O is asynchronous because chip select uses the shared I2C
expander. A synchronous database adapter cannot block that executor safely.

The user explicitly authorized revising the SD layer to use EKV. EKV 1.0.0 is
an asynchronous no_std transactional key-value database with ordered cursors.
Its Flash contract forbids a failed logical write from changing bytes outside
that write. A direct byte-write adapter on a 512-byte FAT block device would
violate that contract: a sector read/modify/write can damage adjacent bytes.

Sources: [EKV flash contract](https://docs.rs/ekv/1.0.0/ekv/flash/trait.Flash.html),
[EKV configuration](https://docs.rs/ekv/1.0.0/ekv/config/index.html).

## Decision

- Pin EKV to 1.0.0 with CRC, 4096-byte pages, up to 4096 logical pages,
  and values up to 4096 bytes. Persist every format-affecting configuration
  parameter in an exact-match format header; refuse incompatible files.
- Keep EKV and sector/file adapters in theo-storage, independent of ESP HAL.
  Its transitive embassy-sync 0.6 remains private to EKV; the firmware keeps
  embassy-sync 0.8. No guards or channels cross those version boundaries.
- Back each logical page with two slots in a fixed-length preallocated file.
  Each slot contains one 512-byte header sector followed by the 4096-byte
  page payload. Headers carry page identity, generation and CRCs.
- To replace a page, invalidate the older slot and sync, write its complete
  new payload and sync, then publish its header and sync. Never overwrite the
  current valid slot. Only successful completion changes the active version.
- At reopen, lazily validate both slots and use the newest valid one. Two
  invalid copies are corruption, never an invitation to implicitly format.
- Create only in a newly allocated, unpublished file. Initialize every page
  before publishing the format marker. Existing files are opened without
  creation or truncation. Publishing the file and servicing requests belong
  to the forthcoming single-owner firmware storage service.
- Mark a page-store instance disabled before awaiting any operation. Restore
  it only after complete success; cancellation/error requires discarding the
  filesystem mount until reboot, preserving ADR-0010's existing policy.
- The FAT adapter exclusively borrows the existing mount, performs bounded
  whole-sector data operations, and never grows the file. Close it before
  handing the mount back to the owner. It creates no second mount or SD driver.

## Consequences and limits

The file occupies `512 + pages * 2 * (512 + 4096)` bytes: a little over 2.25
times logical capacity. The adapter holds one 4096-byte page buffer and a
bounded lazy version cache (one Option<Version> per configured page), allocated
on the heap. A small logical write copies a full page and uses three durability
barriers; actual SD latency and catalogue import throughput must be measured.

This is software recovery under the contract that failed sector writes leave
other sectors intact and sync orders durable writes. It is not a claim that
consumer SD media or FAT metadata survive arbitrary physical power loss. FAT
can still rewrite directory/dirty-marker metadata even for fixed-length file
writes; corruption of the allocation chain or directory can make both copies
unreachable. Hardware power-loss tests and a backup policy remain necessary.

No database is initialized at boot in this change. The existing logging task
and sleep behaviour remain active as before; integrating database requests
and long initialization with log flushing and bounded shutdown is the next
storage-service step, before enabling subscriptions or network downloads.

## Validation

Host tests exercise actual EKV on a real host file, real FAT32 formatting,
mount/unmount/reopen, unchanged file size and a neighbouring log file.
Fault injection covers sector tears, multi-key commits, slot reuse, corruption,
failed sync and cancellation. ESP compilation and release/link checks verify
target compatibility; hardware flashing is not authorized by this ADR.
