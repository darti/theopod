use speculoos::prelude::*;

use crate::dma_layout::{Chunk, PayloadError, descriptor_chunks, frame_payload};

/// Bytes per descriptor of the firmware's `PsramTxBuf` (63 bursts of 64).
const CHUNK: usize = 4032;
/// `MAX_TRANSFER_LEN` of the firmware's `EspQspi`.
const MAX_TRANSFER_LEN: usize = 32_704;
const BURST: usize = 64;
/// A burst-aligned 1 KB frame.
const FRAME: core::ops::Range<usize> = 0x1000..0x1400;

fn chunks(len: usize) -> Vec<Chunk> {
    descriptor_chunks(len, CHUNK).collect()
}

fn chunk(offset: usize, len: usize, last: bool) -> Chunk {
    Chunk { offset, len, last }
}

#[test]
fn descriptor_chunks_one_byte_is_one_last_chunk() {
    assert_that!(chunks(1)).is_equal_to(vec![chunk(0, 1, true)]);
}

#[test]
fn descriptor_chunks_exactly_one_chunk_is_one_last_chunk() {
    assert_that!(chunks(CHUNK)).is_equal_to(vec![chunk(0, CHUNK, true)]);
}

#[test]
fn descriptor_chunks_one_past_a_chunk_adds_a_one_byte_chunk() {
    assert_that!(chunks(CHUNK + 1))
        .is_equal_to(vec![chunk(0, CHUNK, false), chunk(CHUNK, 1, true)]);
}

#[test]
fn descriptor_chunks_max_transfer_ends_with_the_remainder() {
    let chunks = chunks(MAX_TRANSFER_LEN);

    // 8 full descriptors, then 448 bytes.
    assert_that!(chunks).has_length(9);
    assert_that!(chunks[8]).is_equal_to(chunk(8 * CHUNK, 448, true));
}

#[test]
fn descriptor_chunks_tile_the_transfer_with_burst_aligned_starts() {
    let chunks = chunks(MAX_TRANSFER_LEN);

    let mut next = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        assert_that!(chunk.offset).is_equal_to(next);
        assert_that!(chunk.offset % BURST).is_equal_to(0);
        assert_that!(chunk.last).is_equal_to(index + 1 == chunks.len());
        next += chunk.len;
    }
    assert_that!(next).is_equal_to(MAX_TRANSFER_LEN);
}

#[test]
fn descriptor_chunks_empty_transfer_has_no_chunks() {
    assert_that!(chunks(0)).is_empty();
}

#[test]
#[should_panic(expected = "chunk")]
fn descriptor_chunks_zero_chunk_size_panics() {
    let _ = descriptor_chunks(1, 0);
}

#[test]
fn frame_payload_whole_aligned_payload_is_sent_in_place() {
    let payload = FRAME.start..FRAME.start + 256;

    assert_that!(frame_payload(FRAME, payload, MAX_TRANSFER_LEN, BURST)).is_equal_to(Ok(true));
}

#[test]
fn frame_payload_ending_at_the_frame_end_is_sent_in_place() {
    let payload = FRAME.end - 128..FRAME.end;

    assert_that!(frame_payload(FRAME, payload, MAX_TRANSFER_LEN, BURST)).is_equal_to(Ok(true));
}

#[test]
fn frame_payload_misaligned_start_returns_its_offset() {
    let payload = FRAME.start + 32..FRAME.start + 96;

    assert_that!(frame_payload(FRAME, payload, MAX_TRANSFER_LEN, BURST))
        .is_equal_to(Err(PayloadError::Misaligned { offset: 32 }));
}

#[test]
fn frame_payload_longer_than_a_transfer_returns_too_long() {
    let payload = FRAME.start..FRAME.start + 512;

    assert_that!(frame_payload(FRAME, payload, 256, BURST))
        .is_equal_to(Err(PayloadError::TooLong { len: 512 }));
}

#[test]
fn frame_payload_at_the_transfer_limit_is_sent_in_place() {
    let payload = FRAME.start..FRAME.start + 256;

    assert_that!(frame_payload(FRAME, payload, 256, BURST)).is_equal_to(Ok(true));
}

#[test]
fn frame_payload_outside_the_frame_is_not_a_frame_payload() {
    let payload = 0x2000..0x2010;

    assert_that!(frame_payload(FRAME, payload, MAX_TRANSFER_LEN, BURST)).is_equal_to(Ok(false));
}

#[test]
fn frame_payload_straddling_the_frame_end_is_not_a_frame_payload() {
    let payload = FRAME.end - 64..FRAME.end + 64;

    assert_that!(frame_payload(FRAME, payload, MAX_TRANSFER_LEN, BURST)).is_equal_to(Ok(false));
}

#[test]
fn frame_payload_empty_is_not_a_frame_payload() {
    let payload = FRAME.start + 32..FRAME.start + 32;

    assert_that!(frame_payload(FRAME, payload, MAX_TRANSFER_LEN, BURST)).is_equal_to(Ok(false));
}
