//! `core/media/media_recorder.rs` -- `MediaRecorder` as one `interface!` table.
//!
//! Rows are VERBATIM from the `MediaRecorder` block of `install/generated_registry.rs`: names, order,
//! kinds, lengths, `required` counts and descriptor flags.
//!
//! EVERY MEMBER HERE DESCRIBES A RECORDER'S STATE MACHINE, NOT AN ENCODER. `state` moves
//! `inactive -> recording -> paused -> recording -> inactive`; `start()` refuses a stream with no
//! tracks (`NotSupportedError`) and refuses a second call (`InvalidStateError`); `pause`, `resume` and
//! `requestData` refuse an inactive recorder (`InvalidStateError`) while `stop()` on one is a silent
//! no-op. That machine is observable in full, it is what the recorded cases ask about, and none of it
//! needs a byte of encoded video.
//!
//! WHAT IS DELIBERATELY MISSING, and it is the one thing here a stub would have faked: no
//! `dataavailable` event is ever fired, because it carries a `Blob` of encoded media and there is no
//! encoder to produce one. `requestData()` therefore validates its state, answers `undefined` (which
//! is what the recording says it returns) and queues nothing. An empty Blob would satisfy a case and
//! be a lie about the bytes; a missing event is a gap that is visible.
//!
//! THE BITRATES ARE RECORDED, NOT CHOSEN: a fresh recorder reports `videoBitsPerSecond` 2500000 and
//! `audioBitsPerSecond` 128000, and `{ bitsPerSecond: 200000 }` splits into 180000 / 20000 -- one
//! tenth to the audio track.
//!
//! THE HANDLERS CARRY A BRAND CHECK, which the shared `event_handler` kit does not. The bundle's
//! oracle records `Object.getOwnPropertyDescriptor(MediaRecorder.prototype, "onstart").get.call(
//! Object.create(MediaRecorder.prototype))` as a `TypeError` for all six handlers, so each one checks
//! the receiver and then delegates to the SAME `EventTarget` handler store the kit uses
//! (`core/events/event_target.rs`'s `et_on_get` / `et_on_set`) -- assigning a handler still registers
//! a real listener, and reading one off a non-instance is the recorded error.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{
    build_fn, build_fn_data, call_method, make_dom_exc, new_global, str_val, throw_type_error,
};
use crate::core::dom::media_codecs::recorder_supported;
use crate::core::events::event_target::{et_on_get, et_on_set};
use crate::core::media::media_stream::STREAM;
use crate::install::kits::{define_accessor, define_value};
use crate::install::live::method_row;
use crate::install::MemberDef;

const REC: &str = "MediaRecorder";

/// Brave's defaults for a recorder that has not been told otherwise, and the split a single
/// `bitsPerSecond` gets: one tenth to the audio track.
const DEFAULT_VIDEO_BPS: f64 = 2_500_000.0;
const DEFAULT_AUDIO_BPS: f64 = 128_000.0;
const AUDIO_SHARE: f64 = 10.0;

const BITRATE_MODES: &[&str] = &["constant", "variable"];

// ---- the state machine ---------------------------------------------------------------------------

/// Fire `type` at `target` from a MICROTASK, the way the deleted JS did: `start()` returns before the
/// event runs, so a handler assigned on the line after `start()` still sees it.
fn fire(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, ty: &str) {
    let t = str_val(scope, ty);
    let data = st::array(scope, &[target.into(), t]);
    let f = build_fn_data(scope, cb_fire_now, "", 0, data.into());
    let global = scope.get_current_context().global(scope);
    let _ = call_method(scope, global.into(), "queueMicrotask", &[f.into()]);
}

fn cb_fire_now(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(target) = data.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(ty) = data.get_index(scope, 1) else { return };
    let Some(ev) = new_global(scope, "Event", &[ty]) else { return };
    let _ = crate::binding::protos::engine_call(scope, target.into(), "dispatchEvent", &[ev.into()]);
}

/// A dictionary member that is an `unsigned long` with a default: WebIDL reads `undefined` as absent
/// and converts anything else, so `{ videoBitsPerSecond: null }` really is 0 and not the default.
fn bps(scope: &mut v8::PinScope, o: v8::Local<v8::Value>, name: &str, dflt: f64) -> f64 {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(o) else { return dflt };
    let k = str_val(scope, name);
    match obj.get(scope, k) {
        Some(v) if !v.is_undefined() => st::to_ulong(scope, v) as f64,
        _ => dflt,
    }
}

