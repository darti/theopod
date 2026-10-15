use speculoos::prelude::*;

use crate::memory_check::{
    PatternMismatch, check_pattern, check_pattern_with, fill_pattern, find_mismatch,
};

#[test]
fn check_pattern_sound_memory_passes_and_leaves_zeros() {
    let mut words = vec![0x1234; 1000];

    let result = check_pattern(&mut words);

    assert_that!(result).is_ok();
    assert_that!(words.iter().all(|&word| word == 0)).is_true();
}

#[test]
fn check_pattern_empty_region_passes() {
    assert_that!(check_pattern(&mut [])).is_ok();
}

#[test]
fn fill_pattern_depends_on_the_address() {
    let mut words = vec![0; 64];

    fill_pattern(&mut words, false);

    // Address lines shorted together would alias distinct words.
    let mut distinct = words.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_that!(distinct).has_length(64);
}

#[test]
fn fill_pattern_inverted_complements_every_bit() {
    let mut plain = vec![0; 64];
    let mut inverted = vec![0; 64];

    fill_pattern(&mut plain, false);
    fill_pattern(&mut inverted, true);

    assert_that!(plain.iter().zip(&inverted).all(|(a, b)| *a == !*b)).is_true();
}

#[test]
fn find_mismatch_reports_the_first_corrupted_word() {
    let mut words = vec![0; 64];
    fill_pattern(&mut words, true);
    let expected = words[40];
    words[40] ^= 0x0100;
    words[50] ^= 0x0001;

    let result = find_mismatch(&words, true);

    assert_that!(result).is_equal_to(Some(PatternMismatch {
        index: 40,
        expected,
        actual: expected ^ 0x0100,
    }));
}

#[test]
fn find_mismatch_intact_pattern_finds_nothing() {
    let mut words = vec![0; 64];
    fill_pattern(&mut words, false);

    assert_that!(find_mismatch(&words, false)).is_none();
}

#[test]
fn check_pattern_with_bit_stuck_high_is_found_by_the_complement_pass() {
    let mut words = vec![0; 64];
    // Bit 0 of word 5 reads 1 whatever is written: the plain pattern has it
    // set there (odd index times odd constant), so only the complement fails.
    let stuck = |words: &mut [u32]| words[5] |= 1;
    let mut expected = vec![0; 64];
    fill_pattern(&mut expected, true);

    let result = check_pattern_with(&mut words, stuck);

    assert_that!(result).is_equal_to(Err(PatternMismatch {
        index: 5,
        expected: expected[5],
        actual: expected[5] | 1,
    }));
}

#[test]
fn check_pattern_with_fault_in_the_plain_pass_stops_before_the_complement() {
    let mut words = vec![0; 64];
    let mut fills = 0;
    let flip = |words: &mut [u32]| {
        fills += 1;
        words[0] ^= 0x8000_0000;
    };

    let result = check_pattern_with(&mut words, flip);

    assert_that!(result.map_err(|mismatch| mismatch.index)).is_equal_to(Err(0));
    assert_that!(fills).is_equal_to(1);
}

#[test]
fn check_pattern_with_sound_memory_syncs_after_each_fill() {
    let mut words = vec![0; 64];
    let mut seen = Vec::new();
    let sync = |words: &mut [u32]| seen.push(words[1]);

    let result = check_pattern_with(&mut words, sync);

    // Once after the plain fill, once after the complement, none after the
    // final zeroing.
    let mut plain = vec![0; 64];
    fill_pattern(&mut plain, false);
    assert_that!(result).is_ok();
    assert_that!(seen).is_equal_to(vec![plain[1], !plain[1]]);
}
