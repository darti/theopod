//! Decoder benchmark on core1 (`just audio-bench`, t-164, ADR-0026).
//!
//! Runs as a task in core1's executor next to the SD owner, as ADR-0026
//! proposes. Copies 30 s clips (AAC-LC 48 kHz stereo 192 kbps raw access
//! units, MP3 128 kbps 44.1 kHz) into PSRAM, then decodes every frame with
//! Helix AAC, Helix MP3 and nanomp3-core, the decoder state in internal RAM then in
//! PSRAM. Logs cycles per frame (CCOUNT, interrupts included), the MHz
//! needed for real time and the stack depth of one decode call.

use core::ffi::c_void;

use allocator_api2::{boxed::Box, vec::Vec};
use esp_alloc::{ExternalMemory, InternalMemory};
use esp_hal::xtensa_lx::timer::get_cycle_count;

const CPU_HZ: u64 = 240_000_000;
/// Stack below the measuring frame left unpainted: the paint loop's own frame.
const PAINT_MARGIN: usize = 512;
const PAINT: u32 = 0x5a5a_5a5a;

static AAC: &[u8] = include_bytes!(concat!(env!("THEO_AUDIO_CLIPS"), "/aac.bin"));
static AAC_LEN: &[u8] = include_bytes!(concat!(env!("THEO_AUDIO_CLIPS"), "/aac.len"));
static MP3: &[u8] = include_bytes!(concat!(env!("THEO_AUDIO_CLIPS"), "/mp3.bin"));

#[repr(C)]
#[derive(Default)]
struct AacFrameInfo {
    bit_rate: i32,
    n_chans: i32,
    samp_rate_core: i32,
    samp_rate_out: i32,
    bits_per_sample: i32,
    output_samps: i32,
    profile: i32,
    tns_used: i32,
    pns_used: i32,
}

unsafe extern "C" {
    fn theo_helix_aac_state_size() -> i32;
    fn AACInitDecoderPre(ptr: *mut c_void, size: i32) -> *mut c_void;
    fn AACSetRawBlockParams(decoder: *mut c_void, copy_last: i32, info: *mut AacFrameInfo) -> i32;
    fn AACDecode(
        decoder: *mut c_void,
        input: *mut *mut u8,
        bytes_left: *mut i32,
        output: *mut i16,
    ) -> i32;
    fn theo_helix_arena(ptr: *mut c_void, size: i32);
    fn theo_helix_arena_used() -> i32;
    fn MP3InitDecoder() -> *mut c_void;
    fn MP3FindSyncWord(input: *mut u8, bytes: i32) -> i32;
    fn MP3Decode(
        decoder: *mut c_void,
        input: *mut *mut u8,
        bytes_left: *mut i32,
        output: *mut i16,
        use_size: i32,
    ) -> i32;
}

/// Helix MP3's "no main data yet" (bit reservoir) and "input exhausted".
const ERR_MP3_INDATA_UNDERFLOW: i32 = -1;
const ERR_MP3_MAINDATA_UNDERFLOW: i32 = -2;
/// Bump arena Helix MP3 allocates its state from (shim `helix_malloc`).
const MP3_ARENA: usize = 32 * 1024;

/// Per-run statistics, logged as one line.
struct Stats {
    cycles: Vec<u32, &'static ExternalMemory>,
    stack: usize,
    peak: i16,
    errors: u32,
}

impl Stats {
    fn new() -> Self {
        Self {
            cycles: Vec::new_in(&ExternalMemory),
            stack: 0,
            peak: 0,
            errors: 0,
        }
    }

    fn log(mut self, name: &str, frames_per_s_x1000: u64) {
        let n = self.cycles.len();
        if n == 0 {
            log::error!(
                "audio-bench {name}: no frame decoded ({} errors)",
                self.errors
            );
            return;
        }
        let total: u64 = self.cycles.iter().map(|&c| u64::from(c)).sum();
        self.cycles.sort_unstable();
        let mean = total / n as u64;
        let median = self.cycles[n / 2];
        let p99 = self.cycles[n * 99 / 100];
        let max = self.cycles[n - 1];
        // MHz for real time = cycles per frame x frames per second.
        let khz = mean * frames_per_s_x1000 / 1_000_000;
        let worst_ms_x100 = u64::from(max) * 100_000 / CPU_HZ;
        log::info!(
            "audio-bench {name}: frames={n} errors={} mean={mean} median={median} p99={p99} max={max} cycles/frame; realtime={}.{:03} MHz ({}.{} % of a 240 MHz core); worst frame {}.{:02} ms; stack={} B; peak={}",
            self.errors,
            khz / 1000,
            khz % 1000,
            khz / 2400,
            (khz % 2400) / 240,
            worst_ms_x100 / 100,
            worst_ms_x100 % 100,
            self.stack,
            self.peak,
        );
    }
}

