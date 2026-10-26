//! `core/media/media_stream.rs` -- `MediaStream`, `MediaStreamTrack` and
//! `CanvasCaptureMediaStreamTrack` as three `interface!` tables.
//!
//! Rows are VERBATIM from the three blocks of `install/generated_registry.rs` they replace: names,
//! order, kinds, lengths, `required` counts, nullability and descriptor flags.
//!
//! WHAT IS REAL HERE, and why device absence is not a workaround. The capture was taken on a machine
//! with no camera and no microphone, so there is no `getUserMedia` track to answer for -- but there is
//! exactly one track source that needs no device at all, and the recording used it:
//! `canvas.captureStream()`. A canvas capture track is genuinely `live`, `enabled`, unmuted, of kind
//! `"video"`, with the canvas's own width, height and aspect ratio in its settings, and the bundle's
//! oracle reads every one of those off a real one. None of that is invented.
//!
//! `HTMLCanvasElement.captureStream` IS THIS LANE'S BODY, on an interface this lane does not own.
//! `HTMLCanvasElement` is still a generated-registry shell, so its `captureStream` row carries the
//! capture's descriptor and the D2 stub; the body is [`cb_canvas_capture_stream`] here, in Rust,
//! named by that row -- the mount moved into the row, which deleted the
//! `install_canvas_capture_stream` pass. The same arrangement `core/html/html_canvas_element.rs`
//! already uses for `getContext`, `toDataURL` and `toBlob`, which are registry rows with Rust bodies
//! mounted over them. It has to live with the track it mints, because minting a track means minting
//! its state, and that state is this file's.
//!
//! WHAT IS DELIBERATELY NOT IMPLEMENTED:
//!
//!   * `CanvasCaptureMediaStreamTrack.requestFrame()` asks the source to deliver its next frame.
//!     Nothing here consumes frames, so nothing is presented -- the call is real and the frame never
//!     arrives, the same shape as `HTMLVideoElement.requestVideoFrameCallback`.
//!   * `MediaStreamTrack.getCaptureHandle()` and `oncapturehandlechange` belong to the Capture Handle
//!     API, which needs a captured TAB. There is none, so both stay `stub` rows -- and because the
//!     `getCaptureHandle` row is nullable, the stub's answer is `null`, which is what the capture
//!     recorded off a canvas track.
//!   * `MediaStreamTrack.stats` is Chrome's per-frame delivery counter. A track that has delivered no
//!     frame has none, and `null` is the recorded answer.
//!
//! INSTANCE STATE is a v8 private slot (`binding::instance_state`), not a `WeakMap` and not an own
//! property: `Object.getOwnPropertyNames(track)` is `[]`, and the slot is also the brand that makes a
//! foreign receiver answer `Illegal invocation`.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{
    build_fn_data, call_method, get_prop, make_dom_exc, rejected_promise, resolved_promise, shell_proto,
    str_val, throw_type_error,
};
use crate::install::live::method_row;
use crate::install::MemberDef;
use crate::install::kits::str_data_getter as acc_get;
use crate::install::kits::str_data_accessor as acc_get_set;

pub(crate) const TRACK: &str = "MediaStreamTrack";
pub(crate) const STREAM: &str = "MediaStream";

/// A video track's content-hint vocabulary, and an audio track's. An unrecognised hint is IGNORED
/// (not an error), which is why assigning `"x"` reads back `""` -- a recorded case.
const VIDEO_HINTS: &[&str] = &["", "motion", "detail", "text"];
const AUDIO_HINTS: &[&str] = &["", "speech", "speech-recognition", "music"];

// ---- minting ------------------------------------------------------------------------------------

/// One fresh object on `iface`'s prototype, with no own property -- the shape `Object.create(P)` had
/// in the JS this replaces.
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    Some(o)
}

