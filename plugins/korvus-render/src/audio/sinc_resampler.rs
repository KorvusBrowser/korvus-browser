//! Blink's `SincResampler` (third_party/blink/renderer/platform/audio/sinc_resampler.cc, Chromium 150): the
//! converter `AudioBus::TryCreateBySampleRateConverting` runs one channel at a time when `decodeAudioData`
//! hands back a file whose rate is not the context's (8 kHz PCM decoded in a 48 kHz context comes back at
//! 48 kHz, six times as long -- Chrome 150, measured).
//!
//! The arithmetic is the x86 build's, step for step, because the samples are what a page hashes: the 33
//! windowed-sinc kernels in double through fdlibm's `sin`/`cos`, stored as float; each output sample two
//! float "convolutions" of 32 taps over a 512-frame block, accumulated the way the SSE path accumulates them
//! -- single taps until the input pointer is 16-byte aligned, then four lanes added pairwise once at the end,
//! then the leftover taps -- and the two blended in double.

extern "C" {
    fn korvus_fdlibm_sin(x: f64) -> f64;
    fn korvus_fdlibm_cos(x: f64) -> f64;
}

const BLOCK_SIZE: usize = 512;
const KERNEL_SIZE: usize = 32;
const KERNEL_OFFSETS: usize = 32;

/// One channel's resampler state (`SincResampler`'s members). `input` is the class's `input_buffer_`, an
/// `AudioFloatArray` -- aligned (to 32 bytes on x86) and zeroed -- so a frame index's alignment is its own.
struct SincResampler {
    scale_factor: f64,
    kernels: Vec<f32>,
    virtual_source_index: f64,
    input: Vec<f32>,
    primed: bool,
}

impl SincResampler {
    fn new(scale_factor: f64) -> Self {
        let mut r = SincResampler {
            scale_factor,
            kernels: vec![0.0; KERNEL_SIZE * (KERNEL_OFFSETS + 1)],
            virtual_source_index: 0.0,
            input: vec![0.0; BLOCK_SIZE + KERNEL_SIZE],
            primed: false,
        };
        r.initialize_kernel();
        r
    }

    /// `InitializeKernel()`: Blackman-windowed sinc kernels at 33 sub-sample offsets.
    fn initialize_kernel(&mut self) {
        let alpha = 0.16;
        let a0 = 0.5 * (1.0 - alpha);
        let a1 = 0.5;
        let a2 = 0.5 * alpha;
        let mut sinc_scale_factor = if self.scale_factor > 1.0 { 1.0 / self.scale_factor } else { 1.0 };
        sinc_scale_factor *= 0.9;
        let n = KERNEL_SIZE as i32;
        let half_size = n / 2;
        let pi = std::f64::consts::PI;
        let two_pi = 2.0 * std::f64::consts::PI;
        for offset_index in 0..=KERNEL_OFFSETS {
            let subsample_offset = offset_index as f64 / KERNEL_OFFSETS as f64;
            for i in 0..n {
                let s = sinc_scale_factor * pi * ((i - half_size) as f64 - subsample_offset);
                // SAFETY: pure functions of one double.
                let mut sinc = if s == 0.0 { 1.0 } else { (unsafe { korvus_fdlibm_sin(s) }) / s };
                sinc *= sinc_scale_factor;
                let x = (i as f64 - subsample_offset) / n as f64;
                // SAFETY: as above.
                let window = unsafe { a0 - a1 * korvus_fdlibm_cos(two_pi * x) + a2 * korvus_fdlibm_cos(two_pi * 2.0 * x) };
                self.kernels[i as usize + offset_index * KERNEL_SIZE] = (sinc * window) as f32;
            }
        }
    }

