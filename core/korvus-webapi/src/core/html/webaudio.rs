//! Web Audio — the context, node and param lifecycle, native, recording into a `spec::AudioOp`
//! graph that `korvus-render` executes at readback.
//!
//! ## What was here before
//!
//! A **256-entry recorded tape**, in `html_canvas_element.rs`, and eight `undefined` attributes.
//! `AudioContext.state`, `.sampleRate`, `.baseLatency`, `.currentTime`, `.destination`,
//! `.destination.channelCount`, `OfflineAudioContext.length` and `.sampleRate` all read `undefined`;
//! `sampleRate` alone is read by every audio-fingerprinting script that exists. `getChannelData`
//! returned `tape[i % 256]` across 44,100 samples, `getFloatFrequencyData` returned
//! `freq[i % 256]`, and `startRendering()` fabricated an `AudioBuffer` without ever looking at the
//! graph. Three one-expression detections followed and all three were verified live against Brave
//! 151 on this machine:
//!
//! ```text
//! 1. A fresh AnalyserNode with NOTHING connected
//!      Brave  f[0] = -Infinity        Korvus  f[0] = -170.02041625976562
//! 2. f[0] === f[256] === f[512] === f[768]     -- a detected period of exactly 256
//! 3. d[i] === d[i+256] across all 44,100 samples, so all the energy sat at multiples of 172.27 Hz
//! ```
//!
//! The tape did not even reproduce the machine it was recorded from: the fingerprint window is
//! samples 4500–5000 and the capture was taken there, but `getChannelData` tiled from zero, so
//! index 4500 served `tape[148]`.
//!
//! ## The shape, and why it is the WebGL one
//!
//! `webgl.rs` records into a per-context `GlOp` list held in `OpState` and keyed by a `v8::Private`
//! slot on the context object; `readPixels` and `toDataURL` both go through the backend, which is
//! what makes them agree. This is the same structure with `AudioOp` in place of `GlOp` — one store,
//! one id, one list. `AudioBuffer.getChannelData` reads the PCM one `render_audio` produced;
//! `AnalyserNode`'s four readbacks come from `query_audio` running the same list through the same
//! `Engine`. Neither has a recorded fallback to fall back to.
//!
//! ## What the persona still supplies, and what it no longer does
//!
//! `AudioContext.sampleRate` is device-varying and comes from `deviceOracle.audio.sampleRate` — the
//! LIVE context's rate. `OfflineAudioContext.sampleRate` is its constructor's third argument, per
//! spec, and is never the persona's. The 256-entry `samples`/`floatFreq` tape and the recorded
//! `reduction` constant are gone from the profile: a recording kept "as a fallback" is exactly how
//! the previous state came to be believed working.
//!
//! ## Which nodes compute
//!
//! Oscillator, Gain, DynamicsCompressor, Analyser, AudioBufferSource, ConstantSource, Delay,
//! BiquadFilter, StereoPanner, WaveShaper, ChannelMerger, ChannelSplitter and the destination all
//! render. `ConvolverNode`, `PannerNode`, `IIRFilterNode` and `ScriptProcessorNode` are real objects
//! with the right surface but have no processor, so a graph routed through one is SILENT rather than
//! quietly patched around — a visible red line instead of an invisible fallback. `AudioListener`'s
//! nine params store and read back the values set on them; nothing consumes them, because the node
//! that would (`PannerNode`, HRTF) does not render.

use deno_core::v8;
use spec::{
    AudioNodeKind, AudioOp, AudioParamId, AudioProp, AudioQuery, AudioQueryResult, AutomationEvent,
    BiquadKind, OscillatorType, RenderParams,
};
use std::collections::HashMap;
use std::time::Instant;

use super::html_canvas_element::priv_get;
use crate::binding::protos::{
    body_data, build_fn_data, get_prop, int_val, require_args, str_val,
    throw_dom, throw_type_error,
};

/// The store id, on a `BaseAudioContext`.
const CTX_PRIV: &str = "korvus_audio_ctx";
/// A node's owning context and its own id.
const NODE_CTX_PRIV: &str = "korvus_audio_nctx";
const NODE_ID_PRIV: &str = "korvus_audio_node";
/// An `AudioParam`'s owner and which param it is.
const PARAM_NODE_PRIV: &str = "korvus_audio_pnode";
const PARAM_NAME_PRIV: &str = "korvus_audio_pname";
/// An `AudioBuffer`'s shape. Its channel data lives on `korvus_audio_ch{n}` as real
/// `Float32Array`s, so `getChannelData` hands back the SAME array every call (as a browser does)
/// and a page that writes into it is writing into the buffer.
const BUF_LEN_PRIV: &str = "korvus_audio_blen";
const BUF_RATE_PRIV: &str = "korvus_audio_brate";
const BUF_CH_PRIV: &str = "korvus_audio_bch";

// ── store ─────────────────────────────────────────────────────────────────────────────────────

/// What a node's own attributes read back as. This is a PROJECTION of the op list, not a second
/// source of truth: every field here is written in the same statement that records the op, so the
/// two cannot drift.
#[derive(Debug, Clone)]
pub struct NodeMeta {
    pub kind: Option<AudioNodeKind>,
    /// The interface name, for the nodes that have no `AudioNodeKind` because nothing renders them.
    pub iface: &'static str,
    pub arg: f64,
    pub inputs: u32,
    pub outputs: u32,
    pub channel_count: u32,
    pub channel_count_mode: &'static str,
    pub channel_interpretation: &'static str,
    pub osc_type: String,
    pub biquad_type: String,
    pub fft_size: u32,
    pub min_db: f64,
    pub max_db: f64,
    pub smoothing: f64,
    pub looping: bool,
    pub loop_start: f64,
    pub loop_end: f64,
    pub oversample: String,
    /// `AudioScheduledSourceNode.start()` has been called. `stop()` before it is an
    /// `InvalidStateError`, which makes the ORDER observable and is not answerable from the op list
    /// alone (a `Stop` with no `Start` is exactly what has to be refused).
    pub started: bool,
    /// `ScriptProcessorNode.bufferSize` — `createScriptProcessor`'s first argument.
    pub buffer_size: u32,
    /// `PannerNode`'s non-`AudioParam` geometry: `panningModel`, `distanceModel`, and the six
    /// distance/cone scalars. Stored and readable; consumed by nothing, because the node that would
    /// consume them (HRTF panning) has no processor — see the module comment.
    pub panning_model: String,
    pub distance_model: String,
    pub ref_distance: f64,
    pub max_distance: f64,
    pub rolloff_factor: f64,
    pub cone_inner_angle: f64,
    pub cone_outer_angle: f64,
    pub cone_outer_gain: f64,
    pub params: HashMap<&'static str, f32>,
}

impl NodeMeta {
    fn new(iface: &'static str, kind: Option<AudioNodeKind>, arg: f64) -> Self {
        let (inputs, outputs) = match iface {
            "OscillatorNode" | "AudioBufferSourceNode" | "ConstantSourceNode" => (0, 1),
            "AudioDestinationNode" => (1, 0),
            "ChannelSplitterNode" => (1, arg.max(1.0) as u32),
            "ChannelMergerNode" => (arg.max(1.0) as u32, 1),
            _ => (1, 1),
        };
        // The per-node channelCount/mode/interpretation defaults from the spec's table.
        let (cc, mode, interp) = match iface {
            "ChannelSplitterNode" => (arg.max(1.0) as u32, "explicit", "discrete"),
            "ChannelMergerNode" => (1, "explicit", "speakers"),
            "DynamicsCompressorNode" | "StereoPannerNode" | "PannerNode" => (2, "clamped-max", "speakers"),
            _ => (2, "max", "speakers"),
        };
        let mut params = HashMap::new();
        for (n, d) in param_defaults(iface) {
            params.insert(*n, *d);
        }
        Self {
            kind,
            iface,
            arg,
            inputs,
            outputs,
            channel_count: cc,
            channel_count_mode: mode,
            channel_interpretation: interp,
            osc_type: "sine".into(),
            biquad_type: "lowpass".into(),
            fft_size: 2048,
            min_db: -100.0,
            max_db: -30.0,
            smoothing: 0.8,
            looping: false,
            loop_start: 0.0,
            loop_end: 0.0,
            oversample: "none".into(),
            started: false,
            buffer_size: 0,
            panning_model: "equalpower".into(),
            distance_model: "inverse".into(),
            ref_distance: 1.0,
            max_distance: 10000.0,
            rolloff_factor: 1.0,
            cone_inner_angle: 360.0,
            cone_outer_angle: 360.0,
            cone_outer_gain: 0.0,
            params,
        }
    }
}

/// One `BaseAudioContext`'s recorded state.
pub struct AudioCtx {
    pub ops: Vec<AudioOp>,
    pub offline: bool,
    pub sample_rate: f32,
    /// `OfflineAudioContext.length`, in frames. For a live context this is 0 and the render window
    /// is derived from the clock instead.
    pub length: u32,
    pub channels: u32,
    pub max_channels: u32,
    /// When the live context was constructed — `currentTime` is the quantum-aligned time since.
    pub created: Instant,
    pub rendered: bool,
    /// An offline context's render is on its thread and its answer task has not run: the context is
    /// `running` (Blink sets it in `startOfflineRendering` and closes it in `FireCompletionEvent`).
    pub rendering: bool,
    /// A live context made with no user activation: Chrome's autoplay policy holds it
    /// `suspended` -- `currentTime` 0, nothing rendered (an analyser reads -Infinity dB and a flat
    /// 128), `outputLatency` 0 -- and its `resume()` waits for a user gesture this engine never
    /// makes. Brave 151 / Chrome 150, measured 2026-10-01 (c8/repro/async/audio.html). Before this
    /// a live context ran from construction, so a page's oscillator-and-analyser probe read a real
    /// spectrum no browser shows without a click.
    pub suspended: bool,
    next_node: u32,
    pub nodes: HashMap<u32, NodeMeta>,
}

impl AudioCtx {
    fn mint(&mut self) -> u32 {
        self.next_node += 1;
        self.next_node
    }

    /// The context's render position in frames — how much of the graph the backend should run to
    /// answer a question asked NOW.
    ///
    /// For an offline context this is its whole length once `startRendering()` has run and zero
    /// before, which is exactly what an offline context has rendered. For a live one it is the
    /// quantum-aligned frames elapsed since construction, capped: a page that reads an analyser
    /// after idling for a minute gets the spectrum of its graph at four seconds, because replaying
    /// a minute of DSP to answer one `getFloatFrequencyData` would cost more than the answer is
    /// worth and — for the periodic signals fingerprinting builds — is the same spectrum.
    pub fn position(&self) -> u32 {
        if self.offline {
            return if self.rendered { self.length } else { 0 };
        }
        if self.suspended {
            return 0;
        }
        let cap = (self.sample_rate * 4.0) as u32;
        let elapsed = self.created.elapsed().as_secs_f64();
        let frames = (elapsed * self.sample_rate as f64) as u32;
        (frames / 128 * 128).min(cap)
    }

    pub fn current_time(&self) -> f64 {
        self.position() as f64 / self.sample_rate as f64
    }
}

#[derive(Default)]
pub struct AudioStore {
    pub ctx: HashMap<u32, AudioCtx>,
    next_ctx: u32,
}

// ── param metadata ────────────────────────────────────────────────────────────────────────────

/// `(name, default)` for every `AudioParam` an interface exposes.
fn param_defaults(iface: &str) -> &'static [(&'static str, f32)] {
    match iface {
        "OscillatorNode" => &[("frequency", 440.0), ("detune", 0.0)],
        "GainNode" => &[("gain", 1.0)],
        "DynamicsCompressorNode" => &[
            ("threshold", -24.0),
            ("knee", 30.0),
            ("ratio", 12.0),
            ("attack", 0.003),
            ("release", 0.25),
        ],
        "AudioBufferSourceNode" => &[("playbackRate", 1.0), ("detune", 0.0)],
        "ConstantSourceNode" => &[("offset", 1.0)],
        "DelayNode" => &[("delayTime", 0.0)],
        "BiquadFilterNode" => &[("frequency", 350.0), ("detune", 0.0), ("Q", 1.0), ("gain", 0.0)],
        "StereoPannerNode" => &[("pan", 0.0)],
        "PannerNode" => &[
            ("positionX", 0.0),
            ("positionY", 0.0),
            ("positionZ", 0.0),
            ("orientationX", 1.0),
            ("orientationY", 0.0),
            ("orientationZ", 0.0),
        ],
        "AudioListener" => &[
            ("positionX", 0.0),
            ("positionY", 0.0),
            ("positionZ", 0.0),
            ("forwardX", 0.0),
            ("forwardY", 0.0),
            ("forwardZ", -1.0),
            ("upX", 0.0),
            ("upY", 1.0),
            ("upZ", 0.0),
        ],
        _ => &[],
    }
}

/// `(min, max, automationRate)` for a param, keyed by its name. The bounds are the spec's, and
/// `automationRate` is `"k-rate"` only for the params the spec fixes there.
fn param_range(name: &str, sample_rate: f32, max_delay: f64) -> (f32, f32, &'static str) {
    let most = f32::MAX;
    match name {
        "frequency" => (-sample_rate / 2.0, sample_rate / 2.0, "a-rate"),
        "detune" => (-153600.0, 153600.0, "a-rate"),
        "gain" | "offset" => (-most, most, "a-rate"),
        "threshold" => (-100.0, 0.0, "k-rate"),
        "knee" => (0.0, 40.0, "k-rate"),
        "ratio" => (1.0, 20.0, "k-rate"),
        "attack" | "release" => (0.0, 1.0, "k-rate"),
        "playbackRate" => (-most, most, "k-rate"),
        "delayTime" => (0.0, max_delay as f32, "a-rate"),
        "Q" => (-most, most, "a-rate"),
        "pan" => (-1.0, 1.0, "a-rate"),
        _ => (-most, most, "a-rate"),
    }
}