/// A track on a given prototype. `iface` chooses the prototype (a canvas capture track wears
/// `CanvasCaptureMediaStreamTrack.prototype`); the BRAND is always `MediaStreamTrack`, because every
/// row that reads a track reads it through the base interface.
pub(crate) fn mint_track<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    kind: &str,
    muted: bool,
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, iface)?;
    let s = st::attach(scope, o, TRACK);
    let id = st::uuid();
    st::set_str(scope, s, "kind", kind);
    st::set_str(scope, s, "id", &id);
    st::set_str(scope, s, "label", "");
    st::set_bool(scope, s, "enabled", true);
    st::set_bool(scope, s, "muted", muted);
    st::set_str(scope, s, "readyState", "live");
    st::set_str(scope, s, "contentHint", "");
    let c = v8::Object::new(scope);
    st::set(scope, s, "constraints", c.into());
    let g = v8::Object::new(scope);
    st::set(scope, s, "settings", g.into());
    st::set_null(scope, s, "canvas");
    Some(o)
}

/// A stream over the tracks given.
pub(crate) fn mint_stream<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    tracks: &[v8::Local<v8::Value>],
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, STREAM)?;
    let s = st::attach(scope, o, STREAM);
    let id = st::uuid();
    st::set_str(scope, s, "id", &id);
    let arr = st::array(scope, tracks);
    st::set(scope, s, "tracks", arr.into());
    Some(o)
}

/// A `MediaStream` of the synthetic tracks a media-source plugin described (ABI 3, fake-media / D-media). Each
/// `descriptors_json` entry is `{"id":..,"kind":"video"|"audio","label":..,"settings":{..}}`. Every track is a
/// plain `MediaStreamTrack` (NOT a `CanvasCaptureMediaStreamTrack`), `live` and enabled, carrying the descriptor's
/// id, label and `getSettings()`. `None` on malformed JSON or no usable track, so `getUserMedia` then rejects as
/// device-absent exactly as with no plugin (I9/J2). Mirrors [`mint_track`]'s field set so the track reads
/// identically; it is minted directly here only to carry the fed id and settings.
pub(crate) fn mint_fed_stream<'s>(scope: &mut v8::PinScope<'s, '_>, descriptors_json: &str) -> Option<v8::Local<'s, v8::Object>> {
    let parsed: serde_json::Value = serde_json::from_str(descriptors_json).ok()?;
    let arr = parsed.as_array()?;
    let mut tracks: Vec<v8::Local<v8::Value>> = Vec::new();
    for d in arr {
        let kind = d.get("kind").and_then(|v| v.as_str()).unwrap_or("video");
        if kind != "video" && kind != "audio" {
            continue;
        }
        let Some(o) = mint(scope, TRACK) else { continue };
        let s = st::attach(scope, o, TRACK);
        let id = d.get("id").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(st::uuid);
        let label = d.get("label").and_then(|v| v.as_str()).unwrap_or("");
        st::set_str(scope, s, "kind", kind);
        st::set_str(scope, s, "id", &id);
        st::set_str(scope, s, "label", label);
        st::set_bool(scope, s, "enabled", true);
        st::set_bool(scope, s, "muted", false);
        st::set_str(scope, s, "readyState", "live");
        st::set_str(scope, s, "contentHint", "");
        let c = v8::Object::new(scope);
        st::set(scope, s, "constraints", c.into());
        // The descriptor's `settings` object verbatim (width/height/frameRate/...), round-tripped through JSON; an
        // empty object when absent. `getSettings()` returns a copy of this (see `cb_track_get_settings`).
        let settings = d
            .get("settings")
            .and_then(|v| serde_json::to_string(v).ok())
            .and_then(|j| v8::String::new(scope, &j))
            .and_then(|j| v8::json::parse(scope, j))
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
            .unwrap_or_else(|| v8::Object::new(scope));
        st::set(scope, s, "settings", settings.into());
        st::set_null(scope, s, "canvas");
        tracks.push(o.into());
    }
    if tracks.is_empty() {
        return None;
    }
    mint_stream(scope, &tracks)
}

