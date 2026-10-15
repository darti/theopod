use speculoos::prelude::*;

use crate::dma_layout::{DmaChunk, FramePayloadError, descriptor_chunks, frame_payload};

/// Bytes per descriptor of the firmware's `PsramTxBuf` (63 bursts of 64).
/// Keep in sync with the const assert in `src/device/psram_tx_buf.rs`.
const CHUNK: usize = 4032;
/// `MAX_TRANSFER_LEN` of the firmware's `EspQspi`. Keep in sync with the
/// const assert in `src/device/esp_qspi.rs`.
const MAX_TRANSFER_LEN: usize = 32_704;
const BURST: usize = 64;
/// A 1 KB frame.
const FRAME_LEN: usize = 0x400;

fn chunks(len: usize) -> Vec<DmaChunk> {
    descriptor_chunks(len, CHUNK).collect()
}

fn chunk(offset: usize, len: usize, last: bool) -> DmaChunk {
    DmaChunk { offset, len, last }
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
fn descriptor_chunks_whole_descriptors_fill_exactly_that_many() {
    // `PsramTxBuf::prepare` indexes descriptors by chunk: a source of its
    // full capacity must need exactly all of them.
    let chunks = chunks(8 * CHUNK);

    assert_that!(chunks).has_length(8);
    assert_that!(chunks[7]).is_equal_to(chunk(7 * CHUNK, CHUNK, true));
}

#[test]
fn frame_payload_aligned_range_is_sent_in_place() {
    assert_that!(frame_payload(FRAME_LEN, 0..256, MAX_TRANSFER_LEN, BURST)).is_ok();
}

#[test]
fn frame_payload_ending_at_the_frame_end_is_sent_in_place() {
    let bytes = FRAME_LEN - 128..FRAME_LEN;

    assert_that!(frame_payload(FRAME_LEN, bytes, MAX_TRANSFER_LEN, BURST)).is_ok();
}

#[test]
fn frame_payload_at_the_transfer_limit_is_sent_in_place() {
    assert_that!(frame_payload(FRAME_LEN, 0..256, 256, BURST)).is_ok();
}

#[test]
fn frame_payload_misaligned_start_returns_its_offset() {
    assert_that!(frame_payload(FRAME_LEN, 32..96, MAX_TRANSFER_LEN, BURST))
        .is_equal_to(Err(FramePayloadError::Misaligned { offset: 32 }));
}

#[test]
fn frame_payload_one_past_the_transfer_limit_returns_too_long() {
    assert_that!(frame_payload(FRAME_LEN, 0..257, 256, BURST))
        .is_equal_to(Err(FramePayloadError::TooLong { len: 257 }));
}

#[test]
fn frame_payload_too_long_and_misaligned_reports_too_long() {
    assert_that!(frame_payload(FRAME_LEN, 32..512, 256, BURST))
        .is_equal_to(Err(FramePayloadError::TooLong { len: 480 }));
}

#[test]
fn frame_payload_past_the_frame_end_returns_out_of_frame() {
    let bytes = FRAME_LEN - 64..FRAME_LEN + 64;

    assert_that!(frame_payload(
        FRAME_LEN,
        bytes.clone(),
        MAX_TRANSFER_LEN,
        BURST
    ))
    .is_equal_to(Err(FramePayloadError::OutOfFrame { bytes }));
}

#[test]
fn frame_payload_empty_returns_out_of_frame() {
    assert_that!(frame_payload(FRAME_LEN, 64..64, MAX_TRANSFER_LEN, BURST))
        .is_equal_to(Err(FramePayloadError::OutOfFrame { bytes: 64..64 }));
}

#[test]
#[expect(clippy::reversed_empty_ranges, reason = "the invalid input under test")]
fn frame_payload_reversed_returns_out_of_frame() {
    assert_that!(frame_payload(FRAME_LEN, 128..64, MAX_TRANSFER_LEN, BURST))
        .is_equal_to(Err(FramePayloadError::OutOfFrame { bytes: 128..64 }));
}