fn param_id(name: &str) -> Option<AudioParamId> {
    Some(match name {
        "frequency" => AudioParamId::Frequency,
        "detune" => AudioParamId::Detune,
        "gain" => AudioParamId::Gain,
        "threshold" => AudioParamId::Threshold,
        "knee" => AudioParamId::Knee,
        "ratio" => AudioParamId::Ratio,
        "attack" => AudioParamId::Attack,
        "release" => AudioParamId::Release,
        "playbackRate" => AudioParamId::PlaybackRate,
        "offset" => AudioParamId::Offset,
        "delayTime" => AudioParamId::DelayTime,
        "Q" => AudioParamId::Q,
        "pan" => AudioParamId::Pan,
        // AudioListener's and PannerNode's params: real, stored, readable — and not consumed,
        // because the node that would consume them does not render. Returning `None` here is what
        // keeps them OUT of the op list rather than recording an op nothing executes.
        _ => return None,
    })
}

// ── store access ──────────────────────────────────────────────────────────────────────────────

fn store_op_state(scope: &mut v8::PinScope) -> std::rc::Rc<std::cell::RefCell<deno_core::OpState>> {
    deno_core::JsRuntime::op_state_from(scope)
}

fn priv_set(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, val: v8::Local<v8::Value>) {
    if let Some(pname) = v8::String::new(scope, name) {
        let key = v8::Private::for_api(scope, Some(pname));
        obj.set_private(scope, key, val);
    }
}

fn priv_u32(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str) -> Option<u32> {
    priv_get(scope, obj, name).and_then(|v| v.uint32_value(scope))
}

fn priv_f64(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str) -> Option<f64> {
    priv_get(scope, obj, name).and_then(|v| v.number_value(scope))
}

fn node_of(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Option<(u32, u32)> {
    Some((priv_u32(scope, obj, NODE_CTX_PRIV)?, priv_u32(scope, obj, NODE_ID_PRIV)?))
}

/// Record one op on a context.
fn push(scope: &mut v8::PinScope, ctx: u32, op: AudioOp) {
    let op_state = store_op_state(scope);
    let mut st = op_state.borrow_mut();
    if let Some(c) = st.borrow_mut::<AudioStore>().ctx.get_mut(&ctx) {
        c.ops.push(op);
    }
}

fn edit<R>(scope: &mut v8::PinScope, ctx: u32, f: impl FnOnce(&mut AudioCtx) -> R) -> Option<R> {
    let op_state = store_op_state(scope);
    let mut st = op_state.borrow_mut();
    st.borrow_mut::<AudioStore>().ctx.get_mut(&ctx).map(f)
}

fn read<R>(scope: &mut v8::PinScope, ctx: u32, f: impl FnOnce(&AudioCtx) -> R) -> Option<R> {
    let op_state = store_op_state(scope);
    let st = op_state.borrow();
    st.borrow::<AudioStore>().ctx.get(&ctx).map(f)
}

fn backend(scope: &mut v8::PinScope) -> Option<std::sync::Arc<dyn spec::RenderBackend>> {
    let op_state = store_op_state(scope);
    let st = op_state.borrow();
    st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
}

/// Ask the backend a synchronous question about a context's recorded graph.
///
/// The graph, the frame count, the channel count and the rate all come out of the SAME `AudioCtx`
/// that `startRendering` renders from, so an answer here and a sample there describe one run.
fn ask(scope: &mut v8::PinScope, ctx: u32, q: AudioQuery) -> AudioQueryResult {
    let Some((ops, frames, channels, rate)) =
        read(scope, ctx, |c| (c.ops.clone(), c.position(), c.channels, c.sample_rate))
    else {
        return AudioQueryResult::None;
    };
    match backend(scope) {
        Some(b) => b.query_audio(&ops, &q, frames, channels, rate, &RenderParams::default()),
        None => AudioQueryResult::None,
    }
}

// ── object construction ───────────────────────────────────────────────────────────────────────

/// A fresh instance wearing the REALM's prototype for `iface` (`shell_proto` reads the
/// sealed map, not `globalThis[iface]`, which a page can write).
///
/// It is spelled `shell_proto` rather than `install::live::set_table_proto` because this one helper
/// mints instances of BOTH kinds: twenty-eight of the interfaces it is called for are `interface!`
/// tables, and `PeriodicWave` is still a generated registry shell. The read
/// is the same either way -- it is a MINT, not an install -- and naming one of the two would be a
/// lie about the other.
fn new_shell<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Object> {
    let obj = crate::install::live::mint_table(scope, iface);
    obj
}

/// The `MediaStream` a `MediaStreamAudioDestinationNode` exposes: one live audio track over a WebAudio source, as
/// Blink's node builds it (modules/webaudio/media_stream_audio_destination_node.cc). It was a bare shell on
/// `MediaStream.prototype` with no state, so `stream.getAudioTracks()` -- and every member after it -- threw
/// `Illegal invocation` (BrowserScan reads `getSettings`/`getCapabilities`/`getConstraints` off this track).
///
/// The source's id is `"WebAudio-" + a UUID`; its name, which is the track's `label`, is the node's interface
/// name. Its capabilities are the node's own (`deviceId`, echo cancellation / AGC / noise suppression / voice
/// isolation all `[false]`, `sampleSize` 16..16). Its settings are the audio format the destination handler
/// sets at construction (`WebAudioMediaStreamSource::SetFormat`): the node's channel count (2), the context's
/// sample rate, a 10 ms FIFO for `latency`, and the S16 `sampleSize` every audio source reports. Chrome 150,
/// measured on an `AudioContext`'s node: `{channelCount: 2, deviceId: "WebAudio-…", latency: 0.01, sampleRate:
/// 48000, sampleSize: 16}`, the track `live`, enabled, unmuted, its constraints `{}`.
fn destination_stream<'s>(scope: &mut v8::PinScope<'s, '_>, ctx: u32) -> Option<v8::Local<'s, v8::Object>> {
    use crate::binding::instance_state as st;
    use crate::core::media::media_stream::{mint_stream, mint_track, TRACK};
    // `kDefaultNumberOfChannels`: the handler is made with 2, whatever `channelCount` is set to afterwards.
    const CHANNELS: f64 = 2.0;
    let rate = read(scope, ctx, |c| c.sample_rate)?;
    let track = mint_track(scope, TRACK, "audio", false)?;
    let ts = st::state(scope, track.into(), TRACK)?;
    st::set_str(scope, ts, "label", "MediaStreamAudioDestinationNode");
    let device_id = format!("WebAudio-{}", st::uuid());
    // `media::AudioParameters` holds an int rate; the FIFO is `sample_rate / 100` frames, and
    // `GetBufferDuration()` is whole microseconds of float arithmetic.
    let int_rate = rate as i64;
    let frames = (rate / 100.0) as i64;
    let micros = ((frames * 1_000_000) as f32 / int_rate.max(1) as f32) as i64;
    let settings = v8::Object::new(scope);
    st::set_num(scope, settings, "channelCount", CHANNELS);
    st::set_str(scope, settings, "deviceId", &device_id);
    st::set_num(scope, settings, "latency", micros as f64 / 1_000_000.0);
    st::set_num(scope, settings, "sampleRate", int_rate as f64);
    st::set_num(scope, settings, "sampleSize", 16.0);
    st::set(scope, ts, "settings", settings.into());
    let caps = v8::Object::new(scope);
    let agc = only_false(scope);
    st::set(scope, caps, "autoGainControl", agc);
    st::set_str(scope, caps, "deviceId", &device_id);
    let ec = only_false(scope);
    st::set(scope, caps, "echoCancellation", ec);
    let ns = only_false(scope);
    st::set(scope, caps, "noiseSuppression", ns);
    let size = v8::Object::new(scope);
    st::set_num(scope, size, "max", 16.0);
    st::set_num(scope, size, "min", 16.0);
    st::set(scope, caps, "sampleSize", size.into());
    let vi = only_false(scope);
    st::set(scope, caps, "voiceIsolation", vi);
    st::set(scope, ts, "capabilities", caps.into());
    mint_stream(scope, &[track.into()])
}

/// `[false]`: a boolean capability the source cannot turn on.
fn only_false<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    crate::binding::instance_state::array(scope, &[f]).into()
}

/// A fresh `AudioNode`: the right prototype, its context and id on private slots, and nothing else.
///
/// A real browser's audio nodes have NO own properties — every attribute is a prototype accessor.
/// The previous implementation set own data properties (`type`, `frequency`, `fftSize`, …), which
/// read back correctly and showed six extra names to `Object.getOwnPropertyNames(osc)`: a shape
/// difference a page checks in one expression.
fn new_node<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ctx_obj: v8::Local<v8::Object>,
    ctx: u32,
    iface: &'static str,
    kind: Option<AudioNodeKind>,
    arg: f64,
) -> v8::Local<'s, v8::Object> {
    let obj = new_shell(scope, iface);
    init_node(scope, obj, ctx_obj, ctx, iface, kind, arg);
    obj
}

/// Turn an already-allocated object into node `iface` of context `ctx`: mint the id, register the
/// projection, record the `Create`, and hang the three privates that make every accessor find it.
///
/// Split out of [`new_node`] because a CONSTRUCTOR does not get to choose its object — V8 has
/// already built `this` from the interface's `.prototype` by the time the body runs — while a
/// factory method (`ctx.createOscillator()`) has to allocate one. Both routes must produce the same
/// node or `new OscillatorNode(ctx)` and `ctx.createOscillator()` would be distinguishable, which
/// is a one-expression tell.
fn init_node(
    scope: &mut v8::PinScope,
    obj: v8::Local<v8::Object>,
    ctx_obj: v8::Local<v8::Object>,
    ctx: u32,
    iface: &'static str,
    kind: Option<AudioNodeKind>,
    arg: f64,
) -> u32 {
    let id = edit(scope, ctx, |c| {
        let id = c.mint();
        c.nodes.insert(id, NodeMeta::new(iface, kind, arg));
        id
    })
    .unwrap_or(0);
    if let Some(k) = kind {
        push(scope, ctx, AudioOp::Create { id, kind: k, arg });
    }
    let cv = int_val(scope, ctx as i64);
    priv_set(scope, obj, NODE_CTX_PRIV, cv);
    let iv = int_val(scope, id as i64);
    priv_set(scope, obj, NODE_ID_PRIV, iv);
    let ov: v8::Local<v8::Value> = ctx_obj.into();
    priv_set(scope, obj, "korvus_audio_ctxobj", ov);
    id
}

/// A node's `AudioParam`, created once and cached on a private so `osc.frequency ===
/// osc.frequency` — which it is in every browser, and which a freshly built object every call
/// would not be.
fn param_obj<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    node: v8::Local<v8::Object>,
    name: &str,
) -> v8::Local<'s, v8::Value> {
    let slot = format!("korvus_audio_p_{name}");
    if let Some(v) = priv_get(scope, node, &slot) {
        return v;
    }
    let obj = new_shell(scope, "AudioParam");
    let nv: v8::Local<v8::Value> = node.into();
    priv_set(scope, obj, PARAM_NODE_PRIV, nv);
    let sv = str_val(scope, name);
    priv_set(scope, obj, PARAM_NAME_PRIV, sv);
    let v: v8::Local<v8::Value> = obj.into();
    priv_set(scope, node, &slot, v);
    v
}

/// An `AudioBuffer` of `channels × length` at `rate`, with real `Float32Array`s behind it.
fn new_buffer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    channels: usize,
    length: usize,
    rate: f32,
    fill: Option<&[f32]>,
) -> v8::Local<'s, v8::Object> {
    let obj = new_shell(scope, "AudioBuffer");
    let lv = int_val(scope, length as i64);
    priv_set(scope, obj, BUF_LEN_PRIV, lv);
    let rv: v8::Local<v8::Value> = v8::Number::new(scope, rate as f64).into();
    priv_set(scope, obj, BUF_RATE_PRIV, rv);
    let cv = int_val(scope, channels as i64);
    priv_set(scope, obj, BUF_CH_PRIV, cv);
    for c in 0..channels {
        let store = v8::ArrayBuffer::new(scope, length * 4);
        let Some(arr) = v8::Float32Array::new(scope, store, 0, length) else { continue };
        if let Some(pcm) = fill {
            // The interleaved PCM `render_audio` returns, de-interleaved once. This is the ONLY
            // place samples enter an AudioBuffer, so there is one route from the graph to what a
            // page reads and no second one to disagree with it.
            for i in 0..length {
                let v = pcm.get(i * channels + c).copied().unwrap_or(0.0);
                let n = v8::Number::new(scope, v as f64);
                arr.set_index(scope, i as u32, n.into());
            }
        }
        let av: v8::Local<v8::Value> = arr.into();
        priv_set(scope, obj, &format!("korvus_audio_ch{c}"), av);
    }
    obj
}