/// Runs `decode` once with the free stack below this frame painted, and
/// returns the bytes of stack it (and any interrupt meanwhile) touched.
#[inline(never)]
fn stack_depth(stack_bottom: usize, decode: &mut dyn FnMut()) -> usize {
    let marker = 0u32;
    let top = core::ptr::addr_of!(marker) as usize - PAINT_MARGIN;
    let bottom = (stack_bottom + 256 + 3) & !3;
    let mut at = bottom;
    while at < top {
        // SAFETY: [bottom, top) is core1's own free stack below this frame;
        // nothing else lives there and the loop itself makes no call.
        unsafe { core::ptr::write_volatile(at as *mut u32, PAINT) };
        at += 4;
    }
    decode();
    let mut at = bottom;
    // SAFETY: same region, read back after the call returned.
    while at < top && unsafe { core::ptr::read_volatile(at as *const u32) } == PAINT {
        at += 4;
    }
    core::ptr::addr_of!(marker) as usize - at
}

fn peak(pcm: &[i16]) -> i16 {
    pcm.iter().map(|s| s.saturating_abs()).max().unwrap_or(0)
}

async fn helix_aac<A: allocator_api2::alloc::Allocator>(
    alloc: A,
    clip: &mut [u8],
    sizes: &[u8],
    stack_bottom: usize,
) -> Stats {
    let mut stats = Stats::new();
    // SAFETY: plain C function without arguments.
    let size = unsafe { theo_helix_aac_state_size() } as usize;
    let mut state = Vec::with_capacity_in(size, &alloc);
    state.resize(size, 0u8);
    let mut pcm = Vec::with_capacity_in(2 * 1024, &alloc);
    pcm.resize(2 * 1024, 0i16);
    // SAFETY: `state` outlives the decoder and holds `size` bytes, the size
    // AACInitDecoderPre consumes; Helix keeps no other reference.
    let decoder = unsafe { AACInitDecoderPre(state.as_mut_ptr().cast(), size as i32) };
    let mut info = AacFrameInfo {
        n_chans: 2,
        samp_rate_core: 48_000,
        profile: 1,
        ..AacFrameInfo::default()
    };
    // SAFETY: valid decoder and frame info.
    if decoder.is_null() || unsafe { AACSetRawBlockParams(decoder, 0, &mut info) } != 0 {
        log::error!("audio-bench helix-aac: init failed (state {size} B)");
        return stats;
    }
    log::info!("audio-bench helix-aac: state {size} B");
    let mut offset = 0;
    for (index, len) in sizes.chunks_exact(2).enumerate() {
        let len = usize::from(u16::from_le_bytes([len[0], len[1]]));
        let frame = &mut clip[offset..offset + len];
        offset += len;
        let mut input = frame.as_mut_ptr();
        let mut left = len as i32;
        let mut result = 0;
        let mut call = || {
            // SAFETY: `input`/`left` describe one whole access unit, `pcm`
            // holds 2 x 1024 samples, the maximum for stereo AAC-LC.
            result = unsafe { AACDecode(decoder, &mut input, &mut left, pcm.as_mut_ptr()) };
        };
        if index.is_multiple_of(64) {
            stats.stack = stats.stack.max(stack_depth(stack_bottom, &mut call));
        } else {
            let start = get_cycle_count();
            call();
            let cycles = get_cycle_count().wrapping_sub(start);
            stats.cycles.push(cycles);
        }
        if result != 0 || left != 0 {
            stats.errors += 1;
        }
        stats.peak = stats.peak.max(peak(&pcm));
        embassy_futures::yield_now().await;
    }
    stats
}

async fn nanomp3<A: allocator_api2::alloc::Allocator>(
    alloc: A,
    clip: &[u8],
    stack_bottom: usize,
) -> Stats {
    let mut stats = Stats::new();
    // The 18.6 KB decoder must not be built on core1's stack (t-121): copy
    // a const-initialized value straight into the allocation.
    static FRESH: nanomp3_core::Decoder = nanomp3_core::Decoder::new();
    let Ok(mut decoder) = Box::<nanomp3_core::Decoder, _>::try_new_uninit_in(&alloc) else {
        log::error!("audio-bench nanomp3: no memory");
        return stats;
    };
    // SAFETY: FRESH is a valid decoder made only of plain data, and the copy
    // fully initializes the allocation before assume_init.
    let mut decoder = unsafe {
        core::ptr::copy_nonoverlapping(&raw const FRESH, decoder.as_mut_ptr(), 1);
        decoder.assume_init()
    };
    log::info!(
        "audio-bench nanomp3: state {} B",
        core::mem::size_of::<nanomp3_core::Decoder>()
    );
    let mut pcm = Vec::with_capacity_in(nanomp3_core::MAX_SAMPLES_PER_FRAME, &alloc);
    pcm.resize(nanomp3_core::MAX_SAMPLES_PER_FRAME, 0i16);
    let mut offset = 0;
    let mut index = 0usize;
    while offset < clip.len() {
        let input = &clip[offset..];
        let mut outcome = (0, Err(nanomp3_core::DecodeError::NoFrame));
        let mut call = || outcome = decoder.decode(input, &mut pcm);
        if index.is_multiple_of(64) {
            stats.stack = stats.stack.max(stack_depth(stack_bottom, &mut call));
        } else {
            let start = get_cycle_count();
            call();
            let cycles = get_cycle_count().wrapping_sub(start);
            if outcome.1.is_ok() {
                stats.cycles.push(cycles);
            }
        }
        index += 1;
        let (used, result) = outcome;
        if used == 0 {
            break;
        }
        offset += used;
        match result {
            Ok(_) => stats.peak = stats.peak.max(peak(&pcm)),
            // The first frame has no reservoir; the cut-off last one no frame.
            Err(
                nanomp3_core::DecodeError::ReservoirUnavailable(_)
                | nanomp3_core::DecodeError::NoFrame,
            ) => (),
            Err(_) => stats.errors += 1,
        }
        embassy_futures::yield_now().await;
    }
    stats
}

