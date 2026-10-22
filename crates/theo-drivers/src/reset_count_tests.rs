use super::reset_count::{decode_resets, encode_resets};

#[test]
fn decode_resets_reads_back_what_was_encoded() {
    assert_eq!(decode_resets(encode_resets(2)), 2);
}

#[test]
fn decode_resets_random_contents_read_as_zero() {
    assert_eq!(decode_resets([0xDEAD_BEEF, 0x1234_5678]), 0);
}

#[test]
fn decode_resets_one_corrupted_word_reads_as_zero() {
    let [count, check] = encode_resets(2);
    assert_eq!(decode_resets([count ^ 1, check]), 0);
}

#[test]
fn decode_resets_all_zero_reads_as_zero() {
    assert_eq!(decode_resets([0, 0]), 0);
}