/// Read an `AudioBuffer`'s channels back out as plain Rust — what `bufferSource.buffer = b` sends
/// to the renderer.
fn buffer_channels(scope: &mut v8::PinScope, buf: v8::Local<v8::Object>) -> Option<(Vec<Vec<f32>>, f32)> {
    let n = priv_u32(scope, buf, BUF_CH_PRIV)? as usize;
    let rate = priv_f64(scope, buf, BUF_RATE_PRIV)? as f32;
    let len = priv_u32(scope, buf, BUF_LEN_PRIV)? as usize;
    let mut out = Vec::with_capacity(n);
    for c in 0..n {
        let mut ch = vec![0.0f32; len];
        if let Some(arr) = priv_get(scope, buf, &format!("korvus_audio_ch{c}"))
            .and_then(|v| v8::Local::<v8::Float32Array>::try_from(v).ok())
        {
            for (i, s) in ch.iter_mut().enumerate() {
                *s = arr.get_index(scope, i as u32).and_then(|v| v.number_value(scope)).unwrap_or(0.0) as f32;
            }
        }
        out.push(ch);
    }
    Some((out, rate))
}

// ── argument reading ──────────────────────────────────────────────────────────────────────────

fn num(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> f64 {
    args.get(i).number_value(scope).filter(|n| n.is_finite()).unwrap_or(0.0)
}
fn opt_num(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> Option<f64> {
    let v = args.get(i);
    if v.is_undefined() {
        None
    } else {
        v.number_value(scope).filter(|n| n.is_finite())
    }
}
fn f32_seq(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<f32> {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return Vec::new() };
    let len = get_prop(scope, obj, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0);
    (0..len)
        .map(|i| obj.get_index(scope, i).and_then(|x| x.number_value(scope)).unwrap_or(0.0) as f32)
        .collect()
}

fn osc_type(s: &str) -> Option<OscillatorType> {
    Some(match s {
        "sine" => OscillatorType::Sine,
        "square" => OscillatorType::Square,
        "sawtooth" => OscillatorType::Sawtooth,
        "triangle" => OscillatorType::Triangle,
        "custom" => OscillatorType::Custom,
        _ => return None,
    })
}

fn biquad_type(s: &str) -> Option<BiquadKind> {
    Some(match s {
        "lowpass" => BiquadKind::Lowpass,
        "highpass" => BiquadKind::Highpass,
        "bandpass" => BiquadKind::Bandpass,
        "lowshelf" => BiquadKind::Lowshelf,
        "highshelf" => BiquadKind::Highshelf,
        "peaking" => BiquadKind::Peaking,
        "notch" => BiquadKind::Notch,
        "allpass" => BiquadKind::Allpass,
        _ => return None,
    })
}

// ── context construction ──────────────────────────────────────────────────────────────────────

/// `new AudioContext()` / `new OfflineAudioContext(channels, length, rate)`.
///
/// The three-argument offline form and the single-dictionary form are both accepted, because pages
/// use both and a `TypeError` on the dictionary form is a behavioural tell.
fn construct(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, offline: bool, persona_rate: f32) {
    let this = args.this();
    let (channels, length, rate) = if !offline {
        // `optional AudioContextOptions contextOptions = {}`, converted as WebIDL converts it (Chrome 150):
        // `latencyHint`, `sampleRate`, `sinkId`, each read once (korvus read none of them). A given
        // `sampleRate` is the context's -- refused outside the AudioBuffer range, in Blink's sentence -- and the
        // hardware's otherwise.
        let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "AudioContextOptions", "Failed to construct 'AudioContext': ") else { return };
        let rate = match get_prop(scope, o, "sampleRate").filter(|v| v.is_number()).and_then(|v| v.number_value(scope)) {
            None => persona_rate,
            Some(r) if (3000.0..=768000.0).contains(&r) => r as f32,
            Some(r) => {
                let given = crate::core::geometry::dom_matrix::js_number((r as f32) as f64);
                throw_dom(
                    scope,
                    &format!("Failed to construct 'AudioContext': The hardware sample rate provided ({given}) is outside the range [3000, 768000]."),
                    "NotSupportedError",
                );
                return;
            }
        };
        (2u32, 0u32, rate)
    } else if args.length() == 0 {
        // WebIDL's overload resolution by argument count (Chrome 150): no argument is too few for either
        // overload, and two match neither
        throw_type_error(scope, "Failed to construct 'OfflineAudioContext': 1 argument required, but only 0 present.");
        return;
    } else if args.length() == 2 {
        throw_type_error(scope, "Failed to construct 'OfflineAudioContext': Overload resolution failed.");
        return;
    } else if args.length() >= 3 {
        (
            (num(scope, &args, 0) as u32).max(1),
            num(scope, &args, 1) as u32,
            num(scope, &args, 2) as f32,
        )
    } else {
        // The dictionary form, `OfflineAudioContextOptions`, converted as WebIDL converts it (Chrome 150):
        // `length` (REQUIRED), `numberOfChannels`, `sampleRate` (REQUIRED), each read once -- `{}` is
        // refused at `length`, the first member read, in Blink's sentence, before any range check (a different
        // error from the NotSupportedError a zero-length context gets; the corpus records both). korvus read
        // `length` and `sampleRate` both before refusing, in a sentence of its own.
        let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "OfflineAudioContextOptions", "Failed to construct 'OfflineAudioContext': ") else { return };
        let mut g = |k: &str| get_prop(scope, o, k).filter(|v| v.is_number()).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
        let (len, ch, sr) = (g("length"), g("numberOfChannels"), g("sampleRate"));
        (ch.max(1.0) as u32, len as u32, sr as f32)
    };
    if offline && (length == 0 || !(3000.0..=768000.0).contains(&rate)) {
        throw_dom(
            scope,
            "Failed to construct 'OfflineAudioContext': The number of frames provided (0) is less than the minimum bound (1).",
            "NotSupportedError",
        );
        return;
    }
    let op_state = store_op_state(scope);
    let id = {
        let mut st = op_state.borrow_mut();
        let s = st.borrow_mut::<AudioStore>();
        s.next_ctx += 1;
        let id = s.next_ctx;
        s.ctx.insert(
            id,
            AudioCtx {
                ops: Vec::new(),
                offline,
                sample_rate: rate,
                length,
                channels,
                max_channels: if offline { channels } else { 2 },
                created: Instant::now(),
                rendered: false,
                rendering: false,
                suspended: !offline,
                next_node: 0,
                nodes: HashMap::new(),
            },
        );
        id
    };
    let iv = int_val(scope, id as i64);
    priv_set(scope, this, CTX_PRIV, iv);
    // Node 0 is the destination and exists from the start; the renderer knows it by that id, so it
    // is registered here rather than `Create`d.
    edit(scope, id, |c| {
        c.nodes.insert(0, NodeMeta::new("AudioDestinationNode", None, 0.0));
        if let Some(m) = c.nodes.get_mut(&0) {
            m.channel_count = channels;
            m.channel_count_mode = "explicit";
        }
    });
}

pub(crate) fn cb_new_audio_context(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST. `data` is the persona's sample RATE here rather than
    // the `"<name>|<length>"` string `ctor_name` reads, so the interface names itself.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "AudioContext") {
        return;
    }
    let rate = crate::binding::protos::body_data(scope, &args).number_value(scope).filter(|r| *r > 0.0).unwrap_or(48000.0) as f32;
    construct(scope, args, false, rate);
}

pub(crate) fn cb_new_offline_context(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST: a bare `OfflineAudioContext()` answered the argument
    // count, where Chrome answers "Please use the 'new' operator" and reaches the count only on a
    // real `new`.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "OfflineAudioContext") {
        return;
    }
    construct(scope, args, true, 44100.0);
}

// ── node construction: `new XNode(context, options)` ──────────────────────────────────────────

/// The node interfaces WebIDL gives a `constructor(BaseAudioContext, optional options)`, with the
/// `AudioNodeKind` the renderer executes them as and the default constructor scalar.
///
/// Every one of these was reachable ONLY through its factory method before: `new OscillatorNode(ctx)`
/// built a bare shell with no context, no id and no entry in the store, so every accessor on it read
/// `undefined`. The whole recorded corpus for these interfaces constructs them this way, which is
/// how ~110 members sat unverifiable behind a working DSP.
///
/// `None` for the kind is a node with no processor (see the module comment): a real object with the
/// right surface that contributes silence, not a variant the op list carries and nothing executes.
/// (interface, its kind, its constructor scalar's default, its options dictionary)
const NODE_CTORS: &[(&str, Option<AudioNodeKind>, f64, &str)] = &[
    ("OscillatorNode", Some(AudioNodeKind::Oscillator), 0.0, "OscillatorOptions"),
    ("GainNode", Some(AudioNodeKind::Gain), 0.0, "GainOptions"),
    ("DynamicsCompressorNode", Some(AudioNodeKind::DynamicsCompressor), 0.0, "DynamicsCompressorOptions"),
    ("AnalyserNode", Some(AudioNodeKind::Analyser), 0.0, "AnalyserOptions"),
    ("AudioBufferSourceNode", Some(AudioNodeKind::BufferSource), 0.0, "AudioBufferSourceOptions"),
    ("ConstantSourceNode", Some(AudioNodeKind::ConstantSource), 0.0, "ConstantSourceOptions"),
    ("BiquadFilterNode", Some(AudioNodeKind::BiquadFilter), 0.0, "BiquadFilterOptions"),
    ("StereoPannerNode", Some(AudioNodeKind::StereoPanner), 0.0, "StereoPannerOptions"),
    ("WaveShaperNode", Some(AudioNodeKind::WaveShaper), 0.0, "WaveShaperOptions"),
    ("DelayNode", Some(AudioNodeKind::Delay), 1.0, "DelayOptions"),
    ("ChannelMergerNode", Some(AudioNodeKind::ChannelMerger), 6.0, "ChannelMergerOptions"),
    ("ChannelSplitterNode", Some(AudioNodeKind::ChannelSplitter), 6.0, "ChannelSplitterOptions"),
    ("ConvolverNode", None, 0.0, "ConvolverOptions"),
    ("PannerNode", None, 0.0, "PannerOptions"),
    ("IIRFilterNode", None, 0.0, "IIRFilterOptions"),
    ("ScriptProcessorNode", None, 0.0, "AudioNodeOptions"),
    ("MediaElementAudioSourceNode", None, 0.0, "MediaElementAudioSourceOptions"),
    ("MediaStreamAudioSourceNode", None, 0.0, "MediaStreamAudioSourceOptions"),
    ("MediaStreamAudioDestinationNode", None, 0.0, "AudioNodeOptions"),
];

fn node_ctor_spec(name: &str) -> Option<(&'static str, Option<AudioNodeKind>, f64, &'static str)> {
    NODE_CTORS.iter().find(|(n, _, _, _)| *n == name).copied()
}

/// Read a dictionary member as a number, `None` when the member is absent or `undefined`.
fn dict_num(scope: &mut v8::PinScope, opts: Option<v8::Local<v8::Object>>, key: &str) -> Option<f64> {
    let o = opts?;
    let v = get_prop(scope, o, key)?;
    if v.is_undefined() {
        return None;
    }
    v.number_value(scope).filter(|n| n.is_finite())
}

fn dict_val<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    opts: Option<v8::Local<v8::Object>>,
    key: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let o = opts?;
    let v = get_prop(scope, o, key)?;
    if v.is_undefined() { None } else { Some(v) }
}

/// `new XNode(context, options)` for every interface in [`NODE_CTORS`].
///
/// The options dictionary is applied by WRITING THE PUBLIC MEMBERS on the freshly built node —
/// `this.type = "square"`, `this.frequency.value = 880` — rather than by a second copy of the
/// recording logic. That is deliberate: those setters are the ones that push `AudioOp`s, so an
/// option set here reaches the DSP by exactly the route a page's own assignment does, and there is
/// no path by which a constructor option could become a stored-but-unrendered value.
pub(crate) fn cb_new_audio_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let key = crate::binding::protos::ctor_name(scope, &args);
    let Some((iface, kind, default_arg, options_dict)) = node_ctor_spec(&key) else { return };
    // The row's own required-argument count, bound beside the name by `install::live::audio` -- three
    // of the sixteen need TWO; this body used to spell 1 for all of them.
    let required = crate::binding::protos::ctor_required(scope, &args).max(1);
    // THE `new` CHECK COMES FIRST, as it does in Chrome. Chrome 150 answers "Failed to
    // construct 'ChannelMergerNode': Please use the 'new' operator, this DOM object constructor cannot be called as
    // a function." for a plain call and the argument count only for a real `new` (measured headless
    // 2026-09-12); this body had no new-target check at all, so a call answered an argument count.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, iface) {
        return;
    }
    let ctx_obj = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let ctx = ctx_obj.and_then(|o| priv_u32(scope, o, CTX_PRIV));
    let (Some(ctx_obj), Some(ctx)) = (ctx_obj, ctx) else {
        let msg = crate::binding::protos::arity_msg(iface, required, args.length());
        throw_type_error(scope, &msg);
        return;
    };
    // The node's options dictionary, converted as WebIDL converts it (Chrome 150): the
    // `AudioNodeOptions` members first (for a dictionary that inherits them -- `AudioBufferSourceOptions` and
    // `ConstantSourceOptions` do not), then its own, sorted, each read once and checked as read; a REQUIRED member
    // absent (`IIRFilterOptions.feedback` before `feedforward`, `mediaElement`, `mediaStream`) refused in Blink's
    // sentence before any node exists. korvus read the members in its own order (an AudioBufferSourceNode read
    // `channelCount`), checked `feedforward` first, and answered in sentences of its own.
    let Ok(converted) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), options_dict, &format!("Failed to construct '{iface}': ")) else { return };
    let opts = Some(converted);

    // The one constructor scalar a kind needs rides on `Create`, so it has to be known BEFORE the
    // node is minted — `maxDelayTime` fixes `delayTime`'s maxValue, and the merger/splitter width
    // fixes numberOfInputs/numberOfOutputs.
    let arg = match iface {
        "DelayNode" => dict_num(scope, opts, "maxDelayTime").unwrap_or(default_arg).clamp(0.0, 180.0),
        "ChannelMergerNode" => dict_num(scope, opts, "numberOfInputs").unwrap_or(default_arg).clamp(1.0, 32.0),
        "ChannelSplitterNode" => dict_num(scope, opts, "numberOfOutputs").unwrap_or(default_arg).clamp(1.0, 32.0),
        _ => default_arg,
    };
    let this = args.this();
    init_node(scope, this, ctx_obj, ctx, iface, kind, arg);
    if iface == "MediaStreamAudioDestinationNode" {
        // Its whole point is the `MediaStream` it exposes, and a browser mints one per node.
        if let Some(s) = destination_stream(scope, ctx) {
            let sv: v8::Local<v8::Value> = s.into();
            priv_set(scope, this, "korvus_audio_stream", sv);
        }
    }
    apply_node_options(scope, this, iface, opts);
}

