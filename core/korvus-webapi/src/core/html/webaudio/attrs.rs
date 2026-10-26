//! The Web Audio attributes — every one of them a native prototype ACCESSOR, and the install pass.
//!
//! Every audio member in `brave-151.surface.json` is an accessor on the prototype, and the
//! instances have no own properties at all. The previous implementation set own data properties on
//! the node objects (`type`, `frequency`, `fftSize`, `reduction`, …), which read back correctly and
//! showed six extra names to `Object.getOwnPropertyNames(osc)` — a shape difference a page checks in
//! one expression, and exactly the kind of convenience this engine does not take.

use super::*;

use crate::install::MemberDef;

/// An event-handler IDL attribute: stored on a private, reads back `null` when unset — the shape
/// `oncomplete`/`onstatechange`/`onended` have in every browser.
fn handler_slot(name: &str) -> String {
    format!("korvus_audio_{name}")
}


fn num_val<'s>(scope: &mut v8::PinScope<'s, '_>, n: f64) -> v8::Local<'s, v8::Value> {
    v8::Number::new(scope, n).into()
}

fn cb_attr_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (_iface, name) = key.split_once('.').unwrap_or(("", key.as_str()));
    let this = args.this();

    if name.starts_with("on") {
        match priv_get(scope, this, &handler_slot(name)) {
            Some(v) => rv.set(v),
            None => rv.set_null(),
        }
        return;
    }

    // ── BaseAudioContext / AudioContext / OfflineAudioContext ─────────────────────────────────
    if let Some(ctx) = priv_u32(scope, this, CTX_PRIV) {
        let Some((offline, rate, length, channels, rendered, suspended, rendering)) =
            read(scope, ctx, |c| (c.offline, c.sample_rate, c.length, c.channels, c.rendered, c.suspended, c.rendering))
        else {
            rv.set_undefined();
            return;
        };
        match name {
            "sampleRate" => {
                rv.set(num_val(scope, rate as f64));
                return;
            }
            "currentTime" => {
                let t = read(scope, ctx, |c| c.current_time()).unwrap_or(0.0);
                rv.set(num_val(scope, t));
                return;
            }
            "state" => {
                // An offline context: `suspended` until `startRendering()`, `running` while it renders,
                // `closed` once the render's answer task has run (PHASE 3 F7).
                let s = if offline {
                    if rendering {
                        "running"
                    } else if rendered {
                        "closed"
                    } else {
                        "suspended"
                    }
                } else if rendered {
                    "closed"
                } else if suspended {
                    "suspended"
                } else {
                    "running"
                };
                rv.set(str_val(scope, s));
                return;
            }
            "length" => {
                rv.set(num_val(scope, length as f64));
                return;
            }
            "baseLatency" => {
                // A render quantum block of 512 frames at the context's rate — the relation that
                // reproduces Brave's measured 0.010666666666666666 at 48 kHz exactly, rather than
                // that number written down.
                rv.set(num_val(scope, 512.0 / rate as f64));
                return;
            }
            "outputLatency" => {
                // Nothing is playing while suspended (V17): Brave 151 answers 0.
                rv.set(num_val(scope, if suspended { 0.0 } else { 1024.0 / rate as f64 }));
                return;
            }
            "sinkId" => {
                rv.set(str_val(scope, ""));
                return;
            }
            "destination" => {
                if let Some(d) = priv_get(scope, this, "korvus_audio_dest") {
                    rv.set(d);
                    return;
                }
                let d = new_shell(scope, "AudioDestinationNode");
                let cv = int_val(scope, ctx as i64);
                priv_set(scope, d, NODE_CTX_PRIV, cv);
                let iv = int_val(scope, 0);
                priv_set(scope, d, NODE_ID_PRIV, iv);
                let ov: v8::Local<v8::Value> = this.into();
                priv_set(scope, d, "korvus_audio_ctxobj", ov);
                let dv: v8::Local<v8::Value> = d.into();
                priv_set(scope, this, "korvus_audio_dest", dv);
                let _ = channels;
                rv.set(dv);
                return;
            }
            "listener" => {
                if let Some(l) = priv_get(scope, this, "korvus_audio_listener") {
                    rv.set(l);
                    return;
                }
                let l = new_node(scope, this, ctx, "AudioListener", None, 0.0);
                let lv: v8::Local<v8::Value> = l.into();
                priv_set(scope, this, "korvus_audio_listener", lv);
                rv.set(lv);
                return;
            }
            // The two context-owned singletons. Cached on the context, as a browser's are — a fresh
            // object per read would make `ctx.audioWorklet !== ctx.audioWorklet`, which no browser
            // does and which is a one-expression tell.
            "playbackStats" | "audioWorklet" => {
                let iface = if name == "playbackStats" { "AudioPlaybackStats" } else { "AudioWorklet" };
                if offline && name == "playbackStats" {
                    // `playbackStats` is AudioContext's, not BaseAudioContext's.
                    rv.set_undefined();
                    return;
                }
                let slot = format!("korvus_audio_{name}");
                if let Some(v) = priv_get(scope, this, &slot) {
                    rv.set(v);
                    return;
                }
                let o = new_shell(scope, iface);
                let ov: v8::Local<v8::Value> = o.into();
                priv_set(scope, this, &slot, ov);
                rv.set(ov);
                return;
            }
            _ => {}
        }
    }

    // ── AudioBuffer ───────────────────────────────────────────────────────────────────────────
    if let Some(nch) = priv_u32(scope, this, BUF_CH_PRIV) {
        let len = priv_u32(scope, this, BUF_LEN_PRIV).unwrap_or(0);
        let rate = priv_f64(scope, this, BUF_RATE_PRIV).unwrap_or(44100.0);
        match name {
            "length" => rv.set(num_val(scope, len as f64)),
            "sampleRate" => rv.set(num_val(scope, rate)),
            "numberOfChannels" => rv.set(num_val(scope, nch as f64)),
            "duration" => rv.set(num_val(scope, len as f64 / rate.max(1.0))),
            _ => rv.set_undefined(),
        }
        return;
    }

    // ── AudioParam ────────────────────────────────────────────────────────────────────────────
    if let Some((node, pname)) = param_owner(scope, this) {
        let ctx = priv_get(scope, this, PARAM_NODE_PRIV)
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
            .and_then(|o| priv_u32(scope, o, NODE_CTX_PRIV));
        let Some(ctx) = ctx else {
            rv.set_undefined();
            return;
        };
        let Some((value, iface_of_node, arg, rate)) = read(scope, ctx, |c| {
            let m = c.nodes.get(&node)?;
            Some((*m.params.get(pname.as_str()).unwrap_or(&0.0), m.iface, m.arg, c.sample_rate))
        })
        .flatten() else {
            rv.set_undefined();
            return;
        };
        let (min, max, arate) = param_range(&pname, rate, arg);
        let default = param_defaults(iface_of_node)
            .iter()
            .find(|(n, _)| *n == pname)
            .map(|(_, d)| *d)
            .unwrap_or(0.0);
        match name {
            "value" => rv.set(num_val(scope, value as f64)),
            "defaultValue" => rv.set(num_val(scope, default as f64)),
            "minValue" => rv.set(num_val(scope, min as f64)),
            "maxValue" => rv.set(num_val(scope, max as f64)),
            "automationRate" => rv.set(str_val(scope, arate)),
            _ => rv.set_undefined(),
        }
        return;
    }

    // ── OfflineAudioCompletionEvent / AudioProcessingEvent ────────────────────────────────────
    if name == "renderedBuffer" {
        match priv_get(scope, this, "korvus_audio_rendered") {
            Some(v) => rv.set(v),
            None => rv.set_null(),
        }
        return;
    }
    if matches!(name, "playbackTime" | "inputBuffer" | "outputBuffer") {
        match priv_get(scope, this, &format!("korvus_audio_{name}")) {
            Some(v) => rv.set(v),
            None => rv.set_undefined(),
        }
        return;
    }

    // ── AudioNode and every node interface ────────────────────────────────────────────────────
    let Some((ctx, id)) = node_of(scope, this) else {
        rv.set_undefined();
        return;
    };
    // The params first: they are objects, cached on the node, and `osc.frequency ===
    // osc.frequency` has to hold.
    let is_param = read(scope, ctx, |c| {
        c.nodes.get(&id).map(|m| m.params.contains_key(name)).unwrap_or(false)
    })
    .unwrap_or(false);
    if is_param {
        let p = param_obj(scope, this, name);
        rv.set(p);
        return;
    }
    let Some(meta) = read(scope, ctx, |c| c.nodes.get(&id).cloned()).flatten() else {
        rv.set_undefined();
        return;
    };
    match name {
        "context" => match priv_get(scope, this, "korvus_audio_ctxobj") {
            Some(v) => rv.set(v),
            None => rv.set_undefined(),
        },
        "numberOfInputs" => rv.set(num_val(scope, meta.inputs as f64)),
        "numberOfOutputs" => rv.set(num_val(scope, meta.outputs as f64)),
        "channelCount" => rv.set(num_val(scope, meta.channel_count as f64)),
        "channelCountMode" => rv.set(str_val(scope, meta.channel_count_mode)),
        "channelInterpretation" => rv.set(str_val(scope, meta.channel_interpretation)),
        "maxChannelCount" => {
            let m = read(scope, ctx, |c| c.max_channels).unwrap_or(2);
            rv.set(num_val(scope, m as f64));
        }
        "type" => {
            let t = if meta.iface == "BiquadFilterNode" { &meta.biquad_type } else { &meta.osc_type };
            let t = t.clone();
            rv.set(str_val(scope, &t));
        }
        "fftSize" => rv.set(num_val(scope, meta.fft_size as f64)),
        "frequencyBinCount" => rv.set(num_val(scope, (meta.fft_size / 2) as f64)),
        "minDecibels" => rv.set(num_val(scope, meta.min_db)),
        "maxDecibels" => rv.set(num_val(scope, meta.max_db)),
        "smoothingTimeConstant" => rv.set(num_val(scope, meta.smoothing)),
        "loop" => rv.set(v8::Boolean::new(scope, meta.looping).into()),
        "loopStart" => rv.set(num_val(scope, meta.loop_start)),
        "loopEnd" => rv.set(num_val(scope, meta.loop_end)),
        "oversample" => {
            let o = meta.oversample.clone();
            rv.set(str_val(scope, &o));
        }
        "reduction" => {
            // Computed, from the same run that produced the samples. The tape served one recorded
            // constant here (-19.004104614257812) for every graph, every threshold, every input.
            match ask(scope, ctx, AudioQuery::Reduction { node: id }) {
                AudioQueryResult::Scalar(v) => rv.set(num_val(scope, v as f64)),
                _ => rv.set(num_val(scope, 0.0)),
            }
        }
        "buffer" | "curve" => match priv_get(scope, this, &format!("korvus_audio_{name}")) {
            Some(v) => rv.set(v),
            None => rv.set_null(),
        },
        "normalize" => {
            let v = priv_get(scope, this, "korvus_audio_normalize").map(|v| v.boolean_value(scope)).unwrap_or(true);
            rv.set(v8::Boolean::new(scope, v).into());
        }
        "bufferSize" => rv.set(num_val(scope, meta.buffer_size as f64)),
        // PannerNode geometry: stored, readable, and consumed by nothing — see the module comment.
        // These read off the node's own projection rather than a constant, so a page that WRITES
        // one reads its write back, which is what every browser does and what a constant did not.
        "panningModel" => {
            let s = meta.panning_model.clone();
            rv.set(str_val(scope, &s));
        }
        "distanceModel" => {
            let s = meta.distance_model.clone();
            rv.set(str_val(scope, &s));
        }
        "refDistance" => rv.set(num_val(scope, meta.ref_distance)),
        "maxDistance" => rv.set(num_val(scope, meta.max_distance)),
        "rolloffFactor" => rv.set(num_val(scope, meta.rolloff_factor)),
        "coneInnerAngle" => rv.set(num_val(scope, meta.cone_inner_angle)),
        "coneOuterAngle" => rv.set(num_val(scope, meta.cone_outer_angle)),
        "coneOuterGain" => rv.set(num_val(scope, meta.cone_outer_gain)),
        "mediaElement" | "mediaStream" | "stream" => {
            match priv_get(scope, this, &format!("korvus_audio_{name}")) {
                Some(v) => rv.set(v),
                None => rv.set_undefined(),
            }
        }
        _ => rv.set_undefined(),
    }
}

