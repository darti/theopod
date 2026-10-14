//! Disjoint bands of framebuffer rows that need flushing.
//!
//! - [`DirtyRows`]: up to `N` sorted, disjoint bands of rows, widened to
//!   even rows, for [`crate::flush_bands`].
//!
//! Sending one band per dirty area instead of their bounding box keeps two
//! small changes far apart (a status bar and a bottom bar) from costing a
//! near-full frame. Bands that touch once widened are merged, so no 2-row
//! block is sent twice; past `N` bands, the two closest are merged.

use core::ops::Range;

/// Up to `N` sorted, disjoint bands of rows of a `height`-row panel.
///
/// Every band starts on an even row and ends on an even row or the panel's
/// last row, as [`crate::flush_rows`] would widen it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirtyRows<const N: usize> {
    bands: [Range<u16>; N],
    count: usize,
    height: u16,
}

impl<const N: usize> DirtyRows<N> {
    /// No dirty rows on a panel of `height` rows.
    #[must_use]
    pub const fn new(height: u16) -> Self {
        const {
            assert!(N > 0, "DirtyRows needs room for one band");
        }
        Self {
            bands: [const { 0..0 }; N],
            count: 0,
            height,
        }
    }

    /// Every row of a panel of `height` rows, as one band.
    #[must_use]
    pub fn all(height: u16) -> Self {
        let mut rows = Self::new(height);
        rows.add(0..height);
        rows
    }

    /// Tells whether no row is dirty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The bands, sorted and disjoint.
    #[must_use]
    pub fn bands(&self) -> &[Range<u16>] {
        &self.bands[..self.count]
    }

    /// Marks `rows` dirty, clamped to the panel; empty `rows` are ignored.
    pub fn add(&mut self, rows: Range<u16>) {
        let start = rows.start.min(self.height) & !1;
        let end = rows.end.min(self.height);
        if rows.start >= end {
            return;
        }
        // Rounding up may pass the panel (odd height) or u16: clamp.
        let end = u16::try_from(usize::from(end).next_multiple_of(2))
            .map_or(self.height, |end| end.min(self.height));
        let mut merged = start..end;

        // Rebuild in order, absorbing every band `merged` touches.
        let mut bands: [Range<u16>; N] = [const { 0..0 }; N];
        let mut count = 0;
        let mut inserted = false;
        let mut spill = None;
        for band in self.bands() {
            if band.end < merged.start {
                push(&mut bands, &mut count, &mut spill, band.clone());
            } else if band.start > merged.end {
                if !inserted {
                    push(&mut bands, &mut count, &mut spill, merged.clone());
                    inserted = true;
                }
                push(&mut bands, &mut count, &mut spill, band.clone());
            } else {
                merged = merged.start.min(band.start)..merged.end.max(band.end);
            }
        }
        if !inserted {
            push(&mut bands, &mut count, &mut spill, merged);
        }
        self.bands = bands;
        self.count = count;
        if let Some(extra) = spill {
            self.merge_closest(extra);
        }
    }

    /// Fits `extra`, which sorts after every band, by merging the two
    /// closest neighbours among the bands and `extra`.
    fn merge_closest(&mut self, extra: Range<u16>) {
        let bands = &mut self.bands[..self.count];
        let last = bands.len() - 1;
        // Start of the right neighbour of band `index`: the next band, or
        // `extra` after the last one.
        let next_start = |bands: &[Range<u16>], index: usize| {
            if index == last {
                extra.start
            } else {
                bands[index + 1].start
            }
        };
        let closest = (0..bands.len())
            .min_by_key(|&index| next_start(bands, index) - bands[index].end)
            .unwrap_or(last);
        if closest == last {
            bands[last].end = extra.end;
        } else {
            // Band `closest` absorbs its neighbour; the rest shift left.
            bands[closest].end = bands[closest + 1].end;
            bands[closest + 1..].rotate_left(1);
            bands[last] = extra;
        }
    }
}

/// Appends `band` to `bands`, or keeps it in `spill` once `bands` is full.
fn push<const N: usize>(
    bands: &mut [Range<u16>; N],
    count: &mut usize,
    spill: &mut Option<Range<u16>>,
    band: Range<u16>,
) {
    if *count < N {
        bands[*count] = band;
        *count += 1;
    } else {
        *spill = Some(band);
    }
}