/// ABI 3 (fake-media SESSION 2): the latest fed RGBA frame for a `<video>` node, if its `srcObject` is a
/// `MediaStream` whose first LIVE video track has a frame in the plugin frame store. `None` for a node with no
/// `MediaStream` srcObject, no live video track, or no fed frame -- so the paint path draws nothing then (I9/J2).
/// Re-read from v8 on every paint, so a stopped track (`readyState` != "live") or a cleared `srcObject` stops
/// painting at once, regardless of what bytes the store still holds.
pub(crate) fn fed_video_frame_for<'s>(scope: &mut v8::PinScope<'s, '_>, node: u32) -> Option<plugin_host::Frame> {
    let el = v8::Local::<v8::Object>::try_from(crate::page::node_object(scope, node)).ok()?;
    let stream = crate::core::html::html_media_element::src_object(scope, el)?;
    let s = st::state(scope, stream, STREAM)?;
    let tracks = v8::Local::<v8::Array>::try_from(st::get(scope, s, "tracks")).ok()?;
    for i in 0..tracks.length() {
        let Some(t) = tracks.get_index(scope, i) else { continue };
        let Some(ts) = st::state(scope, t, TRACK) else { continue };
        if st::get_str(scope, ts, "kind") == "video" && st::get_str(scope, ts, "readyState") == "live" {
            if let Some(f) = plugin_host::fed_video_frame(&st::get_str(scope, ts, "id")) {
                return Some(f);
            }
        }
    }
    None
}

/// The dimensions of `node`'s fed frame -- [`fed_video_frame_for`] without the RGBA copy, for the `<video>` state
/// getters (`videoWidth`/`videoHeight`/`readyState`), which are polled and need only the size. `None` in exactly
/// the same cases (no `MediaStream` srcObject, no live video track, no fed frame), re-read from v8 every call.
pub(crate) fn fed_video_dims_for<'s>(scope: &mut v8::PinScope<'s, '_>, node: u32) -> Option<(u32, u32)> {
    let el = v8::Local::<v8::Object>::try_from(crate::page::node_object(scope, node)).ok()?;
    let stream = crate::core::html::html_media_element::src_object(scope, el)?;
    let s = st::state(scope, stream, STREAM)?;
    let tracks = v8::Local::<v8::Array>::try_from(st::get(scope, s, "tracks")).ok()?;
    for i in 0..tracks.length() {
        let Some(t) = tracks.get_index(scope, i) else { continue };
        let Some(ts) = st::state(scope, t, TRACK) else { continue };
        if st::get_str(scope, ts, "kind") == "video" && st::get_str(scope, ts, "readyState") == "live" {
            if let Some(d) = plugin_host::fed_video_dims(&st::get_str(scope, ts, "id")) {
                return Some(d);
            }
        }
    }
    None
}

/// Shallow-copy every own enumerable property of `from` into a fresh object -- what `Object.assign({},
/// x)` did in the JS this replaces.
fn copy_obj<'s>(scope: &mut v8::PinScope<'s, '_>, from: v8::Local<v8::Value>) -> v8::Local<'s, v8::Object> {
    let out = v8::Object::new(scope);
    let Ok(src) = v8::Local::<v8::Object>::try_from(from) else { return out };
    let Some(names) = src.get_own_property_names(scope, v8::GetPropertyNamesArgs::default()) else { return out };
    for i in 0..names.length() {
        let Some(k) = names.get_index(scope, i) else { continue };
        if let Some(v) = src.get(scope, k) {
            out.set(scope, k, v);
        }
    }
    out
}

// ---- MediaStreamTrack ---------------------------------------------------------------------------

/// The readonly string/bool fields, read by name off the state object.
fn cb_track_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// `stats`: Chrome's per-frame delivery counter. A track that has delivered no frame has none.
fn cb_track_null(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), TRACK).is_none() {
        return;
    }
    let n = v8::null(scope);
    rv.set(n.into());
}

/// `enabled = v`: WebIDL `boolean`, so ToBoolean and never an error.
fn cb_track_enabled_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let b = args.get(0).boolean_value(scope);
    st::set_bool(scope, s, "enabled", b);
}

