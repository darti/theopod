# Changelog

All notable changes to Theo Pod are documented here.
Generated from Conventional Commits by git-cliff and release-plz.

## [0.1.1] - 2026-10-31

### Features
- **podcasts:** persist bounded episode identity and lifecycle (t-107) ([2addb14](https://github.com/darti/theopod/commit/2addb140d920c08d5b2f08f2b0550be6ba414b66))
- **podcasts:** reconcile complete Radio France history (t-108) ([9be57a2](https://github.com/darti/theopod/commit/9be57a27b661ff51cd1b78d964af39ef729264b3))

### Fixes
- **storage:** recover EKV metadata rollover and bound future SD regions (t-116) ([acca56f](https://github.com/darti/theopod/commit/acca56f68ca927ad8eae5d8baf6f6a312b409408))
- **release:** give new podcast crate its own baseline (t-117) ([#2](https://github.com/darti/theopod/pull/2)) ([95a8143](https://github.com/darti/theopod/commit/95a8143796d39b2e76dff67b93de483b0f78dc36))

### Documentation
- **podcasts:** verify shared storage and linked TLS feasibility (t-106) ([08cfbfa](https://github.com/darti/theopod/commit/08cfbfa241495a000ef421a2bae18a6b4f1bed2e))

## [0.1.0] - 2026-10-29

### Features
- **display:** render Slint on the CO5300 AMOLED panel ([f954dfb](https://github.com/darti/theopod/commit/f954dfb93a2d488e7600c484bf347d732d6b2b79))
- **BREAKING** async DMA display, input task and panel power in drivers (t-8) ([ecc8e69](https://github.com/darti/theopod/commit/ecc8e69dd031af91c2d13a506a8e43a5f3b91c78))
- **BREAKING** share an async I2C bus for touch (t-2) ([026c847](https://github.com/darti/theopod/commit/026c84715c60a14a26c804480ade69d360bcd020))
- **ui:** replace hello world with a counter button (t-33) ([9295a88](https://github.com/darti/theopod/commit/9295a88c73e1ac4f5d3c8d908653c606cfebe445))
- **BREAKING** **touch:** drive the CST820 and forward touches to Slint (t-19) ([d065668](https://github.com/darti/theopod/commit/d0656684c1f2d4c670939579592df8742fa61f1e))
- **BREAKING** **display:** rotate the view with the QMI8658 accelerometer (t-32) ([60049d8](https://github.com/darti/theopod/commit/60049d83aabc37e1db207a889fd0cf530f71a04d))
- **imu:** reject an IMU_AXES that is not a proper rotation (t-42) ([3a88b8a](https://github.com/darti/theopod/commit/3a88b8a5cb09df1d89ea7d1f4e7b5070710d4ebd))
- **device:** keep PSRAM at 80 MHz and self-test it at boot (t-27) ([26c1839](https://github.com/darti/theopod/commit/26c1839f8a7fffcb2d69515b55b1904ee95de691))
- **display:** reboot when the display stays broken (t-44) ([5e22f8e](https://github.com/darti/theopod/commit/5e22f8edcd8a12fa85824accd1eba4024cedf543))
- **device:** check PSRAM itself, past the data cache, at boot (t-45) ([487ddd3](https://github.com/darti/theopod/commit/487ddd348796229082608b937c6dfc90b2b84107))
- stop with a BOOT long press, wake on the next press (t-49) ([95fe131](https://github.com/darti/theopod/commit/95fe131c6a62f89963927a0ab7cb29ac74e10463))
- park touch, panel rails and IMU before deep sleep (t-50) ([89111b5](https://github.com/darti/theopod/commit/89111b5f6992efce204acc816573a75f0cc88d25))
- **input:** bound the stop's wait for the panel to sleep (t-54) ([f94683f](https://github.com/darti/theopod/commit/f94683f364875423578d2d5585064d63696c704d))
- **device:** log why the chip started (t-51) ([8158187](https://github.com/darti/theopod/commit/815818738799f7626bf9e0fff1d0438ecf3adf22))
- **device:** feed the main watchdog from the UI loop (t-57) ([e289fda](https://github.com/darti/theopod/commit/e289fdab01f878d304d9763b90731ed56d08f1c5))
- **device:** sleep-check diagnostic and a passive serial monitor (t-67) ([1991dbc](https://github.com/darti/theopod/commit/1991dbcc5bda47e2a02d14e785698515b727d682))
- **device:** stop into deep sleep after three watchdog reboots (t-66) ([49a688c](https://github.com/darti/theopod/commit/49a688c7808072151e859c63e0377abddb1fc5ba))
- **device:** free the I2C bus at every start (t-84) ([5d1c3b2](https://github.com/darti/theopod/commit/5d1c3b218d81bdf1db643736edd3dc39468e749c))
- **storage:** mount V2 microSD over SPI and persist boot logs (t-94) ([547b80f](https://github.com/darti/theopod/commit/547b80fd3ae928fdc91068122f0efa8eee3cf851))
- **logging:** persist runtime logs with bounded SD rotation and recovery (t-94) ([611863e](https://github.com/darti/theopod/commit/611863e59ff591c3abe1eb792c47f01a66fac723))
- **input:** watch bounded task progress before feeding watchdog (t-83) ([529127c](https://github.com/darti/theopod/commit/529127cb99e808b0c59e5fe84a0d4a759859f6df))
- **storage:** adapt EKV to async FAT sector storage ([1eb20d6](https://github.com/darti/theopod/commit/1eb20d63cd9ac4086a41728f20ef5e3500d7bd43))
- **storage:** migrate SD logs to LittleFS on a dedicated core (t-99) ([0b8fffa](https://github.com/darti/theopod/commit/0b8fffa37e29369885bfe3fc59115e7c7fb55a6e))

### Fixes
- fix ESP toolchain setup and Xtensa build ([e537f8c](https://github.com/darti/theopod/commit/e537f8cacf82b6fbecc3ac63df611800d67516d2))
- **BREAKING** **display:** read the framebuffer in place from PSRAM (t-4) ([0d8ea44](https://github.com/darti/theopod/commit/0d8ea44e6c5ab367a111ca511a624a090d557682))
- **BREAKING** **touch:** time out a stuck I2C bus and reset a failing touch controller (t-20, t-34) ([bb7e4a7](https://github.com/darti/theopod/commit/bb7e4a7630948a22e8ee9a07b04bf0b0295f1171))
- **BREAKING** **display:** make panel flushes cancel-safe (t-3, t-29, t-30, t-31) ([1b33da7](https://github.com/darti/theopod/commit/1b33da7709f83b2ef93f61d6db656f3eb9c5c048))
- **BREAKING** **display:** stop resending on permanent flush errors, flush row bands (t-1, t-5, t-28) ([4d5e68b](https://github.com/darti/theopod/commit/4d5e68b70d8920099848553c4644ce4e7f854de7))
- **BREAKING** address the branch review (t-1, t-3, t-5, t-30, t-34) ([d6cafa3](https://github.com/darti/theopod/commit/d6cafa33c8adc2085da017bdc6faf44f6250cfa0))
- **display:** wait for SPI idle before releasing chip select (t-47) ([d2e5ba0](https://github.com/darti/theopod/commit/d2e5ba0824e88af03ef4b0a7a8fcacccea3aedf7))
- **BREAKING** address the round-2 review (t-43, t-44, t-46, t-47, t-48) ([803f9fd](https://github.com/darti/theopod/commit/803f9fd7601f3fa24d0bb90850cd2e219a23fb49))
- **display:** close the open area in init, count the sleep-in in the watchdog budget (t-56) ([c31f609](https://github.com/darti/theopod/commit/c31f609559d24d033633cc1b872c9285ba4e8300))
- **input:** log the start cause after 500 ms, or before an earlier stop (t-76) ([0b59f94](https://github.com/darti/theopod/commit/0b59f94cce845bd8db3bf677d75f38b8179d3dfd))
- **storage:** provision onboard SD and avoid non-Embassy timer panic (t-103) ([528c6ad](https://github.com/darti/theopod/commit/528c6adabcee274c327d9af6f87896eb04bec744))

### Performance
- **imu:** run the accelerometer in low-power mode at 21 Hz (t-41) ([991fe50](https://github.com/darti/theopod/commit/991fe50eab4a3d1bcaf8c5626af4a6d34c7a0226))

### Refactoring
- **device:** place the I2C bus mutex in a StaticCell (t-21) ([5a00c39](https://github.com/darti/theopod/commit/5a00c39397173d599a138ffd909ce4be2fc58f6f))
- **device:** share the TCA9554 as a mutex-protected service (t-43) ([7f2d73f](https://github.com/darti/theopod/commit/7f2d73f8e60d14e98848061109e1e878fbc71c02))
- **BREAKING** **display:** let the display own the framebuffer (t-46) ([0dcabd4](https://github.com/darti/theopod/commit/0dcabd4969e905e1829163181ea32f3a5e428105))
- move the input tasks out of main.rs (t-53) ([7454e97](https://github.com/darti/theopod/commit/7454e97e04b0a57bdec251efb855544f5d58a90c))
- **BREAKING** **display:** split the framebuffer off the QSPI transport (t-58) ([1256c75](https://github.com/darti/theopod/commit/1256c759fb52dd44c735f690183a27bc62bd3cd2))
- **device:** split Touch and DeepSleep out of device/mod.rs (t-69) ([d231e20](https://github.com/darti/theopod/commit/d231e200a534e15327f0a2531fc44c1d7c76027b))
- share reset counters and park sequence (t-85) ([9985632](https://github.com/darti/theopod/commit/9985632444ec4cc8b164071ebc388e4ddf46745c))

### Documentation
- **test:** keep regression tests inline, tagged by task (t-7) ([835e234](https://github.com/darti/theopod/commit/835e23445101877d8889acfbb62956f84aecc1d2))
- **adr:** record the dropped hardware landscape rotation (t-40) ([569f62b](https://github.com/darti/theopod/commit/569f62b13a8d49ee3dbbfcd2d58ef798d0ea22ea))
- **agents:** add worktree, serial-port and panel-check gotchas ([6579d37](https://github.com/darti/theopod/commit/6579d37695432f2e0a4763d565c30204f93bfb46))
- **adr:** the V2 cannot read the CO5300 back (t-68) ([6a655ea](https://github.com/darti/theopod/commit/6a655eadf69d97b7e27b57e982a0eec35df43ba6))

### Tests
- **button:** host-test the BOOT long-press race (t-52) ([6b36149](https://github.com/darti/theopod/commit/6b361496cf3c7007bd3d82901199238ef857adbc))
- **storage:** validate LittleFS faults and sleep cycles on board (t-103) ([91fb77a](https://github.com/darti/theopod/commit/91fb77aeb6c4a54f020d9c4df5b557d830aa3d2d))

### Maintenance
- license the project under MIT OR Apache-2.0 (t-22) ([d220183](https://github.com/darti/theopod/commit/d220183bb6cad0485f62b21cccee8de8a517dd2e))
- add just miri and just audit (t-48) ([2f101c2](https://github.com/darti/theopod/commit/2f101c22b57b3918cda9ddcbd69e47661a0b6537))
- **release:** automate audited GitHub firmware releases (t-111) ([0d82a91](https://github.com/darti/theopod/commit/0d82a91017687418727132a3f12590783ed4fe1a))

### Other
- Document project guidance in English ([410f49b](https://github.com/darti/theopod/commit/410f49b1c8949d097ed8f7fe0d800801f185f154))
- Target ESP32-S3 AMOLED V2 hardware ([14b6efe](https://github.com/darti/theopod/commit/14b6efe43cdc1f86056c612815aa80bcdb1fec55))
- Upgrade deps ([952a671](https://github.com/darti/theopod/commit/952a671775c8b1a69774afbb7c2a567655b1f82f))