/// `new MediaRecorder(stream, options)`.
pub(crate) fn cb_recorder_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("MediaRecorder");
        throw_type_error(scope, &msg);
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'MediaRecorder': 1 argument required, but only 0 present.");
        return;
    }
    let stream = args.get(0);
    if !st::is_a(scope, stream, STREAM) {
        throw_type_error(scope, "Failed to construct 'MediaRecorder': parameter 1 is not of type 'MediaStream'.");
        return;
    }
    // `MediaRecorderOptions`, converted as WebIDL converts it (Chrome 150): its members in
    // lexicographic order, each read once and converted as read (korvus read `mimeType` first, five of the seven)
    let Ok(converted) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "MediaRecorderOptions", "Failed to construct 'MediaRecorder': ") else { return };
    let opts: v8::Local<v8::Value> = converted.into();

    let mime = match st::dict_get(scope, opts, "mimeType") {
        Some(v) => match st::to_str(scope, v) {
            Some(s) => s,
            None => return,
        },
        None => String::new(),
    };
    if !mime.is_empty() && !recorder_supported(scope, &mime) {
        let msg = format!(
            "Failed to construct 'MediaRecorder': Failed to initialize native MediaRecorder the type provided ({mime}) is not supported."
        );
        if let Some(e) = make_dom_exc(scope, &msg, "NotSupportedError") {
            scope.throw_exception(e);
        }
        return;
    }
    let mode = match st::dict_get(scope, opts, "audioBitrateMode") {
        Some(v) => match st::to_str(scope, v) {
            Some(s) => s,
            None => return,
        },
        None => "variable".to_string(),
    };
    if !BITRATE_MODES.contains(&mode.as_str()) {
        throw_type_error(
            scope,
            &format!(
                "Failed to construct 'MediaRecorder': Failed to read the 'audioBitrateMode' property from 'MediaRecorderOptions': The provided value '{mode}' is not a valid enum value of type BitrateMode."
            ),
        );
        return;
    }
    let mut video = bps(scope, opts, "videoBitsPerSecond", DEFAULT_VIDEO_BPS);
    let mut audio = bps(scope, opts, "audioBitsPerSecond", DEFAULT_AUDIO_BPS);
    if let Ok(obj) = v8::Local::<v8::Object>::try_from(opts) {
        let k = str_val(scope, "bitsPerSecond");
        if let Some(v) = obj.get(scope, k) {
            if !v.is_undefined() {
                let total = st::to_ulong(scope, v) as f64;
                audio = (total / AUDIO_SHARE).floor();
                video = total - audio;
            }
        }
    }

    let s = st::attach(scope, args.this(), REC);
    st::set(scope, s, "stream", stream);
    st::set_str(scope, s, "mimeType", &mime);
    st::set_str(scope, s, "state", "inactive");
    st::set_num(scope, s, "videoBitsPerSecond", video);
    st::set_num(scope, s, "audioBitsPerSecond", audio);
    st::set_str(scope, s, "audioBitrateMode", &mode);
}

fn cb_rec_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REC) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

fn cb_rec_on_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), REC).is_none() {
        return;
    }
    let ty = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this: v8::Local<v8::Value> = args.this().into();
    let v = et_on_get(scope, this, &ty);
    rv.set(v);
}

fn cb_rec_on_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), REC).is_none() {
        return;
    }
    let ty = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this: v8::Local<v8::Value> = args.this().into();
    let v = args.get(0);
    et_on_set(scope, this, &ty, v);
}

fn throw_state(scope: &mut v8::PinScope, op: &str, state: &str) {
    let msg = format!("Failed to execute '{op}' on 'MediaRecorder': The MediaRecorder's state is '{state}'.");
    if let Some(e) = make_dom_exc(scope, &msg, "InvalidStateError") {
        scope.throw_exception(e);
    }
}

/// `start(timeslice)`: a recorder that is already running refuses, and a stream with no tracks has
/// nothing to record -- the algorithm's own `NotSupportedError`, not a device-absence workaround.
fn cb_rec_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REC) else { return };
    let state = st::get_str(scope, s, "state");
    if state != "inactive" {
        throw_state(scope, "start", &state);
        return;
    }
    // `timeslice` says how often to emit a `dataavailable`; there is nothing to emit, so it is read
    // for its WebIDL conversion and otherwise has nothing to act on.
    let ts = args.get(0);
    if !ts.is_undefined() {
        st::to_ulong(scope, ts);
    }
    let stream = st::get(scope, s, "stream");
    let empty = match st::state(scope, stream, STREAM) {
        Some(ss) => st::get_array(scope, ss, "tracks").map(|a| a.length() == 0).unwrap_or(true),
        None => true,
    };
    if empty {
        if let Some(e) = make_dom_exc(scope, "Failed to execute 'start' on 'MediaRecorder': The MediaStream has no tracks.", "NotSupportedError") {
            scope.throw_exception(e);
        }
        return;
    }
    st::set_str(scope, s, "state", "recording");
    fire(scope, args.this(), "start");
}

/// `stop()`: stopping an INACTIVE recorder throws nothing and leaves it inactive -- recorded. The
/// `dataavailable` that belongs here needs an encoded Blob; see the header.
fn cb_rec_stop(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REC) else { return };
    if st::get_str(scope, s, "state") == "inactive" {
        return;
    }
    st::set_str(scope, s, "state", "inactive");
    fire(scope, args.this(), "stop");
}

