use speculoos::prelude::*;

use crate::decode::decode;

/// AudioSpecificConfig of the podcasts: AAC-LC, 48 kHz, stereo.
const ASC_LC_48K_STEREO: [u8; 2] = [0x11, 0x90];

fn raw(asc: &[u8], frames: &[&[u8]]) -> Vec<u8> {
    let mut input = vec![0, u8::try_from(asc.len()).expect("short config")];
    input.extend_from_slice(asc);
    for frame in frames {
        let len = u16::try_from(frame.len()).expect("short frame");
        input.extend_from_slice(&len.to_le_bytes());
        input.extend_from_slice(frame);
    }
    input
}

#[test]
fn decode_empty_input_does_nothing() {
    let summary = decode(&[]);

    assert_that!(summary.frames).is_equal_to(0);
}

#[test]
fn decode_podcast_config_is_accepted() {
    let summary = decode(&raw(&ASC_LC_48K_STEREO, &[]));

    assert_that!(summary.init).is_equal_to(0);
}

// Regression (t-169): with an empty AudioSpecificConfig (ADTS path), the bit
// readers of getbits.h computed the bytes left as an unsigned difference that
// wrapped once past the end, and read past the input buffer.
#[test]
fn decode_empty_config_rejects_without_reading_past_it() {
    let summary = decode(include_bytes!("../regressions/t169-empty-config-read.bin"));

    assert_that!(summary.init).is_not_equal_to(0);
}

// Regression (t-169): a pulse_data() start band or offsets past the window
// added pulses beyond the spectrum (getics.cpp); the frame is now rejected.
#[test]
fn decode_pulse_outside_window_rejects_the_frame() {
    let summary = decode(include_bytes!(
        "../regressions/t169-pulse-outside-window.bin"
    ));

    assert_that!(summary.frames).is_equal_to(1);
    assert_that!(summary.decoded).is_equal_to(0);
}

// Regression (t-169): an unbounded escape prefix (unpack_idx.cpp) and pulse
// amplitudes overflowed the dequantisation table index.
#[test]
fn decode_unbounded_escape_stays_within_the_table() {
    let summary = decode(include_bytes!("../regressions/t169-unbounded-escape.bin"));

    assert_that!(summary.frames).is_equal_to(1);
}

// Regression (t-169): huffspec_fxp.cpp kept coefficients of the previous
// section in the spectrum, which then indexed past inverseQuantTable.
#[test]
fn decode_stale_spectrum_is_cleared() {
    let summary = decode(include_bytes!("../regressions/t169-stale-spectrum.bin"));

    assert_that!(summary.frames).is_equal_to(1);
}

// Regression (t-169): a reserved sampling frequency index (0xc..0xf) in an
// ADTS header indexed past samp_rate_info.
#[test]
fn decode_reserved_adts_sampling_index_does_not_index_past_the_table() {
    let summary = decode(include_bytes!(
        "../regressions/t169-sampling-index-adts.bin"
    ));

    assert_that!(summary.decoded).is_equal_to(0);
}

// Regression (t-169): a reserved sampling frequency index in a program
// config element indexed past samp_rate_info; the config is now rejected.
#[test]
fn decode_reserved_pce_sampling_index_rejects_the_config() {
    let summary = decode(include_bytes!("../regressions/t169-sampling-index-pce.bin"));

    assert_that!(summary.init).is_not_equal_to(0);
}

// Regression (t-169): get_tns.cpp let the filter bottom go negative (start
// band -59 with this input), and apply_tns() read win_sfb_top[] before the
// table. ASan reports it only when the address falls in a redzone, so
// without the patch this test fails on most runs, not all.
#[test]
fn decode_tns_negative_bottom_is_clamped() {
    let summary = decode(include_bytes!(
        "../regressions/t169-tns-negative-bottom.bin"
    ));

    assert_that!(summary.frames).is_equal_to(3);
}

// Regression (t-169): an SBR crossover band at or past the master frequency
// table read past it (sbr_reset_dec.cpp); the header is now rejected.
#[test]
fn decode_sbr_crossover_past_the_master_table_is_rejected() {
    let summary = decode(include_bytes!("../regressions/t169-sbr-crossover.bin"));

    assert_that!(summary.frames).is_equal_to(7);
}

// Regression (t-169): switching between plain AAC and SBR frames left
// ltp_buffer_state in the other layout, and sbr_dec() copied 288 samples
// before ltp_buffer, over mc_info.bDownSampledSbr (UBSan: invalid bool).
#[test]
fn decode_mode_switch_keeps_the_ltp_buffer_state_in_range() {
    let summary = decode(include_bytes!("../regressions/t169-ltp-buffer-state.bin"));

    assert_that!(summary.frames).is_equal_to(10);
}