fn cb_attr_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (_iface, name) = key.split_once('.').unwrap_or(("", key.as_str()));
    let this = args.this();
    let v = args.get(0);

    if name.starts_with("on") {
        priv_set(scope, this, &handler_slot(name), v);
        return;
    }

    let Some((ctx, id)) = node_of(scope, this) else { return };

    // An AudioParam attribute is read-only in every browser (`osc.frequency = 5` silently does
    // nothing in sloppy mode); the accessors for them are installed without a setter, so nothing
    // reaches here for those names.
    match name {
        "type" => {
            let s = v.to_rust_string_lossy(scope);
            let is_biquad = read(scope, ctx, |c| c.nodes.get(&id).map(|m| m.iface == "BiquadFilterNode")).flatten().unwrap_or(false);
            if is_biquad {
                let Some(k) = biquad_type(&s) else { return };
                edit(scope, ctx, |c| {
                    if let Some(m) = c.nodes.get_mut(&id) {
                        m.biquad_type = s.clone();
                    }
                });
                push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::BiquadType(k) });
            } else {
                // `type = "custom"` without a PeriodicWave is an InvalidStateError, per spec.
                if s == "custom" {
                    throw_dom(scope, "Failed to set the 'type' property on 'OscillatorNode': The provided value 'custom' is not a valid enum value.", "InvalidStateError");
                    return;
                }
                let Some(k) = osc_type(&s) else { return };
                edit(scope, ctx, |c| {
                    if let Some(m) = c.nodes.get_mut(&id) {
                        m.osc_type = s.clone();
                    }
                });
                push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::OscillatorType(k) });
            }
        }
        "fftSize" => {
            let n = v.number_value(scope).unwrap_or(0.0) as u32;
            if !(32..=32768).contains(&n) || !n.is_power_of_two() {
                throw_dom(scope, "Failed to set the 'fftSize' property on 'AnalyserNode': The value provided is not a power of two, or is outside the range [32, 32768].", "IndexSizeError");
                return;
            }
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    m.fft_size = n;
                }
            });
            push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::FftSize(n) });
        }
        "minDecibels" | "maxDecibels" | "smoothingTimeConstant" => {
            let n = v.number_value(scope).unwrap_or(f64::NAN);
            if !n.is_finite() {
                return;
            }
            if name == "smoothingTimeConstant" && !(0.0..=1.0).contains(&n) {
                throw_dom(scope, "Failed to set the 'smoothingTimeConstant' property on 'AnalyserNode': The value provided is outside the range [0, 1].", "IndexSizeError");
                return;
            }
            // The two decibel bounds are ORDERED, not independent: `minDecibels` must stay below
            // `maxDecibels` (default -30), so `min = 1` is an IndexSizeError while `max = 1` is
            // simply a wider window. Both are what Brave 151 answers, and the asymmetry is the
            // whole point of the pair of recorded cases.
            let (cur_min, cur_max) =
                read(scope, ctx, |c| c.nodes.get(&id).map(|m| (m.min_db, m.max_db))).flatten().unwrap_or((-100.0, -30.0));
            let bad = match name {
                "minDecibels" => n >= cur_max,
                "maxDecibels" => n <= cur_min,
                _ => false,
            };
            if bad {
                throw_dom(scope, &format!("Failed to set the '{name}' property on 'AnalyserNode': minDecibels must be less than maxDecibels."), "IndexSizeError");
                return;
            }
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    match name {
                        "minDecibels" => m.min_db = n,
                        "maxDecibels" => m.max_db = n,
                        _ => m.smoothing = n,
                    }
                }
            });
            let prop = match name {
                "minDecibels" => AudioProp::MinDecibels(n),
                "maxDecibels" => AudioProp::MaxDecibels(n),
                _ => AudioProp::SmoothingTimeConstant(n),
            };
            push(scope, ctx, AudioOp::SetProp { id, prop });
        }
        "channelCount" => {
            let n = (v.number_value(scope).unwrap_or(2.0) as u32).clamp(1, 32);
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    m.channel_count = n;
                }
            });
            push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::ChannelCount(n) });
        }
        "channelCountMode" => {
            let s = v.to_rust_string_lossy(scope);
            let code = match s.as_str() {
                "max" => 0,
                "clamped-max" => 1,
                "explicit" => 2,
                _ => return,
            };
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    m.channel_count_mode = match code {
                        0 => "max",
                        1 => "clamped-max",
                        _ => "explicit",
                    };
                }
            });
            push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::ChannelCountMode(code) });
        }
        "channelInterpretation" => {
            let s = v.to_rust_string_lossy(scope);
            let code = match s.as_str() {
                "speakers" => 0,
                "discrete" => 1,
                _ => return,
            };
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    m.channel_interpretation = if code == 0 { "speakers" } else { "discrete" };
                }
            });
            push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::ChannelInterpretation(code) });
        }
        "loop" => {
            let b = v.boolean_value(scope);
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    m.looping = b;
                }
            });
            push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::Loop(b) });
        }
        "loopStart" | "loopEnd" => {
            let n = v.number_value(scope).unwrap_or(0.0);
            if !n.is_finite() {
                return;
            }
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    if name == "loopStart" {
                        m.loop_start = n;
                    } else {
                        m.loop_end = n;
                    }
                }
            });
            let prop = if name == "loopStart" { AudioProp::LoopStart(n) } else { AudioProp::LoopEnd(n) };
            push(scope, ctx, AudioOp::SetProp { id, prop });
        }
        "oversample" => {
            let s = v.to_rust_string_lossy(scope);
            let n = match s.as_str() {
                "none" => 1,
                "2x" => 2,
                "4x" => 4,
                _ => return,
            };
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    m.oversample = s.clone();
                }
            });
            push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::Oversample(n) });
        }
        "curve" => {
            priv_set(scope, this, "korvus_audio_curve", v);
            let c = if v.is_null() || v.is_undefined() { None } else { Some(f32_seq(scope, v)) };
            push(scope, ctx, AudioOp::SetProp { id, prop: AudioProp::Curve(c) });
        }
        "buffer" => {
            priv_set(scope, this, "korvus_audio_buffer", v);
            let is_source = read(scope, ctx, |c| c.nodes.get(&id).map(|m| m.iface == "AudioBufferSourceNode"))
                .flatten()
                .unwrap_or(false);
            if !is_source {
                return; // ConvolverNode: stored so it reads back, and consumed by nothing.
            }
            let Ok(b) = v8::Local::<v8::Object>::try_from(v) else { return };
            // The channel data is read HERE, at assignment, which is when a browser acquires the
            // buffer's content. A source that read it at render time would play whatever the page
            // wrote afterwards, which is not what `[[acquire the content]]` means.
            if let Some((channels, rate)) = buffer_channels(scope, b) {
                push(scope, ctx, AudioOp::SetBuffer { id, channels, sample_rate: rate });
            }
        }
        "normalize" => {
            let b: v8::Local<v8::Value> = v8::Boolean::new(scope, v.boolean_value(scope)).into();
            priv_set(scope, this, "korvus_audio_normalize", b);
        }
        // PannerNode geometry. Written to the node's projection so it reads back; no `AudioOp`,
        // because the node that would consume it has no processor and an op nothing executes is the
        // fiction this seam exists to prevent.
        "panningModel" | "distanceModel" => {
            let s = v.to_rust_string_lossy(scope);
            let valid = if name == "panningModel" {
                matches!(s.as_str(), "equalpower" | "HRTF")
            } else {
                matches!(s.as_str(), "linear" | "inverse" | "exponential")
            };
            if !valid {
                return;
            }
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    if name == "panningModel" {
                        m.panning_model = s.clone();
                    } else {
                        m.distance_model = s.clone();
                    }
                }
            });
        }
        "refDistance" | "maxDistance" | "rolloffFactor" | "coneInnerAngle" | "coneOuterAngle"
        | "coneOuterGain" => {
            let n = v.number_value(scope).unwrap_or(f64::NAN);
            if !n.is_finite() {
                return;
            }
            if matches!(name, "refDistance" | "maxDistance" | "rolloffFactor") && n < 0.0 {
                throw_dom(scope, &format!("Failed to set the '{name}' property on 'PannerNode': The value provided is outside the range [0, Infinity]."), "RangeError");
                return;
            }
            edit(scope, ctx, |c| {
                if let Some(m) = c.nodes.get_mut(&id) {
                    match name {
                        "refDistance" => m.ref_distance = n,
                        "maxDistance" => m.max_distance = n,
                        "rolloffFactor" => m.rolloff_factor = n,
                        "coneInnerAngle" => m.cone_inner_angle = n,
                        "coneOuterAngle" => m.cone_outer_angle = n,
                        _ => m.cone_outer_gain = n,
                    }
                }
            });
        }
        _ => {}
    }
}

