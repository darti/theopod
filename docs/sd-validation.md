# t-94 microSD validation

> Historical FAT implementation evidence. Since ADR-0012, the firmware uses
> LittleFS; these hardware results do not validate the new filesystem, core 1
> worker or provisioning format. See the LittleFS follow-up below for the
> new implementation; host/build verification is recorded in ADR-0012.

Board: Waveshare ESP32-S3-Touch-AMOLED-1.8 V2, ESP32-S3 revision 0.2.
Inserted card: FAT32 in an MBR type 0x0b partition, start LBA 32768,
122,232,832 partition sectors; physical capacity 122,265,600 sectors
(59,700 MiB). SPI3: SCK=GPIO2, MOSI=GPIO1, MISO=GPIO3, CS=TCA9554 EXIO7.

Tests use `just` recipes. Diagnostic `sd-check` builds are not production
firmware. Full/read-only/timeout tests inject failures at the block-write
boundary; they do not fill or modify the card's write protection.

| Scenario | Evidence | Result |
| --- | --- | --- |
| Real runtime logs and reset | `target/sd-live.log`, `target/sd-full.log`: rotation events read from the previous `/theo.log`, 1008 bytes | Passed |
| Full media, injected `NoSpace` | `target/sd-full.log`: disabled once, 1877 pending bytes retained, firmware running | Passed |
| Read-only media, injected `ReadOnly` | `target/sd-readonly.log`: disabled once, 1877 pending bytes retained | Passed |
| Hung storage, injected pending write | `target/sd-timeout.log`: timeout, 1877 pending bytes retained; UI task continues | Passed |
| File rotation, isolated fixtures | `target/sd-rotate-fixed.log`: active 4096 bytes, archive 131072 bytes, `Ok(())` | Passed |
| Physical removal during use | `target/sd-rotate-fixed.log`: real removal causes an I/O error, retains 108 bytes, then orientation continues through Deg0/90/180/270 | Passed |
| Boot physically without card | `target/sd-no-card.log`: initialization timeout is reported, IMU and orientation start normally | Passed |
| Deep sleep and repeated wake | `target/sd-sleep.log`: three timer wakes, RTC counter 1/2/3, reset cause 0x05; prior log sizes 2993/4967/6963/8959 bytes with pre-sleep records | Passed |
| Watchdog reboot | `target/sd-watchdog.log`: MWDT0 reset `PASS`, existing 18516-byte log reread, then 1749 new bytes synced | Passed |
| Normal BOOT stop/wake, touch and rotation | `target/sd-boot-wake.log`: pre-sleep sync, wake `Ext1`, FAT mounted, 1547 new bytes synced; user confirms touch and rotation work | Passed |

Host tests cover transactional formatting, capacity/newline edge cases,
overflow policy, partial writes, failed-write retry without loss/duplication,
ESP_LOG module filters and rotation boundaries. Firmware-target compilation
checks the concrete filesystem and async SPI/I2C path.

Verification (2026-10-23): `just fmt`, `just check`, `just test` (330 tests,
including 11 storage regressions), `just lint` (all firmware features plus
host crates), `just audit` and release `just build` pass.

Rotation exposed a multi-sector adapter bug: the original stream rejected
requests larger than 512 bytes. The corrected stream accepts aligned
multi-sector buffers and makes one-sector progress per poll. A host guard
also verifies sector addressing above the 4 GiB byte boundary. The passing
hardware run retains isolated `sdtest2.log`/`sdtest2.1` fixtures (4096/131072
bytes); the earlier failed run may leave `sdtest.log`. Production logs are
not used as rotation fixtures.

The device is left running normal firmware (no diagnostic feature). On its
final boot, `target/sd-final.log` rereads 20399 bytes including the previous
MWDT0/reset-health records, then syncs 1533 new log bytes.

Production behavior: `/theo.log`, 128 KiB limit, one `/theo.1.log` archive;
4096 queued bytes, 1024-byte maximum record, one-second periodic sync,
500 ms I/O timeout, 1.2 s final sleep-flush handshake. Startup initialization
and mount have separate 3 s and 2 s bounds. Failure disables SD for the rest
of the boot; reinsertion is picked up at the next boot. USB logs continue.

## t-103 LittleFS follow-up — 2026-10-29

