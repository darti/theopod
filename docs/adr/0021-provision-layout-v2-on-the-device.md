# ADR-0021: Provision layout v2 on the device

- Status: Implemented; provisioning validated on the board. Amended by ADR-0022: layout v3, `provision_v3` writes an empty redb length record
- Date: 2026-11-11
- Related: t-133, t-121; ADR-0015, ADR-0018, ADR-0020

## Context

Podcasts need a card with the v2 layout (ADR-0020): descriptors, a catalogue
database, a 16 MiB log LittleFS and raw media units. Only the host
`prepare_image` tool produced it, as an image file to copy onto a card by
hand. A card that stays inside the board (the only practical case) could get
the legacy whole-card LittleFS from `just sd-provision`, never v2. Boot never
formats (ADR-0015), and that rule stays.

Provisioning a 59700 MiB card writes about 36 MB (database pages and log
volume). At about 2 ms per board sector write that takes minutes, far past the
2 s mount budget, so it cannot run as a normal storage request.

## Decision

- `theo_podcasts::provision::provision_v2(device, card_sectors, allocator)`
  is the one v2 provisioner, HAL-independent. It first zeroes sectors 0 and
  8, which also hold a whole-card LittleFS's two superblocks (4 KiB blocks),
  and syncs. It then formats the log volume (with an empty `/theo.log`),
  creates the catalogue in place, syncs, and only then writes both
  descriptors, syncing after each. The media region is not written. Region
  syncs are deferred to that one barrier: nothing can discover the regions
  before a descriptor.
- An interrupted run therefore leaves a **blank** card: no descriptor and no
  mountable LittleFS. Normal boot keeps it offline (ADR-0015, it never
  formats) and the recipe can be run again. A failure on the second
  descriptor copy leaves one valid copy, which is already a complete layout
  (`CardLayout::select` accepts one copy).
- `prepare_image` calls it on a new image file (still `O_EXCL`, never an
  existing path or device).
- The firmware calls it only in an explicit build:
  `just sd-provision-v2 <sectors> <card id>` sets
  `THEO_SD_PROVISION=v2:<sectors>:<card id>` with the `sd-provision`
  feature. The value is parsed at compile time (`provision::parse_request`):
  a missing or malformed value fails the build rather than booting as a
  no-op.
- The card id is the CID manufacturer ID and product serial number (ten hex
  digits), read once at SD init with CMD10 and logged at every boot ("microSD
  card id"). A CMD10 failure only leaves the id unknown.
- Before serving any request, the storage owner provisions **only if**
  `Target::authorizes` holds: exact sector count, same card id, and no
  descriptor at all (blank card or legacy LittleFS). A v2 layout, a v1 or
  corrupt descriptor, an unknown id or a failed descriptor read writes
  nothing. A v1 card is refused, not upgraded.
- This erases whatever the card held, including a whole-card LittleFS with
  logs. It is the one deliberate exception to "never carve out of an existing
  whole-card LittleFS": an explicit, separately built, card-bound request.
- In a v2 build, core0 skips the mount and keeps its storage token without
  dropping it (`sd::provisions_v2`): a core0 request would time out during
  the run and poison the shared sector bus (first board run: failed after
  1 s, `Io`).
- Each sector operation keeps its own `IO_BUDGET` (500 ms) deadline; no
  request deadline covers the whole run. The run happens on core1 and the
  storage stays offline for that boot. A PSRAM allocation failure before the
  run logs and leaves the card online and unchanged. The normal firmware,
  flashed after, mounts the new layout.
- The provisioning future runs from PSRAM through `run_in`, and the catalogue
  is created in place (`Catalogue::create_into`), to fit core1's 48 KiB stack
  (t-121). The firmware side lives in `src/device/sd_provision.rs`.

## Consequences

- A blank card or a legacy whole-card LittleFS can get podcasts without
  removing it from the board.
- The size and CID guard makes the build specific to one physical card: a
  swapped card of the same capacity is refused. A reflash of the same build
  on a provisioned card is a no-op.
- espflash's monitor resets the chip when it attaches; reattaching during the
  run interrupts it (the card is left blank, see above).
- Normal builds and `just sd-provision` (legacy `format`) are unchanged,
  except that normal boot also logs the card id.
- There is no on-device re-provisioning of a v2 or v1 card; that would need
  a separate, explicit decision.

## Alternatives considered

- Copy a `prepare_image` image with `dd`: needs the card in a computer and a
  full-size image; kept for host use.
- Provision automatically when no descriptor is found: would let boot erase
  user data, contrary to ADR-0015.
- Guard by sector count only: any same-size card would pass.

## Verification

Host: `provision` tests (the published layout reopens a catalogue and log
volume; a failure at any sampled write publishes nothing, except a failure on
the last descriptor copy, which leaves a complete layout; a sync failure
publishes nothing; syncs happen only at the barriers; an undersized card
writes nothing); `provision_guard` tests (parsing, size, id, descriptor
refusals); `create_into` and `prepare_image` tests. A malformed
`THEO_SD_PROVISION` fails `cargo check --features sd-provision`.

Board (2026-11-11, 59700 MiB card `c9000031f6`, previously a whole-card
LittleFS): the normal firmware logged the card id. The first v2 run failed
after 1 s (core0's mount request poisoned the bus, fixed above) and left the
card blank as designed. The second run finished in 242 s: "SD v2
provisioning done ... 59647 media units". The normal firmware then
discovered the layout and mounted the new log region (`theo.log` 0 bytes).
The catalogue does not open: "catalogue open failed: Storage(Disabled)".
On the host, opening a freshly provisioned 4096-page catalogue takes 73,872
sector reads (18 per page), far beyond the 2 s mount budget; podcasts stay
offline until that open path is fixed (follow-up to t-121/ADR-0020).
