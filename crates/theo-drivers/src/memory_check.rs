//! Pattern test of a RAM region, for external RAM clocked without tuning.
//!
//! - [`check_pattern`], [`PatternMismatch`]: writes and reads back an
//!   address-dependent pattern and its complement, then zeroes the region.
//!
//! The firmware runs PSRAM at 80 MHz without the timing tuning ESP-IDF
//! performs (ADR-0004): a marginal clock would corrupt data silently, the
//! heap spill included. Checking the framebuffer region at boot turns that
//! into a panic. Every access is volatile, so the compiler cannot fold a
//! read into the value it just wrote.

use core::ptr;

/// First word that did not read back as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatternMismatch {
    /// Index of the word in the region.
    pub index: usize,
    /// Value written.
    pub expected: u32,
    /// Value read back.
    pub actual: u32,
}

/// Checks that every word of `words` holds a pattern and its complement.
///
/// On success every word is zero. Data a CPU cache still holds is read
/// from the cache, so on a cached region the last cache-sized part may
/// not reach the memory itself: check regions much larger than the cache.
///
/// # Errors
///
/// Returns the first [`PatternMismatch`]; the region's contents are then
/// unspecified.
pub fn check_pattern(words: &mut [u32]) -> Result<(), PatternMismatch> {
    check_with(words, |_| {})
}

/// [`check_pattern`], calling `after_fill` between each fill and its check,
/// so tests can stand in for faulty memory.
pub(crate) fn check_with(
    words: &mut [u32],
    mut after_fill: impl FnMut(&mut [u32]),
) -> Result<(), PatternMismatch> {
    for inverted in [false, true] {
        fill_pattern(words, inverted);
        after_fill(words);
        if let Some(mismatch) = find_mismatch(words, inverted) {
            return Err(mismatch);
        }
    }
    for word in words.iter_mut() {
        // SAFETY: `word` is a valid, exclusive reference.
        unsafe { ptr::write_volatile(word, 0) };
    }
    Ok(())
}

/// Word of the pattern at `index`: a multiplicative hash by an odd
/// constant, a bijection, so words at different addresses differ.
fn pattern(index: usize, inverted: bool) -> u32 {
    // Truncation intended: regions stay far below 2^32 words.
    let word = (index as u32).wrapping_mul(0x9E37_79B9);
    if inverted { !word } else { word }
}

/// Writes the pattern, or its complement, over `words`.
pub(crate) fn fill_pattern(words: &mut [u32], inverted: bool) {
    for (index, word) in words.iter_mut().enumerate() {
        // SAFETY: `word` is a valid, exclusive reference.
        unsafe { ptr::write_volatile(word, pattern(index, inverted)) };
    }
}

/// Finds the first word of `words` that differs from the pattern.
pub(crate) fn find_mismatch(words: &[u32], inverted: bool) -> Option<PatternMismatch> {
    words.iter().enumerate().find_map(|(index, word)| {
        let expected = pattern(index, inverted);
        // SAFETY: `word` is a valid reference.
        let actual = unsafe { ptr::read_volatile(word) };
        (actual != expected).then_some(PatternMismatch {
            index,
            expected,
            actual,
        })
    })
}