The user explicitly authorized board flash and SD formatting. The onboard card
was identified as 122,265,600 sectors (59,700 MiB). No Mac external disk was
formatted. Normal firmware is restored after the provisioning image.

| Check | Evidence | Result |
| --- | --- | --- |
| Initial runtime bring-up | `target/littlefs-before-format.log`: panic from an Embassy Timer registered with block_on's non-Embassy waker | Fixed: blocking sector wait now polls its deadline clock directly; cancellation still poisons the session |
| Explicit onboard provisioning | `target/littlefs-provision-board.log`: `formatted LittleFS`, volume mounted, initial log absent, 1458 bytes synced | Passed |
| Normal firmware restored | `target/littlefs-normal-board.log`: LittleFS mounted without provisioning message | Passed |
| Persistence after reset | Normal boot read 1458 bytes of previous log, then synced 1458 new bytes and another 80 bytes | Passed |
| UI/watchdog progress | `UI loop healthy: watchdog reboot count cleared`, PSRAM self-test passed, IMU samples and rotation logged | Passed; panel appearance and touch response not independently observed |

365 host tests and firmware check/lint/release build pass after the runtime fix.
Provisioning tests preserve existing files and forbid formatting after I/O
errors. A separate deadline regression covers both in-flight and already
expired blocking requests without using an Embassy timer. Independent review
found no blocking findings. `just audit` passes with the working tree's
pulldown-cmark 0.13.5 update from the concurrent release work.