/// Write each present option through the node's own public setter.
///
/// Ordering is the spec's: the three `AudioNodeOptions` first (a later `channelCount` write must not
/// be undone by the node-specific pass), then the interface's own members.
fn apply_node_options(
    scope: &mut v8::PinScope,
    node: v8::Local<v8::Object>,
    iface: &str,
    opts: Option<v8::Local<v8::Object>>,
) {
    let Some(_) = opts else { return };
    let set = |scope: &mut v8::PinScope, name: &str, v: v8::Local<v8::Value>| {
        if let Some(k) = v8::String::new(scope, name) {
            node.set(scope, k.into(), v);
        }
    };
    for name in ["channelCount", "channelCountMode", "channelInterpretation"] {
        if let Some(v) = dict_val(scope, opts, name) {
            set(scope, name, v);
        }
    }
    // The plain attributes, per interface. `maxDelayTime`, `numberOfInputs` and `numberOfOutputs`
    // are absent here on purpose: they are constructor scalars, already consumed above, and writing
    // them as attributes would be writing a read-only member.
    let attrs: &[&str] = match iface {
        "OscillatorNode" => &["type"],
        "BiquadFilterNode" => &["type"],
        "AudioBufferSourceNode" => &["buffer", "loop", "loopStart", "loopEnd"],
        "AnalyserNode" => &["fftSize", "minDecibels", "maxDecibels", "smoothingTimeConstant"],
        "WaveShaperNode" => &["curve", "oversample"],
        "ConvolverNode" => &["buffer"],
        "PannerNode" => &[
            "panningModel",
            "distanceModel",
            "refDistance",
            "maxDistance",
            "rolloffFactor",
            "coneInnerAngle",
            "coneOuterAngle",
            "coneOuterGain",
        ],
        _ => &[],
    };
    for name in attrs {
        if let Some(v) = dict_val(scope, opts, name) {
            set(scope, name, v);
        }
    }
    // `ConvolverNode`'s option is the NEGATION of its attribute, which is the one place in the API
    // where the dictionary member and the member it sets do not share a name.
    if iface == "ConvolverNode" {
        if let Some(v) = dict_val(scope, opts, "disableNormalization") {
            let n: v8::Local<v8::Value> = v8::Boolean::new(scope, !v.boolean_value(scope)).into();
            set(scope, "normalize", n);
        }
    }
    if iface == "MediaElementAudioSourceNode" {
        if let Some(v) = dict_val(scope, opts, "mediaElement") {
            priv_set(scope, node, "korvus_audio_mediaElement", v);
        }
    }
    if iface == "MediaStreamAudioSourceNode" {
        if let Some(v) = dict_val(scope, opts, "mediaStream") {
            priv_set(scope, node, "korvus_audio_mediaStream", v);
        }
    }
    // The `AudioParam` options: `o.frequency` is read-only, so the value goes to `.value`, which is
    // the setter that records `SetParam`. A param the interface does not have is simply not there
    // to be written, so an unknown member is dropped exactly as WebIDL drops it.
    for (name, _) in param_defaults(iface) {
        let Some(v) = dict_num(scope, opts, name) else { continue };
        let Some(p) = v8::String::new(scope, name)
            .and_then(|k| node.get(scope, k.into()))
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        else {
            continue;
        };
        // The value without an automation event, as the node's `Create` sets it.
        attrs::set_param_value(scope, p, v as f32, false);
    }
    // `periodicWave` is applied last: `setPeriodicWave` overrides `type`, which the attribute pass
    // above may have written, and that is the order the spec gives.
    if iface == "OscillatorNode" {
        if let Some(w) = dict_val(scope, opts, "periodicWave") {
            let f = v8::String::new(scope, "setPeriodicWave")
                .and_then(|k| node.get(scope, k.into()))
                .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
            if let Some(f) = f {
                f.call(scope, node.into(), &[w]);
            }
        }
    }
}

/// `new AudioBuffer(options)` — `length` and `sampleRate` are REQUIRED members, so the no-argument
/// and empty-dictionary forms are a `TypeError` before anything is allocated.
pub(crate) fn cb_new_audio_buffer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST: a bare `AudioBuffer()` answered the required-member
    // TypeError below, where Chrome answers "Please use the 'new' operator".
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "AudioBuffer") {
        return;
    }
    // then WebIDL's argument count: `new AudioBuffer()` is "1 argument required" (Chrome 150), `new AudioBuffer(undefined)`
    // the dictionary's refusal
    if !crate::install::webidl::require_args(scope, &args, "AudioBuffer", 1) {
        return;
    }
    // `AudioBufferOptions`, converted as WebIDL converts it (Chrome 150): `length` (REQUIRED),
    // `numberOfChannels`, `sampleRate` (REQUIRED), each read once -- `{}` refused at `length` in Blink's sentence
    let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "AudioBufferOptions", "Failed to construct 'AudioBuffer': ") else { return };
    let opts = Some(o);
    let (Some(len), Some(rate)) = (dict_num(scope, opts, "length"), dict_num(scope, opts, "sampleRate")) else {
        throw_dom(scope, "Failed to construct 'AudioBuffer': the number of frames provided is outside the allowed range.", "NotSupportedError");
        return;
    };
    let ch = dict_num(scope, opts, "numberOfChannels").unwrap_or(1.0);
    if len < 1.0 || !(3000.0..=768000.0).contains(&rate) || !(1.0..=32.0).contains(&ch) {
        throw_dom(
            scope,
            "Failed to construct 'AudioBuffer': the number of frames provided is outside the allowed range.",
            "NotSupportedError",
        );
        return;
    }
    let b = new_buffer(scope, ch as usize, len as usize, rate as f32, None);
    rv.set(b.into());
}

/// `new PeriodicWave(context, options)` — the wave the oscillator plays, built from its Fourier
/// coefficients. Same required-context rule as a node; the coefficients ride on the object, which is
/// where `setPeriodicWave` reads them back from.
pub(crate) fn cb_new_periodic_wave(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST, as it does in Chrome. Chrome 150 answers "Failed to
    // construct 'PeriodicWave': Please use the 'new' operator, this DOM object constructor cannot be called as
    // a function." for a plain call and the argument count only for a real `new` (measured headless
    // 2026-09-12); this body had no new-target check at all, so a call answered an argument count.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "PeriodicWave") {
        return;
    }
    let ctx_obj = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    if ctx_obj.and_then(|o| priv_u32(scope, o, CTX_PRIV)).is_none() {
        throw_type_error(
            scope,
            &format!(
                "Failed to construct 'PeriodicWave': 1 argument required, but only {} present.",
                args.length()
            ),
        );
        return;
    }
    let opts = v8::Local::<v8::Object>::try_from(args.get(1)).ok();
    let this = args.this();
    let real = dict_val(scope, opts, "real").unwrap_or_else(|| v8::undefined(scope).into());
    priv_set(scope, this, "korvus_audio_wreal", real);
    let imag = dict_val(scope, opts, "imag").unwrap_or_else(|| v8::undefined(scope).into());
    priv_set(scope, this, "korvus_audio_wimag", imag);
    let norm = dict_val(scope, opts, "disableNormalization").map(|v| !v.boolean_value(scope)).unwrap_or(true);
    let nv: v8::Local<v8::Value> = v8::Boolean::new(scope, norm).into();
    priv_set(scope, this, "korvus_audio_wnorm", nv);
}

/// The two audio events, whose dictionaries carry REQUIRED members — so the no-argument and
/// empty-dictionary forms are `TypeError`s rather than an event with `undefined` buffers.
pub(crate) fn cb_new_audio_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let iface = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // THE `new` CHECK COMES FIRST, BEFORE THE ARGUMENT COUNT, which is the order WebIDL
    // and Chrome use: `AudioProcessingEvent()` and `OfflineAudioCompletionEvent()` both answered
    // "2 arguments required, but only 0 present." where Chrome answers "Please use the 'new'
    // operator". One body, two interfaces, one check -- the name is this callback's `data`.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, &iface) {
        return;
    }
    let needed: &[&str] = if iface == "AudioProcessingEvent" {
        &["playbackTime", "inputBuffer", "outputBuffer"]
    } else {
        &["renderedBuffer"]
    };
    if args.length() < 2 {
        throw_type_error(
            scope,
            &format!(
                "Failed to construct '{iface}': 2 arguments required, but only {} present.",
                args.length()
            ),
        );
        return;
    }
    // The arguments as WebIDL converts them, in order (Chrome 150): `type` to a string, then the
    // init dictionary -- `bubbles`, `cancelable`, `composed`, then the event's own members, each required, an
    // `AudioBuffer` checked as one -- so `{}` names `inputBuffer`, the first of them Chrome reads, in Blink's message.
    // korvus read `playbackTime` first, answered its own message, and kept `type` unconverted.
    let prefix = format!("Failed to construct '{iface}': ");
    let t = args.get(0);
    if t.is_symbol() {
        throw_type_error(scope, &format!("{prefix}Cannot convert a Symbol value to a string"));
        return;
    }
    let Some(t) = t.to_string(scope) else { return };
    let dict_name = if iface == "AudioProcessingEvent" { "AudioProcessingEventInit" } else { "OfflineAudioCompletionEventInit" };
    let Ok(opts) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), dict_name, &prefix) else { return };
    let this = args.this();
    for member in needed {
        let v = get_prop(scope, opts, member).unwrap_or_else(|| v8::undefined(scope).into());
        let slot = if *member == "renderedBuffer" {
            "korvus_audio_rendered".to_string()
        } else {
            format!("korvus_audio_{member}")
        };
        priv_set(scope, this, &slot, v);
    }
    priv_set(scope, this, "korvus_audio_evtype", t.into());
}

/// `new AudioWorkletNode(context, name)` — a real constructor whose second argument is the
/// registered processor's name. Nothing has been registered, because `AudioWorklet.addModule` loads
/// no module, so a well-formed call is an `InvalidStateError`: the honest answer for a processor
/// that does not exist, and not a node that silently is not the one the page asked for.
pub(crate) fn cb_new_worklet_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST, as it does in Chrome. Chrome 150 answers "Failed to
    // construct 'AudioWorkletNode': Please use the 'new' operator, this DOM object constructor cannot be called as
    // a function." for a plain call and the argument count only for a real `new` (measured headless
    // 2026-09-12); this body had no new-target check at all, so a call answered an argument count.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "AudioWorkletNode") {
        return;
    }
    let ok_ctx = v8::Local::<v8::Object>::try_from(args.get(0))
        .ok()
        .and_then(|o| priv_u32(scope, o, CTX_PRIV))
        .is_some();
    if !ok_ctx || args.length() < 2 {
        throw_type_error(
            scope,
            &format!(
                "Failed to construct 'AudioWorkletNode': 2 arguments required, but only {} present.",
                args.length()
            ),
        );
        return;
    }
    let name = args.get(1).to_rust_string_lossy(scope);
    throw_dom(
        scope,
        &format!("Failed to construct 'AudioWorkletNode': AudioWorkletNode cannot be created: The node name '{name}' is not defined in AudioWorkletGlobalScope."),
        "InvalidStateError",
    );
}

// THE FAMILY'S "EXISTS BUT IS NOT CONSTRUCTABLE" BODY IS `binding::protos::cb_illegal_ctor` NOW.
// This file carried a byte-identical second copy, for the four interfaces the retired
// swap mounted it on -- `new AudioParamMap()` is `TypeError: Illegal constructor` in every browser
// and returning a bare object instead is a one-expression tell. The reason it needed
// `ConstructorBehavior::Allow` is unchanged and now lives with the row: Chrome's interface objects
// all HAVE a `[[Construct]]`, and one that reports "is not a constructor" is distinguishable from
// one that reports "Illegal constructor".

// ── the method dispatcher ─────────────────────────────────────────────────────────────────────