/// `AudioParam.value = x` — the one param write that is not an automation event. It is here rather
/// than in [`cb_attr_set`] because it is the only setter whose receiver is an `AudioParam` and not
/// an `AudioNode`, and folding it into the node path would have needed the node lookup twice.
fn cb_param_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // ToNumber, exactly as WebIDL asks: `param.value = "x"` stores NaN, it does not throw.
    let n = args.get(0).number_value(scope).unwrap_or(f64::NAN) as f32;
    set_param_value(scope, args.this(), n, true);
}

/// Write an `AudioParam`'s value. From script (`schedule`), Blink's setter also schedules the value as
/// an automation event at the context's current time (`AudioParam::setValue(value, exception_state)`
/// -> `setValueAtTime(IntrinsicValue(), currentTime)`), which is what puts the param on the
/// sample-accurate path for the quantum it lands in -- an oscillator's first 128 frames after
/// `frequency.value = f` are rendered by a-rate kernel, the rest by the k-rate one, and the two
/// round differently. A node's constructor options set the value without one
/// (`OscillatorNode::Create` calls the overload that does not schedule).
pub(crate) fn set_param_value(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, n: f32, schedule: bool) {
    let Some((node, name)) = param_owner(scope, this) else { return };
    let ctx = priv_get(scope, this, PARAM_NODE_PRIV)
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|o| priv_u32(scope, o, NODE_CTX_PRIV));
    let Some(ctx) = ctx else { return };
    edit(scope, ctx, |c| {
        if let Some(m) = c.nodes.get_mut(&node) {
            if let Some(slot) = m.params.get_mut(name.as_str()) {
                *slot = n;
            }
        }
    });
    if let Some(p) = param_id(&name) {
        push(scope, ctx, AudioOp::SetParam { id: node, param: p, value: n });
        if schedule && n.is_finite() {
            let time = read(scope, ctx, |c| c.current_time()).unwrap_or(0.0);
            push(scope, ctx, AudioOp::Automate { id: node, param: p, event: spec::AutomationEvent::SetValueAtTime { value: n, time } });
        }
    }
}

