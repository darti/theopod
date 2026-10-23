# t-94 microSD validation

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
