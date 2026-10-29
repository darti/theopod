# Radio France implementation feasibility

Date: 2026-10-29. This records measured progress, not completion of the podcast feature.

## Source contract

Inspected Aerion commit `12037d21d9465885efa9eeab951045898ca178af`:

- [`worker-script.js`](https://github.com/Aerion/rss-radio-france-pour-tous/blob/12037d21d9465885efa9eeab951045898ca178af/worker-script.js)
  exposes `/rss/{showId}?page={zeroBasedPage}` and a 302 `/audio/{manifestationId}` redirect.
- [`src/api.js`](https://github.com/Aerion/rss-radio-france-pour-tous/blob/12037d21d9465885efa9eeab951045898ca178af/src/api.js)
  also accepts `page=-1` to assemble the full history server-side. Prefer page-by-page
  retrieval on the device, to keep responses and retries manageable.
- [`src/feed.js`](https://github.com/Aerion/rss-radio-france-pour-tous/blob/12037d21d9465885efa9eeab951045898ca178af/src/feed.js)
  emits `atom:link rel="next"`; enclosures omit length. GUIDs use diffusion IDs,
  except historical entries on/before 2022-09-12, which use manifestation IDs.
  A synchronization must not assume GUID and audio identity are interchangeable.
- The worker code sets Content-Type but no explicit ETag/Last-Modified.
  Conditional retrieval is an optimization only if the deployed server supplies validators.

Live search/feed requests failed in this execution environment: workers.dev
returned 502 then 530; rss-rf.aerion.me returned 530. This does not establish a
permanent service outage. A representative live multi-page RSS capture and
final CDN response/range behaviour remain unverified. No synthetic fixture is
presented as a downloaded live feed.

## Database experiments

EKV 1.0.0 and redb 4.4.0 (experimental no_std API) both compiled for
`xtensa-esp32s3-none-elf` in a disposable Cargo project, through a temporary
just recipe using the project's ESP environment.

Redb passed file insert/reopen, update/reopen, deletion and aborted-transaction
checks on the host with a 64 KiB configured cache and a 61,440-byte resulting
file. Its StorageBackend is synchronous and does not directly fit the current
async SD owner; it was not added to the firmware.

EKV is asynchronous, supports ordered scans and multi-key transactions, and
does not require std or a complete catalogue in RAM. Its flash contract rules
out directly implementing sub-sector writes over FAT read/modify/write.
Following explicit user authorization to revise the SD layer, the selected
solution is the double-slot PageStore and FatPageFile in theo-storage, recorded
in ADR-0011. Its host tests cover actual EKV and actual FAT32 separately,
file reopen, sector interruption across every write of a multi-key commit,
cancellation, failed sync, slot reuse and format/corruption rejection.

The configured page count is a file-creation choice, from 32 to 4096. Each
logical page is 4096 bytes; the physical file size is
`512 + pages * 2 * 4608`. At 4096 pages this is 37,749,248 bytes for 16 MiB
logical space. Values are capped at 4096 bytes; keys retain EKV's 64-byte limit.
The page buffer is 4096 heap bytes; the version cache has one Option<Version>
per page. No whole-database RAM image is required.

This layer is not connected to firmware boot yet. Creation/publication,
single-owner multiplexing with logs, per-operation timeouts and sleep handling
must be implemented before enabling database initialization or downloads.
Neither tests nor compilation establish physical SD power-loss safety or
throughput; FAT metadata and the card controller remain outside the simulated
failure model. No board was flashed or monitored.

## Network compatibility experiment

The registry reports esp-radio 1.0.0-beta.2 as yanked; beta.1 is the available
candidate with esp-hal ~1.2, esp-alloc 0.11 and esp-radio-rtos-driver 0.4.2.
Do not derive version selection from the documentation's latest link alone.

A disposable combination of esp-radio 1.0.0-beta.1, esp-rtos 0.4,
embassy-net 0.9.1 and reqwless 0.14 resolved. Xtensa compilation then failed
inside embedded-tls 0.18.0's optional Rust PKI code because its der 0.8
prerelease requirement resolved to a stable version with incompatible APIs
(`SequenceOf` import and `ValueOrd` derive errors).

The candidate's PKI code also compares the hostname to CommonName directly;
support for the deployed certificates' SAN/wildcard identities cannot be
assumed. No certificate-verification bypass was added. A verified TLS stack,
trust/time bootstrap, live fixture capture, transport limits and retry schedule
remain open. No network dependency from this experiment was added to the project.

## Next work

Complete the storage-owner integration for the selected EKV adapter, including
bounded initialization and interruption-safe publication of a new database
file. Resolve HTTPS verification and capture a real complete feed before
considering the initial feasibility task complete. Then implement the catalogue,
reconciliation and download/reclamation tasks in the approved plan.