fn cb_param_rate_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // `automationRate` is settable in the IDL and the value is only advisory here: every param this
    // renderer evaluates is evaluated per sample, so declaring one k-rate cannot change the output.
    // Storing it would therefore be storing a value with no consequence; the getter reports the
    // spec's fixed rate for the param instead, which is what a browser reports for the params whose
    // rate is fixed.
    let _ = (scope, args);
}
// ── install: the twenty-eight tables ──────────────────────────────────────────────────────────
//
// Until this lane this file held a 178-line `AUDIO` const --
// `(interface, [(attribute, writable)], [(method, arity)])` transcribed from
// `brave-151.surface.json` -- and walked it, fetching each prototype out of the generated shell
// registry and defining members onto it. Two things were wrong with that beyond the second
// installer invariant I2 forbids: the member SHAPE (which attributes are writable, what each
// method's arity is) was written down TWICE, once in the generated registry and once in that const,
// and none of the members entered the install ledger. The rows below are the one copy, derived from
// the `InterfaceDef`s they replace, and `AUDIO` is deleted.

/// One audio attribute row, over the ONE pair of generic callbacks keyed on `"{interface}.{member}"`
/// -- which is why the interface comes from the ROW'S OWN LABEL rather than a loop variable: member
/// names repeat across this family (`buffer` is an `AudioBufferSourceNode`'s and a
/// `ConvolverNode`'s, `type` an `OscillatorNode`'s and a `BiquadFilterNode`'s).
fn audio_attr_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, writable: bool) {
    let iface = member.label.split("::").next().unwrap_or("");
    let key = format!("{iface}.{}", member.name);
    let data = str_val(scope, &key);
    let g = build_fn_data(scope, cb_attr_get, &format!("get {}", member.name), 0, data);
    let s = if writable {
        let data = str_val(scope, &key);
        Some(build_fn_data(scope, cb_attr_set, &format!("set {}", member.name), 1, data))
    } else {
        None
    };
    crate::install::kits::define_accessor(scope, proto, member, g, s);
}