/// `contentHint = v`: an unrecognised hint is ignored, which is the spec's own rule and a recorded
/// case (`t.contentHint = "x"` reads back `""`).
fn cb_track_hint_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let Some(h) = st::to_str(scope, args.get(0)) else { return };
    let kind = st::get_str(scope, s, "kind");
    let ok = if kind == "audio" { AUDIO_HINTS } else { VIDEO_HINTS };
    if ok.contains(&h.as_str()) {
        st::set_str(scope, s, "contentHint", &h);
    }
}

/// `stop()`: the track ends. A track that has already ended stays ended.
fn cb_track_stop(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    st::set_str(scope, s, "readyState", "ended");
    // Drop any fed frame for this track (ABI 3, fake-media): a stopped track must stop painting and the frame
    // store must not grow. The paint path's liveness check already refuses an "ended" track; this reclaims memory.
    plugin_host::clear_video_frame(&st::get_str(scope, s, "id"));
}

/// `getCapabilities()`: the source's capabilities, a fresh dictionary each call (its lists and ranges too). A
/// canvas capture track is not constrainable -- there is no device behind it to ask -- so its set is empty,
/// which is an answer and not a placeholder; a WebAudio destination's source has the node's own
/// (`core/html/webaudio.rs`, `destination_stream`).
fn cb_track_capabilities(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let caps = st::get(scope, s, "capabilities");
    if caps.is_object() {
        if let Some(copy) = v8::json::stringify(scope, caps).and_then(|j| v8::json::parse(scope, j)) {
            rv.set(copy);
            return;
        }
    }
    let o = v8::Object::new(scope);
    rv.set(o.into());
}

/// `getConstraints()` / `getSettings()`: a COPY, so a page cannot reach into the track's state through
/// the object it is handed.
fn cb_track_copy(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let src = st::get(scope, s, &name);
    let out = copy_obj(scope, src);
    rv.set(out.into());
}

/// `applyConstraints(c)`: nothing here is constrainable, so an EMPTY constraint set is satisfiable and
/// anything naming a constrainable property is not. That is the algorithm's own outcome for a source
/// with no settable capability, not a shortcut around it.
fn cb_track_apply_constraints(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    if st::get_str(scope, s, "readyState") == "ended" {
        let Some(e) = make_dom_exc(
            scope,
            "Failed to execute 'applyConstraints' on 'MediaStreamTrack': Cannot apply constraints to stopped track.",
            "InvalidStateError",
        ) else {
            return;
        };
        rv.set(rejected_promise(scope, e));
        return;
    }
    let mut first_required: Option<String> = None;
    if let Ok(c) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        if let Some(names) = c.get_own_property_names(scope, v8::GetPropertyNamesArgs::default()) {
            for i in 0..names.length() {
                let Some(k) = names.get_index(scope, i) else { continue };
                let key = k.to_rust_string_lossy(scope);
                if key != "advanced" {
                    first_required = Some(key);
                    break;
                }
            }
        }
    }
    if let Some(k) = first_required {
        let Some(e) = make_dom_exc(
            scope,
            &format!("Failed to execute 'applyConstraints' on 'MediaStreamTrack': Cannot satisfy constraint '{k}'."),
            "OverconstrainedError",
        ) else {
            return;
        };
        rv.set(rejected_promise(scope, e));
        return;
    }
    let empty = v8::Object::new(scope);
    st::set(scope, s, "constraints", empty.into());
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

/// `clone()`: an independent track over the SAME source, with its own id. It keeps the original's
/// prototype, so cloning a canvas capture track yields a `CanvasCaptureMediaStreamTrack` -- a recorded
/// case.
fn cb_track_clone(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(s) = st::state_or_throw(scope, this.into(), TRACK) else { return };
    // from the template of the interface `this` is an instance of
    let o = match this.get_prototype(scope) {
        Some(proto) => crate::binding::protos::mint_on(scope, proto),
        None => v8::Object::new(scope),
    };
    let ns = st::attach(scope, o, TRACK);
    // the clone shares the source, and with it the source's capabilities
    for k in ["kind", "label", "enabled", "muted", "readyState", "contentHint", "canvas", "capabilities"] {
        let v = st::get(scope, s, k);
        st::set(scope, ns, k, v);
    }
    for k in ["constraints", "settings"] {
        let v = st::get(scope, s, k);
        let c = copy_obj(scope, v);
        st::set(scope, ns, k, c.into());
    }
    let id = st::uuid();
    st::set_str(scope, ns, "id", &id);
    rv.set(o.into());
}