/// Every Web Audio method, keyed by the `"Interface.member"` bound as this function's `data` at
/// install time — the same mechanism `webgl.rs` uses, and for the same reason: a hundred
/// near-identical wrappers would carry no more information and one more place to get an arity wrong.
fn cb_audio(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, member) = key.split_once('.').unwrap_or(("AudioNode", key.as_str()));
    let this = args.this();

    // ── BaseAudioContext factories ────────────────────────────────────────────────────────────
    if let Some(ctx) = priv_u32(scope, this, CTX_PRIV) {
        use AudioNodeKind as K;
        let simple: Option<(&'static str, Option<K>, f64)> = match member {
            "createOscillator" => Some(("OscillatorNode", Some(K::Oscillator), 0.0)),
            "createGain" => Some(("GainNode", Some(K::Gain), 0.0)),
            "createDynamicsCompressor" => Some(("DynamicsCompressorNode", Some(K::DynamicsCompressor), 0.0)),
            "createAnalyser" => Some(("AnalyserNode", Some(K::Analyser), 0.0)),
            "createBufferSource" => Some(("AudioBufferSourceNode", Some(K::BufferSource), 0.0)),
            "createConstantSource" => Some(("ConstantSourceNode", Some(K::ConstantSource), 0.0)),
            "createBiquadFilter" => Some(("BiquadFilterNode", Some(K::BiquadFilter), 0.0)),
            "createStereoPanner" => Some(("StereoPannerNode", Some(K::StereoPanner), 0.0)),
            "createWaveShaper" => Some(("WaveShaperNode", Some(K::WaveShaper), 0.0)),
            // No processor renders these, so they get a node object with the right surface and NO
            // `AudioOp::Create` — a graph routed through one is silent, which is visible, instead of
            // being patched around, which is not.
            "createConvolver" => Some(("ConvolverNode", None, 0.0)),
            "createPanner" => Some(("PannerNode", None, 0.0)),
            _ => None,
        };
        if let Some((ifc, kind, arg)) = simple {
            let n = new_node(scope, this, ctx, ifc, kind, arg);
            rv.set(n.into());
            return;
        }
        match member {
            "createDelay" => {
                let max = opt_num(scope, &args, 0).unwrap_or(1.0).clamp(0.0, 180.0);
                let n = new_node(scope, this, ctx, "DelayNode", Some(K::Delay), max);
                rv.set(n.into());
                return;
            }
            "createChannelMerger" => {
                let n_in = opt_num(scope, &args, 0).unwrap_or(6.0).clamp(1.0, 32.0);
                let n = new_node(scope, this, ctx, "ChannelMergerNode", Some(K::ChannelMerger), n_in);
                rv.set(n.into());
                return;
            }
            "createChannelSplitter" => {
                let n_out = opt_num(scope, &args, 0).unwrap_or(6.0).clamp(1.0, 32.0);
                let n = new_node(scope, this, ctx, "ChannelSplitterNode", Some(K::ChannelSplitter), n_out);
                rv.set(n.into());
                return;
            }
            "createIIRFilter" => {
                if !require_args(scope, &args, 2, iface, member) {
                    return;
                }
                // The coefficient arrays are the filter. Empty ones describe no filter at all, and
                // Blink refuses them rather than building a pass-through.
                let ff = f32_seq(scope, args.get(0));
                let fb = f32_seq(scope, args.get(1));
                if ff.is_empty() || fb.is_empty() || ff.len() > 20 || fb.len() > 20 || fb[0] == 0.0 {
                    throw_dom(scope, "Failed to execute 'createIIRFilter' on 'BaseAudioContext': The feedforward and feedback coefficients must be non-empty.", "NotSupportedError");
                    return;
                }
                let n = new_node(scope, this, ctx, "IIRFilterNode", None, 0.0);
                rv.set(n.into());
                return;
            }
            "createScriptProcessor" => {
                let size = opt_num(scope, &args, 0).unwrap_or(0.0) as u32;
                let n = new_node(scope, this, ctx, "ScriptProcessorNode", None, 0.0);
                if let Some((_, nid)) = node_of(scope, n) {
                    edit(scope, ctx, |c| {
                        if let Some(m) = c.nodes.get_mut(&nid) {
                            m.buffer_size = size;
                        }
                    });
                }
                rv.set(n.into());
                return;
            }
            "createMediaStreamDestination" => {
                let n = new_node(scope, this, ctx, "MediaStreamAudioDestinationNode", None, 0.0);
                if let Some(s) = destination_stream(scope, ctx) {
                    let sv: v8::Local<v8::Value> = s.into();
                    priv_set(scope, n, "korvus_audio_stream", sv);
                }
                rv.set(n.into());
                return;
            }
            "createMediaElementSource" | "createMediaStreamSource" => {
                if !require_args(scope, &args, 1, iface, member) {
                    return;
                }
                let ifc = if member == "createMediaElementSource" {
                    "MediaElementAudioSourceNode"
                } else {
                    "MediaStreamAudioSourceNode"
                };
                let n = new_node(scope, this, ctx, ifc, None, 0.0);
                let slot = if member == "createMediaElementSource" {
                    "korvus_audio_mediaElement"
                } else {
                    "korvus_audio_mediaStream"
                };
                let a: v8::Local<v8::Value> = args.get(0);
                priv_set(scope, n, slot, a);
                rv.set(n.into());
                return;
            }
            "createBuffer" => {
                if !require_args(scope, &args, 3, iface, member) {
                    return;
                }
                let ch = (num(scope, &args, 0) as usize).clamp(1, 32);
                let len = num(scope, &args, 1) as usize;
                let rate = num(scope, &args, 2) as f32;
                if len == 0 || !(3000.0..=768000.0).contains(&rate) {
                    throw_dom(scope, "Failed to execute 'createBuffer' on 'BaseAudioContext': invalid buffer geometry.", "NotSupportedError");
                    return;
                }
                let b = new_buffer(scope, ch, len, rate, None);
                rv.set(b.into());
                return;
            }
            "createPeriodicWave" => {
                if !require_args(scope, &args, 2, iface, member) {
                    return;
                }
                let real = f32_seq(scope, args.get(0));
                let imag = f32_seq(scope, args.get(1));
                // Index 0 is DC and index 1 is the fundamental, so a wave needs at least two
                // coefficients to be a wave — Blink's IndexSizeError, and the empty pair is the
                // recorded case.
                if real.len() < 2 || imag.len() < 2 || real.len() != imag.len() {
                    throw_dom(scope, "Failed to execute 'createPeriodicWave' on 'BaseAudioContext': The length of the real array must match the imaginary array and be at least 2.", "IndexSizeError");
                    return;
                }
                let normalize = v8::Local::<v8::Object>::try_from(args.get(2))
                    .ok()
                    .and_then(|o| get_prop(scope, o, "disableNormalization"))
                    .map(|v| !v.boolean_value(scope))
                    .unwrap_or(true);
                let w = new_shell(scope, "PeriodicWave");
                // The coefficients travel on the wave object itself; `setPeriodicWave` reads them
                // back off it when the oscillator is told to use it.
                let rl: v8::Local<v8::Value> = args.get(0);
                priv_set(scope, w, "korvus_audio_wreal", rl);
                let im: v8::Local<v8::Value> = args.get(1);
                priv_set(scope, w, "korvus_audio_wimag", im);
                let nv: v8::Local<v8::Value> = v8::Boolean::new(scope, normalize).into();
                priv_set(scope, w, "korvus_audio_wnorm", nv);
                let _ = (real, imag);
                rv.set(w.into());
                return;
            }
            "startRendering" => {
                start_rendering(scope, this, ctx, &mut rv);
                return;
            }
            // An OFFLINE context's `resume()` / `suspend(when)` (Blink `OfflineAudioContext::resumeContext`
            // / `suspendContext`): rendering here is synchronous, so a context is
            // either not started or finished (closed). The refusals reject with Blink's messages; a
            // suspend scheduled before `startRendering()` waits, as Chrome's does until rendering
            // reaches it.
            "resume" | "suspend" if read(scope, ctx, |c| c.offline).unwrap_or(false) => {
                let (rendered, frames, rate) = read(scope, ctx, |c| (c.rendered, c.length, c.sample_rate)).unwrap_or((false, 0, 1.0));
                let refusal = if member == "resume" {
                    Some(if rendered { "cannot resume a closed offline context".to_string() } else { "cannot resume an offline context that has not started".to_string() })
                } else {
                    let when = args.get(0).number_value(scope).unwrap_or(f64::NAN);
                    let duration = frames as f64 / rate as f64;
                    if rendered {
                        Some("the rendering is already finished".to_string())
                    } else if when < 0.0 {
                        Some(format!("negative suspend time ({}) is not allowed", crate::core::geometry::dom_matrix::js_number(when)))
                    } else if duration <= when {
                        Some(format!(
                            "cannot schedule a suspend at {} seconds because it is greater than or equal to the total render duration of {frames} frames ({} seconds)",
                            crate::core::geometry::dom_matrix::js_number(when),
                            crate::core::geometry::dom_matrix::js_number(duration)
                        ))
                    } else {
                        None
                    }
                };
                match refusal {
                    Some(msg) => {
                        let full = format!("Failed to execute '{member}' on 'OfflineAudioContext': {msg}");
                        if let Some(e) = crate::binding::protos::make_dom_exc(scope, &full, "InvalidStateError") {
                            let p = crate::binding::protos::rejected_promise(scope, e);
                            rv.set(p);
                        }
                    }
                    None => {
                        let p = crate::binding::protos::pending_promise(scope);
                        rv.set(p);
                    }
                }
                return;
            }
            "resume" | "suspend" | "close" => {
                if member == "close" {
                    edit(scope, ctx, |c| c.rendered = true);
                }
                // A suspended live context resumes only on a user gesture (V17): the promise waits.
                if member == "resume" && read(scope, ctx, |c| !c.offline && c.suspended && !c.rendered).unwrap_or(false) {
                    let p = crate::binding::protos::pending_promise(scope);
                    rv.set(p);
                    return;
                }
                let resolver = v8::PromiseResolver::new(scope).unwrap();
                let p = resolver.get_promise(scope);
                let u = v8::undefined(scope);
                resolver.resolve(scope, u.into());
                rv.set(p.into());
                return;
            }
            "decodeAudioData" => {
                decode_audio_data(scope, &args, &mut rv);
                return;
            }
            "getOutputTimestamp" => {
                let o = v8::Object::new(scope);
                let t = read(scope, ctx, |c| c.current_time()).unwrap_or(0.0);
                if let Some(k) = v8::String::new(scope, "contextTime") {
                    let n = v8::Number::new(scope, t);
                    o.create_data_property(scope, k.into(), n.into());
                }
                if let Some(k) = v8::String::new(scope, "performanceTime") {
                    let n = v8::Number::new(scope, t * 1000.0);
                    o.create_data_property(scope, k.into(), n.into());
                }
                rv.set(o.into());
                return;
            }
            "setSinkId" => {
                let resolver = v8::PromiseResolver::new(scope).unwrap();
                let p = resolver.get_promise(scope);
                let u = v8::undefined(scope);
                resolver.resolve(scope, u.into());
                rv.set(p.into());
                return;
            }
            _ => {}
        }
    }

    // ── AudioNode ─────────────────────────────────────────────────────────────────────────────
    if let Some((ctx, id)) = node_of(scope, this) {
        match member {
            "connect" => {
                if !require_args(scope, &args, 1, "AudioNode", "connect") {
                    return;
                }
                let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
                    throw_type_error(scope, "Failed to execute 'connect' on 'AudioNode': parameter 1 is not of type 'AudioNode'.");
                    return;
                };
                let out_idx = opt_num(scope, &args, 1).unwrap_or(0.0) as u32;
                let in_idx = opt_num(scope, &args, 2).unwrap_or(0.0) as u32;
                if let Some((_, to)) = node_of(scope, target) {
                    push(scope, ctx, AudioOp::Connect { from: id, from_output: out_idx, to, to_input: in_idx });
                    rv.set(target.into());
                    return;
                }
                // Connecting to an AudioParam: the source's output drives the param at a-rate.
                if let Some((pnode, pname)) = param_owner(scope, target) {
                    if let Some(p) = param_id(&pname) {
                        push(scope, ctx, AudioOp::ConnectParam { from: id, from_output: out_idx, to: pnode, param: p });
                    }
                    rv.set_undefined();
                    return;
                }
                throw_type_error(scope, "Failed to execute 'connect' on 'AudioNode': parameter 1 is not of type 'AudioNode'.");
                return;
            }
            "disconnect" => {
                // `disconnect(n)` names an OUTPUT index, and a node with no outputs (the
                // destination) has none to name. Every other overload takes a node or a param.
                if args.length() >= 1 && args.get(0).is_number() {
                    let out = num(scope, &args, 0) as u32;
                    let outputs = read(scope, ctx, |c| c.nodes.get(&id).map(|m| m.outputs)).flatten().unwrap_or(0);
                    if out >= outputs {
                        throw_dom(scope, "Failed to execute 'disconnect' on 'AudioNode': The output index provided is outside the range.", "IndexSizeError");
                        return;
                    }
                }
                let to = v8::Local::<v8::Object>::try_from(args.get(0))
                    .ok()
                    .and_then(|o| node_of(scope, o))
                    .map(|(_, n)| n);
                push(scope, ctx, AudioOp::Disconnect { from: id, to });
                rv.set_undefined();
                return;
            }
            "start" => {
                let when = opt_num(scope, &args, 0).unwrap_or(0.0).max(0.0);
                let offset = opt_num(scope, &args, 1).unwrap_or(0.0).max(0.0);
                let duration = opt_num(scope, &args, 2).filter(|d| *d >= 0.0);
                edit(scope, ctx, |c| {
                    if let Some(m) = c.nodes.get_mut(&id) {
                        m.started = true;
                    }
                });
                push(scope, ctx, AudioOp::Start { id, when, offset, duration });
                rv.set_undefined();
                return;
            }
            "stop" => {
                // A source that has never been started cannot be stopped — the spec's
                // InvalidStateError, and the ORDER is observable, which a stored flag is the only
                // way to answer.
                let started = read(scope, ctx, |c| c.nodes.get(&id).map(|m| m.started)).flatten().unwrap_or(false);
                if !started {
                    throw_dom(scope, "Failed to execute 'stop' on 'AudioScheduledSourceNode': cannot call stop without calling start first.", "InvalidStateError");
                    return;
                }
                let when = opt_num(scope, &args, 0).unwrap_or(0.0).max(0.0);
                push(scope, ctx, AudioOp::Stop { id, when });
                rv.set_undefined();
                return;
            }
            "setPeriodicWave" => {
                if !require_args(scope, &args, 1, "OscillatorNode", "setPeriodicWave") {
                    return;
                }
                let Ok(w) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
                let real = priv_get(scope, w, "korvus_audio_wreal").map(|v| f32_seq(scope, v)).unwrap_or_default();
                let imag = priv_get(scope, w, "korvus_audio_wimag").map(|v| f32_seq(scope, v)).unwrap_or_default();
                let normalize = priv_get(scope, w, "korvus_audio_wnorm").map(|v| v.boolean_value(scope)).unwrap_or(true);
                edit(scope, ctx, |c| {
                    if let Some(m) = c.nodes.get_mut(&id) {
                        m.osc_type = "custom".into();
                    }
                });
                push(scope, ctx, AudioOp::SetPeriodicWave { id, real, imag, normalize });
                rv.set_undefined();
                return;
            }
            "getFloatFrequencyData" | "getByteFrequencyData" | "getFloatTimeDomainData"
            | "getByteTimeDomainData" => {
                if !require_args(scope, &args, 1, "AnalyserNode", member) {
                    return;
                }
                analyser_read(scope, &args, ctx, id, member);
                rv.set_undefined();
                return;
            }
            "getChannelData" | "copyFromChannel" | "copyToChannel" => {}
            "getFrequencyResponse" => {
                if !require_args(scope, &args, 3, iface, member) {
                    return;
                }
                // Real, and answered where the filter lives: the biquad's own transfer function.
                biquad_response(scope, &args, ctx, id);
                rv.set_undefined();
                return;
            }
            "setPosition" | "setOrientation" => {
                // AudioListener/PannerNode geometry. Stored so it reads back; nothing consumes it,
                // because the node that would (HRTF panning) does not render.
                let names: &[&str] = if member == "setPosition" {
                    &["positionX", "positionY", "positionZ"]
                } else if iface == "AudioListener" {
                    // The listener's orientation is SIX coordinates (forward + up); a panner's is
                    // three. Same member name, different arity, and the corpus checks both.
                    &["forwardX", "forwardY", "forwardZ", "upX", "upY", "upZ"]
                } else {
                    &["orientationX", "orientationY", "orientationZ"]
                };
                if !require_args(scope, &args, names.len(), iface, member) {
                    return;
                }
                for (i, n) in names.iter().enumerate() {
                    let v = num(scope, &args, i as i32) as f32;
                    edit(scope, ctx, |c| {
                        if let Some(m) = c.nodes.get_mut(&id) {
                            m.params.insert(leak(n), v);
                        }
                    });
                }
                rv.set_undefined();
                return;
            }
            _ => {}
        }
    }

    // ── AudioBuffer ───────────────────────────────────────────────────────────────────────────
    if priv_u32(scope, this, BUF_CH_PRIV).is_some() {
        match member {
            "getChannelData" => {
                if !require_args(scope, &args, 1, "AudioBuffer", "getChannelData") {
                    return;
                }
                let c = num(scope, &args, 0) as usize;
                let n = priv_u32(scope, this, BUF_CH_PRIV).unwrap_or(0) as usize;
                if c >= n {
                    throw_dom(scope, "Failed to execute 'getChannelData' on 'AudioBuffer': channel index exceeds number of channels.", "IndexSizeError");
                    return;
                }
                match priv_get(scope, this, &format!("korvus_audio_ch{c}")) {
                    Some(v) => rv.set(v),
                    None => rv.set_undefined(),
                }
                return;
            }
            "copyFromChannel" | "copyToChannel" => {
                if !require_args(scope, &args, 2, "AudioBuffer", member) {
                    return;
                }
                copy_channel(scope, &args, this, member == "copyToChannel");
                rv.set_undefined();
                return;
            }
            _ => {}
        }
    }

    // ── AudioParam ────────────────────────────────────────────────────────────────────────────
    if let Some((node, name)) = param_owner(scope, this) {
        let ctx = priv_get(scope, this, PARAM_NODE_PRIV)
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
            .and_then(|o| priv_u32(scope, o, NODE_CTX_PRIV));
        let Some(ctx) = ctx else { return };
        let Some(p) = param_id(&name) else {
            rv.set(this.into());
            return;
        };
        let ev = match member {
            "setValueAtTime" => Some(AutomationEvent::SetValueAtTime {
                value: num(scope, &args, 0) as f32,
                time: num(scope, &args, 1),
            }),
            "linearRampToValueAtTime" => Some(AutomationEvent::LinearRampToValueAtTime {
                value: num(scope, &args, 0) as f32,
                time: num(scope, &args, 1),
            }),
            "exponentialRampToValueAtTime" => Some(AutomationEvent::ExponentialRampToValueAtTime {
                value: num(scope, &args, 0) as f32,
                time: num(scope, &args, 1),
            }),
            "setTargetAtTime" => Some(AutomationEvent::SetTargetAtTime {
                target: num(scope, &args, 0) as f32,
                time: num(scope, &args, 1),
                time_constant: num(scope, &args, 2),
            }),
            "setValueCurveAtTime" => {
                if !require_args(scope, &args, 3, "AudioParam", member) {
                    return;
                }
                let curve = f32_seq(scope, args.get(0));
                // A curve is interpolated BETWEEN its points, so fewer than two of them describes
                // nothing — the spec's InvalidStateError, and the empty curve is what the corpus
                // records.
                if curve.len() < 2 {
                    throw_dom(scope, "Failed to execute 'setValueCurveAtTime' on 'AudioParam': The curve has fewer than 2 elements.", "InvalidStateError");
                    return;
                }
                Some(AutomationEvent::SetValueCurveAtTime {
                    curve,
                    time: num(scope, &args, 1),
                    duration: num(scope, &args, 2),
                })
            }
            "cancelScheduledValues" => Some(AutomationEvent::CancelScheduledValues { time: num(scope, &args, 0) }),
            "cancelAndHoldAtTime" => Some(AutomationEvent::CancelAndHoldAtTime { time: num(scope, &args, 0) }),
            _ => None,
        };
        if let Some(e) = ev {
            if !require_args(scope, &args, 1, "AudioParam", member) {
                return;
            }
            push(scope, ctx, AudioOp::Automate { id: node, param: p, event: e });
            // Every AudioParam method returns the param, for chaining.
            rv.set(this.into());
            return;
        }
    }

    rv.set_undefined();
}