// Regression (t-169): the IMDCT exponent made exp + 15 - SCALING negative,
// a negative shift count (trans4m_freq_2_time_fxp.cpp).
#[test]
fn decode_imdct_shift_stays_in_range() {
    let summary = decode(include_bytes!("../regressions/t169-imdct-shift.bin"));

    assert_that!(summary.frames).is_equal_to(2);
}

// Regression (t-169): the M/S q-format difference gave a shift of 51
// (ms_synt.cpp).
#[test]
fn decode_ms_shift_stays_in_range() {
    let summary = decode(include_bytes!("../regressions/t169-ms-shift.bin"));

    assert_that!(summary.frames).is_equal_to(2);
}

// Regression (t-169): an SBR header left upsamplingFactor at 0, and the bit
// rate was shifted by -1 (pvmp4audiodecoderframe.cpp).
#[test]
fn decode_zero_upsampling_factor_does_not_shift_the_bit_rate_negatively() {
    let summary = decode(include_bytes!("../regressions/t169-bitrate-shift.bin"));

    assert_that!(summary.frames).is_equal_to(8);
}

// Regression (t-169): a truncated SBR element made buf_getbits() refill
// past the SBR payload, into the stream structure (heap overflow).
#[test]
fn decode_sbr_bit_reader_stays_in_the_payload() {
    let summary = decode(include_bytes!(
        "../regressions/t169-sbr-payload-overrun.bin"
    ));

    assert_that!(summary.frames).is_equal_to(5);
}

// Regression (t-169): extractFrameInfo() accepted more envelopes than its
// tables hold and decreasing borders (index past the frame grid tables).
#[test]
fn decode_sbr_frame_borders_are_checked() {
    let summary = decode(include_bytes!("../regressions/t169-sbr-frame-borders.bin"));

    assert_that!(summary.frames).is_equal_to(3);
}

// Regression (t-169): ADTS frames kept SBR active after a rejected header,
// and the high band was generated from stale frequency tables.
#[test]
fn decode_sbr_without_frequency_tables_is_rejected() {
    let summary = decode(include_bytes!("../regressions/t169-sbr-stale-tables.bin"));

    assert_that!(summary.frames).is_equal_to(10);
}

// Regression (t-169): a master table of tiny bands built more than
// MAX_NUM_PATCHES patches and overflowed the limiter's stack arrays.
#[test]
fn decode_sbr_patch_count_is_bounded() {
    let summary = decode(include_bytes!("../regressions/t169-sbr-patch-count.bin"));

    assert_that!(summary.frames).is_equal_to(15);
}

// Regression (t-169): SBR envelope exponents differing by 32 or more gave
// out-of-range shifts (calc_sbr_envelope.cpp).
#[test]
fn decode_sbr_envelope_shift_stays_in_range() {
    let summary = decode(include_bytes!("../regressions/t169-sbr-envelope-shift.bin"));

    assert_that!(summary.frames).is_equal_to(3);
}

// Regression (t-169): a program config element without a front element
// left a stale channel layout, and the frame loop indexed past pChVars.
#[test]
fn decode_program_config_without_front_element_is_rejected() {
    let summary = decode(include_bytes!("../regressions/t169-pce-without-front.bin"));

    assert_that!(summary.frames).is_equal_to(5);
}

// Regression (t-169): an inconsistent window group table walked
// pns_intensity_right() past mask[] (heap overflow).
#[test]
fn decode_pns_walk_stops_on_inconsistent_groups() {
    let summary = decode(include_bytes!("../regressions/t169-pns-groups.bin"));

    assert_that!(summary.frames).is_equal_to(64);
}

// Regression (t-169): the short-window overlap prefetched one coefficient
// past the window table on its last iteration (global overflow).
#[test]
fn decode_short_window_overlap_stays_in_the_table() {
    let summary = decode(include_bytes!(
        "../regressions/t169-short-window-prefetch.bin"
    ));

    assert_that!(summary.frames).is_equal_to(2);
}

// Regression (t-169): an LTP prediction with a Q format of 0 shifted the
// window by -1 (trans4m_time_2_freq_fxp.cpp).
#[test]
fn decode_ltp_q_format_below_one_does_not_shift_negatively() {
    let summary = decode(include_bytes!("../regressions/t169-ltp-q-format.bin"));

    assert_that!(summary.frames).is_equal_to(10);
}

// Regression (t-169): explicitly signalled PS ran without its buffers, and
// its pointer arrays overlapped on a 64-bit host (ps_hybrid_filter_bank_allocation.cpp).
#[test]
fn decode_explicit_ps_has_its_buffers() {
    let summary = decode(include_bytes!("../regressions/t169-explicit-ps.bin"));

    assert_that!(summary.frames).is_equal_to(4);
}