/// `CanvasCaptureMediaStreamTrack.canvas`: the canvas the track captures. Only a canvas capture track
/// has one; every other track's state says null, which is what this getter then answers.
fn cb_track_canvas(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let v = st::get(scope, s, "canvas");
    if v.is_undefined() {
        let n = v8::null(scope);
        rv.set(n.into());
    } else {
        rv.set(v);
    }
}



pub fn install_media_stream_track(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "kind" | "id" | "label" | "muted" | "readyState" => acc_get(scope, proto, member, cb_track_field, member.name),
        "enabled" => acc_get_set(scope, proto, member, cb_track_field, "enabled", cb_track_enabled_set, "enabled"),
        "contentHint" => acc_get_set(scope, proto, member, cb_track_field, "contentHint", cb_track_hint_set, "contentHint"),
        "stats" => acc_get(scope, proto, member, cb_track_null, "stats"),
        "applyConstraints" => method_row(scope, proto, member, cb_track_apply_constraints),
        "clone" => method_row(scope, proto, member, cb_track_clone),
        "getCapabilities" => method_row(scope, proto, member, cb_track_capabilities),
        "getConstraints" => {
            let d = str_val(scope, "constraints");
            let f = build_fn_data(scope, cb_track_copy, member.name, member.length as i32, d);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        "getSettings" => {
            let d = str_val(scope, "settings");
            let f = build_fn_data(scope, cb_track_copy, member.name, member.length as i32, d);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        "stop" => method_row(scope, proto, member, cb_track_stop),
        other => debug_assert!(false, "install_media_stream_track: unexpected member {other}"),
    }
}

/// `requestFrame()`: ask the source to deliver its next frame to the track.
///
/// Its whole observable effect from script is that it returns `undefined` -- WebIDL declares it
/// `undefined requestFrame()` -- and whether a frame is then presented is invisible to the page.
/// Nothing here consumes frames, so nothing is presented, which is the same shape as
/// `HTMLVideoElement.requestVideoFrameCallback` in `core/html/html_media_element.rs`: the call is real
/// and the frame never arrives. It is a BODY rather than the D2 stub because the stub answers `null`
/// for an `Object`-typed row and the capture records `undefined` here -- measured 2026-09-11, and the
/// one member of `CanvasCaptureMediaStreamTrack` that was not `verified` before this lane.
fn cb_track_request_frame(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    st::state_or_throw(scope, args.this().into(), TRACK);
}

pub fn install_canvas_capture_track(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "canvas" => acc_get(scope, proto, member, cb_track_canvas, "canvas"),
        "requestFrame" => method_row(scope, proto, member, cb_track_request_frame),
        other => debug_assert!(false, "install_canvas_capture_track: unexpected member {other}"),
    }
}

// ---- MediaStream --------------------------------------------------------------------------------

fn track_state<'s>(scope: &mut v8::PinScope<'s, '_>, t: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    st::state(scope, t, TRACK)
}

fn stream_tracks<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    match st::get_array(scope, s, "tracks") {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    }
}

fn cb_stream_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), STREAM) else { return };
    let v = st::get(scope, s, "id");
    rv.set(v);
}

/// `active`: a stream is active while at least one of its tracks has not ended. An empty stream is
/// inactive, which is what `new MediaStream().active` records.
fn cb_stream_active(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), STREAM) else { return };
    let tracks = stream_tracks(scope, s);
    let mut live = false;
    for t in tracks {
        if let Some(ts) = track_state(scope, t) {
            if st::get_str(scope, ts, "readyState") == "live" {
                live = true;
                break;
            }
        }
    }
    rv.set_bool(live);
}