/// `&'static str` for a name that came from a fixed table. The tables are all `&'static`, so this
/// only ever runs on a string that already is one; it exists because the loop that writes them back
/// has a `&&str`.
fn leak(s: &str) -> &'static str {
    for known in [
        "frequency", "detune", "gain", "threshold", "knee", "ratio", "attack", "release",
        "playbackRate", "offset", "delayTime", "Q", "pan", "positionX", "positionY", "positionZ",
        "forwardX", "forwardY", "forwardZ", "upX", "upY", "upZ", "orientationX", "orientationY",
        "orientationZ",
    ] {
        if known == s {
            return known;
        }
    }
    "gain"
}

fn param_owner(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Option<(u32, String)> {
    let node = priv_get(scope, obj, PARAM_NODE_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let id = priv_u32(scope, node, NODE_ID_PRIV)?;
    let name = priv_get(scope, obj, PARAM_NAME_PRIV)?.to_rust_string_lossy(scope);
    Some((id, name))
}

// ── the readbacks ─────────────────────────────────────────────────────────────────────────────

// ── work off the main thread ──────────────────────────────────────────────────────────────────
//
// Blink does two things of Web Audio's on another thread and answers the page from a task it posts back
// when they are done: `decodeAudioData` decodes on a worker-pool thread (`DecodeOnBackgroundThread` ->
// `NotifyDecodingComplete`), and an `OfflineAudioContext` renders on its offline render thread
// (`OfflineAudioDestinationHandler::DoOfflineRendering` -> `FinishOfflineRendering` -> `NotifyComplete`). The
// main thread is free meanwhile: Chrome 150 runs ~950 MessageChannel round trips while it renders 44,100 frames
// of the classic fingerprint graph. Both ran ON the page's thread here, inside the call (a 441,000-frame render
// blocked `startRendering()` for 56-62 ms, and no task could run). Now the work is a thread of its own, and its
// answer is a task queued when the work is seen done -- after whatever the page queued in the meantime, as a
// task Chromium posts on completion is. Nothing is delayed: the answer comes as soon as the work is.

/// What a job hands back.
enum AudioJobResult {
    /// The decoded channels and their rate, or `None` for data that does not decode.
    Decoded(Option<(Vec<Vec<f32>>, f32)>),
    /// An offline context's interleaved PCM.
    Rendered(Vec<f32>),
}

/// The jobs in flight, by id. Each slot is filled by its thread and taken by its answer task.
#[derive(Default)]
struct AudioJobs {
    next: u64,
    slots: HashMap<u64, std::sync::Arc<std::sync::Mutex<Option<AudioJobResult>>>>,
}

/// How long a pending job waits before its answer task looks at it again -- the cadence
/// `binding::protos::Settle::TaskGated` polls work on another thread at.
const JOB_POLL_MS: f64 = 0.25;

/// Run `work` on a thread of its own; when it is done, queue `answer` as a task. `answer`'s data is an array
/// whose element 0 this sets to the job id ([`take_job`] reads the result by it).
fn start_job(
    scope: &mut v8::PinScope,
    answer_data: v8::Local<v8::Array>,
    answer: impl v8::MapFnTo<v8::FunctionCallback>,
    work: impl FnOnce() -> AudioJobResult + Send + 'static,
) {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let id = {
        let op_state = store_op_state(scope);
        let mut st = op_state.borrow_mut();
        if !st.has::<AudioJobs>() {
            st.put(AudioJobs::default());
        }
        let jobs = st.borrow_mut::<AudioJobs>();
        jobs.next += 1;
        jobs.slots.insert(jobs.next, slot.clone());
        jobs.next
    };
    let filled = slot.clone();
    std::thread::spawn(move || {
        let r = work();
        if let Ok(mut s) = filled.lock() {
            *s = Some(r);
        }
    });
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    answer_data.set_index(scope, 0, idv);
    let answer_data = v8::Local::new(scope, answer_data);
    let answer_fn = build_fn_data(scope, answer, "", 0, answer_data.into());
    let pack = v8::Array::new(scope, 2);
    pack.set_index(scope, 0, idv);
    pack.set_index(scope, 1, answer_fn.into());
    let poll = build_fn_data(scope, cb_job_poll, "", 0, pack.into());
    crate::core::intersection_observer::rendering::queue_task(scope, poll, 0.0);
}

/// A job's watch task: done -> its answer is queued as a task of its own; not yet -> look again shortly.
fn cb_job_poll(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(pack) = v8::Local::<v8::Array>::try_from(body_data(scope, &args)) else { return };
    let Some(id) = pack.get_index(scope, 0).and_then(|v| v.number_value(scope)).map(|n| n as u64) else { return };
    let Some(answer) = pack.get_index(scope, 1).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let done = {
        let op_state = store_op_state(scope);
        let st = op_state.borrow();
        st.try_borrow::<AudioJobs>()
            .and_then(|j| j.slots.get(&id).cloned())
            .is_none_or(|slot| slot.lock().map(|s| s.is_some()).unwrap_or(true))
    };
    if done {
        crate::core::intersection_observer::rendering::queue_task(scope, answer, 0.0);
    } else {
        let again = build_fn_data(scope, cb_job_poll, "", 0, pack.into());
        crate::core::intersection_observer::rendering::queue_task(scope, again, JOB_POLL_MS);
    }
}

/// The finished job's result, removed from the table.
fn take_job(scope: &mut v8::PinScope, id: u64) -> Option<AudioJobResult> {
    let slot = {
        let op_state = store_op_state(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<AudioJobs>().and_then(|j| j.slots.remove(&id))
    }?;
    let mut guard = slot.lock().ok()?;
    guard.take()
}

/// `callback->InvokeAndReportException(this, arg)`: a throwing callback is reported, not propagated.
fn invoke_and_report(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, recv: v8::Local<v8::Value>, arg: v8::Local<v8::Value>) {
    let exc = {
        v8::tc_scope!(let tc, scope);
        let _ = f.call(tc, recv, &[arg]);
        let e = if tc.has_caught() {
            tc.exception().map(|e| {
                let m = tc.message();
                (v8::Global::new(tc, e), crate::core::events::report::caught_site(tc, e, m))
            })
        } else {
            None
        };
        tc.reset();
        e
    };
    if let Some((e, site)) = exc {
        let e = v8::Local::new(scope, &e);
        crate::core::events::report::report_exception_at(scope, e, site);
    }
}

/// A `statechange` at an audio context, as `BaseAudioContext::SetContextState` posts one (a task on the
/// media-element-event queue; `NotifyStateChange` dispatches a plain `Event`). The `onstatechange` attribute is a
/// webaudio private, fired after the registered listeners as `oncomplete` is.
fn queue_state_change(scope: &mut v8::PinScope, ctx_obj: v8::Local<v8::Object>) {
    let ctx_obj = v8::Local::new(scope, ctx_obj);
    let task = build_fn_data(scope, cb_state_change, "", 0, ctx_obj.into());
    crate::core::intersection_observer::rendering::queue_task(scope, task, 0.0);
}

fn cb_state_change(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(this) = v8::Local::<v8::Object>::try_from(body_data(scope, &args)) else { return };
    let ty = str_val(scope, "statechange");
    let Some(ev) = crate::binding::protos::new_global(scope, "Event", &[ty]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let tv: v8::Local<v8::Value> = this.into();
    crate::core::events::event_target::et_dispatch(scope, tv, ev);
    if let Some(f) = priv_get(scope, this, "korvus_audio_onstatechange").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        invoke_and_report(scope, f, tv, ev.into());
    }
}

/// `startRendering()` — run the recorded graph and resolve with the samples it produced.
///
/// `render_audio` gets the op list, the frame count and the rate, and what it returns IS the buffer. There is no
/// path here that consults a recording, and none that fabricates a shape.
///
/// The ORDER is Blink's (`OfflineAudioContext::startOfflineRendering`, then `FireCompletionEvent`): the context
/// goes `running` before the call returns, the render runs on a thread of its own (see "work off the main
/// thread"), and the task that answers it closes the context (`statechange` posted), dispatches `complete` to the
/// listeners and then the `oncomplete` attribute, and resolves the promise last. Chrome 150, measured (no
/// DevTools): state `suspended` > `running` after the call > `closed`, `statechange` fired for `running` and
/// `closed`, `complete`'s listener and `oncomplete` before the promise's reaction.
fn start_rendering(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, ctx: u32, rv: &mut v8::ReturnValue) {
    let Some((ops, frames, channels, rate, offline)) =
        read(scope, ctx, |c| (c.ops.clone(), c.length, c.channels, c.sample_rate, c.offline))
    else {
        return;
    };
    if !offline {
        throw_type_error(scope, "startRendering is not a function");
        return;
    }
    // `startOfflineRendering`'s refusals, rejected (a promise operation): a closed context, then one that is not
    // suspended. korvus rendered the graph again.
    let (rendered, rendering) = read(scope, ctx, |c| (c.rendered, c.rendering)).unwrap_or((false, false));
    let refusal = if rendered && !rendering {
        Some("cannot call startRendering on an OfflineAudioContext in a stopped state.")
    } else if rendering {
        Some("cannot startRendering when an OfflineAudioContext is running")
    } else {
        None
    };
    if let Some(msg) = refusal {
        let full = format!("Failed to execute 'startRendering' on 'OfflineAudioContext': {msg}");
        if let Some(e) = crate::binding::protos::make_dom_exc(scope, &full, "InvalidStateError") {
            rv.set(crate::binding::protos::rejected_promise(scope, e));
        }
        return;
    }
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    // `rendered` keeps meaning "this context's render has begun" for the readbacks and the resume/suspend
    // refusals; `rendering` is the `running` state the page reads until the answer task closes the context.
    edit(scope, ctx, |c| {
        c.rendered = true;
        c.rendering = true;
    });
    queue_state_change(scope, this);
    let render = backend(scope);
    // the rendered buffer as the page reads it back: through the plugins that fill the readback slot (ABI 2)
    let through_plugins = plugin_host::has_readback(plugin_host::READBACK_AUDIO);
    let data = v8::Array::new(scope, 3);
    let tv: v8::Local<v8::Value> = this.into();
    let rv_: v8::Local<v8::Value> = resolver.into();
    data.set_index(scope, 1, tv);
    data.set_index(scope, 2, rv_);
    start_job(scope, data, cb_offline_complete, move || {
        AudioJobResult::Rendered(match render {
            Some(b) => {
                let mut pcm = b.render_audio(&ops, frames, channels, rate, &RenderParams::default());
                if through_plugins {
                    plugin_host::readback_samples(&mut pcm);
                }
                pcm
            }
            // No render backend: silence. Deliberately NOT a recorded buffer — a fallback that fires
            // invisibly is how the previous state came to be believed working.
            None => vec![0.0; frames as usize * channels as usize],
        })
    });
    rv.set(promise.into());
}

/// The offline render's answer task (`OfflineAudioDestinationHandler::NotifyComplete` ->
/// `OfflineAudioContext::FireCompletionEvent`). `data` carries `[job id, context, resolver]`.
///
/// The `complete` event goes through the normal dispatch path ([`et_dispatch`]) so a listener added with
/// `addEventListener('complete')` -- creepjs's `getRenderedBuffer` adds it on the line AFTER the call -- fires,
/// carrying the rendered buffer as `renderedBuffer`; the `oncomplete` attribute, a webaudio private rather than a
/// DOM on-handler, is fired after it.
fn cb_offline_complete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(body_data(scope, &args)) else { return };
    let Some(id) = data.get_index(scope, 0).and_then(|v| v.number_value(scope)).map(|n| n as u64) else { return };
    let Some(this) = data.get_index(scope, 1).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return;
    };
    let Some(ctx) = priv_u32(scope, this, CTX_PRIV) else { return };
    let Some((frames, channels, rate)) = read(scope, ctx, |c| (c.length, c.channels, c.sample_rate)) else { return };
    let pcm = match take_job(scope, id) {
        Some(AudioJobResult::Rendered(p)) => p,
        _ => vec![0.0; frames as usize * channels as usize],
    };
    let buf = new_buffer(scope, channels as usize, frames as usize, rate, Some(&pcm));
    let bv: v8::Local<v8::Value> = buf.into();
    // "We set the state to closed here so that the oncomplete event handler sees that the context has been closed."
    edit(scope, ctx, |c| c.rendering = false);
    queue_state_change(scope, this);
    let ev = new_shell(scope, "OfflineAudioCompletionEvent");
    // The event's own `type` slot drives listener matching in [`et_dispatch`]; a bare shell has none,
    // so `addEventListener('complete')` would never match (the `oncomplete` attribute, called directly
    // below, does not need it). `renderedBuffer` reads the private the getter exposes.
    let tyv = str_val(scope, "complete");
    crate::core::events::event::evt_slot_write(scope, ev, "type", tyv);
    priv_set(scope, ev, "korvus_audio_rendered", bv);
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let tv: v8::Local<v8::Value> = this.into();
    crate::core::events::event_target::et_dispatch(scope, tv, ev);
    if let Some(f) = priv_get(scope, this, "korvus_audio_oncomplete")
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    {
        invoke_and_report(scope, f, tv, ev.into());
    }
    // "Call the offline rendering completion event listener and resolve the promise too": the event first.
    if let Some(r) = data.get_index(scope, 2).filter(|v| v.is_promise()) {
        // SAFETY: a PromiseResolver is its promise's object; it was stored as one by `start_rendering`
        let r = unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(r) };
        r.resolve(scope, bv);
    }
}

