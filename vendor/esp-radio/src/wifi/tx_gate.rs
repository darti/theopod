//! Theo patch (t-172): TX in-flight gate that heals a leaked count.
//!
//! The driver counts frames handed to the Wi-Fi blob and holds further
//! frames once `tx_queue_size` are in flight. The blob sometimes never calls
//! the tx-done callback for a frame, so the count drifts up; at the limit,
//! TX and RX (whose token needs a TX token) stop until the chip resets.
//! Healing: when the gate has held frames for `HEAL_MS` without any done
//! callback, the count is assumed leaked and restarts at zero. A frame
//! still in the blob then only costs one extra queued frame, which
//! `esp_wifi_internal_tx` rejects if its own buffers are full.
//!
//! Dependency-free so its tests run on the host (`just test`).

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Time held at the limit, without a done callback, before healing.
pub(crate) const HEAL_MS: u32 = 200;

/// Window over which heals are aggregated into one warning (t-179).
pub(crate) const REPORT_MS: u32 = 60_000;

/// Sentinel for "not held" (and for `HealLog`, "no window yet"): a real
/// start is stored as at least 1.
const NOT_HELD: u32 = 0;

/// Outcome of [`TxGate::admit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Below the limit.
    Send,
    /// At the limit: wait for a done callback.
    Hold,
    /// Held for `HEAL_MS`: the count was reset, send.
    Healed,
}

/// Frames in flight and the start of the current hold, if any.
pub(crate) struct TxGate {
    /// Crate-visible so the host tests can replay a racing callback.
    pub(crate) inflight: AtomicUsize,
    held_since: AtomicU32,
}

impl TxGate {
    /// An empty gate: nothing in flight, nothing held.
    pub(crate) const fn new() -> Self {
        Self {
            inflight: AtomicUsize::new(0),
            held_since: AtomicU32::new(NOT_HELD),
        }
    }

    /// Whether another frame may go to the blob at `now_ms` (wrapping).
    pub(crate) fn admit(&self, limit: usize, now_ms: u32) -> Admit {
        if self.inflight.load(Ordering::SeqCst) < limit {
            // A done callback racing the hold start below can leave a stale
            // start behind; drop it so the next hold gets its full `HEAL_MS`.
            if self.held_since.load(Ordering::SeqCst) != NOT_HELD {
                self.held_since.store(NOT_HELD, Ordering::SeqCst);
            }
            return Admit::Send;
        }
        let since = self.held_since.load(Ordering::SeqCst);
        if since == NOT_HELD {
            self.held_since.store(now_ms.max(1), Ordering::SeqCst);
            return Admit::Hold;
        }
        if now_ms.wrapping_sub(since) < HEAL_MS {
            return Admit::Hold;
        }
        self.inflight.store(0, Ordering::SeqCst);
        self.held_since.store(NOT_HELD, Ordering::SeqCst);
        Admit::Healed
    }

    /// Counts a frame handed to the blob.
    pub(crate) fn sent(&self) {
        self.inflight.fetch_add(1, Ordering::SeqCst);
    }

    /// Counts a tx-done callback or a rejected frame; ends any hold.
    pub(crate) fn done(&self) {
        let mut x = self.inflight.load(Ordering::SeqCst);
        while x > 0 {
            match self.inflight.compare_exchange_weak(
                x,
                x - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => x = current,
            }
        }
        self.held_since.store(NOT_HELD, Ordering::SeqCst);
    }
}

/// Rate limit for the heal warning (t-179): the first heal is reported at
/// once, later ones are counted and reported with the first heal after
/// `REPORT_MS`, so frequent heals stay visible without flooding the log.
///
/// Heals after the last report stay counted until the next heal reports
/// them: the count of the final window is unreported if heals stop.
/// Safe for racing callers: one caller wins each window with a CAS and
/// drains the count, so no heal is lost or reported twice.
pub(crate) struct HealLog {
    /// Start of the current window, `NOT_HELD` before the first heal.
    window_start: AtomicU32,
    /// Heals not yet reported.
    pending: AtomicU32,
}

impl HealLog {
    /// A log that has seen no heal.
    pub(crate) const fn new() -> Self {
        Self {
            window_start: AtomicU32::new(NOT_HELD),
            pending: AtomicU32::new(0),
        }
    }

    /// Records a heal at `now_ms` (wrapping). Returns the number of heals to
    /// report now, or `None` while the current window is still open.
    pub(crate) fn record(&self, now_ms: u32) -> Option<u32> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let start = self.window_start.load(Ordering::SeqCst);
        if start != NOT_HELD && now_ms.wrapping_sub(start) < REPORT_MS {
            return None;
        }
        // Only the caller that moves the window reports; a racing caller's
        // heal stays in `pending` for this report or the next one.
        self.window_start
            .compare_exchange(start, now_ms.max(1), Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        Some(self.pending.swap(0, Ordering::SeqCst))
    }

    /// Heals counted but not yet reported.
    #[cfg(test)]
    pub(crate) fn unreported(&self) -> u32 {
        self.pending.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[path = "tx_gate_tests.rs"]
mod tests;