/// `pause()` / `resume()`: `data` carries the member name, the state it refuses and the state it moves
/// to; both are a no-op when the recorder is already in the target state.
fn cb_rec_pause_resume(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REC) else { return };
    let op = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let state = st::get_str(scope, s, "state");
    if state == "inactive" {
        throw_state(scope, &op, &state);
        return;
    }
    let want = if op == "pause" { "paused" } else { "recording" };
    if state == want {
        return;
    }
    st::set_str(scope, s, "state", want);
    fire(scope, args.this(), &op);
}

/// `requestData()`: nothing to hand over -- see the header on `dataavailable`. The state check is the
/// observable part and it is real.
fn cb_rec_request_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REC) else { return };
    let state = st::get_str(scope, s, "state");
    if state == "inactive" {
        throw_state(scope, "requestData", &state);
    }
}

/// `MediaRecorder.isTypeSupported(type)`: the SAME grammar `MediaSource.isTypeSupported` and
/// `canPlayType` read, in `core/dom/media_codecs.rs`, over the ACTIVE DEVICE's recorded grid.
///
/// It is the worn browser's claim, not this build's capability: there is no muxer and no encoder
/// here, and a `MediaRecorder` that is constructed still records nothing (ruling R7). What the
/// answer must match is what the captured browser answered -- which is not "false for everything",
/// and never was: Brave 151 answers true for 60 spellings of `video/webm` alone.
fn cb_rec_is_type_supported(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = st::to_str(scope, args.get(0)) else { return };
    rv.set_bool(recorder_supported(scope, &t));
}

fn acc_field(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, m: &'static MemberDef) {
    let d = str_val(scope, m.name);
    let g = build_fn_data(scope, cb_rec_field, &format!("get {}", m.name), 0, d);
    define_accessor(scope, proto, m, g, None);
}

fn acc_handler(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, m: &'static MemberDef) {
    let ty = m.name.strip_prefix("on").unwrap_or(m.name);
    let d = str_val(scope, ty);
    let g = build_fn_data(scope, cb_rec_on_get, &format!("get {}", m.name), 0, d);
    let d = str_val(scope, ty);
    let s = build_fn_data(scope, cb_rec_on_set, &format!("set {}", m.name), 1, d);
    define_accessor(scope, proto, m, g, Some(s));
}

fn method_data(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, m: &'static MemberDef, data: &str) {
    let d = str_val(scope, data);
    let f = build_fn_data(scope, cb_rec_pause_resume, m.name, m.length as i32, d);
    define_value(scope, proto, m, f.into(), true);
}

pub fn install_media_recorder(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "stream" | "mimeType" | "state" | "videoBitsPerSecond" | "audioBitsPerSecond" | "audioBitrateMode" => {
            acc_field(scope, target, member)
        }
        "onstart" | "onstop" | "ondataavailable" | "onpause" | "onresume" | "onerror" => acc_handler(scope, target, member),
        "pause" | "resume" => method_data(scope, target, member, member.name),
        "requestData" => method_row(scope, target, member, cb_rec_request_data),
        "start" => method_row(scope, target, member, cb_rec_start),
        "stop" => method_row(scope, target, member, cb_rec_stop),
        // A STATIC row: `target` is the interface object, not the prototype.
        "isTypeSupported" => {
            let f = build_fn(scope, cb_rec_is_type_supported, member.name, member.length as i32);
            let v: v8::Local<v8::Value> = if member.required > 0 {
                crate::binding::protos::make_arity_guard(
                    scope,
                    "MediaRecorder",
                    member.name,
                    member.required as i32,
                    member.length as i32,
                    f.into(),
                    member.ty,
                )
                .into()
            } else {
                f.into()
            };
            define_value(scope, target, member, v, true);
        }
        other => debug_assert!(false, "install_media_recorder: unexpected member {other}"),
    }
}

// The six `on*` rows are `event_handler` in the capture's shape and a BRANDED pair here; the table
// spells them `handler` so their descriptor, position and nullability come from the same row the
// registry had.
webapi::interface! {
    "MediaRecorder" : "EventTarget" [value, window] {
    ctor          "MediaRecorder" len 1                        = stub,
    attr          "stream" : Object readonly                   = install_media_recorder,
    attr          "mimeType" : Object readonly                 = install_media_recorder,
    attr          "state" : Object readonly                    = install_media_recorder,
    handler       "onstart" nullable                           = install_media_recorder,
    handler       "onstop" nullable                            = install_media_recorder,
    handler       "ondataavailable" nullable                   = install_media_recorder,
    handler       "onpause" nullable                           = install_media_recorder,
    handler       "onresume" nullable                          = install_media_recorder,
    handler       "onerror" nullable                           = install_media_recorder,
    attr          "videoBitsPerSecond" : Object readonly       = install_media_recorder,
    attr          "audioBitsPerSecond" : Object readonly       = install_media_recorder,
    attr          "audioBitrateMode" : Object readonly         = install_media_recorder,
    method        "pause" : Object                             = install_media_recorder,
    method        "requestData" : Object                       = install_media_recorder,
    method        "resume" : Object                            = install_media_recorder,
    method        "start" : Object                             = install_media_recorder,
    method        "stop" : Object                              = install_media_recorder,
    static_method "isTypeSupported" : Object len 1 required 1  = install_media_recorder
    }
}