/// `getTracks()` / `getAudioTracks()` / `getVideoTracks()`: a fresh array each time, so a page cannot
/// mutate the stream through the list it is handed. `data` is the kind filter, empty for all.
fn cb_stream_tracks(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), STREAM) else { return };
    let want = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let all = stream_tracks(scope, s);
    let mut out = Vec::new();
    for t in all {
        if want.is_empty() {
            out.push(t);
        } else if let Some(ts) = track_state(scope, t) {
            if st::get_str(scope, ts, "kind") == want {
                out.push(t);
            }
        }
    }
    let arr = st::array(scope, &out);
    rv.set(arr.into());
}

fn cb_stream_get_track_by_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), STREAM) else { return };
    let Some(want) = st::to_str(scope, args.get(0)) else { return };
    for t in stream_tracks(scope, s) {
        if let Some(ts) = track_state(scope, t) {
            if st::get_str(scope, ts, "id") == want {
                rv.set(t);
                return;
            }
        }
    }
    let n = v8::null(scope);
    rv.set(n.into());
}

/// `addTrack(t)` / `removeTrack(t)`. `data` says which; both take a real `MediaStreamTrack` and
/// nothing else, and adding a track the stream already has is a no-op (the spec's own set semantics).
fn cb_stream_edit_tracks(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), STREAM) else { return };
    let op = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let t = args.get(0);
    if !st::is_a(scope, t, TRACK) {
        throw_type_error(
            scope,
            &format!("Failed to execute '{op}' on 'MediaStream': parameter 1 is not of type 'MediaStreamTrack'."),
        );
        return;
    }
    let cur = stream_tracks(scope, s);
    let mut next: Vec<v8::Local<v8::Value>> = Vec::with_capacity(cur.len() + 1);
    let mut found = false;
    for x in cur {
        if x == t {
            found = true;
            if op == "removeTrack" {
                continue;
            }
        }
        next.push(x);
    }
    if op == "addTrack" && !found {
        next.push(t);
    }
    let arr = st::array(scope, &next);
    st::set(scope, s, "tracks", arr.into());
}

/// `clone()`: a new stream with a new id over CLONES of this stream's tracks, which is what the spec
/// says and what `MediaStream.clone()` is for.
fn cb_stream_clone(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), STREAM) else { return };
    let mut cloned = Vec::new();
    for t in stream_tracks(scope, s) {
        let Ok(o) = v8::Local::<v8::Object>::try_from(t) else { continue };
        if let Some(c) = call_method(scope, o.into(), "clone", &[]) {
            cloned.push(c);
        }
    }
    let Some(o) = mint_stream(scope, &cloned) else { return };
    rv.set(o.into());
}

/// `new MediaStream()`, `new MediaStream(stream)`, `new MediaStream(sequence<MediaStreamTrack>)`.
pub(crate) fn cb_stream_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("MediaStream");
        throw_type_error(scope, &msg);
        return;
    }
    let this = args.this();
    let arg = args.get(0);
    let mut tracks: Vec<v8::Local<v8::Value>> = Vec::new();
    if !arg.is_null_or_undefined() {
        if let Some(from) = st::state(scope, arg, STREAM) {
            tracks = stream_tracks(scope, from);
        } else {
            // A sequence: anything iterable. `Array.from` runs the SAME iteration protocol the
            // binding layer would, including a user `[Symbol.iterator]`.
            let Some(items) = iterable_items(scope, arg) else {
                throw_type_error(
                    scope,
                    "Failed to construct 'MediaStream': The provided value is not of type '(MediaStream or sequence<MediaStreamTrack>)'.",
                );
                return;
            };
            for t in items {
                if !st::is_a(scope, t, TRACK) {
                    throw_type_error(scope, "Failed to construct 'MediaStream': Invalid Array element type");
                    return;
                }
                tracks.push(t);
            }
        }
    }
    let s = st::attach(scope, this, STREAM);
    let id = st::uuid();
    st::set_str(scope, s, "id", &id);
    let arr = st::array(scope, &tracks);
    st::set(scope, s, "tracks", arr.into());
}

