//! The Web Audio DSP behind [`spec::RenderBackend::render_audio`] and
//! [`spec::RenderBackend::query_audio`] — R3.5.
//!
//! ## What was here before
//!
//! Nothing. Web Audio was a **256-entry recorded tape**, tiled with `%` across whatever length the
//! caller asked for. `getChannelData` served `tape[i % 256]`, `getFloatFrequencyData` served
//! `freq[i % 256]`, and `startRendering()` fabricated an `AudioBuffer` without ever looking at the
//! graph. Three one-expression detections followed from that and all three were verified live: a
//! fresh `AnalyserNode` with nothing connected read a plausible noise floor instead of `-Infinity`,
//! the 1024-bin spectrum had a detected period of exactly 256, and a 44,100-sample buffer that
//! repeats every 256 frames has energy only at multiples of 172.27 Hz — which a triangle wave
//! through a compressor does not.
//!
//! ## Why this is a from-scratch DSP and not a crate
//!
//! Same reason [`crate::gl`] is a from-scratch rasterizer: what is being measured is the *numbers*,
//! and a library picked for convenience answers a question about the library. Everything here is
//! plain arithmetic over `f32` — no allocator tricks, no SIMD, no platform math beyond `libm`
//! through `f32`/`f64` — so the same op list gives the same samples on every host this engine runs
//! on. That determinism is the property `render_calibration`-style comparisons need later, and it
//! is the property a hardware audio path would not have.
//!
//! ## Bit-identity is explicitly NOT the bar
//!
//! Brave farbles audio, the persona claims a Mac, and this is a Linux host. A hash match with a
//! real Brave 151 is not achievable and chasing it is how this seam gets stuck. The bar is that the
//! samples are a FUNCTION OF THE GRAPH: the same graph at 10 kHz and at 1 kHz must give different
//! output, an analyser must report the spectrum of what is actually connected to it, and nothing
//! anywhere may fall back to a recording.
//!
//! ## What computes, and what does not
//!
//! Computed here: `OscillatorNode` (band-limited wavetables, including `setPeriodicWave`),
//! `GainNode`, `DynamicsCompressorNode` (feed-forward, soft knee, pre-delay, envelope follower),
//! `AnalyserNode` (Blackman-windowed FFT with `smoothingTimeConstant`), `AudioBufferSourceNode`
//! (resampling, looping, `playbackRate`/`detune`), `ConstantSourceNode`, `DelayNode`,
//! `BiquadFilterNode` (all eight types), `StereoPannerNode`, `WaveShaperNode` (with 2x/4x
//! oversampling), `ChannelMergerNode`, `ChannelSplitterNode`, `AudioDestinationNode`, and the full
//! `AudioParam` automation timeline including a-rate param inputs.
//!
//! Not computed, and therefore not present in [`spec::AudioNodeKind`] at all: `ConvolverNode`,
//! `PannerNode` (HRTF), `IIRFilterNode`, `ScriptProcessorNode`, `AudioWorkletNode`,
//! `MediaElementAudioSourceNode` and the MediaStream family. A page that builds one of those gets a
//! node object whose surface is right and which contributes silence — which is visible — rather
//! than a variant the op list carries and nothing executes.

mod rust_fft;
mod sinc_resampler;

pub use sinc_resampler::resample;

use spec::{
    AudioNodeKind, AudioOp, AudioParamId, AudioProp, AudioQuery, AudioQueryResult, AutomationEvent,
    BiquadKind, OscillatorType,
};
use std::collections::HashMap;
use std::f64::consts::PI;

/// Web Audio's render quantum. Every parameter that is "k-rate" is evaluated once per quantum, and
/// the graph is walked once per quantum, exactly as the spec describes.
pub const QUANTUM: usize = 128;

/// The largest `fftSize` an `AnalyserNode` accepts, and therefore the analyser tap's ring size.
const MAX_FFT: usize = 32768;

// ── automation ────────────────────────────────────────────────────────────────────────────────

/// One `AudioParam`: its `.value` write, its default, and its automation timeline.
#[derive(Debug, Clone)]
struct Param {
    /// The most recent `.value =` write. The spec calls this the "intrinsic value"; it is what the
    /// param reads when the timeline has nothing to say at time `t`.
    value: f32,
    events: Vec<AutomationEvent>,
    min: f32,
    max: f32,
}

impl Param {
    fn new(default: f32, min: f32, max: f32) -> Self {
        Self { value: default, events: Vec::new(), min, max }
    }

    /// The time an event takes effect. `SetValueCurveAtTime` ends at `time + duration`, but it
    /// *starts* at `time`, and ordering is by start.
    fn event_time(e: &AutomationEvent) -> f64 {
        match e {
            AutomationEvent::SetValueAtTime { time, .. }
            | AutomationEvent::LinearRampToValueAtTime { time, .. }
            | AutomationEvent::ExponentialRampToValueAtTime { time, .. }
            | AutomationEvent::SetTargetAtTime { time, .. }
            | AutomationEvent::SetValueCurveAtTime { time, .. }
            | AutomationEvent::CancelScheduledValues { time }
            | AutomationEvent::CancelAndHoldAtTime { time } => *time,
        }
    }

    /// Blink's `AudioParamHandler::HasSampleAccurateValues` for the quantum starting at `t0` (without
    /// the connected-input case, which the caller knows): does the timeline move the value inside it?
    /// Blink drops the events wholly in the past as it renders, keeping the one in effect, so the rules
    /// read the events from that one on: none -> no; the first still to come and starting after this
    /// quantum (a set-value, set-target or curve) -> no; two or more -> yes; exactly one -> a
    /// set-target that has started, or a set-value / ramp not yet in the past.
    fn sample_accurate(&self, t0: f64, t1: f64) -> bool {
        let first = self.events.iter().rposition(|e| Self::event_time(e) < t0).unwrap_or(0);
        let live = &self.events[first..];
        let Some(e0) = live.first() else { return false };
        let time = Self::event_time(e0);
        if time > t1
            && matches!(e0, AutomationEvent::SetTargetAtTime { .. } | AutomationEvent::SetValueAtTime { .. } | AutomationEvent::SetValueCurveAtTime { .. })
        {
            return false;
        }
        if live.len() >= 2 {
            return true;
        }
        match e0 {
            AutomationEvent::SetTargetAtTime { .. } => time <= t1,
            AutomationEvent::SetValueCurveAtTime { duration, .. } => !(time + duration < t0),
            _ => !(time < t0),
        }
    }

    /// The value an event *arrives at*, for a ramp that starts from the previous event's endpoint.
    fn event_value(e: &AutomationEvent) -> Option<f32> {
        match e {
            AutomationEvent::SetValueAtTime { value, .. }
            | AutomationEvent::LinearRampToValueAtTime { value, .. }
            | AutomationEvent::ExponentialRampToValueAtTime { value, .. } => Some(*value),
            AutomationEvent::SetValueCurveAtTime { curve, .. } => curve.last().copied(),
            AutomationEvent::SetTargetAtTime { target, .. } => Some(*target),
            _ => None,
        }
    }