/// The four `AnalyserNode` readbacks, all out of one `query_audio`.
fn analyser_read(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, ctx: u32, id: u32, member: &str) {
    let Ok(target) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let tlen = get_prop(scope, target, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0) as usize;
    if tlen == 0 {
        return;
    }
    let (fft, min_db, max_db) = read(scope, ctx, |c| {
        c.nodes.get(&id).map(|m| (m.fft_size, m.min_db, m.max_db)).unwrap_or((2048, -100.0, -30.0))
    })
    .unwrap_or((2048, -100.0, -30.0));
    let bins = (fft / 2) as usize;
    let freq = member.contains("Frequency");
    let q = if freq {
        AudioQuery::FrequencyDb { node: id, bins }
    } else {
        AudioQuery::TimeDomain { node: id, len: fft as usize }
    };
    let AudioQueryResult::Samples(data) = ask(scope, ctx, q) else { return };
    let n = tlen.min(if freq { bins } else { fft as usize });
    match member {
        "getFloatFrequencyData" | "getFloatTimeDomainData" => {
            for i in 0..n {
                let v = v8::Number::new(scope, data[i] as f64);
                target.set_index(scope, i as u32, v.into());
            }
        }
        "getByteFrequencyData" => {
            // dB mapped across [minDecibels, maxDecibels] into 0..255 — and a -Infinity bin lands
            // at 0, which is what a browser reports for a bin with no energy.
            let span = (max_db - min_db).max(1e-9);
            for i in 0..n {
                let db = data[i] as f64;
                let x = if db.is_finite() { ((db - min_db) / span * 255.0).clamp(0.0, 255.0) } else { 0.0 };
                let v = v8::Number::new(scope, x.floor());
                target.set_index(scope, i as u32, v.into());
            }
        }
        _ => {
            for i in 0..n {
                let x = (128.0 + data[i] * 128.0).clamp(0.0, 255.0);
                let v = v8::Number::new(scope, x.floor() as f64);
                target.set_index(scope, i as u32, v.into());
            }
        }
    }
}

/// `biquad.getFrequencyResponse(freqs, mag, phase)` — evaluated from the same transfer function the
/// renderer filters with, so the curve a page plots is the curve its audio went through.
fn biquad_response(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, ctx: u32, id: u32) {
    let freqs = f32_seq(scope, args.get(0));
    let (Ok(mag), Ok(phase)) = (
        v8::Local::<v8::Object>::try_from(args.get(1)),
        v8::Local::<v8::Object>::try_from(args.get(2)),
    ) else {
        return;
    };
    let Some((kind, f0, q, gain_db, rate)) = read(scope, ctx, |c| {
        let m = c.nodes.get(&id)?;
        Some((
            m.biquad_type.clone(),
            *m.params.get("frequency").unwrap_or(&350.0) as f64
                * 2f64.powf(*m.params.get("detune").unwrap_or(&0.0) as f64 / 1200.0),
            *m.params.get("Q").unwrap_or(&1.0) as f64,
            *m.params.get("gain").unwrap_or(&0.0) as f64,
            c.sample_rate as f64,
        ))
    })
    .flatten() else {
        return;
    };
    let Some(k) = biquad_type(&kind) else { return };
    let coef = spec_biquad_coefficients(k, f0, q, gain_db, rate);
    for (i, f) in freqs.iter().enumerate() {
        let w = 2.0 * std::f64::consts::PI * (*f as f64) / rate;
        let (c1, s1) = (w.cos(), -w.sin());
        let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let nr = coef.0 + coef.1 * c1 + coef.2 * c2;
        let ni = coef.1 * s1 + coef.2 * s2;
        let dr = 1.0 + coef.3 * c1 + coef.4 * c2;
        let di = coef.3 * s1 + coef.4 * s2;
        let den = dr * dr + di * di;
        let (hr, hi) = if den > 0.0 { ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den) } else { (0.0, 0.0) };
        let m = (hr * hr + hi * hi).sqrt();
        let p = hi.atan2(hr);
        let mv = v8::Number::new(scope, m);
        mag.set_index(scope, i as u32, mv.into());
        let pv = v8::Number::new(scope, p);
        phase.set_index(scope, i as u32, pv.into());
    }
}