/// WebIDL `sequence<T>`: `v` must have a callable `[Symbol.iterator]`, and the elements are what
/// `Array.from` gets out of it -- the SAME iteration protocol the binding layer runs, including a
/// user-defined iterator. `None` means "not a sequence", which is a different error from "a sequence
/// with a wrong element", so the caller can say which.
pub(crate) fn iterable_items<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    let sym = v8::Symbol::get_iterator(scope);
    if !obj.get(scope, sym.into()).map(|f| f.is_function()).unwrap_or(false) {
        return None;
    }
    array_from(scope, v)
}

/// `Array.from(v)` over anything it accepts -- an iterable OR a plain array-like. This is the
/// conversion `Array.from` itself performs, and the two call sites that want it
/// (`RTCPeerConnection.setConfiguration`'s `iceServers`, `setHeaderExtensionsToNegotiate`) are the
/// two the deleted JS spelled with `Array.from` rather than with a `for...of`.
pub(crate) fn array_from<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    let arr = crate::binding::protos::call_intrinsic(scope, "Array.from", &[v])?;
    let arr = v8::Local::<v8::Array>::try_from(arr).ok()?;
    Some(st::array_items(scope, arr))
}

fn method_data(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: &str,
) {
    use crate::binding::protos::make_arity_guard;
    let d = str_val(scope, data);
    let f = build_fn_data(scope, cb, m.name, m.length as i32, d);
    let v: v8::Local<v8::Value> = if m.required > 0 {
        make_arity_guard(scope, "MediaStream", m.name, m.required as i32, m.length as i32, f.into(), m.ty).into()
    } else {
        f.into()
    };
    crate::install::kits::define_value(scope, proto, m, v, true);
}

pub fn install_media_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "id" => acc_get(scope, proto, member, cb_stream_id, "id"),
        "active" => acc_get(scope, proto, member, cb_stream_active, "active"),
        "getTracks" => method_data(scope, proto, member, cb_stream_tracks, ""),
        "getAudioTracks" => method_data(scope, proto, member, cb_stream_tracks, "audio"),
        "getVideoTracks" => method_data(scope, proto, member, cb_stream_tracks, "video"),
        "getTrackById" => method_row(scope, proto, member, cb_stream_get_track_by_id),
        "addTrack" => method_data(scope, proto, member, cb_stream_edit_tracks, "addTrack"),
        "removeTrack" => method_data(scope, proto, member, cb_stream_edit_tracks, "removeTrack"),
        "clone" => method_row(scope, proto, member, cb_stream_clone),
        other => debug_assert!(false, "install_media_stream: unexpected member {other}"),
    }
}

// ---- HTMLCanvasElement.captureStream -------------------------------------------------------------

/// `canvas.captureStream(frameRate?)`: the one track source on this machine that needs no device.
///
/// The track's settings are the canvas's OWN geometry -- width, height and the aspect ratio to four
/// decimal places -- and `frameRate` appears only when the page asked for one, which is what the
/// recording shows. A negative or non-finite rate is the binding layer's TypeError.
///
/// THE BODY IS THIS LANE'S AND THE ROW IS `HTMLCanvasElement`'S, which is W5.1 L4's split and is
/// unchanged. What changed is who MOUNTS it: `install_canvas_capture_stream` used to
/// define this function over the table's `stub` row from `install_window_media_rtc`, minutes after the
/// installer had mounted that row -- an unbranded `build_fn` in a slot the install ledger claims, so
/// `installer::audit_surface` reported `HTMLCanvasElement::captureStream` as `clobber:engine`. The row
/// names this callback directly now (`core/html/html_canvas_element.rs`'s
/// `install_html_canvas_element`), so the ONE installer mounts it and that pass is deleted.
pub(crate) fn cb_canvas_capture_stream(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let arg = args.get(0);
    let mut fps: Option<f64> = None;
    if !arg.is_null_or_undefined() {
        let n = arg.number_value(scope).unwrap_or(f64::NAN);
        if !n.is_finite() || n < 0.0 {
            throw_type_error(scope, "Failed to execute 'captureStream' on 'HTMLCanvasElement': The frameRate provided is invalid.");
            return;
        }
        fps = Some(n);
    }
    let iface = if crate::binding::protos::iface_object(scope, "CanvasCaptureMediaStreamTrack").map(|v| v.is_function()).unwrap_or(false) {
        "CanvasCaptureMediaStreamTrack"
    } else {
        TRACK
    };
    let Some(t) = mint_track(scope, iface, "video", false) else { return };
    let Some(ts) = st::state(scope, t.into(), TRACK) else { return };
    let w = get_prop(scope, this, "width").and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let h = get_prop(scope, this, "height").and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let w = if w.is_finite() { w } else { 0.0 };
    let h = if h.is_finite() { h } else { 0.0 };
    let settings = v8::Object::new(scope);
    st::set_num(scope, settings, "width", w);
    st::set_num(scope, settings, "height", h);
    let ar = if h != 0.0 { ((w / h) * 10000.0).round() / 10000.0 } else { 0.0 };
    st::set_num(scope, settings, "aspectRatio", ar);
    if let Some(f) = fps {
        st::set_num(scope, settings, "frameRate", f);
    }
    st::set(scope, ts, "settings", settings.into());
    st::set(scope, ts, "canvas", this.into());
    let Some(s) = mint_stream(scope, &[t.into()]) else { return };
    rv.set(s.into());
}

