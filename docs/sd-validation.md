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

Further on-board tests are recorded below. Physical card removal, actually
filling the 59,700 MiB medium, and interrupting SD power remain unperformed.
The historical FAT matrix does not substitute for those physical tests.
Keep t-103 open for its remaining scope.

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