/// The biquad's normalized `(b0, b1, b2, a1, a2)`. Duplicating three lines of the renderer's
/// coefficient formula here would be the same mistake the 2D path made, so this is the only place
/// `webapi` computes anything about the filter — and it computes only what `getFrequencyResponse`
/// needs, from the same standard cookbook forms.
fn spec_biquad_coefficients(kind: BiquadKind, freq: f64, q: f64, gain_db: f64, rate: f64) -> (f64, f64, f64, f64, f64) {
    let nyq = rate / 2.0;
    let f = (freq / nyq).clamp(1e-6, 0.999_999);
    let w0 = std::f64::consts::PI * f;
    let (sn, cs) = (w0.sin(), w0.cos());
    let a = 10f64.powf(gain_db / 40.0);
    let (b0, b1, b2, a0, a1, a2) = match kind {
        BiquadKind::Lowpass => {
            let al = sn / (2.0 * 10f64.powf(q / 20.0)).max(1e-9);
            ((1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0, 1.0 + al, -2.0 * cs, 1.0 - al)
        }
        BiquadKind::Highpass => {
            let al = sn / (2.0 * 10f64.powf(q / 20.0)).max(1e-9);
            ((1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0, 1.0 + al, -2.0 * cs, 1.0 - al)
        }
        BiquadKind::Bandpass => {
            let al = sn / (2.0 * q.max(1e-9));
            (al, 0.0, -al, 1.0 + al, -2.0 * cs, 1.0 - al)
        }
        BiquadKind::Notch => {
            let al = sn / (2.0 * q.max(1e-9));
            (1.0, -2.0 * cs, 1.0, 1.0 + al, -2.0 * cs, 1.0 - al)
        }
        BiquadKind::Allpass => {
            let al = sn / (2.0 * q.max(1e-9));
            (1.0 - al, -2.0 * cs, 1.0 + al, 1.0 + al, -2.0 * cs, 1.0 - al)
        }
        BiquadKind::Peaking => {
            let al = sn / (2.0 * q.max(1e-9));
            (1.0 + al * a, -2.0 * cs, 1.0 - al * a, 1.0 + al / a, -2.0 * cs, 1.0 - al / a)
        }
        BiquadKind::Lowshelf => {
            let s = 2.0 * a.sqrt() * (sn / 2.0) * ((1.0 / q.max(1e-9) - 1.0) + 2.0).max(0.0).sqrt();
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
            let s = 2.0 * a.sqrt() * (sn / 2.0) * ((1.0 / q.max(1e-9) - 1.0) + 2.0).max(0.0).sqrt();
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
    (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0)
}

/// `copyFromChannel(dest, ch, offset)` / `copyToChannel(src, ch, offset)`.
fn copy_channel(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    buf: v8::Local<v8::Object>,
    to: bool,
) {
    let Ok(other) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let c = num(scope, args, 1) as usize;
    let offset = num(scope, args, 2).max(0.0) as usize;
    let n = priv_u32(scope, buf, BUF_CH_PRIV).unwrap_or(0) as usize;
    if c >= n {
        throw_dom(scope, "Failed to execute on 'AudioBuffer': channel index exceeds number of channels.", "IndexSizeError");
        return;
    }
    let Some(chan) = priv_get(scope, buf, &format!("korvus_audio_ch{c}"))
        .and_then(|v| v8::Local::<v8::Float32Array>::try_from(v).ok())
    else {
        return;
    };
    let blen = chan.length() as usize;
    let olen = get_prop(scope, other, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0) as usize;
    for i in 0..olen {
        if to {
            let src = blen.min(offset + i);
            if src >= blen {
                break;
            }
            if let Some(v) = other.get_index(scope, i as u32) {
                chan.set_index(scope, (offset + i) as u32, v);
            }
        } else {
            if offset + i >= blen {
                break;
            }
            if let Some(v) = chan.get_index(scope, (offset + i) as u32) {
                other.set_index(scope, i as u32, v);
            }
        }
    }
}

/// `decodeAudioData(bytes)` — a real decoder for the one container this engine can honestly decode.
///
/// Linear-PCM RIFF/WAVE (8/16/24/32-bit integer and 32-bit float) decodes to an `AudioBuffer`.
/// Anything else REJECTS with `EncodingError`, which is what the spec requires of data the browser
/// cannot decode, and is the honest answer here: shipping an MP3 or AAC decoder is a codec, not a
/// Web Audio seam, and returning a fabricated buffer for one would be the tape again.
fn decode_audio_data(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue) {
    // `(ArrayBuffer audioData, optional DecodeSuccessCallback? successCallback, optional DecodeErrorCallback?
    // errorCallback)`, converted first -- a promise operation, so a bad argument rejects (Chrome 150,
    // 2026-10-11: a 5 for a callback is "parameter 2 is not of type 'Function'."; korvus went on to decode)
    if let Err(e) = crate::install::ctor_dicts::convert_method_args_caught(scope, "BaseAudioContext::decodeAudioData", args, "Failed to execute 'decodeAudioData' on 'BaseAudioContext': ") {
        rv.set(crate::binding::protos::rejected_promise(scope, e));
        return;
    }
    // Blink's `BaseAudioContext::decodeAudioData` (Chrome 150, measured with no DevTools attached):
    //   * an ArrayBuffer that is detached -- or cannot be -- is a `DataCloneError` ("Cannot decode detached
    //     ArrayBuffer" / "Cannot transfer the ArrayBuffer"): the error callback runs NOW, inside the call, and the
    //     promise is returned rejected;
    //   * otherwise the buffer is TRANSFERRED (the page's `byteLength` reads 0 once the call returns) and decoded
    //     on another thread; the answer comes in a task posted back (`NotifyDecodingComplete`) -- after a 0 ms
    //     timer the page queued right after the call -- which settles the promise first and then calls the
    //     callback: undecodable data is a `DOMException` `EncodingError`, "Unable to decode audio data".
    // korvus decoded in the call, kept the bytes in the page's buffer, rejected with a plain `Error` and called the
    // error callback synchronously.
    let this = args.this();
    let ctx_v: v8::Local<v8::Value> = this.into();
    let on_success = v8::Local::<v8::Function>::try_from(args.get(1)).ok();
    let on_error = v8::Local::<v8::Function>::try_from(args.get(2)).ok();
    let Ok(buffer) = v8::Local::<v8::ArrayBuffer>::try_from(args.get(0)) else { return };
    let refusal = if buffer.was_detached() || !buffer.is_detachable() {
        Some("Cannot decode detached ArrayBuffer")
    } else {
        None
    };
    let bytes = if refusal.is_none() { raw_bytes(scope, buffer.into()) } else { None };
    let refusal = refusal.or_else(|| match buffer.detach(None) {
        Some(true) => None,
        _ => Some("Cannot transfer the ArrayBuffer"),
    });
    if let Some(msg) = refusal {
        let Some(e) = crate::binding::protos::make_dom_exc(scope, msg, "DataCloneError") else { return };
        if let Some(f) = on_error {
            invoke_and_report(scope, f, ctx_v, e);
        }
        rv.set(crate::binding::protos::rejected_promise(scope, e));
        return;
    }
    let bytes = bytes.unwrap_or_default();
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    let data = v8::Array::new(scope, 4);
    let rv_: v8::Local<v8::Value> = resolver.into();
    data.set_index(scope, 1, ctx_v);
    data.set_index(scope, 2, rv_);
    let callbacks = v8::Array::new(scope, 2);
    let none: v8::Local<v8::Value> = v8::undefined(scope).into();
    callbacks.set_index(scope, 0, on_success.map(|f| f.into()).unwrap_or(none));
    callbacks.set_index(scope, 1, on_error.map(|f| f.into()).unwrap_or(none));
    data.set_index(scope, 3, callbacks.into());
    // `AudioBus::CreateBusFromInMemoryAudioFile(data, mix_to_mono = false, context sample rate)`: a file at
    // another rate is converted to the context's, every channel through the backend's `SincResampler`.
    let context_rate = priv_u32(scope, this, CTX_PRIV).and_then(|ctx| read(scope, ctx, |c| c.sample_rate));
    let render = backend(scope);
    start_job(scope, data, cb_decode_complete, move || {
        let decoded = decode_wav(&bytes).map(|(channels, rate)| match (context_rate, render) {
            (Some(to), Some(b)) if to != rate => {
                let converted: Option<Vec<Vec<f32>>> =
                    channels.iter().map(|c| b.resample_audio(c, rate as f64, to as f64)).collect();
                match converted {
                    Some(c) => (c, to),
                    None => (channels, rate),
                }
            }
            _ => (channels, rate),
        });
        AudioJobResult::Decoded(decoded)
    });
    rv.set(promise.into());
}

/// `decodeAudioData`'s answer task (`BaseAudioContext::HandleDecodeAudioData`). `data` carries
/// `[job id, context, resolver, [successCallback, errorCallback]]`. The promise settles first, then the callback
/// runs with the context as `this`, its exception reported rather than thrown.
fn cb_decode_complete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(body_data(scope, &args)) else { return };
    let Some(id) = data.get_index(scope, 0).and_then(|v| v.number_value(scope)).map(|n| n as u64) else { return };
    let Some(ctx_v) = data.get_index(scope, 1) else { return };
    let Some(r) = data.get_index(scope, 2).filter(|v| v.is_promise()) else { return };
    // SAFETY: a PromiseResolver is its promise's object; `decode_audio_data` stored it as one
    let resolver = unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(r) };
    let callbacks = data.get_index(scope, 3).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    let on_success = callbacks
        .and_then(|a| a.get_index(scope, 0))
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    let on_error = callbacks
        .and_then(|a| a.get_index(scope, 1))
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    let decoded = match take_job(scope, id) {
        Some(AudioJobResult::Decoded(d)) => d,
        _ => None,
    };
    match decoded {
        Some((channels, rate)) => {
            let len = channels.first().map(|c: &Vec<f32>| c.len()).unwrap_or(0);
            let n = channels.len();
            let buf = new_buffer(scope, n.max(1), len, rate, None);
            for (c, samples) in channels.iter().enumerate() {
                if let Some(arr) = priv_get(scope, buf, &format!("korvus_audio_ch{c}"))
                    .and_then(|v| v8::Local::<v8::Float32Array>::try_from(v).ok())
                {
                    for (i, s) in samples.iter().enumerate() {
                        let v = v8::Number::new(scope, *s as f64);
                        arr.set_index(scope, i as u32, v.into());
                    }
                }
            }
            resolver.resolve(scope, buf.into());
            if let Some(f) = on_success {
                invoke_and_report(scope, f, ctx_v, buf.into());
            }
        }
        None => {
            let Some(e) = crate::binding::protos::make_dom_exc(scope, "Unable to decode audio data", "EncodingError") else { return };
            resolver.reject(scope, e);
            if let Some(f) = on_error {
                invoke_and_report(scope, f, ctx_v, e);
            }
        }
    }
}

fn raw_bytes(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut out = vec![0u8; view.byte_length()];
        view.copy_contents(&mut out);
        return Some(out);
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let len = ab.byte_length();
        let view = v8::Uint8Array::new(scope, ab, 0, len)?;
        let mut out = vec![0u8; len];
        view.copy_contents(&mut out);
        return Some(out);
    }
    None
}

/// RIFF/WAVE, linear PCM only. Returns `(channels, sampleRate)`.
fn decode_wav(b: &[u8]) -> Option<(Vec<Vec<f32>>, f32)> {
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let u16at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as usize;
    let u32at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
    let (mut fmt, mut data) = (None, None);
    let mut p = 12usize;
    while p + 8 <= b.len() {
        let id = &b[p..p + 4];
        let size = u32at(p + 4);
        let body = p + 8;
        if body + size > b.len() {
            break;
        }
        if id == b"fmt " {
            fmt = Some(body);
        } else if id == b"data" {
            data = Some((body, size));
        }
        p = body + size + (size & 1);
    }
    let (f, (d, dlen)) = (fmt?, data?);
    let mut format = u16at(f);
    // WAVE_FORMAT_EXTENSIBLE: the real format tag leads the sub-format GUID (FFmpeg's `ff_get_wav_header`).
    if format == 0xFFFE && f + 26 <= b.len() {
        format = u16at(f + 24);
    }
    let channels = u16at(f + 2).max(1);
    let rate = u32at(f + 4) as f32;
    let bits = u16at(f + 14);
    let bytes = bits / 8;
    if bytes == 0 || (format != 1 && format != 3) {
        return None;
    }
    // content::DecodeAudioFileData's sanity checks (media::limits): 1..=32 channels, 3 kHz..768 kHz.
    if channels > 32 || !(3000.0..=768_000.0).contains(&rate) {
        return None;
    }
    // Integer PCM reaches Blink as FFmpeg's samples through `media::AudioBuffer`, whose conversion to float is
    // `FixedSampleTypeTraits::ToFloat` (media/base/audio_sample_types.h): the offset from the zero point times
    // `1 / (max - zero)` above it and `1 / (zero - min)` below, each inverse computed in float -- 8-bit and 16-bit
    // PCM do NOT scale both halves alike (16-bit: 1/32767 and 1/32768). 24-bit PCM is decoded to 32-bit
    // (`sample << 8`), where both factors are 2^-31.
    let s8 = |x: u8| -> f32 {
        let v = x as f32 - 128.0;
        if v < 0.0 { v * (1.0f32 / 128.0) } else { v * (1.0f32 / 127.0) }
    };
    let s16 = |x: i16| -> f32 {
        let v = x as f32;
        if v < 0.0 { v * (1.0f32 / 32768.0) } else { v * (1.0f32 / 32767.0) }
    };
    let s32 = |x: i32| -> f32 { x as f32 * (1.0f32 / 2_147_483_648.0) };
    let frames = dlen / (bytes * channels);
    // "number_of_frames == 0" decodes to nothing: an EncodingError.
    if frames == 0 {
        return None;
    }
    let mut out = vec![vec![0.0f32; frames]; channels];
    for i in 0..frames {
        for (c, ch) in out.iter_mut().enumerate() {
            let o = d + (i * channels + c) * bytes;
            ch[i] = match (format, bits) {
                (1, 8) => s8(b[o]),
                (1, 16) => s16(i16::from_le_bytes([b[o], b[o + 1]])),
                (1, 24) => s32(i32::from_le_bytes([0, b[o], b[o + 1], b[o + 2]])),
                (1, 32) => s32(i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])),
                (3, 32) => f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]),
                _ => return None,
            };
        }
    }
    Some((out, rate))
}

pub(crate) mod attrs;

// THE SIX WEB AUDIO INTERFACES THAT STAY GENERATED ROWS GET THEIR CONSTRUCTOR FROM THEIR ROW NOW
// (the pass was `install_audio_shell_ctors`, written by W5.1 lane L17 and deleted here).
//
// This function called `make_constructable_with_data` six times from `install::live::audio`, i.e. it
// SWAPPED six interface objects for freshly-built ones after `create_interface` had mounted them.
// Nothing about the swap was per-interface: `install::ctors::row_ctor` names the same six bodies and
// the ONE create builds each interface object with it, from the row's own `length` and prototype.
// The ledger's six `clobber:unbranded` verdicts on their `constructor` slots went with the swap.
//
// The two shapes are unchanged, and both are the capture's. `PeriodicWave` and `AudioWorkletNode`
// are objects this engine really builds ([`cb_new_periodic_wave`] / [`cb_new_worklet_node`]);
// `AudioWorklet`, `AudioParamMap`, `AudioPlaybackStats` and `AudioSinkInfo` are reached only through
// another object's accessor and their interface object throws "Illegal constructor", which is what
// `cb_illegal_ctor` over a `ConstructorBehavior::Allow` function gives and what Chrome 150 answers
// (measured headless 2026-09-12). `ChannelMergerNode` and `ChannelSplitterNode` share
// [`cb_new_audio_node`], keyed on the interface name bound as the constructor's `data` -- which the
// row now supplies, since the row is where the name lives.
//
// NONE of the six has a member this file implements -- `PeriodicWave` and `AudioWorklet` have no
// prototype member at all in the capture, and the other four's are shell stubs -- so there is
// nothing for a table to own. They stay generated rows.
//
// (L17's note, still true: "tabling them would only swap the shell's receiver-checked stubs for the
// D2 stub's unchecked ones" EXPIRED at `c564fe58`, which made the shell stub and the D2 stub one
// stub and brought the check with it. A lane that wants these four as tables may take them; what it
// will not gain is a body.)