// ---- the tables ---------------------------------------------------------------------------------

webapi::interface! {
    "MediaStreamTrack" : "EventTarget" [interface, window] {
    ctor    "MediaStreamTrack" len 0                     = stub,
    attr    "kind" : Object readonly                     = install_media_stream_track,
    attr    "id" : Object readonly                       = install_media_stream_track,
    attr    "label" : Object readonly                    = install_media_stream_track,
    attr    "enabled" : Object                           = install_media_stream_track,
    attr    "muted" : Object readonly                    = install_media_stream_track,
    handler "onmute" nullable                            = event_handler,
    handler "onunmute" nullable                          = event_handler,
    attr    "readyState" : Object readonly               = install_media_stream_track,
    handler "onended" nullable                           = event_handler,
    attr    "stats" : Object readonly                    = install_media_stream_track,
    attr    "contentHint" : Object                       = install_media_stream_track,
    method  "applyConstraints" : Promise                 = install_media_stream_track,
    method  "clone" : Object                             = install_media_stream_track,
    method  "getCapabilities" : Object                   = install_media_stream_track,
    method  "getConstraints" : Object                    = install_media_stream_track,
    method  "getSettings" : Object                       = install_media_stream_track,
    method  "stop" : Object                              = install_media_stream_track,
    handler "oncapturehandlechange" nullable             = event_handler,
    method  "getCaptureHandle" : Object nullable         = stub
    }
}

webapi::interface! {
    "CanvasCaptureMediaStreamTrack" : "MediaStreamTrack" [interface, window] {
    ctor   "CanvasCaptureMediaStreamTrack" len 0        = stub,
    attr   "canvas" : Object readonly                   = install_canvas_capture_track,
    method "requestFrame" : Object                      = install_canvas_capture_track
    }
}

webapi::interface! {
    "MediaStream" : "EventTarget" [value, window] {
    ctor    "MediaStream" len 0                               = stub,
    attr    "id" : Object readonly                            = install_media_stream,
    attr    "active" : Object readonly                        = install_media_stream,
    handler "onaddtrack" nullable                             = event_handler,
    handler "onremovetrack" nullable                          = event_handler,
    handler "onactive"                                        = event_handler,
    handler "oninactive"                                      = event_handler,
    method  "addTrack" : Object len 1 required 1              = install_media_stream,
    method  "clone" : Object                                  = install_media_stream,
    method  "getAudioTracks" : Object                         = install_media_stream,
    method  "getTrackById" : Object len 1 required 1 nullable = install_media_stream,
    method  "getTracks" : Object                              = install_media_stream,
    method  "getVideoTracks" : Object                         = install_media_stream,
    method  "removeTrack" : Object len 1 required 1           = install_media_stream
    }
}
