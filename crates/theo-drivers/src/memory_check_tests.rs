use speculoos::prelude::*;

use crate::memory_check::{Mismatch, check_pattern, fill_pattern, find_mismatch};

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

    assert_that!(result).is_equal_to(Some(Mismatch {
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
