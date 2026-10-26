//! Chrome's Web Audio FFT since Chrome 152: the `rustfft` crate behind Blink's `FFTFrame`
//! (`third_party/blink/renderer/platform/audio/rustfft_ffi.rs`, enabled by the stable `WebAudioRustFft` runtime
//! flag; it replaced PFFFT). Only the inverse real transform of an even size is here -- the one Blink's
//! `PeriodicWaveImpl::CreateBandLimitedTables` calls through `FFTFrame::DoInverseFFT` -- and it is the upstream
//! code's arithmetic step for step: a half-size complex FFT planned by `FftPlanner::new()` (which picks the same
//! AVX / SSE kernels at run time that Chrome's build of the same crate version picks on the same CPU), the
//! twiddles computed in double and stored as float, the packed spectrum unpacked with the same 0.5 factors, and
//! the result scaled by `1 / half_size`.
//!
//! Ported from Chromium's `rustfft_ffi.rs`:
//! Copyright 2026 The Chromium Authors. Use of this source code is governed by a BSD-style license (Chromium's
//! LICENSE, listed in the release's third-party notices).

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// `prepare_inverse_fft_even`: the half-size complex input from Blink's packed spectrum (`real[0]` the DC
/// component, `imag[0]` the Nyquist one).
fn prepare_inverse_even(real: &[f32], imag: &[f32], twiddles: &[Complex<f32>], half: usize, z: &mut [Complex<f32>]) {
    let limit = (half - 1) / 2;
    z[0] = Complex::new(0.5 * (real[0] + imag[0]), 0.5 * (real[0] - imag[0]));
    for k in 1..=limit {
        let x_k = Complex::new(real[k], imag[k]);
        let x_mk_conj = Complex::new(real[half - k], -imag[half - k]);
        let f_even = (x_k + x_mk_conj) * 0.5;
        let x_diff = x_k - x_mk_conj;
        let f_odd = twiddles[k].conj() * x_diff;
        let i_f_odd = Complex::new(-f_odd.im, f_odd.re);
        z[k] = f_even + i_f_odd;
        z[half - k] = (f_even - i_f_odd).conj();
    }
    if half.is_multiple_of(2) {
        let k = half / 2;
        z[k] = Complex::new(real[k], -imag[k]);
    }
}

/// `RustFft::do_inverse_fft` for an even `size`: `real` and `imag` are the packed spectrum (`size / 2` each);
/// the time-domain result is `size` floats.
pub(super) fn inverse_real_even(real: &[f32], imag: &[f32], size: usize) -> Vec<f32> {
    debug_assert!(size.is_multiple_of(2) && real.len() >= size / 2 && imag.len() >= size / 2);
    let half = size / 2;
    let mut planner = FftPlanner::<f32>::new();
    let inverse = planner.plan_fft_inverse(half);
    let twiddles: Vec<Complex<f32>> = (0..half)
        .map(|k| {
            let angle = -2.0 * std::f64::consts::PI * (k as f64) / (size as f64);
            let (sin, cos) = angle.sin_cos();
            Complex::new((0.5 * cos) as f32, (0.5 * sin) as f32)
        })
        .collect();
    let mut z = vec![Complex::new(0.0f32, 0.0); half];
    prepare_inverse_even(real, imag, &twiddles, half, &mut z);
    let mut scratch = vec![Complex::new(0.0f32, 0.0); inverse.get_inplace_scratch_len()];
    inverse.process_with_scratch(&mut z, &mut scratch);
    let scale = 1.0 / (half as f32);
    let mut out = Vec::with_capacity(size);
    for c in &z {
        out.push(c.re * scale);
        out.push(c.im * scale);
    }
    out
}
