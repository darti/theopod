# Input watchdog implementation plan

User approved the design and all backlog work on 2026-10-29 ("fait tout").

**Goal:** detect stalled input tasks without treating idle inputs as failures.
**Architecture:** HAL-independent InputWatch policy, callbacks at completed driver operations, UI-controlled MWDT0 feeding.
**Tech Stack:** Rust no_std, Embassy, embedded-hal; no new dependencies or unsafe code.
**Spec:** ../specs/2026-10-23-input-watchdog-design.md

## Constraints
Preserve existing APIs and debounce/hold deadlines. Stale boundary is >= 3000 ms. MWDT0 remains 5000 ms. No false reset on stop, absent IMU, or frame dump. Preserve queue backpressure and reboot cap.

## Review focus
Idle and held inputs; pending I2C/reset; stop after latched failure; frame pause rebasing; stale task around 60-second counter clear.

## Task 1: InputWatch policy
- [x] Add input_watch_tests.rs: initial grace, disabled orientation, each stale input, exact boundary, latch, stop, rebase, late progress.
- [x] Observe failing host tests, implement InputTask and InputWatch in input_watch.rs; export from lib.rs.
- [x] Verify host suite.

## Task 2: Driver progress
- [x] Button changed_with_progress and LongPressButton next_with_progress, callback after samples, 250 ms idle probes; old wrappers unchanged.
- [x] Cst820 changed_with_progress, callbacks after completed reads, including errors and unchanged points.
- [x] Tests first: idle, held, debounce/deadline, pending bus, errors; host suite.

## Task 3: Firmware integration
- [x] Initialize InputWatch before spawn, synchronization through blocking critical-section mutex and RefCell.
- [x] Report touch reset and orientation completion; suspend checks before stop handshake.
- [x] UI feeds only without latched fault; log once; do not clear reboot count after fault; rebase after known frame pause.
- [x] Update ADR-0009 and driver architecture map. Run fmt, check, test, lint, build.
- [x] Fresh review and fix material findings.

## Task 4: Board validation and backlog
- [x] Check RTC count retention and repeated watchdog stop with existing diagnostics; normal stop/timer wakes; restore normal firmware.
- [x] Record evidence for t-93/t-85; mark complete only for observed acceptance criteria.
- [x] t-59: user confirmed no instrument; blocker documented, ticket kept open.

## Execution record
Working in current checkout, as recommended by app instructions. Driver work delegated to two agents per dispatching-parallel-agents; policy/integration remain here. No files from the existing untracked spec are discarded.

Software result: 344 host tests and required compile/lint/build checks passed. Independent review: no material findings. Hardware approval pending after automatic rejection. t-59 blocked by lack of instrument (user confirmed). Later Oku writes rejected by auto-review pending authorization.

User explicitly authorized USB flash/reset/monitor and local Oku backlog writes after automatic rejection. Hardware checks resumed.

Final: RTC retention, cap/early stop, three stop/timer wakes, manual BOOT stop/Ext1 wake passed. User confirms touch and rotation. Normal firmware left running, monitor stopped. Only t-59 measurement remains blocked.
