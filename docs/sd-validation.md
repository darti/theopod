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

Full media/removal, rotation, physical power-loss, stack-headroom measurements
and sleep/wake validation of LittleFS remain pending in t-103. The historical
FAT matrix above does not substitute for those tests. Keep t-103 open until
its work is validated and merged into main.