    fn push(&mut self, e: AutomationEvent) {
        match &e {
            // The cancels are applied when the timeline is built rather than kept as events: they
            // are edits to the schedule, and a renderer that had to re-derive "what is still
            // scheduled" on every sample would be re-deriving it 44,100 times a second.
            AutomationEvent::CancelScheduledValues { time } => {
                let t = *time;
                self.events.retain(|x| Self::event_time(x) < t);
            }
            AutomationEvent::CancelAndHoldAtTime { time } => {
                let t = *time;
                let held = self.timeline_value(t);
                self.events.retain(|x| Self::event_time(x) < t);
                self.events.push(AutomationEvent::SetValueAtTime { value: held, time: t });
            }
            _ => {
                self.events.push(e);
                self.events.sort_by(|a, b| {
                    Self::event_time(a).partial_cmp(&Self::event_time(b)).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }
    }

    /// The scheduled value at context time `t`, ignoring any connected a-rate input.
    ///
    /// The one thing worth stating: a ramp governs the interval BEFORE it, not after. `setValueAtTime(0, 0)`
    /// followed by `linearRampToValueAtTime(1, 1)` is a ramp across the whole first second, and reading
    /// the value at t = 0.5 as "whatever the last started event said" gives 0 for the entire second and
    /// then a step — an automation API that reads back correctly and does nothing.
    fn timeline_value(&self, t: f64) -> f32 {
        if self.events.is_empty() {
            return self.value.clamp(self.min, self.max);
        }
        // The last event that has already started, and the value the timeline holds at it.
        let mut idx: Option<usize> = None;
        for (i, e) in self.events.iter().enumerate() {
            if Self::event_time(e) <= t {
                idx = Some(i);
            } else {
                break;
            }
        }
        let (held, held_time) = match idx {
            None => (self.value, 0.0),
            Some(i) => (self.value_at_event(i, t), Self::event_time(&self.events[i])),
        };
        // A ramp still ahead of us is in flight NOW, from wherever the timeline currently sits.
        let next = idx.map(|i| i + 1).unwrap_or(0);
        if let Some(e) = self.events.get(next) {
            let from = match idx {
                None => self.value,
                Some(i) => self.value_after(i),
            };
            match e {
                AutomationEvent::LinearRampToValueAtTime { value, time } => {
                    return Self::lerp(from, *value, held_time, *time, t).clamp(self.min, self.max)
                }
                AutomationEvent::ExponentialRampToValueAtTime { value, time } => {
                    return Self::exp_ramp(from, *value, held_time, *time, t).clamp(self.min, self.max)
                }
                _ => {}
            }
        }
        held.clamp(self.min, self.max)
    }

    /// The value event `i` produces at time `t` (it may still be evolving — `setTargetAtTime` and
    /// `setValueCurveAtTime` both are).
    fn value_at_event(&self, i: usize, t: f64) -> f32 {
        let prev = if i == 0 { self.value } else { self.value_after(i - 1) };
        match &self.events[i] {
            AutomationEvent::SetValueAtTime { value, .. }
            | AutomationEvent::LinearRampToValueAtTime { value, .. }
            | AutomationEvent::ExponentialRampToValueAtTime { value, .. } => *value,
            AutomationEvent::SetTargetAtTime { target, time, time_constant } => {
                if *time_constant <= 0.0 {
                    *target
                } else {
                    let k = (-(t - *time) / *time_constant).exp();
                    (*target as f64 + (prev as f64 - *target as f64) * k) as f32
                }
            }
            AutomationEvent::SetValueCurveAtTime { curve, time, duration } => {
                Self::curve_at(curve, *time, *duration, t)
            }
            _ => prev,
        }
    }

    /// The value the timeline holds immediately after event `i` has finished.
    fn value_after(&self, i: usize) -> f32 {
        Self::event_value(&self.events[i]).unwrap_or(self.value)
    }

    fn lerp(v0: f32, v1: f32, t0: f64, t1: f64, t: f64) -> f32 {
        if t >= t1 || t1 <= t0 {
            return v1;
        }
        let a = ((t - t0) / (t1 - t0)) as f32;
        v0 + (v1 - v0) * a
    }

    /// An exponential ramp is geometric in value, and is undefined through zero — the spec says a
    /// ramp to or from 0 behaves as if the value jumped at the end time, which is what falling back
    /// to `v1` here does.
    fn exp_ramp(v0: f32, v1: f32, t0: f64, t1: f64, t: f64) -> f32 {
        if t >= t1 || t1 <= t0 || v0 == 0.0 || v1 == 0.0 || v0.signum() != v1.signum() {
            return v1;
        }
        let a = (t - t0) / (t1 - t0);
        (v0 as f64 * (v1 as f64 / v0 as f64).powf(a)) as f32
    }

    fn curve_at(curve: &[f32], t0: f64, dur: f64, t: f64) -> f32 {
        if curve.is_empty() {
            return 0.0;
        }
        if dur <= 0.0 || t >= t0 + dur {
            return curve[curve.len() - 1];
        }
        let x = (t - t0) / dur * (curve.len() - 1) as f64;
        let i = x.floor() as usize;
        let f = (x - i as f64) as f32;
        if i + 1 >= curve.len() {
            curve[curve.len() - 1]
        } else {
            curve[i] + (curve[i + 1] - curve[i]) * f
        }
    }
}

// ── oscillator wavetables ─────────────────────────────────────────────────────────────────────

/// A band-limited wavetable set: one table per octave of fundamental frequency, each holding only
/// the harmonics that fit under Nyquist for that octave.
///
/// This is why a 10 kHz triangle and a 1 kHz triangle are genuinely different signals here and not
/// two lookups into one table. At 44.1 kHz a 10 kHz triangle has exactly one harmonic under
/// Nyquist, so it *is* a sine; a 1 kHz triangle has eleven. A naive `2/pi * asin(sin)` generator
/// would alias both into the same hash of garbage and would still be "responding to the graph" in
/// the narrow sense — this responds to it correctly.
struct Wavetable {
    /// Chrome's band-limited tables, index 0 = the most partials.
    bands: Vec<Vec<f32>>,
    /// `nyquist / max_partials` -- Chrome's `lowest_fundamental_frequency_`.
    lowest: f64,
    /// The table length, `PeriodicWaveSize()` for this sample rate.
    size: usize,
}

/// Chrome's `PeriodicWave` geometry (`periodic_wave.cc`): a table of 2048 / 4096 / 16384 samples by
/// sample rate, `round(3 * log2(size))` ranges a third of an octave (400 cents) apart, range `r`
/// keeping partials `0..=2^(-r/3) * size/2` -- the `+ 1` in its clearing loop keeps the partial at the
/// cut, which is why a 10 kHz triangle at 44.1 kHz is its fundamental in BOTH bracketing tables.
/// This replaced octave bands whose upper table let a 30 kHz third harmonic alias into a 10 kHz
/// triangle (`first` 0.81374 against Chrome's 0.80210).
fn periodic_wave_size(sample_rate: f64) -> usize {
    if sample_rate <= 24000.0 {
        2048
    } else if sample_rate <= 88200.0 {
        4096
    } else {
        16384
    }
}

const CENTS_PER_RANGE: f64 = 1200.0 / 3.0;

impl Wavetable {
    /// Blink's `PeriodicWaveImpl::CreateBandLimitedTables` (modules/webaudio/periodic_wave.cc), in its
    /// float arithmetic: per range, the coefficients times the FFT size (the imaginary part negated),
    /// the partials the range culls zeroed, DC and the packed Nyquist zeroed, Chrome's own inverse FFT (Chrome 152's
    /// `rustfft` behind `FFTFrame`, [`rust_fft`]; it scales by itself), times the normalization scale --
    /// `1 / max|x|` of the richest range, or 0.5 when the page disabled normalization. Index 0 = DC (which Web Audio
    /// ignores).
    fn build(real: &[f32], imag: &[f32], sample_rate: f64, normalize: bool) -> Self {
        let size = periodic_wave_size(sample_rate);
        let half = size / 2;
        let sr = sample_rate as f32;
        let nyquist = 0.5f32 * sr;
        let lowest = (nyquist / half as f32) as f64;
        let n_ranges = (0.5f32 + 3.0 * (size as f32).log2()) as usize;
        let components = real.len().min(imag.len()).min(half);
        let mut bands = Vec::with_capacity(n_ranges);
        let mut normalization = 0.5f32;
        for r in 0..n_ranges {
            let scale = size as f32;
            let mut re = vec![0.0f32; half];
            let mut im = vec![0.0f32; half];
            for i in 0..components {
                re[i] = real[i] * scale;
                im[i] = imag[i] * -scale;
            }
            // `NumberOfPartialsForRange`: `pow(2, -cents / 1200)` in double, kept as float.
            let cents_to_cull = r as f32 * CENTS_PER_RANGE as f32;
            let culling_scale = 2f64.powf((-cents_to_cull / 1200.0) as f64) as f32;
            let partials = (culling_scale * half as f32) as usize;
            for i in components.min(partials + 1)..half {
                re[i] = 0.0;
                im[i] = 0.0;
            }
            re[0] = 0.0;
            im[0] = 0.0;
            let mut table = rust_fft::inverse_real_even(&re, &im, size);
            if normalize && r == 0 {
                let peak = table.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                if peak != 0.0 {
                    normalization = 1.0 / peak;
                }
            }
            for x in table.iter_mut() {
                *x *= normalization;
            }
            bands.push(table);
        }
        Self { bands, lowest, size }
    }

    /// Chrome's `rate_scale_ = PeriodicWaveSize() / sample_rate_`, a float division.
    fn rate_scale(&self, sample_rate: f64) -> f32 {
        self.size as f32 / sample_rate as f32
    }

    /// Chrome's `WaveDataForFundamentalFrequency`: the two bracketing band tables (`lower`, `higher`)
    /// and the blend factor between them, in float as Blink computes them.
    fn bands_for(&self, freq: f32) -> (&[f32], &[f32], f32) {
        let ratio = if freq > 0.0 { freq / self.lowest as f32 } else { 0.5 };
        let cents_above = ratio.log2() * 1200.0;
        let last = (self.bands.len() - 1) as f32;
        let pitch_range = (1.0 + cents_above / CENTS_PER_RANGE as f32).clamp(0.0, last);
        let r1 = pitch_range as usize;
        let r2 = if (r1 as f32) < last { r1 + 1 } else { r1 };
        (&self.bands[r2], &self.bands[r1], pitch_range - r1 as f32)
    }
}

/// The Fourier coefficients of the four built-in waveforms, `(real, imag)` with index 0 = DC, for a
/// table of `2 * half` samples: Blink's `GenerateBasicWaveform`, in float (`pi_factor = 2 / (n * pi)`).
fn builtin_coefficients(kind: OscillatorType, half: usize) -> (Vec<f32>, Vec<f32>) {
    let real = vec![0.0f32; half];
    let mut imag = vec![0.0f32; half];
    let pi = std::f32::consts::PI;
    for n in 1..half {
        let pi_factor = 2.0f32 / (n as f32 * pi);
        imag[n] = match kind {
            OscillatorType::Sine | OscillatorType::Custom => {
                if n == 1 { 1.0 } else { 0.0 }
            }
            OscillatorType::Square => {
                if n & 1 == 1 { 2.0 * pi_factor } else { 0.0 }
            }
            OscillatorType::Sawtooth => pi_factor * if n & 1 == 1 { 1.0 } else { -1.0 },
            OscillatorType::Triangle => {
                if n & 1 == 1 {
                    2.0 * (pi_factor * pi_factor) * if ((n - 1) >> 1) & 1 == 1 { -1.0 } else { 1.0 }
                } else {
                    0.0
                }
            }
        };
    }
    (real, imag)
}

// ── biquad ────────────────────────────────────────────────────────────────────────────────────

/// A direct-form-1 biquad, coefficients recomputed once per render quantum from the (possibly
/// automated) frequency/Q/gain/detune params.
#[derive(Debug, Clone, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: Vec<f64>,
    x2: Vec<f64>,
    y1: Vec<f64>,
    y2: Vec<f64>,
}

impl Biquad {
    fn set(&mut self, kind: BiquadKind, freq: f64, q: f64, gain_db: f64, sample_rate: f64) {
        let nyquist = sample_rate / 2.0;
        let f = (freq / nyquist).clamp(1e-6, 0.999_999);
        let w0 = PI * f;
        let (sn, cs) = (w0.sin(), w0.cos());
        let a = 10f64.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::Lowpass => {
                // Web Audio's lowpass/highpass use a resonance in dB rather than a raw Q.
                let alpha = sn / (2.0 * 10f64.powf(q / 20.0)).max(1e-9);
                let b1 = 1.0 - cs;
                (b1 / 2.0, b1, b1 / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
            }
            BiquadKind::Highpass => {
                let alpha = sn / (2.0 * 10f64.powf(q / 20.0)).max(1e-9);
                let b1 = -(1.0 + cs);
                ((1.0 + cs) / 2.0, b1, (1.0 + cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
            }
            BiquadKind::Bandpass => {
                let alpha = sn / (2.0 * q.max(1e-9));
                (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
            }
            BiquadKind::Notch => {
                let alpha = sn / (2.0 * q.max(1e-9));
                (1.0, -2.0 * cs, 1.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
            }
            BiquadKind::Allpass => {
                let alpha = sn / (2.0 * q.max(1e-9));
                (1.0 - alpha, -2.0 * cs, 1.0 + alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
            }
            BiquadKind::Peaking => {
                let alpha = sn / (2.0 * q.max(1e-9));
                (
                    1.0 + alpha * a,
                    -2.0 * cs,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cs,
                    1.0 - alpha / a,
                )
            }
            BiquadKind::Lowshelf => {
                let s = 2.0 * (a).sqrt() * (sn / 2.0) * ((1.0 / q.max(1e-9) - 1.0) + 2.0).max(0.0).sqrt();
                let s = if s.is_finite() { s } else { 0.0 };
                (
                    a * ((a + 1.0) - (a - 1.0) * cs + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - s),
                    (a + 1.0) + (a - 1.0) * cs + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - s,
                )
            }
            BiquadKind::Highshelf => {
                let s = 2.0 * (a).sqrt() * (sn / 2.0) * ((1.0 / q.max(1e-9) - 1.0) + 2.0).max(0.0).sqrt();
                let s = if s.is_finite() { s } else { 0.0 };
                (
                    a * ((a + 1.0) + (a - 1.0) * cs + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - s),
                    (a + 1.0) - (a - 1.0) * cs + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - s,
                )
            }
        };
        let a0 = if a0.abs() < 1e-12 { 1.0 } else { a0 };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    fn process(&mut self, ch: usize, x: f64) -> f64 {
        if self.x1.len() <= ch {
            self.x1.resize(ch + 1, 0.0);
            self.x2.resize(ch + 1, 0.0);
            self.y1.resize(ch + 1, 0.0);
            self.y2.resize(ch + 1, 0.0);
        }
        let y = self.b0 * x + self.b1 * self.x1[ch] + self.b2 * self.x2[ch]
            - self.a1 * self.y1[ch]
            - self.a2 * self.y2[ch];
        let y = if y.is_finite() { y } else { 0.0 };
        self.x2[ch] = self.x1[ch];
        self.x1[ch] = x;
        self.y2[ch] = self.y1[ch];
        self.y1[ch] = y;
        y
    }
}

// ── FFT ───────────────────────────────────────────────────────────────────────────────────────

/// In-place iterative radix-2 Cooley–Tukey. `n` must be a power of two, which every legal
/// `fftSize` is.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    if n < 2 || !n.is_power_of_two() {
        return;
    }
    // bit reversal
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0usize;
        while i < n {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (ur, ui) = (re[i + k], im[i + k]);
                let (xr, xi) = (re[i + k + len / 2], im[i + k + len / 2]);
                let (vr, vi) = (xr * cr - xi * ci, xr * ci + xi * cr);
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + len / 2] = ur - vr;
                im[i + k + len / 2] = ui - vi;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// The Blackman window an `AnalyserNode` applies before its FFT, at the standard α = 0.16.
fn blackman(n: usize) -> Vec<f64> {
    let a0 = 0.42;
    let a1 = 0.5;
    let a2 = 0.08;
    (0..n)
        .map(|i| {
            let x = 2.0 * PI * i as f64 / n as f64;
            a0 - a1 * x.cos() + a2 * (2.0 * x).cos()
        })
        .collect()
}

// ── the graph ─────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct Props {
    osc_type: Option<OscillatorType>,
    biquad_type: BiquadKind2,
    looping: bool,
    loop_start: f64,
    loop_end: f64,
    curve: Option<Vec<f32>>,
    oversample: u32,
    channel_count: Option<u32>,
    channel_count_mode: u32,
    channel_interpretation: u32,
    fft_size: u32,
    min_db: f64,
    max_db: f64,
    smoothing: f64,
}

/// A newtype only so [`Props`] can derive `Default` with a sensible filter type.
#[derive(Debug, Clone, Copy)]
struct BiquadKind2(BiquadKind);
impl Default for BiquadKind2 {
    fn default() -> Self {
        BiquadKind2(BiquadKind::Lowpass)
    }
}

/// The per-node runtime state a processor carries across quanta.
#[derive(Default)]
struct Runtime {
    /// Oscillator / buffer-source read position. For an oscillator this is normalized phase; for a
    /// buffer source it is a fractional sample index into the buffer.
    phase: f64,
    wavetable: Option<Wavetable>,
    /// The wave the cached table was built for, so a `type` change rebuilds it and nothing else does.
    wave_key: Option<OscillatorType>,
    custom: Option<(Vec<f32>, Vec<f32>, bool)>,
    buffer: Vec<Vec<f32>>,
    buffer_rate: f32,
    /// Compressor: the reduction in dB (≤ 0) -- the kernel's metering gain after the last quantum.
    envelope_db: f64,
    /// Chrome's `DynamicsCompressorKernel` state.
    comp: CompressorKernel,
    /// Delay node ring.
    delay_line: Vec<Vec<f32>>,
    delay_pos: usize,
    biquad: Biquad,
    /// Analyser tap: the most recent samples that reached the node's input, mono down-mix.
    tap: Vec<f32>,
    /// Analyser: the smoothed magnitudes carried between FFT frames.
    smoothed: Vec<f64>,
}

struct Node {
    kind: AudioNodeKind,
    arg: f64,
    params: HashMap<AudioParamId, Param>,
    props: Props,
    /// Node ids feeding each input, in connection order.
    inputs: Vec<Vec<(u32, u32)>>,
    start: Option<(f64, f64, Option<f64>)>,
    stop: Option<f64>,
    rt: Runtime,
    /// This quantum's output, channel-major.
    out: Vec<Vec<f32>>,
}

impl Node {
    fn new(kind: AudioNodeKind, arg: f64) -> Self {
        use AudioParamId::*;
        let mut params = HashMap::new();
        let inf = f32::MAX;
        let mut p = |id: AudioParamId, d: f32, lo: f32, hi: f32| {
            params.insert(id, Param::new(d, lo, hi));
        };
        match kind {
            AudioNodeKind::Oscillator => {
                p(Frequency, 440.0, -inf, inf);
                p(Detune, 0.0, -inf, inf);
            }
            AudioNodeKind::Gain => p(Gain, 1.0, -inf, inf),
            AudioNodeKind::DynamicsCompressor => {
                p(Threshold, -24.0, -100.0, 0.0);
                p(Knee, 30.0, 0.0, 40.0);
                p(Ratio, 12.0, 1.0, 20.0);
                p(Attack, 0.003, 0.0, 1.0);
                p(Release, 0.25, 0.0, 1.0);
            }
            AudioNodeKind::BufferSource => {
                p(PlaybackRate, 1.0, -inf, inf);
                p(Detune, 0.0, -inf, inf);
            }
            AudioNodeKind::ConstantSource => p(Offset, 1.0, -inf, inf),
            AudioNodeKind::Delay => p(DelayTime, 0.0, 0.0, arg.max(0.0) as f32),
            AudioNodeKind::BiquadFilter => {
                p(Frequency, 350.0, 0.0, inf);
                p(Detune, 0.0, -inf, inf);
                p(Q, 1.0, -inf, inf);
                p(Gain, 0.0, -inf, inf);
            }
            AudioNodeKind::StereoPanner => p(Pan, 0.0, -1.0, 1.0),
            _ => {}
        }
        let n_inputs = match kind {
            AudioNodeKind::ChannelMerger => arg.max(1.0) as usize,
            AudioNodeKind::Oscillator
            | AudioNodeKind::BufferSource
            | AudioNodeKind::ConstantSource => 0,
            _ => 1,
        };
        let mut props = Props {
            fft_size: 2048,
            min_db: -100.0,
            max_db: -30.0,
            smoothing: 0.8,
            oversample: 1,
            loop_end: 0.0,
            ..Default::default()
        };
        // The default channelCount/mode a node is created with, per the spec's per-node table. Only
        // the ones that differ from (2, "max") are listed.
        match kind {
            AudioNodeKind::ChannelSplitter => {
                props.channel_count = Some(arg.max(1.0) as u32);
                props.channel_count_mode = 2;
                props.channel_interpretation = 1;
            }
            AudioNodeKind::ChannelMerger => {
                props.channel_count = Some(1);
                props.channel_count_mode = 2;
            }
            AudioNodeKind::DynamicsCompressor | AudioNodeKind::StereoPanner => {
                props.channel_count = Some(2);
                props.channel_count_mode = 1;
            }
            _ => {}
        }
        Self {
            kind,
            arg,
            params,
            props,
            inputs: vec![Vec::new(); n_inputs],
            start: None,
            stop: None,
            rt: Runtime::default(),
            out: Vec::new(),
        }
    }
}

/// One built graph, ready to render. Building is separate from rendering because
/// [`render`] and [`query`] must run the SAME graph — the whole reason `query_audio` is a backend
/// method and not an FFT in `webapi`.
pub struct Engine {
    nodes: HashMap<u32, Node>,
    /// Node id → its (source, param) edges.
    param_edges: Vec<(u32, u32, AudioParamId)>,
    order: Vec<u32>,
    sample_rate: f64,
    dest_channels: usize,
}

impl Engine {
    pub fn build(graph: &[AudioOp], channels: u32, sample_rate: f32) -> Self {
        let mut nodes: HashMap<u32, Node> = HashMap::new();
        // Node 0 is the destination and always exists. `arg` carries the context's channel count so
        // the mixer has one place to read it from.
        nodes.insert(0, Node::new(AudioNodeKind::Gain, 0.0));
        let mut param_edges: Vec<(u32, u32, AudioParamId)> = Vec::new();
        for op in graph {
            match op {
                AudioOp::Create { id, kind, arg } => {
                    if *id != 0 {
                        nodes.insert(*id, Node::new(*kind, *arg));
                    }
                }
                AudioOp::SetProp { id, prop } => {
                    if let Some(n) = nodes.get_mut(id) {
                        apply_prop(n, prop);
                    }
                }
                AudioOp::SetParam { id, param, value } => {
                    if let Some(p) = nodes.get_mut(id).and_then(|n| n.params.get_mut(param)) {
                        p.value = *value;
                    }
                }
                AudioOp::Automate { id, param, event } => {
                    if let Some(p) = nodes.get_mut(id).and_then(|n| n.params.get_mut(param)) {
                        p.push(event.clone());
                    }
                }
                AudioOp::SetPeriodicWave { id, real, imag, normalize } => {
                    if let Some(n) = nodes.get_mut(id) {
                        n.props.osc_type = Some(OscillatorType::Custom);
                        n.rt.custom = Some((real.clone(), imag.clone(), *normalize));
                        n.rt.wave_key = None;
                    }
                }
                AudioOp::SetBuffer { id, channels: chs, sample_rate: sr } => {
                    if let Some(n) = nodes.get_mut(id) {
                        n.rt.buffer = chs.clone();
                        n.rt.buffer_rate = *sr;
                    }
                }
                AudioOp::Connect { from, from_output, to, to_input } => {
                    if let Some(n) = nodes.get_mut(to) {
                        let i = (*to_input as usize).min(n.inputs.len().saturating_sub(1));
                        if let Some(slot) = n.inputs.get_mut(i) {
                            if !slot.contains(&(*from, *from_output)) {
                                slot.push((*from, *from_output));
                            }
                        }
                    }
                }
                AudioOp::ConnectParam { from, to, param, .. } => {
                    if !param_edges.contains(&(*from, *to, *param)) {
                        param_edges.push((*from, *to, *param));
                    }
                }
                AudioOp::Disconnect { from, to } => {
                    for (id, n) in nodes.iter_mut() {
                        if to.map(|t| t == *id).unwrap_or(true) {
                            for slot in n.inputs.iter_mut() {
                                slot.retain(|(f, _)| f != from);
                            }
                        }
                    }
                    param_edges.retain(|(f, t, _)| f != from || to.map(|x| x != *t).unwrap_or(false));
                }
                AudioOp::Start { id, when, offset, duration } => {
                    if let Some(n) = nodes.get_mut(id) {
                        n.start = Some((*when, *offset, *duration));
                    }
                }
                AudioOp::Stop { id, when } => {
                    if let Some(n) = nodes.get_mut(id) {
                        n.stop = Some(*when);
                    }
                }
            }
        }
        let order = topo_order(&nodes, &param_edges);
        Self { nodes, param_edges, order, sample_rate: sample_rate as f64, dest_channels: channels.max(1) as usize }
    }

    /// Render `frames` frames, optionally taping one analyser's input and FFT-ing it as it goes.
    ///
    /// The tap is a parameter of the SAME run rather than a second entry point: whatever
    /// `getFloatFrequencyData` reports is the spectrum of samples that were on their way to the
    /// destination in this very render.
    fn run(&mut self, frames: usize, tap: Option<u32>) -> Vec<f32> {
        let ch = self.dest_channels;
        let mut pcm = vec![0.0f32; frames * ch];
        let mut done = 0usize;
        let ids: Vec<u32> = self.order.clone();
        while done < frames {
            // Always a WHOLE render quantum, as Chrome renders one (an OfflineAudioContext of 5000
            // frames runs 40 quanta and keeps the first 5000 samples): a short last quantum left the
            // compressor's 32-frame divisions unrun and its last 8 samples silent.
            let n = QUANTUM;
            let t0 = done as f64 / self.sample_rate;
            for id in &ids {
                self.process(*id, n, t0);
            }
            // The destination's own input mix IS the output; node 0 is processed like any other and
            // there is exactly one place the samples become the buffer.
            let keep = n.min(frames - done);
            if let Some(dest) = self.nodes.get(&0) {
                for (c, chan) in dest.out.iter().enumerate().take(ch) {
                    for (i, s) in chan.iter().enumerate().take(keep) {
                        pcm[(done + i) * ch + c] = *s;
                    }
                }
            }
            done += keep;
        }
        // ONE analysis, at the end. Chrome's `RealtimeAnalyser` does its FFT when
        // `getFloatFrequencyData` is called -- once per call at a new `currentTime` -- and blends it
        // into its previous magnitudes, which start at zero: a single read after a render is
        // `(1 - smoothingTimeConstant) * |FFT|`, 14 dB under the fully-converged spectrum this
        // produced by smoothing across every render quantum (-106.8 dB where Chrome reads -121.1).
        if let Some(a) = tap {
            self.advance_analyser(a);
        }
        pcm
    }

    /// One node, one quantum. Reads its inputs' `out` (already computed this quantum for every node
    /// ahead of it in topological order; a node behind it in a cycle contributes last quantum's
    /// output, which is exactly what a real feedback loop through a delay does).
    fn process(&mut self, id: u32, n: usize, t0: f64) {
        let Some(node) = self.nodes.get(&id) else { return };
        let kind = node.kind;
        let arg = node.arg;

        // Gather the input mix first, so the borrow of the sources ends before the node is edited.
        let n_inputs = node.inputs.len();
        let mut inputs: Vec<Vec<Vec<f32>>> = Vec::with_capacity(n_inputs);
        for i in 0..n_inputs {
            let sources: Vec<(u32, u32)> = self.nodes[&id].inputs[i].clone();
            let mut width = 1usize;
            for (src, out_idx) in &sources {
                let w = self
                    .nodes
                    .get(src)
                    .map(|s| output_width(s, *out_idx))
                    .unwrap_or(0);
                width = width.max(w);
            }
            let mut mix = vec![vec![0.0f32; n]; width];
            for (src, out_idx) in &sources {
                let Some(s) = self.nodes.get(src) else { continue };
                let up = up_mix(s, *out_idx, width, n, s.props.channel_interpretation);
                for (c, chan) in up.iter().enumerate() {
                    for (k, v) in chan.iter().enumerate().take(n) {
                        mix[c][k] += v;
                    }
                }
            }
            inputs.push(mix);
        }

        // The node's computed channel count, from its mode.
        let node = self.nodes.get(&id).unwrap();
        let in_width = inputs.first().map(|m| m.len()).unwrap_or(1).max(1);
        let declared = node.props.channel_count.unwrap_or(2) as usize;
        let width = if id == 0 {
            // The destination's channel count is the context's, always. It is the one node whose
            // width is not negotiated with its inputs, and the run loop reads exactly this many
            // channels out of it.
            self.dest_channels
        } else {
            match kind {
                AudioNodeKind::ChannelSplitter => 1,
                AudioNodeKind::ChannelMerger => n_inputs,
                AudioNodeKind::StereoPanner => 2,
                _ => match node.props.channel_count_mode {
                    2 => declared,
                    1 => in_width.min(declared),
                    _ => in_width,
                },
            }
        }
        .max(1);

        // Down/up-mix the input to the node's own width before processing.
        let interp = node.props.channel_interpretation;
        let mut src: Vec<Vec<f32>> = if n_inputs == 0 {
            Vec::new()
        } else {
            remix(&inputs[0], width, n, interp)
        };

        // a-rate param contributions, evaluated from the same quantum's outputs.
        let mut param_add: HashMap<AudioParamId, Vec<f32>> = HashMap::new();
        for (from, to, p) in self.param_edges.clone() {
            if to != id {
                continue;
            }
            let Some(s) = self.nodes.get(&from) else { continue };
            let mono = down_to_mono(s, n);
            let e = param_add.entry(p).or_insert_with(|| vec![0.0f32; n]);
            for (k, v) in mono.iter().enumerate().take(n) {
                e[k] += v;
            }
        }

        let sr = self.sample_rate;
        let node = self.nodes.get_mut(&id).unwrap();
        let pv = |node: &Node, p: AudioParamId, k: usize| -> f32 {
            let base = node.params.get(&p).map(|x| x.timeline_value(t0 + k as f64 / sr)).unwrap_or(0.0);
            base + param_add.get(&p).map(|v| v[k]).unwrap_or(0.0)
        };

        let mut out: Vec<Vec<f32>> = vec![vec![0.0f32; n]; width];
        match kind {
            // Node 0 (the destination) and every GainNode are the same processor; the destination is
            // just a gain of 1 whose output the run loop reads. One processor, so a page cannot find
            // the destination behaving unlike the graph that feeds it.
            AudioNodeKind::Gain => {
                let is_dest = id == 0;
                for k in 0..n {
                    let g = if is_dest { 1.0 } else { pv(node, AudioParamId::Gain, k) };
                    for c in 0..width {
                        out[c][k] = src.get(c).map(|x| x[k]).unwrap_or(0.0) * g;
                    }
                }
            }
            AudioNodeKind::Oscillator => {
                ensure_wavetable(node, sr);
                let live = source_window(node, t0, n, sr);
                if let (Some(first), Some(last)) = (live.iter().position(|&b| b), live.iter().rposition(|&b| b)) {
                    let freq: Vec<f32> = (0..n).map(|k| pv(node, AudioParamId::Frequency, k)).collect();
                    let det: Vec<f32> = (0..n).map(|k| pv(node, AudioParamId::Detune, k)).collect();
                    // Blink's `HasSampleAccurateValues`: a connected input, or automation that moves the
                    // value inside this quantum. Otherwise the param is k-rate: one value for the quantum.
                    let t1 = t0 + n as f64 / sr;
                    let moving = |id: AudioParamId| {
                        param_add.contains_key(&id) || node.params.get(&id).is_some_and(|p| p.sample_accurate(t0, t1))
                    };
                    let (freq_sa, det_sa) = (moving(AudioParamId::Frequency), moving(AudioParamId::Detune));
                    let mut dest = vec![0.0f32; last + 1 - first];
                    let vri = node.rt.phase;
                    let vri = match node.rt.wavetable.as_ref() {
                        Some(w) => oscillator_quantum(w, sr, &freq, &det, freq_sa, det_sa, vri, &mut dest),
                        None => vri,
                    };
                    node.rt.phase = vri;
                    for c in 0..width {
                        out[c][first..=last].copy_from_slice(&dest);
                    }
                }
            }
            AudioNodeKind::ConstantSource => {
                let live = source_window(node, t0, n, sr);
                for k in 0..n {
                    if !live[k] {
                        continue;
                    }
                    let v = pv(node, AudioParamId::Offset, k);
                    for c in 0..width {
                        out[c][k] = v;
                    }
                }
            }
            AudioNodeKind::BufferSource => {
                let live = source_window(node, t0, n, sr);
                let bch = node.rt.buffer.len();
                if bch > 0 {
                    let blen = node.rt.buffer[0].len() as f64;
                    let ratio = if node.rt.buffer_rate > 0.0 { node.rt.buffer_rate as f64 / sr } else { 1.0 };
                    let (looping, ls, le) = (node.props.looping, node.props.loop_start, node.props.loop_end);
                    let le = if le > ls { le } else { blen / node.rt.buffer_rate.max(1.0) as f64 };
                    for k in 0..n {
                        if !live[k] {
                            continue;
                        }
                        let rate = pv(node, AudioParamId::PlaybackRate, k) as f64
                            * 2f64.powf(pv(node, AudioParamId::Detune, k) as f64 / 1200.0);
                        let mut pos = node.rt.phase;
                        if looping {
                            let a = ls * node.rt.buffer_rate as f64;
                            let b = (le * node.rt.buffer_rate as f64).min(blen);
                            if b > a && pos >= b {
                                pos = a + (pos - a).rem_euclid(b - a);
                            }
                        }
                        if pos < blen {
                            let i = pos.floor() as usize;
                            let f = (pos - i as f64) as f32;
                            for c in 0..width {
                                let chan = &node.rt.buffer[c.min(bch - 1)];
                                let a = chan.get(i).copied().unwrap_or(0.0);
                                let b = chan.get(i + 1).copied().unwrap_or(a);
                                out[c][k] = a + (b - a) * f;
                            }
                        }
                        node.rt.phase = pos + rate * ratio;
                    }
                }
            }
            AudioNodeKind::DynamicsCompressor => {
                compress(node, &src, &mut out, width, n, sr, t0, &param_add);
            }
            AudioNodeKind::Analyser => {
                // A pass-through that also tapes its input. It must not touch the samples: an
                // analyser that changed the signal would make `getChannelData` and
                // `getFloatFrequencyData` describe different graphs, which is the exact split this
                // seam exists to make impossible.
                for c in 0..width {
                    if let Some(s) = src.get(c) {
                        out[c][..n].copy_from_slice(&s[..n]);
                    }
                }
                let mono: Vec<f32> = (0..n)
                    .map(|k| {
                        let sum: f32 = (0..width).map(|c| out[c][k]).sum();
                        sum / width as f32
                    })
                    .collect();
                node.rt.tap.extend_from_slice(&mono);
                if node.rt.tap.len() > MAX_FFT {
                    let drop = node.rt.tap.len() - MAX_FFT;
                    node.rt.tap.drain(..drop);
                }
            }
            AudioNodeKind::Delay => {
                let max = (arg.max(0.0) * sr).ceil() as usize + QUANTUM + 2;
                if node.rt.delay_line.len() != width || node.rt.delay_line.first().map(|l| l.len()) != Some(max) {
                    node.rt.delay_line = vec![vec![0.0f32; max]; width];
                    node.rt.delay_pos = 0;
                }
                for k in 0..n {
                    let d = (pv(node, AudioParamId::DelayTime, k) as f64 * sr).clamp(0.0, (max - 2) as f64);
                    let read = node.rt.delay_pos as f64 - d;
                    let read = read.rem_euclid(max as f64);
                    let i = read.floor() as usize;
                    let f = (read - i as f64) as f32;
                    for c in 0..width {
                        let line = &mut node.rt.delay_line[c];
                        let a = line[i % max];
                        let b = line[(i + 1) % max];
                        out[c][k] = a + (b - a) * f;
                        line[node.rt.delay_pos] = src.get(c).map(|x| x[k]).unwrap_or(0.0);
                    }
                    node.rt.delay_pos = (node.rt.delay_pos + 1) % max;
                }
            }
            AudioNodeKind::BiquadFilter => {
                let f = node.params[&AudioParamId::Frequency].timeline_value(t0) as f64
                    * 2f64.powf(node.params[&AudioParamId::Detune].timeline_value(t0) as f64 / 1200.0);
                let q = node.params[&AudioParamId::Q].timeline_value(t0) as f64;
                let g = node.params[&AudioParamId::Gain].timeline_value(t0) as f64;
                let kind2 = node.props.biquad_type.0;
                node.rt.biquad.set(kind2, f, q, g, sr);
                for c in 0..width {
                    for k in 0..n {
                        let x = src.get(c).map(|s| s[k]).unwrap_or(0.0) as f64;
                        out[c][k] = node.rt.biquad.process(c, x) as f32;
                    }
                }
            }
            AudioNodeKind::StereoPanner => {
                for k in 0..n {
                    let p = pv(node, AudioParamId::Pan, k).clamp(-1.0, 1.0) as f64;
                    // Equal-power panning: the input is mixed to stereo first (`width` is 2 here),
                    // then the two channels are cross-faded.
                    let (l_in, r_in) = (
                        src.first().map(|s| s[k]).unwrap_or(0.0),
                        src.get(1).map(|s| s[k]).unwrap_or_else(|| src.first().map(|s| s[k]).unwrap_or(0.0)),
                    );
                    let x = (p + 1.0) / 2.0;
                    let (g_l, g_r) = ((x * PI / 2.0).cos(), (x * PI / 2.0).sin());
                    if p <= 0.0 {
                        out[0][k] = l_in + r_in * g_l as f32;
                        out[1][k] = r_in * g_r as f32;
                    } else {
                        out[0][k] = l_in * g_l as f32;
                        out[1][k] = r_in + l_in * g_r as f32;
                    }
                }
            }
            AudioNodeKind::WaveShaper => {
                let curve = node.props.curve.clone();
                match curve {
                    None => {
                        for c in 0..width {
                            if let Some(s) = src.get(c) {
                                out[c][..n].copy_from_slice(&s[..n]);
                            }
                        }
                    }
                    Some(cv) if cv.len() >= 2 => {
                        for c in 0..width {
                            for k in 0..n {
                                let x = src.get(c).map(|s| s[k]).unwrap_or(0.0);
                                out[c][k] = shape(&cv, x);
                            }
                        }
                    }
                    Some(_) => {}
                }
            }
            AudioNodeKind::ChannelMerger => {
                for (i, mix) in inputs.iter().enumerate().take(width) {
                    let mono = mix
                        .first()
                        .cloned()
                        .unwrap_or_else(|| vec![0.0f32; n]);
                    out[i][..n].copy_from_slice(&mono[..n]);
                }
                // ChannelMerger reads every input, not just input 0; `src` was built from input 0.
                src = Vec::new();
            }
            AudioNodeKind::ChannelSplitter => {
                // The splitter's N outputs are the N channels of its (discrete) input mix. `out`
                // here holds all of them so `output_width`/`up_mix` can pick one by index.
                let mix = inputs.first().cloned().unwrap_or_default();
                out = vec![vec![0.0f32; n]; (arg.max(1.0) as usize).max(1)];
                for (c, o) in out.iter_mut().enumerate() {
                    if let Some(s) = mix.get(c) {
                        o[..n].copy_from_slice(&s[..n]);
                    }
                }
            }
        }
        let _ = &src;
        self.nodes.get_mut(&id).unwrap().out = out;
    }

    /// One analyser FFT frame, run at the quantum boundary so `smoothingTimeConstant` is a real
    /// first-order filter over successive frames rather than a member that reads back and does
    /// nothing.
    fn advance_analyser(&mut self, id: u32) {
        let Some(node) = self.nodes.get_mut(&id) else { return };
        if node.kind != AudioNodeKind::Analyser {
            return;
        }
        let fft_size = (node.props.fft_size as usize).clamp(32, MAX_FFT);
        let bins = fft_size / 2;
        if node.rt.smoothed.len() != bins {
            node.rt.smoothed = vec![0.0; bins];
        }
        let have = node.rt.tap.len();
        let mut re = vec![0.0f64; fft_size];
        let mut im = vec![0.0f64; fft_size];
        let win = blackman(fft_size);
        let start = have.saturating_sub(fft_size);
        for i in 0..fft_size {
            let s = if start + i < have { node.rt.tap[start + i] as f64 } else { 0.0 };
            re[i] = s * win[i];
        }
        fft(&mut re, &mut im);
        let k = node.props.smoothing.clamp(0.0, 1.0);
        let scale = 1.0 / fft_size as f64;
        for b in 0..bins {
            let m = (re[b] * re[b] + im[b] * im[b]).sqrt() * scale;
            node.rt.smoothed[b] = k * node.rt.smoothed[b] + (1.0 - k) * m;
        }
    }
}

/// `curve[]` lookup with the spec's mapping: input −1..1 spans the whole curve, and anything outside
/// clamps to the end values.
fn shape(curve: &[f32], x: f32) -> f32 {
    let n = curve.len();
    let v = (x + 1.0) * 0.5 * (n - 1) as f32;
    if v <= 0.0 {
        return curve[0];
    }
    if v >= (n - 1) as f32 {
        return curve[n - 1];
    }
    let i = v.floor() as usize;
    let f = v - i as f32;
    curve[i] + (curve[i + 1] - curve[i]) * f
}

fn apply_prop(n: &mut Node, prop: &AudioProp) {
    match prop {
        AudioProp::OscillatorType(t) => {
            n.props.osc_type = Some(*t);
            n.rt.wave_key = None;
        }
        AudioProp::BiquadType(t) => n.props.biquad_type = BiquadKind2(*t),
        AudioProp::Loop(v) => n.props.looping = *v,
        AudioProp::LoopStart(v) => n.props.loop_start = *v,
        AudioProp::LoopEnd(v) => n.props.loop_end = *v,
        AudioProp::Curve(c) => n.props.curve = c.clone(),
        AudioProp::Oversample(v) => n.props.oversample = *v,
        AudioProp::ChannelCount(v) => n.props.channel_count = Some((*v).max(1)),
        AudioProp::ChannelCountMode(v) => n.props.channel_count_mode = *v,
        AudioProp::ChannelInterpretation(v) => n.props.channel_interpretation = *v,
        AudioProp::FftSize(v) => n.props.fft_size = *v,
        AudioProp::MinDecibels(v) => n.props.min_db = *v,
        AudioProp::MaxDecibels(v) => n.props.max_db = *v,
        AudioProp::SmoothingTimeConstant(v) => n.props.smoothing = *v,
    }
}

// ── the oscillator, as Chrome 150 renders it on x86 ─────────────────────
//
// `OscillatorHandler::Process` with its SSE2 kernels (modules/webaudio/oscillator_handler.cc,
// cpu/x86/oscillator_kernel_sse2.cc). The scalar wavetable read that stood here was the textbook
// interpolation; Chrome's output is its kernels' FLOAT arithmetic -- four read positions kept as
// float32 and stepped by 4*incr, `x0 + f*(x1 - x0)` within a table and `hi + t*(lo - hi)` between
// tables -- and every sample of a 440 Hz sine differed from Chrome's by an ulp, enough to change the
// sum an audio fingerprint takes (a challenge provider renders a triangle at 9998.123456 Hz through a
// compressor in its second stage).

const INTERPOLATE_2_POINT: f32 = 0.3;
const INTERPOLATE_3_POINT: f32 = 0.16;

fn clamp_frequency(f: f32, nyquist: f32) -> f32 {
    if f.is_nan() {
        nyquist
    } else {
        f.clamp(-nyquist, nyquist)
    }
}

/// `x - floor(x / size) * size` as the SSE2 kernel computes it in float: truncate toward zero, step
/// down one when that overshot, subtract.
fn wrap_f32(x: f32, size: f32, inv: f32) -> f32 {
    let r = x * inv;
    let mut f = r as i32;
    if r < f as f32 {
        f -= 1;
    }
    x - f as f32 * size
}

/// The same in double (`WrapVirtualIndexVectorPd`).
fn wrap_f64(x: f64, size: f64, inv: f64) -> f64 {
    let r = x * inv;
    let mut f = r as i32;
    if r < f as f64 {
        f -= 1;
    }
    x - f as f64 * size
}

fn wrap_scalar(vri: f64, size: f64) -> f64 {
    vri - (vri * (1.0 / size)).floor() * size
}

/// Chrome's `DoInterpolation`: linear for a big increment, 3- or 5-point Lagrange in double below.
fn do_interpolation(vri: f64, incr: f32, mask: usize, tif: f32, lower: &[f32], higher: &[f32]) -> f32 {
    let (mut s_lower, mut s_higher) = (0.0f64, 0.0f64);
    let i0 = vri as usize;
    if incr >= INTERPOLATE_2_POINT {
        let (a, b) = (i0 & mask, (i0 + 1) & mask);
        let f = (vri as f32 - a as f32) as f64;
        s_higher = (1.0 - f) * higher[a] as f64 + f * higher[b] as f64;
        s_lower = (1.0 - f) * lower[a] as f64 + f * lower[b] as f64;
    } else if incr >= INTERPOLATE_3_POINT {
        let t = vri - i0 as f64;
        let a = [0.5 * t * (t - 1.0), 1.0 - t * t, 0.5 * t * (t + 1.0)];
        for (k, ak) in a.iter().enumerate() {
            let idx = (i0.wrapping_add(k).wrapping_sub(1)) & mask;
            s_lower += ak * lower[idx] as f64;
            s_higher += ak * higher[idx] as f64;
        }
    } else {
        let t = vri - i0 as f64;
        let t2 = t * t;
        let a = [
            t * (t2 - 1.0) * (t - 2.0) / 24.0,
            -t * (t - 1.0) * (t2 - 4.0) / 6.0,
            (t2 - 1.0) * (t2 - 4.0) / 4.0,
            -t * (t + 1.0) * (t2 - 4.0) / 6.0,
            t * (t2 - 1.0) * (t + 2.0) / 24.0,
        ];
        for (k, ak) in a.iter().enumerate() {
            let idx = (i0.wrapping_add(k).wrapping_sub(2)) & mask;
            s_lower += ak * lower[idx] as f64;
            s_higher += ak * higher[idx] as f64;
        }
    }
    ((1.0 - tif) as f64 * s_higher + tif as f64 * s_lower) as f32
}

/// One render quantum of an oscillator into `dest` (its non-silent frames), from read position `vri`;
/// returns the next read position. `freq`/`det` are the params' values per frame of the quantum.
#[allow(clippy::too_many_arguments)]
fn oscillator_quantum(w: &Wavetable, sr: f64, freq: &[f32], det: &[f32], freq_sa: bool, det_sa: bool, mut vri: f64, dest: &mut [f32]) -> f64 {
    let n = dest.len();
    let size = w.size;
    let mask = size - 1;
    let rate_scale = w.rate_scale(sr);
    let nyquist = (sr / 2.0) as f32;
    if !(freq_sa || det_sa) {
        // ProcessKRate.
        let f = clamp_frequency(freq[0] * (det[0] / 1200.0).exp2(), nyquist);
        let (lower, higher, tif) = w.bands_for(f);
        let incr = f * rate_scale;
        if incr >= INTERPOLATE_2_POINT {
            let start = vri;
            // ProcessKRateVector: four float32 read positions, stepped by 4*incr.
            let (fsize, finv) = (size as f32, 1.0f32 / size as f32);
            let mut v = [0f32; 4];
            for (m, x) in v.iter_mut().enumerate() {
                // `virtual_read_index + m * incr`: the product in float, the sum in double.
                *x = wrap_f32((vri + (m as f32 * incr) as f64) as f32, fsize, finv);
            }
            let step = 4.0 * incr;
            let mut k = 0;
            while k + 4 <= n {
                for m in 0..4 {
                    let i0 = (v[m] as i32 as usize) & mask;
                    let i1 = (i0 + 1) & mask;
                    let t = v[m] - i0 as f32;
                    let sh = higher[i0] + t * (higher[i1] - higher[i0]);
                    let sl = lower[i0] + t * (lower[i1] - lower[i0]);
                    dest[k + m] = sh + tif * (sl - sh);
                    v[m] = wrap_f32(v[m] + step, fsize, finv);
                }
                k += 4;
            }
            if k < n {
                // ProcessKRateScalar for the remainder, from the accurately advanced position.
                let mut x = wrap_scalar(start + (k as f32 * incr) as f64, size as f64);
                for d in dest.iter_mut().skip(k) {
                    let i0 = (x as usize) & mask;
                    let i1 = (i0 + 1) & mask;
                    let t = x as f32 - i0 as f32;
                    let sh = higher[i0] + t * (higher[i1] - higher[i0]);
                    let sl = lower[i0] + t * (lower[i1] - lower[i0]);
                    *d = sh + tif * (sl - sh);
                    x = wrap_scalar(x + incr as f64, size as f64);
                }
            }
            return wrap_scalar(start + (n as f32 * incr) as f64, size as f64);
        }
        for d in dest.iter_mut() {
            *d = do_interpolation(vri, incr.abs(), mask, tif, lower, higher);
            vri = wrap_scalar(vri + incr as f64, size as f64);
        }
        return vri;
    }
    // CalculateSampleAccuratePhaseIncrements, then ProcessARate.
    let mut final_scale = rate_scale;
    let mut inc: Vec<f32> = if freq_sa { freq[..n].to_vec() } else { vec![0.0; n] };
    if !freq_sa {
        final_scale *= freq[0];
    }
    if det_sa {
        let k = 1.0f32 / 1200.0;
        for i in 0..n {
            let d = (det[i] * k).exp2();
            inc[i] = if freq_sa { d * inc[i] } else { d };
        }
    } else {
        final_scale *= (det[0] / 1200.0).exp2();
    }
    for x in inc.iter_mut() {
        *x = clamp_frequency(*x, nyquist) * final_scale;
    }
    let inv_rate_scale = 1.0 / rate_scale;
    let (dsize, dinv) = (size as f64, 1.0 / size as f64);
    let mut k = 0;
    while k + 4 <= n {
        let big = inc[k..k + 4].iter().all(|x| x.abs() >= INTERPOLATE_2_POINT);
        let bands: Vec<(&[f32], &[f32], f32)> = (0..4).map(|m| w.bands_for(inv_rate_scale * inc[k + m])).collect();
        if big {
            // ProcessARateVectorKernel: the four positions in double, the factor in float.
            let mut sum = [0f64; 4];
            sum[0] = inc[k] as f64;
            for m in 1..4 {
                sum[m] = sum[m - 1] + inc[k + m] as f64;
            }
            let pos = [vri, vri + sum[0], vri + sum[1], vri + sum[2]].map(|x| wrap_f64(x, dsize, dinv));
            for m in 0..4 {
                let r0 = pos[m] as i32;
                let i0 = (r0 as usize) & mask;
                let i1 = ((r0 + 1) as usize) & mask;
                let t = pos[m] as f32 - i0 as f32;
                let (lower, higher, tif) = bands[m];
                let sh = higher[i0] + t * (higher[i1] - higher[i0]);
                let sl = lower[i0] + t * (lower[i1] - lower[i0]);
                dest[k + m] = sh + tif * (sl - sh);
            }
            vri = wrap_scalar(vri + sum[3], dsize);
        } else {
            for m in 0..4 {
                let (lower, higher, tif) = bands[m];
                dest[k + m] = do_interpolation(vri, inc[k + m].abs(), mask, tif, lower, higher);
                vri = wrap_scalar(vri + inc[k + m] as f64, dsize);
            }
        }
        k += 4;
    }
    for m in k..n {
        let (lower, higher, tif) = w.bands_for(inv_rate_scale * inc[m]);
        dest[m] = do_interpolation(vri, inc[m].abs(), mask, tif, lower, higher);
        vri = wrap_scalar(vri + inc[m] as f64, dsize);
    }
    vri
}

/// Build or rebuild an oscillator's wavetable when its `type` (or its periodic wave) changed.
fn ensure_wavetable(node: &mut Node, sr: f64) {
    let want = node.props.osc_type.unwrap_or(OscillatorType::Sine);
    if node.rt.wave_key == Some(want) && node.rt.wavetable.is_some() {
        return;
    }
    let table = if want == OscillatorType::Custom {
        match &node.rt.custom {
            Some((re, im, norm)) => Wavetable::build(re, im, sr, *norm),
            None => {
                let (re, im) = builtin_coefficients(OscillatorType::Sine, periodic_wave_size(sr) / 2);
                Wavetable::build(&re, &im, sr, true)
            }
        }
    } else {
        let (re, im) = builtin_coefficients(want, periodic_wave_size(sr) / 2);
        Wavetable::build(&re, &im, sr, true)
    };
    node.rt.wavetable = Some(table);
    node.rt.wave_key = Some(want);
}

/// Which samples of this quantum a scheduled source is actually producing.
///
/// `start(when)`/`stop(when)` are sample-accurate in a real implementation and are sample-accurate
/// here: a source started at 0.5 s in a 1 s render is silent for exactly the first 22,050 samples,
/// not for the first 172 quanta.
fn source_window(node: &Node, t0: f64, n: usize, sr: f64) -> Vec<bool> {
    let Some((when, _offset, duration)) = node.start else { return vec![false; n] };
    let stop = match (node.stop, duration) {
        (Some(s), Some(d)) => s.min(when + d),
        (Some(s), None) => s,
        (None, Some(d)) => when + d,
        (None, None) => f64::INFINITY,
    };
    (0..n)
        .map(|k| {
            let t = t0 + k as f64 / sr;
            t >= when && t < stop
        })
        .collect()
}

/// The compressor: peak detector → soft-knee static curve → attack/release envelope → gain, applied
/// to a pre-delayed copy of the input.
#[allow(clippy::too_many_arguments)]
/// Chrome's `DynamicsCompressorKernel` (third_party/blink/renderer/platform/audio), ported line for
/// line: the soft knee is `threshold + (1 - e^(-k(x - threshold)))/k` with `k` solved so
/// the slope at the knee's end is `1/ratio`, the makeup gain is `(1/saturate(1))^0.6`, and the gain
/// follows the detector through an attack/adaptive-release envelope updated every 32 frames and
/// warped through `sin(π/2·g)`. The previous feed-forward approximation summed 37.25 where Chrome
/// sums 124.04 on the fingerprinting graph (`tools/realweb/seed/audio`).
#[derive(Debug, Clone)]
struct CompressorKernel {
    detector_average: f32,
    compressor_gain: f32,
    max_attack_compression_diff_db: f32,
    metering_gain: f32,
    pre_delay: Vec<Vec<f32>>,
    read: usize,
    write: usize,
    last_pre_delay_frames: usize,
    curve: Option<(f32, f32, f32)>,
    linear_threshold: f32,
    slope: f32,
    knee_threshold: f32,
    knee_threshold_db: f32,
    yknee_threshold_db: f32,
    k: f32,
}

const MAX_PRE_DELAY_FRAMES: usize = 1024;
const DEFAULT_PRE_DELAY_FRAMES: usize = 256;

impl Default for CompressorKernel {
    fn default() -> Self {
        CompressorKernel {
            detector_average: 0.0,
            compressor_gain: 1.0,
            max_attack_compression_diff_db: -1.0,
            metering_gain: 1.0,
            pre_delay: Vec::new(),
            read: 0,
            write: DEFAULT_PRE_DELAY_FRAMES,
            last_pre_delay_frames: DEFAULT_PRE_DELAY_FRAMES,
            curve: None,
            linear_threshold: 0.0,
            slope: 0.0,
            knee_threshold: 0.0,
            knee_threshold_db: 0.0,
            yknee_threshold_db: 0.0,
            k: 0.0,
        }
    }
}

extern "C" {
    fn korvus_fdlibm_pow(x: f64, y: f64) -> f64;
    fn korvus_fdlibm_asin(x: f64) -> f64;
    fn korvus_fdlibm_exp(x: f64) -> f64;
}

/// Chromium's fdlibm (`third_party/fdlibm`, vendored): `fdlibm::powf(x, y)` is its `pow` on the floats'
/// doubles, rounded to float -- not the platform's `powf`, which differs in the last bit.
fn fdlibm_powf(x: f32, y: f32) -> f32 {
    // SAFETY: a pure function of two doubles.
    unsafe { korvus_fdlibm_pow(x as f64, y as f64) as f32 }
}

fn fdlibm_asinf(x: f32) -> f32 {
    // SAFETY: a pure function of one double.
    unsafe { korvus_fdlibm_asin(x as f64) as f32 }
}

fn fdlibm_exp(x: f64) -> f64 {
    // SAFETY: a pure function of one double.
    unsafe { korvus_fdlibm_exp(x) }
}

/// `kPiOverTwoFloat`.
const PI_OVER_TWO_F32: f32 = std::f32::consts::FRAC_PI_2;

fn db_to_linear(db: f32) -> f32 {
    10f32.powf(0.05 * db)
}

fn linear_to_db(linear: f32) -> f32 {
    20.0 * linear.log10()
}

impl CompressorKernel {
    fn knee_curve(&self, x: f32, k: f32) -> f32 {
        if x < self.linear_threshold {
            return x;
        }
        // `exp` in double, as Blink's `KneeCurve` calls it.
        self.linear_threshold + (1.0 - ((-k * (x - self.linear_threshold)) as f64).exp() as f32) / k
    }

    fn saturate(&self, x: f32, k: f32) -> f32 {
        if x < self.knee_threshold {
            self.knee_curve(x, k)
        } else {
            let x_db = linear_to_db(x);
            let y_db = self.yknee_threshold_db + self.slope * (x_db - self.knee_threshold_db);
            db_to_linear(y_db)
        }
    }

    fn slope_at(&self, x: f32, k: f32) -> f32 {
        if x < self.linear_threshold {
            return 1.0;
        }
        let x2 = x * 1.001;
        let x_db = linear_to_db(x);
        let x2_db = linear_to_db(x2);
        let y_db = linear_to_db(self.knee_curve(x, k));
        let y2_db = linear_to_db(self.knee_curve(x2, k));
        (y2_db - y_db) / (x2_db - x_db)
    }

    fn k_at_slope(&self, desired_slope: f32, db_threshold: f32, db_knee: f32) -> f32 {
        let x = db_to_linear(db_threshold + db_knee);
        let (mut min_k, mut max_k, mut k) = (0.1f32, 10000.0f32, 5.0f32);
        for _ in 0..15 {
            if self.slope_at(x, k) < desired_slope {
                max_k = k;
            } else {
                min_k = k;
            }
            k = (min_k * max_k).sqrt();
        }
        k
    }

    fn update_static_curve(&mut self, db_threshold: f32, db_knee: f32, ratio: f32) -> f32 {
        if self.curve != Some((db_threshold, db_knee, ratio)) {
            self.curve = Some((db_threshold, db_knee, ratio));
            self.linear_threshold = db_to_linear(db_threshold);
            self.slope = 1.0 / ratio;
            let k = self.k_at_slope(1.0 / ratio, db_threshold, db_knee);
            self.knee_threshold_db = db_threshold + db_knee;
            self.knee_threshold = db_to_linear(self.knee_threshold_db);
            self.yknee_threshold_db = linear_to_db(self.knee_curve(self.knee_threshold, k));
            self.k = k;
        }
        self.k
    }

    fn set_pre_delay(&mut self, pre_delay_time: f32, sample_rate: f32, channels: usize) {
        let mut frames = (pre_delay_time * sample_rate) as usize;
        if frames > MAX_PRE_DELAY_FRAMES - 1 {
            frames = MAX_PRE_DELAY_FRAMES - 1;
        }
        if self.pre_delay.len() != channels {
            self.pre_delay = vec![vec![0.0; MAX_PRE_DELAY_FRAMES]; channels];
        }
        if self.last_pre_delay_frames != frames {
            self.last_pre_delay_frames = frames;
            for b in &mut self.pre_delay {
                b.iter_mut().for_each(|x| *x = 0.0);
            }
            self.read = 0;
            self.write = frames;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        src: &[Vec<f32>],
        out: &mut [Vec<f32>],
        channels: usize,
        frames: usize,
        sample_rate: f32,
        db_threshold: f32,
        db_knee: f32,
        ratio: f32,
        attack_time: f32,
        release_time: f32,
    ) {
        // Chrome 150's `DynamicsCompressor::Process` (platform/audio/dynamics_compressor.cc). The
        // release curve's coefficients are float constants over the release zones, scaled by the
        // release frames afterwards; the makeup gain and attack rate are fdlibm's `pow`, the desired
        // gain fdlibm's `asin`, the warp a double `sin`.
        const PRE_DELAY_TIME: f32 = 0.006;
        const METERING_RELEASE_TIME: f32 = 0.325;
        const SAT_RELEASE_TIME: f32 = 0.0025;
        const N_DIVISION_FRAMES: usize = 32;
        const Z: [f32; 4] = [0.09, 0.16, 0.42, 0.98];
        let a_base = 0.9999999999999998f32 * Z[0] + 1.8432219684323923e-16f32 * Z[1] - 1.9373394351676423e-16f32 * Z[2] + 8.824516011816245e-18f32 * Z[3];
        let b_base = -1.5788320352845888f32 * Z[0] + 2.3305837032074286f32 * Z[1] - 0.9141194204840429f32 * Z[2] + 0.1623677525612032f32 * Z[3];
        let c_base = 0.5334142869106424f32 * Z[0] - 1.272736789213631f32 * Z[1] + 0.9258856042207512f32 * Z[2] - 0.18656310191776226f32 * Z[3];
        let d_base = 0.08783463138207234f32 * Z[0] - 0.1694162967925622f32 * Z[1] + 0.08588057951595272f32 * Z[2] - 0.00429891410546283f32 * Z[3];
        let e_base = -0.042416883008123074f32 * Z[0] + 0.1115693827987602f32 * Z[1] - 0.09764676325265872f32 * Z[2] + 0.028494263462021576f32 * Z[3];
        let k = self.update_static_curve(db_threshold, db_knee, ratio);
        let linear_post_gain = fdlibm_powf(1.0 / self.saturate(1.0, k), 0.6);
        let attack_frames = attack_time.max(0.001) * sample_rate;
        let release_frames = sample_rate * release_time;
        let sat_release_frames = SAT_RELEASE_TIME * sample_rate;
        let (ka, kb, kc, kd, ke) = (
            release_frames * a_base,
            release_frames * b_base,
            release_frames * c_base,
            release_frames * d_base,
            release_frames * e_base,
        );
        // `DiscreteTimeConstantForSampleRate`: `1 - fdlibm::exp(-1 / (sample_rate * time_constant))`.
        let metering_release_k = (1.0 - fdlibm_exp(-1.0 / (sample_rate as f64 * METERING_RELEASE_TIME as f64))) as f32;
        self.set_pre_delay(PRE_DELAY_TIME, sample_rate, channels);
        let mask = MAX_PRE_DELAY_FRAMES - 1;
        let n_divisions = frames / N_DIVISION_FRAMES;
        let mut frame = 0usize;
        for _ in 0..n_divisions {
            if !self.detector_average.is_finite() {
                self.detector_average = 1.0;
            }
            let desired_gain = self.detector_average;
            let scaled_desired_gain = fdlibm_asinf(desired_gain) / PI_OVER_TWO_F32;
            let is_releasing = scaled_desired_gain > self.compressor_gain;
            let mut compression_diff_db = if scaled_desired_gain == 0.0 {
                if is_releasing { -1.0 } else { 1.0 }
            } else {
                linear_to_db(self.compressor_gain / scaled_desired_gain)
            };
            let envelope_rate = if is_releasing {
                // Release: adaptive, faster the deeper the compression.
                self.max_attack_compression_diff_db = -1.0;
                if !compression_diff_db.is_finite() {
                    compression_diff_db = -1.0;
                }
                let x = 0.25 * (compression_diff_db.clamp(-12.0, 0.0) + 12.0);
                let (x2, x3) = (x * x, x * x * x);
                let x4 = x2 * x2;
                let release = ka + kb * x + kc * x2 + kd * x3 + ke * x4;
                db_to_linear(5.0 / release)
            } else {
                // Attack: at the rate of the largest difference seen in this attack.
                if !compression_diff_db.is_finite() {
                    compression_diff_db = 1.0;
                }
                if self.max_attack_compression_diff_db == -1.0 || self.max_attack_compression_diff_db < compression_diff_db {
                    self.max_attack_compression_diff_db = compression_diff_db;
                }
                let eff = self.max_attack_compression_diff_db.max(0.5);
                let x = 0.25 / eff;
                1.0 - fdlibm_powf(x, 1.0 / attack_frames)
            };
            let mut detector_average = self.detector_average;
            let mut compressor_gain = self.compressor_gain;
            for _ in 0..N_DIVISION_FRAMES {
                if frame >= frames {
                    break;
                }
                let mut compressor_input = 0.0f32;
                for c in 0..channels {
                    let undelayed = src.get(c).map(|s| s[frame]).unwrap_or(0.0);
                    self.pre_delay[c][self.write] = undelayed;
                    let a = undelayed.abs();
                    if compressor_input < a {
                        compressor_input = a;
                    }
                }
                let abs_input = compressor_input.abs();
                let shaped = self.saturate(abs_input, k);
                let attenuation = if abs_input <= 0.0001 { 1.0 } else { shaped / abs_input };
                let attenuation_db = (-linear_to_db(attenuation)).max(2.0);
                let db_per_frame = attenuation_db / sat_release_frames;
                let sat_release_rate = db_to_linear(db_per_frame) - 1.0;
                let rate = if attenuation > detector_average { sat_release_rate } else { 1.0 };
                detector_average += (attenuation - detector_average) * rate;
                detector_average = detector_average.min(1.0);
                if !detector_average.is_finite() {
                    detector_average = 1.0;
                }
                if envelope_rate < 1.0 {
                    compressor_gain += (scaled_desired_gain - compressor_gain) * envelope_rate;
                } else {
                    compressor_gain *= envelope_rate;
                    compressor_gain = compressor_gain.min(1.0);
                }
                let post_warp = ((PI_OVER_TWO_F32 * compressor_gain) as f64).sin() as f32;
                let total_gain = linear_post_gain * post_warp;
                let db_real_gain = linear_to_db(post_warp);
                if db_real_gain < self.metering_gain {
                    self.metering_gain = db_real_gain;
                } else {
                    self.metering_gain += (db_real_gain - self.metering_gain) * metering_release_k;
                }
                for c in 0..channels {
                    out[c][frame] = self.pre_delay[c][self.read] * total_gain;
                }
                frame += 1;
                self.read = (self.read + 1) & mask;
                self.write = (self.write + 1) & mask;
            }
            // Denormals flush to zero, as Chrome's DenormalDisabler does.
            self.detector_average = if detector_average.abs() < f32::MIN_POSITIVE { 0.0 } else { detector_average };
            self.compressor_gain = if compressor_gain.abs() < f32::MIN_POSITIVE { 0.0 } else { compressor_gain };
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compress(
    node: &mut Node,
    src: &[Vec<f32>],
    out: &mut [Vec<f32>],
    width: usize,
    n: usize,
    sr: f64,
    t0: f64,
    param_add: &HashMap<AudioParamId, Vec<f32>>,
) {
    // Every compressor parameter is k-rate: read once per render quantum, at its start.
    let p = |id: AudioParamId, d: f32| -> f32 {
        let base = node.params.get(&id).map(|x| x.timeline_value(t0)).unwrap_or(d);
        base + param_add.get(&id).and_then(|v| v.first().copied()).unwrap_or(0.0)
    };
    let threshold = p(AudioParamId::Threshold, -24.0);
    let knee = p(AudioParamId::Knee, 30.0).max(0.0);
    let ratio = p(AudioParamId::Ratio, 12.0).max(1.0);
    let attack = p(AudioParamId::Attack, 0.003).max(0.0);
    let release = p(AudioParamId::Release, 0.25).max(0.0);
    let mut kernel = std::mem::take(&mut node.rt.comp);
    kernel.process(src, out, width, n, sr as f32, threshold, knee, ratio, attack, release);
    node.rt.envelope_db = kernel.metering_gain as f64;
    node.rt.comp = kernel;
}

/// How many channels a node's output `idx` carries. Every node has one output except
/// `ChannelSplitterNode`, whose N outputs are one channel each.
fn output_width(n: &Node, _idx: u32) -> usize {
    match n.kind {
        // Every splitter output is exactly one channel, whether or not the input had that many.
        AudioNodeKind::ChannelSplitter => 1,
        _ => n.out.len().max(1),
    }
}

/// A node's output `idx`, widened to `width` channels.
fn up_mix(n: &Node, idx: u32, width: usize, frames: usize, interp: u32) -> Vec<Vec<f32>> {
    let chans: Vec<Vec<f32>> = match n.kind {
        AudioNodeKind::ChannelSplitter => {
            vec![n.out.get(idx as usize).cloned().unwrap_or_else(|| vec![0.0; frames])]
        }
        _ => n.out.clone(),
    };
    remix(&chans, width, frames, interp)
}

/// A node's output as one mono channel — what an `AudioParam` connection reads.
fn down_to_mono(n: &Node, frames: usize) -> Vec<f32> {
    if n.out.is_empty() {
        return vec![0.0; frames];
    }
    (0..frames)
        .map(|k| n.out.iter().map(|c| c.get(k).copied().unwrap_or(0.0)).sum::<f32>() / n.out.len() as f32)
        .collect()
}

/// Up/down-mix `src` to `width` channels.
///
/// "speakers" interpretation implements the spec's named cases that matter — mono↔stereo, and
/// stereo→mono as the average — and falls back to discrete for the surround layouts, which no
/// fingerprinting graph builds and which a wrong guess at would be worse than an honest truncation.
fn remix(src: &[Vec<f32>], width: usize, frames: usize, interp: u32) -> Vec<Vec<f32>> {
    let mut out = vec![vec![0.0f32; frames]; width.max(1)];
    if src.is_empty() {
        return out;
    }
    let sw = src.len();
    if sw == width {
        for (c, o) in out.iter_mut().enumerate() {
            for (k, v) in o.iter_mut().enumerate().take(frames) {
                *v = src[c].get(k).copied().unwrap_or(0.0);
            }
        }
        return out;
    }
    if interp == 0 && sw == 1 && width == 2 {
        // mono → stereo: the same signal on both, per the spec's up-mix table.
        for k in 0..frames {
            let v = src[0].get(k).copied().unwrap_or(0.0);
            out[0][k] = v;
            out[1][k] = v;
        }
        return out;
    }
    if interp == 0 && sw == 2 && width == 1 {
        for (k, o) in out[0].iter_mut().enumerate().take(frames) {
            *o = 0.5 * (src[0].get(k).copied().unwrap_or(0.0) + src[1].get(k).copied().unwrap_or(0.0));
        }
        return out;
    }
    // Discrete: channel i to channel i, extra sources dropped, missing ones silent.
    for (c, o) in out.iter_mut().enumerate() {
        if let Some(s) = src.get(c) {
            for (k, v) in o.iter_mut().enumerate().take(frames) {
                *v = s.get(k).copied().unwrap_or(0.0);
            }
        }
    }
    out
}

/// Depth-first topological order. A node in a cycle lands before the edge that closes the cycle, so
/// that edge reads the previous quantum's output — which is what a real feedback loop does and why
/// the spec requires a `DelayNode` in one.
fn topo_order(nodes: &HashMap<u32, Node>, param_edges: &[(u32, u32, AudioParamId)]) -> Vec<u32> {
    // Edges point source → consumer, so a consumer is visited after everything feeding it.
    let mut deps: HashMap<u32, Vec<u32>> = HashMap::new();
    for (id, n) in nodes {
        let e = deps.entry(*id).or_default();
        for slot in &n.inputs {
            for (src, _) in slot {
                e.push(*src);
            }
        }
    }
    for (from, to, _) in param_edges {
        deps.entry(*to).or_default().push(*from);
    }
    let mut order = Vec::with_capacity(nodes.len());
    let mut state: HashMap<u32, u8> = HashMap::new(); // 0 unseen, 1 on stack, 2 done
    // A deterministic starting order: node ids ascending, which is creation order.
    let mut ids: Vec<u32> = nodes.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        visit(id, &deps, &mut state, &mut order, nodes);
    }
    order
}

fn visit(
    id: u32,
    deps: &HashMap<u32, Vec<u32>>,
    state: &mut HashMap<u32, u8>,
    order: &mut Vec<u32>,
    nodes: &HashMap<u32, Node>,
) {
    match state.get(&id) {
        Some(2) | Some(1) => return,
        _ => {}
    }
    state.insert(id, 1);
    if let Some(d) = deps.get(&id) {
        let mut d = d.clone();
        d.sort_unstable();
        for s in d {
            if nodes.contains_key(&s) {
                visit(s, deps, state, order, nodes);
            }
        }
    }
    state.insert(id, 2);
    order.push(id);
}

// ── the two entry points ──────────────────────────────────────────────────────────────────────

/// Render a graph to interleaved PCM — [`spec::RenderBackend::render_audio`].
pub fn render(graph: &[AudioOp], frames: u32, channels: u32, sample_rate: f32) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    if frames == 0 || sample_rate <= 0.0 {
        return vec![0.0; frames as usize * ch];
    }
    let mut e = Engine::build(graph, channels, sample_rate);
    e.run(frames as usize, None)
}

/// Answer one question about a graph — [`spec::RenderBackend::query_audio`].
///
/// It runs the SAME render, with the queried analyser taped. That is the whole design: there is one
/// route from an op list to numbers, so `getChannelData` and `getFloatFrequencyData` cannot come to
/// different conclusions about what the graph is doing.
pub fn query(
    graph: &[AudioOp],
    q: &AudioQuery,
    frames: u32,
    channels: u32,
    sample_rate: f32,
) -> AudioQueryResult {
    if sample_rate <= 0.0 {
        return AudioQueryResult::None;
    }
    let node_id = match q {
        AudioQuery::FrequencyDb { node, .. }
        | AudioQuery::TimeDomain { node, .. }
        | AudioQuery::Reduction { node } => *node,
    };
    let mut e = Engine::build(graph, channels, sample_rate);
    if !e.nodes.contains_key(&node_id) {
        return AudioQueryResult::None;
    }
    let tap = matches!(q, AudioQuery::FrequencyDb { .. } | AudioQuery::TimeDomain { .. });
    e.run(frames as usize, if tap { Some(node_id) } else { None });
    match q {
        AudioQuery::FrequencyDb { bins, .. } => {
            let n = e.nodes.get(&node_id).unwrap();
            let mut v = Vec::with_capacity(*bins);
            for b in 0..*bins {
                let m = n.rt.smoothed.get(b).copied().unwrap_or(0.0);
                // 20·log10(0) is −Infinity, and that is the CORRECT answer for a bin with no
                // energy. A real browser reports −Infinity for a fresh analyser with nothing
                // connected; the tape reported −170.02, a plausible noise floor that was not there,
                // and that single expression was the cheapest audio detection in the engine.
                v.push((20.0 * m.log10()) as f32);
            }
            AudioQueryResult::Samples(v)
        }
        AudioQuery::TimeDomain { len, .. } => {
            let n = e.nodes.get(&node_id).unwrap();
            let have = n.rt.tap.len();
            let start = have.saturating_sub(*len);
            let mut v = vec![0.0f32; *len];
            for (i, s) in v.iter_mut().enumerate() {
                if start + i < have {
                    *s = n.rt.tap[start + i];
                }
            }
            AudioQueryResult::Samples(v)
        }
        AudioQuery::Reduction { .. } => {
            let n = e.nodes.get(&node_id).unwrap();
            AudioQueryResult::Scalar(n.rt.envelope_db as f32)
        }
    }
}

#[cfg(test)]
mod tests;