Further on-board tests are recorded below. The historical FAT matrix does
not substitute for physical tests: a power cut and a card removal during
writes were performed on 2026-11-27, and a truly full or write-protected
card was judged not feasible physically (see "Physical tests on current
main — 2026-11-27").

### Automated hardware validation requested by the user

All runs use the real onboard card. Error cases are explicit injections in
the firmware; they do not physically remove, fill or write-protect the card.

| Scenario | Capture | Observed result |
| --- | --- | --- |
| Real log rotation on isolated files | `target/littlefs-rotation.log` | Active file 4096 bytes, archive 131072 bytes; check returned Ok; live log retained |
| Simulated full media | `target/littlefs-full.log` | Storage disabled until reboot; all 1596 pending bytes retained; no panic/reset observed |
| Simulated write protection | `target/littlefs-readonly.log` | Same failure policy; 1596 bytes retained; no panic/reset observed |
| Simulated removal on write | `target/littlefs-removed.log` | Same failure policy; 1596 bytes retained; no panic/reset observed |
| Blocked write | `target/littlefs-timeout.log` | Frontend TimeoutError at the 500 ms budget; worker returned in 500127 us; 1682 bytes retained |
| Identification without response | `target/littlefs-absent.log` | Three-second identification timeout; firmware continued with IMU/UI tasks and no mounted storage |
| Three real deep-sleep cycles | `target/littlefs-sleep.log` | Three Timer wakes with reset cause 0x05 and sleep counts 1/2/3; mount and log sync succeeded after each wake |
| Restore normal firmware | `target/littlefs-validated-normal.log` | Normal mount, previous log size 11687 bytes, 1354 new bytes synced; diagnostic/provisioning modes disabled |

Across the measured sleep/fault runs, worker mount service time was
19.553–24.963 ms. Successful append service times ranged from 108.792 to
473.655 ms, within the 500 ms request budget for these runs. These worker
measurements exclude queue wait and the serial measurement print before its
reply; they are not universal SD latency bounds. The 473.655 ms sample leaves
little margin for slower card activity, which can safely disable logging.

The lowest sampled core-1 stack margin was 22019 of 32768 bytes, measured
inside synchronous storage callbacks. The diagnostic observes a local stack
address and never reads running stack memory. It is a sample of these call
paths, not a canary scan or a proof covering every possible path.

The passive serial reader preserved genuine deep-sleep resets. Previous log
sizes progressed 4802 → 6543 → 8306 → 10069 bytes; the restored firmware then
read 11687 bytes. Input task watchdogs did not report stalled touch/orientation
progress, and real IMU samples were logged; no physical touch gesture or panel
appearance check is claimed. All four injected write failures recovered by
remounting existing logs on subsequent normal/sleep boots.

366 host tests, firmware check/lint (including sd-check and sd-provision) and
release compilation pass. A read-only independent review found no blocking
issues in the sampling or identification injection. Upstream PR #12 remains
open with no GitHub status checks as of this validation; t-104 stays pending.

A separate host regression, `full_littlefs_volume_keeps_failed_append_pending`,
actually fills a 128 KiB LittleFS volume through committed 4 KiB log appends.
The genuine allocator NoSpace result preserves both the failed pending batch
and the previously committed log bytes. This complements the on-board injected
full-media error without filling the physical 59,700 MiB card.

Actual SD supply power cuts, physical card removal, and actual physical full/
write-protected media are outside what this USB connection can actuate. The
host torn-sector tests and injected on-board failures remain distinct evidence.

### Follow-up on current firmware — 2026-11-01 (t-103)

Baseline source: `3befa3d`. The same onboard card reports 122,265,600
sectors. No provisioning or formatting was performed.

The normal release build failed in LLVM `XtensaSizeReduce` during ThinLTO,
both with the default size optimization and with optimization level 2.
The local `sccache` also stalled during Cargo's compiler-version query;
`RUSTC_WRAPPER=` bypassed that separate host-side problem.

A normal-firmware build with `CARGO_PROFILE_RELEASE_LTO=false` compiled and
was flashed. It mounted the existing LittleFS and read a 14,215-byte log,
but panicked at `src/device/logging.rs:36` (`RefCell already borrowed`)
during the first-frame heap report. Watchdog resets followed. This build
does not establish a healthy baseline. Capture:
`target/t-103-2026-11-01-baseline.log`.

The existing `sd-check` build with `THEO_SD_FAULT=observe` compiled with the
normal release settings. `observe` matches no fault-injection branch.
After a physical reset it mounted in 24,656 us and read the same previous
log in 45,802 us. Its first append failed at 500,067 us and disabled storage
until reboot. The minimum sampled stack margin was 10,483 of 49,152 bytes;
this remains a callback sample, not a stack high-water measurement.
Capture: `target/t-103-2026-11-01-physical.log`. The logger panic was not
observed in this diagnostic boot. No causal explanation for the difference
between the two builds has been established.

Physical removal after this timeout cannot validate interruption of an
active write: storage was already disabled. Physical power-loss resilience
remains unvalidated, and t-103 remains open.

The user then physically removed the card and confirmed that panel rotation
and touch still responded. The serial capture records orientation changes.
After the user restarted without the card, initialization reported an SD
card timeout and continued without storage. Startup reached the first IMU
sample and subsequent orientation changes; no panic was observed in this
captured boot. The passive capture does not timestamp individual log lines,
so it does not independently measure the initialization timeout duration.

After reinsertion and restart, the volume mounted in 24,653 us and its
14,215-byte log was readable again (tail request: 45,826 us). The first
append again failed at 500,071 us; recovery of mounting/reading passed,
but recovery of log persistence did not.

Diagnostic-only counters were added to distinguish time spent waiting for
sector RPCs from other worker work, without printing inside callbacks.
In `target/t-103-2026-11-01-io.log`, another boot reports:

| Request | Worker us | Read/write/sync attempts | Sector RPC total us | Longest RPC us | Result |
| --- | ---: | --- | ---: | ---: | --- |
| Mount | 24500 | 12 / 0 / 0 | 22739 | 2624 | Passed |
| Tail | 45276 | 23 / 0 / 0 | 42877 | 2351 | Passed |
| Append | 500108 | 181 / 22 / 1 | 474451 | 53538 | Failed; storage disabled |

Counts include an unsuccessful final attempt. RPC durations include waiting
for core 0 and the card; they are not pure SPI/card timings. These results
show cumulative request cost reaching the budget, rather than one sector
RPC occupying the full 500 ms. They do not establish which layer should be
changed or justify increasing the watchdog-sensitive frontend timeout.
No physical power cut during a healthy append has been validated.

Fresh `just fmt`, `just check`, `just test`, `just lint` (all configured
features and host crates), and default release `just build` pass with
`RUSTC_WRAPPER=`. The earlier compiler crashes did not recur in the final
default release build; no compiler fix or explanation is claimed.
A read-only independent review found no correctness issues in the counters.

Normal firmware was restored with default release settings, without
`sd-check`, provisioning or the no-LTO override. Its final capture,
`target/t-103-2026-11-01-restored-normal.log`, confirms mount, the same
14,215-byte previous log, first IMU sample, and then storage disabling with
`Ok(Err(Io))`. No logger panic was observed in this final boot. Thus the
device is left on normal firmware with SD logging disabled for this boot;
successful new persistence is not claimed. The repeatable first-append
failure must be resolved before testing interruption of healthy writes.

### First-append correction — 2026-11-01 (t-103)

The first allocation deferred a free-block scan until the foreground append.
The safe LittleFS `gc()` operation performs this initial allocation preparation
and consistency/metadata maintenance. `mount_for_writes` now runs it before
returning the mounted filesystem; the board's explicit provisioning branch
does the same. All of this remains inside the existing two-second mount
deadline. Foreground requests still have a 500 ms deadline; shutdown still has
its 1.2 s budget. No volume was formatted for this correction.

A host fixture recreates a 14,215-byte journal built by repeated appends,
a 128 KiB log archive and isolated 128 KiB/4 KiB rotation fixtures. Without
preparation, its first 4 KiB append required 102 sector reads; preparation
reduced that to 36 (29 sector writes in either case). The regression limited
that append to 64 reads: before the read cache below, it failed with I/O
error without the fix, then passed
and verified exact log/archive contents after remount. This budget belongs to
the fixture, not all possible filesystems. Another regression fails sector
reads after raw mount succeeds, checks that preparation returns I/O failure
without changing the image, then successfully reopens the existing file.

| Board capture | Mount service | Successful append service times |
| --- | --- | --- |
| `target/t-103-2026-11-01-prepared.log` | 89.083 ms, 48 reads, no writes | 496.759 / 134.953 / 142.298 ms |
| `target/t-103-2026-11-01-prepared-reboot1.log` | Early mount lines not captured | 240.752 / 111.852 / 113.502 ms |
| `target/t-103-2026-11-01-prepared-reboot2.log` | Early mount lines not captured | 178.657 / 163.208 / 404.543 ms |

The first corrected append still had little margin below 500 ms; these are
successful samples, not worst-case guarantees. Later allocation scans and
metadata work can still increase request cost. The safety deadline and
disable-until-reboot behavior remain in force. The board diagnostics above
show no append failure; the separate `podcasts network stopped: Storage`
message also occurs because this legacy whole-card volume has no podcast
database region, and does not by itself mean the SD logger failed.

Final normal firmware restoration (without diagnostic/provisioning features)
is captured in `target/t-103-2026-11-01-fixed-normal.log`: the previous log
had grown to 20,667 bytes. A second normal boot, captured in
`target/t-103-2026-11-01-fixed-normal-reopen.log`, read 22,576 bytes,
confirming another 1,909 bytes persisted from the normal firmware. No
storage-disable message or logger panic was observed in these final boots.
The second tail begins inside a UTF-8 sequence and is displayed as
`<UTF-8 boundary>` by the existing tail printer; the log-size read succeeded.
This supersedes the pre-fix final-device-state paragraph above: the device
is now left running corrected normal firmware with logging operational.

`just fmt`, `just check`, `just test` (466 passed; six existing extended
tests ignored), `just lint` and default release `just build` pass with
`RUSTC_WRAPPER=`. Independent review found no blocking issue. No dependency
or unsafe-driver change was made. The t-103 physical power-cut/active-write
removal/full/protected-media scope and merge requirement remain open.

### Physical removal after the correction — confirmed 2026-11-02 (t-103)

The user removed the onboard card while the device remained USB-powered.
`target/t-103-physical-removal-power.log` records six successful appends
(229.365 / 130.336 / 123.074 / 136.271 / 135.603 / 157.000 ms), followed
by an append failing in 1.768 ms with I/O error. That request attempted one
sector read and no write; storage disabled until reboot. Two subsequent
orientation changes were logged, with no panic or reset in this capture.

This is physical removal during a functioning logging workload. It does not
establish removal during a card's internal sector-programming phase, since
the failing request stopped at its first read. The user confirmed that touch
still responded after removal. After USB power-off, reinsertion and reconnect,
the volume mounted in 121.914 ms and its 27,388-byte journal remained readable.
However, the first append again exceeded 500 ms (188 read attempts, 24 writes,
one sync attempt), disabling storage. Thus startup preparation alone was
insufficient; the earlier successful samples did not establish a complete fix.

### Read-amplification correction — 2026-11-02 (t-103)

`target/t-103-sector-amplification.log` records a further failed first append:
500.112 ms, 169 read RPCs but only 23 distinct sectors, one read 20 times.
This isolates repeated metadata reads as an additional cost, rather than one
hung sector. Each physical read also incurs the board's I2C chip-select work.

The sector adapter now reserves a 32-sector read-through LRU cache. Board
allocation is fallible and explicitly uses PSRAM (16,896 bytes measured in
the heap report). All affected entries are invalidated before writes/erases;
write data never fills entries, preserving real-media readback validation.
Failed I/O still prevents all subsequent reads, including cache hits. No
request, shutdown or watchdog budget was increased, and no card was formatted.

A real LittleFS regression formerly failed at cold-mount/append cycle 2 under
a 64-sector-read budget. It now passes 64 fresh-adapter mount/append cycles,
including rotation, and verifies exact log/archive data after cold remounts.
Additional tests check physical readback after an acknowledged-but-missing
write, erase invalidation, and poisoned cached reads after partial erase or
failed sync. The adapter readback test passes both Miri aliasing models.

`target/t-103-sector-cache.log` verifies the same 27,388-byte journal on board:
mount 57.815 ms (30 reads), tail 9.435 ms (four reads), and successful appends
174.976 / 175.686 / 47.066 ms. The first append required 13 physical reads.
These are samples, not universal bounds for all cards and volume states.
An independent review found no blocking coherence or cancellation issue.

### Review follow-up — 2026-11-11 (t-103)

A multi-reviewer pass found that, with the read cache in place, the first
append needs only 50 reads without preparation, so the 64-read regression
no longer detected a missing `gc()`. The regression now compares a prepared
and an unprepared first append on identical images (18 vs 50 reads on the
host) and limits the prepared one to 32. Mutation checks confirm that each
of these fails its test: no preparation, single-sector write or erase
invalidation, no deadline check on cache hits, no up-front cache reservation,
and no LRU refresh.

Cache hits now call `SectorIo::check`, so they fail once the request deadline
has passed or the session is disabled. The board figures above predate this
check; it performs no I/O, but they have not been re-measured with it.

A host measurement quantifies the open limit recorded in ADR-0019: preparing
a volume with 4 MiB of media costs 516 extra sector reads, about one per two
data blocks. All board evidence above is from a log-only volume; a card
holding downloaded episodes has not been measured.

### Physical tests on current main — 2026-11-27 (t-103)

Source: `main` at `b3b6a42`, plus the `sd-check stress` diagnostic added by
this change. Same onboard card (122,265,600 sectors, card id `c9000031f6`),
now a region card (layout v3, podcast catalogue opens), whereas all
evidence above was gathered on the legacy whole-card volume. No
provisioning, `sd-provision-v3` or formatting was performed. The user
performed every physical action; the board was powered by USB only (no
battery).

#### First append after the 2026-11-02 regression

`target/t-103-2026-11-27-observe.log` (`just sd-check observe`, boot after a
flash reset): mount 114.454 ms (158 physical reads), tail 6.298 ms, first
append **26.593 ms** (7 physical reads, 15 writes), then 30.124 / 30.949 ms.
The 500 ms first append (188 reads) of 2026-11-02 is not reproduced. That
figure came from the legacy whole-card volume, so this shows no regression
on current main, not an isolated fix of the earlier cause. The power-cut and removal recoveries below supply the post-recovery case: their
first appends took 49.505 ms and 45.687 ms.

These figures include `SectorIo::check` on cache hits; for the board
re-measurement requested in the 2026-11-11 review follow-up they show only
that aggregate timings did not regress. Its cost is not separable: the counters see physical reads only, not cache
hits. Its failure path (a hit refused after the deadline or after
poisoning) was not triggered on the board; the host mutation test remains
the evidence for it.

#### Stress workload

`just sd-check stress` appends isolated 2 KiB batches to the `sdtest2`
rotation files (bounded to 4 KiB + 128 KiB by rotation) whenever the
storage task is idle, 1 ms apart. Live logs and podcast requests keep
priority. The card is therefore writing most of the time (about 35 ms of
every 36 ms), so a physical action lands on an active write with high
probability, without proof that it interrupts a particular sector program.

A first version looped inside the storage task and starved podcast
requests: one waited past its 2.5 s timeout, dropping its guard poisoned
the session and the next append failed in the middle of its writes
(its capture was overwritten by the next run). That failure came from the
diagnostic, not from the card. The corrected version, which this file
contains, then ran 2,634 appends with no failure (median 34.5 ms, p99 53.2 ms, max
74.7 ms) alongside 1,776 podcast requests
(`target/t-103-2026-11-27-stress-flash.log`).

Unlike `rotate`, `stress` does not refuse existing fixtures: it appends to
them, so `just sd-check rotate` refuses to run until `sdtest2.log` and
`sdtest2.1` are removed. Captures under `target/` are local and not
committed; the figures here are quoted from them.

#### Physical tests

Capture: `target/t-103-2026-11-27-powercut.log` (`tools/passive-monitor.py`,
no reset on open; its first lines are cut mid-record because it attached
to a running board).

| Physical action | Observed result | Limits |
| --- | --- | --- |
| Abrupt USB unplug (power cut) during the stress workload, 14:18:48 | 367,864 appends committed since 10:39 without error (about 718 MiB written through rotation); the port vanished after the commit message of append 367,864 and before any measurement for 367,865. Replugged 14:18:55: mount 141.508 ms (193 reads), previous `theo.log` 48,202 bytes readable, catalogue reopened, first append 49.505 ms, then 440 appends with no error (median 34.1 ms, max 54.5 ms) | One cut. Whether it fell inside a sector program, between the commit print and the next request, or in the 1 ms gap cannot be told from the log. The `sdtest2` content was not reread byte for byte; LittleFS kept appending and rotating them, so atomicity of the interrupted record was not verified. Supply cut by unplugging USB, not a controlled SD-rail cut |
| Card removal with power on, during the stress workload, then USB off, reinsertion, USB on | Append 927 failed in the middle of its writes (14 write attempts, 0 reads) with `Io`; storage disabled until reboot; `podcasts network stopped: Storage`; five later rotation changes were logged (Deg90/Deg180), no panic or reset; the user confirmed touch and rotation. Reboot 14:20:04: mount 134.666 ms, previous `theo.log` 51,766 bytes readable, first append 45.687 ms, then 436 appends with no error (max 59.0 ms) | One removal. Unlike 2026-11-02 (failure on the first read), this one failed during writes; whether the card was programming at the moment of contact loss is not observable |

#### Not feasible physically

- **Truly full card:** the 59,700 MiB LittleFS volume cannot be filled by
  the firmware in a reasonable time (logs are bounded to 128 KiB plus one
  archive), nor from a Mac without destroying the volume; a smaller card
  would have to be formatted, which was excluded.
- **Write-protected card:** microSD has no lock tab; an adapter's tab is
  read by the host, and this SPI slot has no such contact. Real protection
  needs the card's CSD `TMP_WRITE_PROTECT` bit, which modifies the card.

For these two, the evidence remains fault injection only: on-board
`full`/`readonly` injections (2026-10-29 table above) and the host
`full_littlefs_volume_keeps_failed_append_pending` regression on a really
full 128 KiB LittleFS volume.

#### Fault injection vs physical evidence

| Scenario | Physical | Injection / host |
| --- | --- | --- |
| Power loss during writes | One USB cut during the active-write workload; sector-level timing and record atomicity unverified | Host torn-sector tests |
| Removal during a write | One removal; append 927 failed during its writes (above); removal during logging on 2026-11-02 | `removed` injection |
| Full media | Not feasible | `full` injection; real NoSpace on a 128 KiB host volume |
| Write-protected media | Not feasible | `readonly` injection |
| Blocked write | — | `timeout` injection |

#### Final state

Normal release firmware (no diagnostic feature) was reflashed:
`target/t-103-2026-11-27-normal.log` shows the region card mounted, the
catalogue open, the previous `theo.log` at 55,007 bytes and storage
mounted, with no disable or panic. The isolated `sdtest2.log`/`sdtest2.1`
fixtures stay on the log volume (at most 4 KiB + 128 KiB). The stress runs
wrote about 720 MiB through them, a wear cost accepted for this test.