async fn helix_mp3<A: allocator_api2::alloc::Allocator>(
    alloc: A,
    clip: &mut [u8],
    stack_bottom: usize,
) -> Stats {
    let mut stats = Stats::new();
    let mut arena = Vec::with_capacity_in(MP3_ARENA, &alloc);
    arena.resize(MP3_ARENA, 0u8);
    let mut pcm = Vec::with_capacity_in(2 * 1152, &alloc);
    pcm.resize(2 * 1152, 0i16);
    // SAFETY: `arena` outlives the decoder (both dropped at the end of this
    // function) and is only handed out by the shim's helix_malloc.
    let decoder = unsafe {
        theo_helix_arena(arena.as_mut_ptr().cast(), MP3_ARENA as i32);
        MP3InitDecoder()
    };
    if decoder.is_null() {
        log::error!("audio-bench helix-mp3: init failed (arena {MP3_ARENA} B)");
        return stats;
    }
    // SAFETY: plain C function without arguments.
    let used = unsafe { theo_helix_arena_used() };
    log::info!("audio-bench helix-mp3: state {used} B");
    let mut input = clip.as_mut_ptr();
    let mut left = clip.len() as i32;
    // SAFETY: `input`/`left` describe the clip.
    let sync = unsafe { MP3FindSyncWord(input, left) };
    if sync < 0 {
        log::error!("audio-bench helix-mp3: no sync word");
        return stats;
    }
    // SAFETY: `sync` is an offset inside the clip.
    input = unsafe { input.add(sync as usize) };
    left -= sync;
    let mut index = 0usize;
    while left > 0 {
        let before = left;
        let mut result = 0;
        let mut call = || {
            // SAFETY: `input`/`left` stay within the clip (Helix advances
            // both), `pcm` holds 2 x 1152 samples, a stereo Layer III frame.
            result = unsafe { MP3Decode(decoder, &mut input, &mut left, pcm.as_mut_ptr(), 0) };
        };
        if index.is_multiple_of(64) {
            stats.stack = stats.stack.max(stack_depth(stack_bottom, &mut call));
        } else {
            let start = get_cycle_count();
            call();
            let cycles = get_cycle_count().wrapping_sub(start);
            if result == 0 {
                stats.cycles.push(cycles);
            }
        }
        index += 1;
        match result {
            0 => stats.peak = stats.peak.max(peak(&pcm)),
            ERR_MP3_MAINDATA_UNDERFLOW => (),
            ERR_MP3_INDATA_UNDERFLOW => break,
            _ => stats.errors += 1,
        }
        if left == before {
            break;
        }
        embassy_futures::yield_now().await;
    }
    stats
}

/// Runs every benchmark once, then idles.
#[embassy_executor::task]
pub async fn run(stack_bottom: usize) {
    // Let boot, logging and the SD owner settle first.
    embassy_time::Timer::after_secs(5).await;
    let mut aac = Vec::with_capacity_in(AAC.len(), &ExternalMemory);
    aac.extend_from_slice(AAC);
    let mut mp3 = Vec::with_capacity_in(MP3.len(), &ExternalMemory);
    mp3.extend_from_slice(MP3);
    log::info!(
        "audio-bench: clips in PSRAM: aac {} B / {} frames, mp3 {} B; stack bottom {stack_bottom:#x}",
        aac.len(),
        AAC_LEN.len() / 2,
        mp3.len()
    );
    // 48000 / 1024 and 44100 / 1152 frames per second, x1000.
    const AAC_FPS: u64 = 46_875;
    const MP3_FPS: u64 = 38_281;
    helix_aac(&InternalMemory, &mut aac, AAC_LEN, stack_bottom)
        .await
        .log("helix-aac state=internal", AAC_FPS);
    helix_aac(&ExternalMemory, &mut aac, AAC_LEN, stack_bottom)
        .await
        .log("helix-aac state=psram", AAC_FPS);
    helix_mp3(&InternalMemory, &mut mp3, stack_bottom)
        .await
        .log("helix-mp3 state=internal", MP3_FPS);
    helix_mp3(&ExternalMemory, &mut mp3, stack_bottom)
        .await
        .log("helix-mp3 state=psram", MP3_FPS);
    nanomp3(&InternalMemory, &mp3, stack_bottom)
        .await
        .log("nanomp3 state=internal", MP3_FPS);
    nanomp3(&ExternalMemory, &mp3, stack_bottom)
        .await
        .log("nanomp3 state=psram", MP3_FPS);
    log::info!("audio-bench: done");
}