/// One `AudioParam` row whose SETTER is not the generic one: `value` is the single write in this
/// whole API that is not an automation event, and `automationRate` is advisory (see
/// [`cb_param_rate_set`]).
fn audio_param_row(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    setter: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let data = str_val(scope, &format!("AudioParam.{}", member.name));
    let g = build_fn_data(scope, cb_attr_get, &format!("get {}", member.name), 0, data);
    let s = crate::binding::protos::build_fn(scope, setter, &format!("set {}", member.name), 1);
    crate::install::kits::define_accessor(scope, proto, member, g, Some(s));
}

/// One audio method row: the generic [`cb_audio`] dispatcher keyed on `"{interface}.{member}"`, with
/// the row's WebIDL name, length and arity guard. Before the cutover the guard came from the
/// registry-wide arity pass, from the same `required` count.
fn audio_method_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::binding::protos::make_arity_guard;
    let iface = member.label.split("::").next().unwrap_or("");
    let data = str_val(scope, &format!("{iface}.{}", member.name));
    let f = build_fn_data(scope, cb_audio, member.name, member.length as i32, data);
    let v: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, iface, member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    crate::install::kits::define_value(scope, proto, member, v, true);
}

/// Every row of all twenty-eight Web Audio tables.
///
/// ONE install fn for the family, because one pair of callbacks already serves it: every attribute
/// reads and writes through [`cb_attr_get`] / [`cb_attr_set`] and every method through [`cb_audio`],
/// each dispatching on `"{interface}.{member}"`. The row carries that key in its label, so the
/// dispatch is the table's and not a second list.
pub fn install_webaudio(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::MemberKind;
    let iface = member.label.split("::").next().unwrap_or("");
    match (iface, member.name, member.kind) {
        ("AudioParam", "value", _) => audio_param_row(scope, proto, member, cb_param_value_set),
        ("AudioParam", "automationRate", _) => audio_param_row(scope, proto, member, cb_param_rate_set),
        // An event-handler IDL attribute is writable and stores on a private, reading back `null`
        // when unset -- which is what `cb_attr_get`'s handler path already does.
        (_, _, MemberKind::Handler) => audio_attr_row(scope, proto, member, true),
        (_, _, MemberKind::Attr { readonly }) => audio_attr_row(scope, proto, member, !readonly),
        (_, _, MemberKind::Method) => audio_method_row(scope, proto, member),
        (_, other, _) => debug_assert!(false, "install_webaudio: unexpected member {iface}.{other}"),
    }
}
webapi::interface! {
    "BaseAudioContext" : "EventTarget" [interface, window] {
        ctor "BaseAudioContext" len 0 = stub,
        attr "destination" : Any readonly = install_webaudio,
        attr "sampleRate" : Any readonly = install_webaudio,
        attr "currentTime" : Any readonly = install_webaudio,
        attr "listener" : Any readonly = install_webaudio,
        attr "state" : Any readonly = install_webaudio,
        handler "onstatechange" nullable = install_webaudio,
        method "createAnalyser" : Any = install_webaudio,
        method "createBiquadFilter" : Any = install_webaudio,
        method "createBuffer" : Any len 3 required 3 = install_webaudio,
        method "createBufferSource" : Any = install_webaudio,
        method "createChannelMerger" : Any = install_webaudio,
        method "createChannelSplitter" : Any = install_webaudio,
        method "createConstantSource" : Any = install_webaudio,
        method "createConvolver" : Any = install_webaudio,
        method "createDelay" : Any = install_webaudio,
        method "createDynamicsCompressor" : Any = install_webaudio,
        method "createGain" : Any = install_webaudio,
        method "createIIRFilter" : Any len 2 required 2 = install_webaudio,
        method "createOscillator" : Any = install_webaudio,
        method "createPanner" : Any = install_webaudio,
        method "createPeriodicWave" : Any len 2 required 2 = install_webaudio,
        method "createScriptProcessor" : Any = install_webaudio,
        method "createStereoPanner" : Any = install_webaudio,
        method "createWaveShaper" : Any = install_webaudio,
        method "decodeAudioData" : Promise len 1 required 1 = install_webaudio,
        attr "audioWorklet" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "AudioContext" : "BaseAudioContext" [value, window] {
        ctor "AudioContext" len 0 = stub,
        attr "baseLatency" : Any readonly = install_webaudio,
        attr "outputLatency" : Any readonly = install_webaudio,
        handler "onerror" nullable = install_webaudio,
        method "close" : Promise = install_webaudio,
        method "createMediaElementSource" : Any len 1 required 1 = install_webaudio,
        method "createMediaStreamDestination" : Any = install_webaudio,
        method "createMediaStreamSource" : Any len 1 required 1 = install_webaudio,
        method "getOutputTimestamp" : Any = install_webaudio,
        method "resume" : Promise = install_webaudio,
        method "suspend" : Promise = install_webaudio,
        attr "playbackStats" : Any readonly = install_webaudio,
        attr "sinkId" : Any readonly = install_webaudio,
        handler "onsinkchange" nullable = install_webaudio,
        method "setSinkId" : Promise len 1 required 1 = install_webaudio
    }
}

webapi::interface! {
    "OfflineAudioContext" : "BaseAudioContext" [value, window] {
        ctor "OfflineAudioContext" len 1 = stub,
        handler "oncomplete" nullable = install_webaudio,
        attr "length" : Any readonly = install_webaudio,
        method "resume" : Promise = install_webaudio,
        method "startRendering" : Promise = install_webaudio,
        method "suspend" : Promise len 1 required 1 = install_webaudio
    }
}

webapi::interface! {
    "AudioNode" : "EventTarget" [interface, window] {
        ctor "AudioNode" len 0 = stub,
        attr "context" : Any readonly = install_webaudio,
        attr "numberOfInputs" : Any readonly = install_webaudio,
        attr "numberOfOutputs" : Any readonly = install_webaudio,
        attr "channelCount" : Any = install_webaudio,
        attr "channelCountMode" : Any = install_webaudio,
        attr "channelInterpretation" : Any = install_webaudio,
        method "connect" : Any len 1 required 1 = install_webaudio,
        method "disconnect" : Any = install_webaudio
    }
}

webapi::interface! {
    "AudioScheduledSourceNode" : "AudioNode" [interface, window] {
        ctor "AudioScheduledSourceNode" len 0 = stub,
        handler "onended" nullable = install_webaudio,
        method "start" : Any = install_webaudio,
        method "stop" : Any = install_webaudio
    }
}

webapi::interface! {
    "OscillatorNode" : "AudioScheduledSourceNode" [value, window] {
        ctor "OscillatorNode" len 1 = stub,
        attr "type" : Any = install_webaudio,
        attr "frequency" : Any readonly = install_webaudio,
        attr "detune" : Any readonly = install_webaudio,
        method "setPeriodicWave" : Any len 1 required 1 = install_webaudio
    }
}

webapi::interface! {
    "GainNode" : "AudioNode" [value, window] {
        ctor "GainNode" len 1 = stub,
        attr "gain" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "DynamicsCompressorNode" : "AudioNode" [value, window] {
        ctor "DynamicsCompressorNode" len 1 = stub,
        attr "threshold" : Any readonly = install_webaudio,
        attr "knee" : Any readonly = install_webaudio,
        attr "ratio" : Any readonly = install_webaudio,
        attr "reduction" : Any readonly = install_webaudio,
        attr "attack" : Any readonly = install_webaudio,
        attr "release" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "AnalyserNode" : "AudioNode" [value, window] {
        ctor "AnalyserNode" len 1 = stub,
        attr "fftSize" : Any = install_webaudio,
        attr "frequencyBinCount" : Any readonly = install_webaudio,
        attr "minDecibels" : Any = install_webaudio,
        attr "maxDecibels" : Any = install_webaudio,
        attr "smoothingTimeConstant" : Any = install_webaudio,
        method "getByteFrequencyData" : Any len 1 required 1 = install_webaudio,
        method "getByteTimeDomainData" : Any len 1 required 1 = install_webaudio,
        method "getFloatFrequencyData" : Any len 1 required 1 = install_webaudio,
        method "getFloatTimeDomainData" : Any len 1 required 1 = install_webaudio
    }
}

webapi::interface! {
    "AudioBufferSourceNode" : "AudioScheduledSourceNode" [value, window] {
        ctor "AudioBufferSourceNode" len 1 = stub,
        attr "buffer" : Any nullable = install_webaudio,
        attr "playbackRate" : Any readonly = install_webaudio,
        attr "detune" : Any readonly = install_webaudio,
        attr "loop" : Any = install_webaudio,
        attr "loopStart" : Any = install_webaudio,
        attr "loopEnd" : Any = install_webaudio,
        method "start" : Any = install_webaudio
    }
}

webapi::interface! {
    "DelayNode" : "AudioNode" [value, window] {
        ctor "DelayNode" len 1 = stub,
        attr "delayTime" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "BiquadFilterNode" : "AudioNode" [value, window] {
        ctor "BiquadFilterNode" len 1 = stub,
        attr "type" : Any = install_webaudio,
        attr "frequency" : Any readonly = install_webaudio,
        attr "detune" : Any readonly = install_webaudio,
        attr "Q" : Any readonly = install_webaudio,
        attr "gain" : Any readonly = install_webaudio,
        method "getFrequencyResponse" : Any len 3 required 3 = install_webaudio
    }
}

webapi::interface! {
    "StereoPannerNode" : "AudioNode" [value, window] {
        ctor "StereoPannerNode" len 1 = stub,
        attr "pan" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "WaveShaperNode" : "AudioNode" [value, window] {
        ctor "WaveShaperNode" len 1 = stub,
        attr "curve" : Any nullable = install_webaudio,
        attr "oversample" : Any = install_webaudio
    }
}

webapi::interface! {
    "ConstantSourceNode" : "AudioScheduledSourceNode" [value, window] {
        ctor "ConstantSourceNode" len 1 = stub,
        attr "offset" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "AudioDestinationNode" : "AudioNode" [interface, window] {
        ctor "AudioDestinationNode" len 0 = stub,
        attr "maxChannelCount" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "AudioBuffer" [value, window] {
        ctor "AudioBuffer" len 1 = stub,
        attr "length" : Any readonly = install_webaudio,
        attr "duration" : Any readonly = install_webaudio,
        attr "sampleRate" : Any readonly = install_webaudio,
        attr "numberOfChannels" : Any readonly = install_webaudio,
        method "copyFromChannel" : Any len 2 required 2 = install_webaudio,
        method "copyToChannel" : Any len 2 required 2 = install_webaudio,
        method "getChannelData" : Any len 1 required 1 = install_webaudio
    }
}

webapi::interface! {
    "AudioListener" [interface, window] {
        ctor "AudioListener" len 0 = stub,
        attr "positionX" : Any readonly = install_webaudio,
        attr "positionY" : Any readonly = install_webaudio,
        attr "positionZ" : Any readonly = install_webaudio,
        attr "forwardX" : Any readonly = install_webaudio,
        attr "forwardY" : Any readonly = install_webaudio,
        attr "forwardZ" : Any readonly = install_webaudio,
        attr "upX" : Any readonly = install_webaudio,
        attr "upY" : Any readonly = install_webaudio,
        attr "upZ" : Any readonly = install_webaudio,
        method "setOrientation" : Any len 6 required 6 = install_webaudio,
        method "setPosition" : Any len 3 required 3 = install_webaudio
    }
}

webapi::interface! {
    "ConvolverNode" : "AudioNode" [value, window] {
        ctor "ConvolverNode" len 1 = stub,
        attr "buffer" : Any nullable = install_webaudio,
        attr "normalize" : Any = install_webaudio
    }
}

webapi::interface! {
    "PannerNode" : "AudioNode" [value, window] {
        ctor "PannerNode" len 1 = stub,
        attr "panningModel" : Any = install_webaudio,
        attr "positionX" : Any readonly = install_webaudio,
        attr "positionY" : Any readonly = install_webaudio,
        attr "positionZ" : Any readonly = install_webaudio,
        attr "orientationX" : Any readonly = install_webaudio,
        attr "orientationY" : Any readonly = install_webaudio,
        attr "orientationZ" : Any readonly = install_webaudio,
        attr "distanceModel" : Any = install_webaudio,
        attr "refDistance" : Any = install_webaudio,
        attr "maxDistance" : Any = install_webaudio,
        attr "rolloffFactor" : Any = install_webaudio,
        attr "coneInnerAngle" : Any = install_webaudio,
        attr "coneOuterAngle" : Any = install_webaudio,
        attr "coneOuterGain" : Any = install_webaudio,
        method "setOrientation" : Any len 3 required 3 = install_webaudio,
        method "setPosition" : Any len 3 required 3 = install_webaudio
    }
}

webapi::interface! {
    "IIRFilterNode" : "AudioNode" [value, window] {
        ctor "IIRFilterNode" len 2 = stub,
        method "getFrequencyResponse" : Any len 3 required 3 = install_webaudio
    }
}

webapi::interface! {
    "ScriptProcessorNode" : "AudioNode" [interface, window] {
        ctor "ScriptProcessorNode" len 0 = stub,
        handler "onaudioprocess" nullable = install_webaudio,
        attr "bufferSize" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "OfflineAudioCompletionEvent" : "Event" [value, window] {
        ctor "OfflineAudioCompletionEvent" len 2 = stub,
        attr "renderedBuffer" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "AudioProcessingEvent" : "Event" [value, window] {
        ctor "AudioProcessingEvent" len 2 = stub,
        attr "playbackTime" : Any readonly = install_webaudio,
        attr "inputBuffer" : Any readonly = install_webaudio,
        attr "outputBuffer" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "MediaElementAudioSourceNode" : "AudioNode" [value, window] {
        ctor "MediaElementAudioSourceNode" len 2 = stub,
        attr "mediaElement" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "MediaStreamAudioSourceNode" : "AudioNode" [value, window] {
        ctor "MediaStreamAudioSourceNode" len 2 = stub,
        attr "mediaStream" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "MediaStreamAudioDestinationNode" : "AudioNode" [value, window] {
        ctor "MediaStreamAudioDestinationNode" len 1 = stub,
        attr "stream" : Any readonly = install_webaudio
    }
}

webapi::interface! {
    "AudioParam" [interface, window] {
        ctor "AudioParam" len 0 = stub,
        attr "value" : Any = install_webaudio,
        attr "automationRate" : Any = install_webaudio,
        attr "defaultValue" : Any readonly = install_webaudio,
        attr "minValue" : Any readonly = install_webaudio,
        attr "maxValue" : Any readonly = install_webaudio,
        method "cancelAndHoldAtTime" : Any len 1 required 1 = install_webaudio,
        method "cancelScheduledValues" : Any len 1 required 1 = install_webaudio,
        method "exponentialRampToValueAtTime" : Any len 2 required 2 = install_webaudio,
        method "linearRampToValueAtTime" : Any len 2 required 2 = install_webaudio,
        method "setTargetAtTime" : Any len 3 required 3 = install_webaudio,
        method "setValueAtTime" : Any len 2 required 2 = install_webaudio,
        method "setValueCurveAtTime" : Any len 3 required 3 = install_webaudio
    }
}