    /// `Process(AudioSourceProvider*, destination)`, the provider being `BufferSourceProvider` over `source`
    /// (`cursor` is how much of it has been consumed; past its end it reads zeros).
    fn process(&mut self, source: &[f32], cursor: &mut usize, destination: &mut [f32]) {
        let consume = |input: &mut [f32], cursor: &mut usize| {
            let take = input.len().min(source.len().saturating_sub(*cursor));
            input[..take].copy_from_slice(&source[*cursor..*cursor + take]);
            input[take..].fill(0.0);
            *cursor += take;
        };
        // r0 = input + K/2, r1 = input, r2 = r0, r3 = r0 + (BLOCK - K/2), r4 = r0 + BLOCK, r5 = r0 + K/2.
        let half = KERNEL_SIZE / 2;
        let (r0, r3, r4, r5) = (half, BLOCK_SIZE, half + BLOCK_SIZE, KERNEL_SIZE);
        if !self.primed {
            consume(&mut self.input[r0..r0 + BLOCK_SIZE + half], cursor);
            self.primed = true;
        }
        let mut out = 0usize;
        loop {
            while self.virtual_source_index < BLOCK_SIZE as f64 {
                let source_index_i = self.virtual_source_index as usize;
                let subsample_remainder = self.virtual_source_index - source_index_i as f64;
                let virtual_offset_index = subsample_remainder * KERNEL_OFFSETS as f64;
                let offset_index = virtual_offset_index as usize;
                let k1 = &self.kernels[offset_index * KERNEL_SIZE..offset_index * KERNEL_SIZE + KERNEL_SIZE];
                let k2 = &self.kernels[(offset_index + 1) * KERNEL_SIZE..(offset_index + 1) * KERNEL_SIZE + KERNEL_SIZE];
                let input = &self.input[source_index_i..source_index_i + KERNEL_SIZE];
                let kernel_interpolation_factor = virtual_offset_index - offset_index as f64;
                let (sum1, sum2) = convolve_x86(input, k1, k2, source_index_i % 4);
                let result = (1.0 - kernel_interpolation_factor) * sum1 as f64 + kernel_interpolation_factor * sum2 as f64;
                destination[out] = result as f32;
                out += 1;
                self.virtual_source_index += self.scale_factor;
                if out == destination.len() {
                    return;
                }
            }
            self.virtual_source_index -= BLOCK_SIZE as f64;
            // Step (3): the last input frames wrap back to the start of the buffer.
            self.input.copy_within(r3..r3 + half, 0);
            self.input.copy_within(r4..r4 + half, r0);
            // Step (4): refresh the buffer with more input.
            consume(&mut self.input[r5..r5 + BLOCK_SIZE], cursor);
        }
    }
}

/// The two 32-tap sums as the SSE build forms them: `misalign` is the input frame's index modulo 4 (the
/// buffer itself is 16-byte aligned), so `(4 - misalign) % 4` taps are added singly first, then whole groups
/// of four go to four lane accumulators (summed `((l0 + l1) + l2) + l3` into the scalar), then the rest singly.
fn convolve_x86(input: &[f32], k1: &[f32], k2: &[f32], misalign: usize) -> (f32, f32) {
    let mut n = KERNEL_SIZE;
    let mut i = 0usize;
    let (mut sum1, mut sum2) = (0.0f32, 0.0f32);
    let mut lead = (4 - misalign) % 4;
    while lead > 0 && n > 0 {
        sum1 += input[i] * k1[i];
        sum2 += input[i] * k2[i];
        i += 1;
        n -= 1;
        lead -= 1;
    }
    let sse_frames = n - n % 4;
    let (mut s1, mut s2) = ([0.0f32; 4], [0.0f32; 4]);
    let mut j = 0;
    while j < sse_frames {
        // `_mm_mul_ps` then `_mm_add_ps`, lane by lane
        let (x, a, b) = (&input[i..i + 4], &k1[i..i + 4], &k2[i..i + 4]);
        for (lane, (acc1, acc2)) in s1.iter_mut().zip(s2.iter_mut()).enumerate() {
            *acc1 += x[lane] * a[lane];
            *acc2 += x[lane] * b[lane];
        }
        i += 4;
        j += 4;
    }
    sum1 += s1[0] + s1[1] + s1[2] + s1[3];
    sum2 += s2[0] + s2[1] + s2[2] + s2[3];
    n %= 4;
    while n > 0 {
        sum1 += input[i] * k1[i];
        sum2 += input[i] * k2[i];
        i += 1;
        n -= 1;
    }
    (sum1, sum2)
}

/// `AudioBus::TryCreateBySampleRateConverting` for one channel: `source.len() / ratio` frames (truncated), the
/// whole buffer resampled in 512-frame calls of one resampler. `None` when the length does not fit an `int`.
pub fn resample(source: &[f32], source_rate: f64, destination_rate: f64) -> Option<Vec<f32>> {
    let ratio = source_rate / destination_rate;
    let length = source.len() as f64 / ratio;
    if !(0.0..=i32::MAX as f64).contains(&length) {
        return None;
    }
    let length = length as usize;
    if source.iter().all(|s| *s == 0.0) {
        return Some(vec![0.0; length]);
    }
    let mut resampler = SincResampler::new(ratio);
    let mut out = vec![0.0f32; length];
    let mut cursor = 0usize;
    for chunk in out.chunks_mut(BLOCK_SIZE) {
        resampler.process(source, &mut cursor, chunk);
    }
    Some(out)
}
