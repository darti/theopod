# Architecture decision records

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-split-hal-independent-drivers-from-device-code.md) | Split HAL-independent drivers from device code | Accepted |
| [0002](0002-define-a-local-qspibus-trait.md) | Define a local QspiBus trait | Accepted |
| [0003](0003-enable-esp-hal-unstable-and-pin-its-minor-version.md) | Enable esp-hal `unstable` and pin its minor version | Accepted |
| [0004](0004-render-into-a-psram-framebuffer-sent-by-async-dma.md) | Render into a PSRAM framebuffer sent by async DMA | Accepted |
| [0005](0005-share-an-async-i2c-bus-owned-by-board.md) | Share an async I2C bus owned by Board | Accepted |
| [0006](0006-rotate-the-view-in-software-from-the-accelerometer.md) | Rotate the view in software from the accelerometer | Accepted |
| [0007](0007-keep-regression-tests-inline-tagged-by-task.md) | Keep regression tests inline, tagged by task | Accepted |
| [0008](0008-stop-with-a-boot-long-press-into-deep-sleep.md) | Stop with a BOOT long press into deep sleep | Accepted |
| [0009](0009-feed-the-main-watchdog-from-the-ui-loop.md) | Feed the main watchdog from the UI loop | Accepted |
| [0010](0010-microsd-over-sdmmc.md) | Use the onboard microSD over SPI | Accepted |
| [0011](0011-adapt-ekv-to-sector-storage-with-copy-on-write-pages.md) | Adapt EKV to sector storage with copy-on-write pages | FAT backing direction superseded by ADR-0012 |
| [0012](0012-use-the-safe-littlefs-rust-api.md) | Use the safe littlefs-rust API for SD files | Implemented; hardware validation pending |
| [0013](0013-automate-github-firmware-releases.md) | Automate GitHub firmware releases | Accepted |
| [0014](0014-gate-podcast-persistence-on-recovery.md) | Gate podcast persistence on interruption recovery | Recovery corrected by ADR-0015; integration gate remains |
| [0015](0015-correct-ekv-publication-and-describe-sd-regions.md) | Correct EKV publication and describe checked SD regions | Local correction and descriptor API accepted; runtime integration pending |
| [0016](0016-prove-shared-storage-and-linked-network-feasibility.md) | Prove shared storage and linked network feasibility | Software feasibility verified; firmware activation pending |
