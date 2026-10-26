//! The DSP's own tests. These assert the properties the tape could not have — that the samples are
//! a function of the graph — rather than any particular byte pattern, which is deliberate: a golden
//! sample vector here would be a recording again, one commit later.

use super::*;
use spec::{AudioNodeKind as K, AudioOp as Op, AudioParamId as P, AudioProp, AudioQuery};

const SR: f32 = 44100.0;

/// The canonical fingerprint graph: a triangle oscillator into a DynamicsCompressor into the
/// destination, at `freq` Hz.
fn fingerprint_graph(freq: f32) -> Vec<Op> {
    vec![
        Op::Create { id: 1, kind: K::Oscillator, arg: 0.0 },
        Op::SetProp { id: 1, prop: AudioProp::OscillatorType(OscillatorType::Triangle) },
        Op::SetParam { id: 1, param: P::Frequency, value: freq },
        Op::Create { id: 2, kind: K::DynamicsCompressor, arg: 0.0 },
        Op::SetParam { id: 2, param: P::Threshold, value: -50.0 },
        Op::SetParam { id: 2, param: P::Knee, value: 40.0 },
        Op::SetParam { id: 2, param: P::Ratio, value: 12.0 },
        Op::SetParam { id: 2, param: P::Attack, value: 0.0 },
        Op::SetParam { id: 2, param: P::Release, value: 0.25 },
        Op::Connect { from: 1, from_output: 0, to: 2, to_input: 0 },
        Op::Connect { from: 2, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ]
}

/// The window a fingerprinting library sums: |d[i]| over samples 4500..5000.
fn fp_sum(pcm: &[f32]) -> f64 {
    pcm[4500..5000].iter().map(|v| v.abs() as f64).sum()
}

#[test]
fn the_same_graph_at_two_frequencies_is_two_signals() {
    let a = render(&fingerprint_graph(10000.0), 44100, 1, SR);
    let b = render(&fingerprint_graph(1000.0), 44100, 1, SR);
    let (sa, sb) = (fp_sum(&a), fp_sum(&b));
    assert!(sa > 0.0 && sb > 0.0, "both graphs must produce sound: {sa} {sb}");
    // The tape returned the SAME sum for both, to the last bit. The bar is that the samples are a
    // function of the graph, not that they match Brave's 124.0167 / 154.9952 — this host is Linux,
    // Brave farbles audio, and the recorded tape was a Mac's. A 1% relative separation is far below
    // what a band-limited triangle actually produces here and far above float noise.
    let rel = (sa - sb).abs() / sa.max(sb);
    assert!(rel > 0.01, "10 kHz and 1 kHz must not give the same fingerprint sum: {sa} vs {sb}");
}

#[test]
fn the_output_is_not_periodic_at_256() {
    // The tape repeated every 256 samples, which put all of a 44,100-sample buffer's energy at
    // multiples of 172.27 Hz. A real signal does not do that, and this is the assertion that says so.
    let pcm = render(&fingerprint_graph(10000.0), 44100, 1, SR);
    let differing = (0..44100 - 256).filter(|&i| pcm[i] != pcm[i + 256]).count();
    assert!(
        differing > 40000,
        "d[i] must not equal d[i+256] in general; only {differing} of 43,844 differ"
    );
}

#[test]
fn silence_in_means_silence_out() {
    // No source: the destination is genuinely zero, not a noise floor. This is the honest form of
    // "nothing is connected" and the thing the tape could never say.
    let pcm = render(&[], 1024, 2, SR);
    assert!(pcm.iter().all(|v| *v == 0.0));
}

#[test]
fn a_fresh_analyser_reads_negative_infinity() {
    // Brave: -Infinity. The tape: -170.02041625976562, a plausible noise floor that was not there.
    let graph = vec![Op::Create { id: 1, kind: K::Analyser, arg: 0.0 }];
    let r = query(&graph, &AudioQuery::FrequencyDb { node: 1, bins: 1024 }, 0, 1, SR);
    let AudioQueryResult::Samples(v) = r else { panic!("expected samples") };
    assert_eq!(v.len(), 1024);
    assert!(v.iter().all(|x| *x == f32::NEG_INFINITY), "first four: {:?}", &v[..4]);
}

#[test]
fn a_connected_analyser_reads_a_real_spectrum_with_no_period() {
    let mut graph = vec![
        Op::Create { id: 1, kind: K::Oscillator, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Frequency, value: 1000.0 },
        Op::Create { id: 3, kind: K::Analyser, arg: 0.0 },
        Op::Connect { from: 1, from_output: 0, to: 3, to_input: 0 },
        Op::Connect { from: 3, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    graph.push(Op::SetProp { id: 3, prop: AudioProp::SmoothingTimeConstant(0.0) });
    let r = query(&graph, &AudioQuery::FrequencyDb { node: 3, bins: 1024 }, 8192, 1, SR);
    let AudioQueryResult::Samples(f) = r else { panic!("expected samples") };
    // The tape gave f[0] === f[256] === f[512] === f[768], a detected period of exactly 256.
    assert!(
        f[0] != f[256] || f[256] != f[512] || f[512] != f[768],
        "the spectrum must not be periodic at 256: {} {} {} {}",
        f[0],
        f[256],
        f[512],
        f[768]
    );
    // A 1 kHz sine at 44.1 kHz with a 2048-point FFT peaks near bin 46.
    let peak = f
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(i, _)| i)
        .unwrap();
    assert!((44..49).contains(&peak), "1 kHz should peak near bin 46, got {peak}");
}

#[test]
fn gain_scales_and_the_destination_reads_it() {
    let base = vec![
        Op::Create { id: 1, kind: K::ConstantSource, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Offset, value: 0.5 },
        Op::Create { id: 2, kind: K::Gain, arg: 0.0 },
        Op::Connect { from: 1, from_output: 0, to: 2, to_input: 0 },
        Op::Connect { from: 2, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let mut g = base.clone();
    g.push(Op::SetParam { id: 2, param: P::Gain, value: 0.25 });
    let pcm = render(&g, 256, 1, SR);
    assert!((pcm[100] - 0.125).abs() < 1e-6, "got {}", pcm[100]);
    let pcm = render(&base, 256, 1, SR);
    assert!((pcm[100] - 0.5).abs() < 1e-6, "got {}", pcm[100]);
}

#[test]
fn start_and_stop_are_sample_accurate() {
    let graph = vec![
        Op::Create { id: 1, kind: K::ConstantSource, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Offset, value: 1.0 },
        Op::Connect { from: 1, from_output: 0, to: 0, to_input: 0 },
        // 100 samples in, off again 100 samples later.
        Op::Start { id: 1, when: 100.0 / 44100.0, offset: 0.0, duration: None },
        Op::Stop { id: 1, when: 200.0 / 44100.0 },
    ];
    let pcm = render(&graph, 512, 1, SR);
    assert_eq!(pcm[99], 0.0);
    assert_eq!(pcm[100], 1.0);
    assert_eq!(pcm[199], 1.0);
    assert_eq!(pcm[200], 0.0);
}

#[test]
fn the_compressor_reduces_and_reports_its_reduction() {
    // A loud constant well over the threshold: the compressor must pull it down and `reduction` must
    // be the dB it pulled -- Chrome's static curve for this graph, since the kernel is its port:
    // knee 30 dB from -50 to -20, `k` solved so the slope at -20 dB is 1/12
    // (`kAtSlope`, k = 38.64), the knee's output there -30.924 dB, then 1/12 dB per dB above it:
    // -30.924 + 20/12 = -29.2574 dB for a 0 dBFS input. (The bound here was "tens of dB",
    // -60 < r < -30, written for the model this replaced; the port reads -29.257454.)
    let graph = vec![
        Op::Create { id: 1, kind: K::ConstantSource, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Offset, value: 1.0 },
        Op::Create { id: 2, kind: K::DynamicsCompressor, arg: 0.0 },
        Op::SetParam { id: 2, param: P::Threshold, value: -50.0 },
        Op::SetParam { id: 2, param: P::Ratio, value: 12.0 },
        Op::Connect { from: 1, from_output: 0, to: 2, to_input: 0 },
        Op::Connect { from: 2, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let pcm = render(&graph, 44100, 1, SR);
    assert!(pcm[40000].abs() < 0.5, "a 0 dBFS input over a -50 dB threshold must be pulled down: {}", pcm[40000]);
    let AudioQueryResult::Scalar(r) = query(&graph, &AudioQuery::Reduction { node: 2 }, 44100, 1, SR) else {
        panic!("expected a scalar")
    };
    assert!((r - -29.2574).abs() < 0.01, "reduction should be Chrome's -29.2574 dB, got {r}");
}

#[test]
fn an_automation_ramp_is_a_ramp() {
    let graph = vec![
        Op::Create { id: 1, kind: K::ConstantSource, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Offset, value: 1.0 },
        Op::Create { id: 2, kind: K::Gain, arg: 0.0 },
        Op::SetParam { id: 2, param: P::Gain, value: 0.0 },
        Op::Automate {
            id: 2,
            param: P::Gain,
            event: AutomationEvent::SetValueAtTime { value: 0.0, time: 0.0 },
        },
        Op::Automate {
            id: 2,
            param: P::Gain,
            event: AutomationEvent::LinearRampToValueAtTime { value: 1.0, time: 1.0 },
        },
        Op::Connect { from: 1, from_output: 0, to: 2, to_input: 0 },
        Op::Connect { from: 2, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let pcm = render(&graph, 44100, 1, SR);
    assert!(pcm[0].abs() < 1e-4);
    assert!((pcm[22050] - 0.5).abs() < 0.01, "half way should be ~0.5, got {}", pcm[22050]);
    assert!((pcm[44099] - 1.0).abs() < 0.01, "the end should be ~1.0, got {}", pcm[44099]);
}

#[test]
fn a_band_limited_triangle_loses_harmonics_as_it_rises() {
    // At 44.1 kHz a 10 kHz triangle has exactly one harmonic under Nyquist, so it IS a sine; a
    // 1 kHz triangle has eleven and is visibly a triangle. This is the property a naive generator
    // would not have, and it is why the two frequencies give different fingerprints for a reason
    // rather than by accident.
    let (re, im) = builtin_coefficients(OscillatorType::Triangle, periodic_wave_size(44100.0) / 2);
    let w = Wavetable::build(&re, &im, 44100.0, true);
    // The table a fundamental reads, blended between its two bracketing bands as the oscillator
    // blends them, at 64 phases of the period.
    let read = |freq: f32, i: usize| {
        let (lower, higher, tif) = w.bands_for(freq);
        let idx = i * w.size / 64;
        higher[idx] + tif * (lower[idx] - higher[idx])
    };
    let sines: Vec<f32> = (0..64).map(|i| read(10000.0, i)).collect();
    let tri: Vec<f32> = (0..64).map(|i| read(1000.0, i)).collect();
    // A pure sine's quarter-period value is its peak; a triangle built from 11 harmonics is not
    // within a thousandth of a sine anywhere near the corner.
    let err: f32 = (0..64)
        .map(|i| (tri[i] - sines[i]).abs())
        .fold(0.0f32, f32::max);
    assert!(err > 0.02, "the two bands must differ; max |Δ| was {err}");
}

#[test]
fn a_delay_delays() {
    let graph = vec![
        Op::Create { id: 1, kind: K::ConstantSource, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Offset, value: 1.0 },
        Op::Create { id: 2, kind: K::Delay, arg: 1.0 },
        Op::SetParam { id: 2, param: P::DelayTime, value: 100.0 / 44100.0 },
        Op::Connect { from: 1, from_output: 0, to: 2, to_input: 0 },
        Op::Connect { from: 2, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let pcm = render(&graph, 512, 1, SR);
    assert!(pcm[50].abs() < 1e-6, "still inside the delay: {}", pcm[50]);
    assert!((pcm[300] - 1.0).abs() < 1e-4, "past the delay: {}", pcm[300]);
}

#[test]
fn a_buffer_source_plays_the_buffer_it_was_given() {
    let data: Vec<f32> = (0..1000).map(|i| (i as f32 / 1000.0) * 2.0 - 1.0).collect();
    let graph = vec![
        Op::Create { id: 1, kind: K::BufferSource, arg: 0.0 },
        Op::SetBuffer { id: 1, channels: vec![data.clone()], sample_rate: SR },
        Op::Connect { from: 1, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let pcm = render(&graph, 1024, 1, SR);
    for i in [0usize, 100, 500, 999] {
        assert!((pcm[i] - data[i]).abs() < 1e-5, "sample {i}: {} vs {}", pcm[i], data[i]);
    }
    assert_eq!(pcm[1000], 0.0, "past the end of the buffer is silence");
}

#[test]
fn the_analyser_does_not_alter_the_signal_it_taps() {
    // The rule the 2D path broke: two readbacks of one graph must agree. An analyser that coloured
    // its pass-through would make `getChannelData` and `getFloatFrequencyData` describe different
    // graphs, which is precisely the split this seam is shaped to prevent.
    let with = vec![
        Op::Create { id: 1, kind: K::Oscillator, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Frequency, value: 1000.0 },
        Op::Create { id: 3, kind: K::Analyser, arg: 0.0 },
        Op::Connect { from: 1, from_output: 0, to: 3, to_input: 0 },
        Op::Connect { from: 3, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let without = vec![
        Op::Create { id: 1, kind: K::Oscillator, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Frequency, value: 1000.0 },
        Op::Connect { from: 1, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let a = render(&with, 2048, 1, SR);
    let b = render(&without, 2048, 1, SR);
    assert_eq!(a, b);
}

#[test]
fn a_stereo_render_has_two_channels_interleaved() {
    let graph = vec![
        Op::Create { id: 1, kind: K::ConstantSource, arg: 0.0 },
        Op::SetParam { id: 1, param: P::Offset, value: 0.7 },
        Op::Connect { from: 1, from_output: 0, to: 0, to_input: 0 },
        Op::Start { id: 1, when: 0.0, offset: 0.0, duration: None },
    ];
    let pcm = render(&graph, 128, 2, SR);
    assert_eq!(pcm.len(), 256);
    // mono up-mixed to stereo: both channels carry it.
    assert!((pcm[0] - 0.7).abs() < 1e-6 && (pcm[1] - 0.7).abs() < 1e-6);
}
