//! `core/html/html_media_element.rs` -- `HTMLMediaElement`, `HTMLVideoElement` and
//! `HTMLAudioElement` as three `interface!` tables.
//!
//! `dom/08z-media-element.js` (262 lines, 5 native masks, 7 `Object.defineProperty` sites, 28 `KB.__`
//! bridge slots) and the `HTMLVideoElement` half of `dom/09a-media-parts.js` are gone. Every row below
//! is mounted by THE installer (`install::live::install_window_media`), with names, order, kinds,
//! lengths, `required` counts, nullability and descriptor flags VERBATIM from the three blocks of
//! `install/generated_registry.rs` they replace.
//!
//! Korvus has no media pipeline -- no decoder, no audio output. What it has is the LOAD:
//! `core/html/media_load.rs` runs HTML's load and resource selection algorithms, fetches the resource,
//! reads its container header (`media_probe.rs`) and drives `networkState`, `readyState`, `paused`,
//! `currentSrc`, `duration`, `error`, the frame size and the media events from what it found. The rows
//! below read that state. An element nothing has loaded answers what Blink's member initialisers give
//! a fresh element (`core/html/media/html_media_element.h`):
//!
//! ```text
//! network_state_ = kNetworkEmpty(0)   ready_state_ = kHaveNothing(0)
//! paused_ = true                      seeking_ = false
//! volume_ = 1.0                       muted_ = false
//! playback_rate_ = 1.0                default_playback_rate_ = 1.0
//! duration_ = NaN                     preserves_pitch_ = true
//! ```
//!
//! PER-ELEMENT STATE. `currentTime`, `volume`, `playbackRate`, `defaultPlaybackRate`,
//! `preservesPitch`, `muted`, `srcObject` and `load()`'s resource-selection flag are per-element and
//! were eight `WeakMap`s in the deleted JS. They are V8 PRIVATE SLOTS on the element wrapper here --
//! the same mechanism `core/events/event.rs` uses for event state, invisible to `Object.keys`, to
//! `getOwnPropertyNames` and to a page walking the object.
//!
//! TWO MEMBERS ARE DELIBERATELY NOT IMPLEMENTED, each for a stated reason: `remote` (needs a
//! RemotePlayback with real state) and `captureStream` (needs a MediaStream on a media element that
//! has no resource to capture). `textTracks`, `addTextTrack` and
//! `HTMLVideoElement.getVideoPlaybackQuality` were `dom/09k-media-rtc.js`'s when this lane was
//! written; W5.1 lane L4 moved their bodies into `core/html/text_tracks.rs` and these rows now name
//! them, so nothing reaches in after this install any more.

use deno_core::v8;

use crate::binding::protos::{
    build_fn_data, cb_const_str_get, cb_null_get, make_dom_exc, rejected_promise, resolved_promise, str_val,
    throw_type_error,
};
use crate::core::dom::media_codecs::cb_can_play_type;
use crate::core::html::html_input_element::{acc1, acc2};
// The three rows of this family that were `dom/09k-media-rtc.js`'s.
use crate::core::html::text_tracks::{cb_media_add_text_track, cb_media_text_tracks, cb_video_playback_quality};
use crate::core::html::media_load;
use crate::install::live::method_row;
use crate::install::MemberDef;

// ---- per-element state: private slots, not WeakMaps ---------------------------------------------

fn media_slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_media_{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}
fn slot_read<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let key = media_slot(scope, name);
    obj.get_private(scope, key).filter(|v| !v.is_undefined())
}
fn slot_write(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let key = media_slot(scope, name);
    obj.set_private(scope, key, v);
}

/// The row's `data` string, for the callbacks a whole family shares.
fn data_str(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> String {
    crate::binding::protos::body_data(scope, args).to_rust_string_lossy(scope)
}

// ---- the constant answers -----------------------------------------------------------------------

/// `webkitAudio/VideoDecodedByteCount`, `HTMLVideoElement`'s `webkitDecodedFrameCount` /
/// `webkitDroppedFrameCount` and `HTMLTrackElement.readyState` (NONE): nothing is decoded, presented
/// or dropped, and no text track is fetched.
pub(crate) fn cb_media_zero(_s: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_int32(0);
}
/// `seeking`: a seek here completes in the task that starts it, so no script ever sees one running.
fn cb_media_false(_s: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(false);
}

/// The element's load state (`core/html/media_load.rs`), read with `f`; `dflt` for a
/// receiver that is not an element or an element nothing has loaded.
fn media_state<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, dflt: R, f: impl FnOnce(&media_load::Media) -> R) -> R {
    match media_load::this_id(scope, this) {
        Some(id) => media_load::read(scope, id, f).unwrap_or(dflt),
        None => dflt,
    }
}

/// `networkState`: kNetworkEmpty(0) until the load algorithm runs, then whatever it reached --
/// NO_SOURCE(3) at once (the synchronous half of resource selection), LOADING(2) while the resource is
/// fetched, IDLE(1) once it is all here.
fn cb_media_network_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = media_state(scope, args.this(), 0, |m| m.network);
    rv.set_int32(n as i32);
}

/// `readyState`: HAVE_NOTHING(0) until the resource's header is read, then HAVE_ENOUGH_DATA(4) --
/// the whole resource is here; or, for a `<video>` painting a plugin-fed `MediaStream` frame, HAVE_ENOUGH_DATA(4)
/// so the state is coherent with the frames it shows (fake-media SESSION 2b).
fn cb_media_ready_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = media_state(scope, args.this(), 0, |m| m.ready);
    // fake-media SESSION 2b: a <video> with a plugin-fed frame reports HAVE_ENOUGH_DATA, so a getUserMedia verifier
    // reading readyState===0 no longer detects the synthetic stream. Self-gated PER ELEMENT on a fed frame for THIS
    // element (`has_fed_frames()` is only the cheap early-out); a resource <video> (n != 0) or a srcObject with no
    // fed frame keeps today's value exactly -> byte-for-byte unchanged with no plugin (I9/J2).
    let n = if n == 0
        && plugin_host::has_fed_frames()
        && media_load::this_id(scope, args.this())
            .and_then(|id| crate::core::media::media_stream::fed_video_dims_for(scope, id))
            .is_some()
    {
        media_load::HAVE_ENOUGH_DATA
    } else {
        n
    };
    rv.set_int32(n as i32);
}

/// `paused`: true until `play()` (or autoplay) starts the element, and again once it pauses or ends.
fn cb_media_paused(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = media_state(scope, args.this(), true, |m| m.paused);
    rv.set_bool(p);
}

/// `currentSrc`: "" until the resource selection algorithm picks a resource, then its URL.
fn cb_media_current_src(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = media_state(scope, args.this(), String::new(), |m| m.current_src.clone());
    let v = str_val(scope, &s);
    rv.set(v);
}

/// `duration`: NaN with no resource, the header's length once read (`Infinity` when it states none).
fn cb_media_duration(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = media_state(scope, args.this(), f64::NAN, |m| m.duration);
    let n = v8::Number::new(scope, d);
    rv.set(n.into());
}

/// `ended`: the playback position has reached the end, forwards, and the element does not loop.
fn cb_media_ended(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let at_end = media_state(scope, args.this(), false, media_load::at_end);
    let looping = at_end && {
        let a = str_val(scope, "loop");
        crate::core::dom::element::attr_call(scope, args.this().into(), "hasAttribute", &[a]).map(|v| v.boolean_value(scope)).unwrap_or(false)
    };
    rv.set_bool(at_end && !looping);
}

/// `error`: null until a resource fails, then that failure's `MediaError` (the same object each read).
fn cb_media_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let e = media_load::this_id(scope, args.this()).and_then(|id| media_load::error_object(scope, id));
    match e {
        Some(o) => rv.set(o.into()),
        None => rv.set_null(),
    }
}

/// `videoWidth` / `videoHeight`: the frame size the header states, 0 before it is read or for audio
/// (`data` is `w` or `h`); or, for a `<video>` painting a plugin-fed `MediaStream` frame, that frame's size
/// (fake-media SESSION 2b).
fn cb_video_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let w = data_str(scope, &args) == "w";
    let n = media_state(scope, args.this(), 0, |m| if m.ready >= media_load::HAVE_METADATA { if w { m.size.0 } else { m.size.1 } } else { 0 });
    // fake-media SESSION 2b: report the fed frame's dimensions so a getUserMedia verifier reading videoWidth/Height
    // sees a real size, not 0. Only when the element reports 0 today (so a loaded resource is never touched) and a
    // fed frame exists for THIS element; a bare/resource <video> or a no-fed srcObject is left exactly as above ->
    // byte-for-byte unchanged with no plugin (I9/J2).
    let n = if n == 0 && plugin_host::has_fed_frames() {
        media_load::this_id(scope, args.this())
            .and_then(|id| crate::core::media::media_stream::fed_video_dims_for(scope, id))
            .map(|(fw, fh)| if w { fw } else { fh })
            .unwrap_or(0)
    } else {
        n
    };
    rv.set_uint32(n);
}

/// `load()`: the media element load algorithm.
fn cb_media_load(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some(id) = media_load::this_id(scope, args.this()) {
        media_load::load_algorithm(scope, id);
    }
}

/// `pause()`: the internal pause steps.
fn cb_media_pause(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some(id) = media_load::this_id(scope, args.this()) {
        media_load::pause(scope, id);
    }
}

/// `play()`: the autoplay policy first, then a resource that already failed, then the internal play
/// steps -- whose promise settles when the element starts playing, fails, pauses or reloads.
fn cb_media_play(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = media_load::this_id(scope, args.this()) else {
        let Some(e) = make_dom_exc(scope, "Failed to load because no supported source was found.", "NotSupportedError") else { return };
        let p = rejected_promise(scope, e);
        rv.set(p);
        return;
    };
    if let Some(p) = media_load::play(scope, id) {
        rv.set(p.into());
    }
}

/// `setSinkId(id)`: `""` means "the default device", which is the one Korvus can honestly claim to be
/// on already; any other id names an output device that does not exist here, and the spec's own
/// answer for that is a `NotFoundError` rejection rather than silence.
fn cb_media_set_sink_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.get(0).to_rust_string_lossy(scope).is_empty() {
        let u = v8::undefined(scope);
        let p = resolved_promise(scope, u.into());
        rv.set(p);
        return;
    }
    let Some(e) = make_dom_exc(
        scope,
        "Failed to execute 'setSinkId' on 'HTMLMediaElement': The requested device could not be found.",
        "NotFoundError",
    ) else {
        return;
    };
    let p = rejected_promise(scope, e);
    rv.set(p);
}

/// `setMediaKeys(mk)`: `null` is "detach", which is where this element permanently is; anything else
/// needs a real CDM, and the spec's answer for "no key system" is a `NotSupportedError` rejection.
fn cb_media_set_media_keys(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.get(0).is_null_or_undefined() {
        let u = v8::undefined(scope);
        let p = resolved_promise(scope, u.into());
        rv.set(p);
        return;
    }
    let Some(e) = make_dom_exc(
        scope,
        "Failed to execute 'setMediaKeys' on 'HTMLMediaElement': No supported key system was found.",
        "NotSupportedError",
    ) else {
        return;
    };
    let p = rejected_promise(scope, e);
    rv.set(p);
}

// ---- the settable numbers -----------------------------------------------------------------------

/// `currentTime`, `volume`, `playbackRate` and `defaultPlaybackRate`: settable, and the setter is
/// meaningful even with no resource. With `readyState` kHaveNothing the spec stores `currentTime` as
/// the "default playback start position" rather than seeking, and the getter returns it; the other
/// three simply hold what script gives them, and only the playing they would control is missing.
///
/// `data` is `<member>|<default>`.
fn cb_media_num_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = data_str(scope, &args);
    let (name, dflt) = d.split_once('|').unwrap_or((d.as_str(), "0"));
    // With a resource's header read, `currentTime` is the playback clock.
    if name == "currentTime" {
        let t = media_state(scope, args.this(), None, |m| (m.ready >= media_load::HAVE_METADATA).then(|| media_load::position(m)));
        if let Some(t) = t {
            let n = v8::Number::new(scope, t);
            rv.set(n.into());
            return;
        }
    }
    match slot_read(scope, args.this(), name) {
        Some(v) => rv.set(v),
        None => {
            let n = v8::Number::new(scope, dflt.parse::<f64>().unwrap_or(0.0));
            rv.set(n.into());
        }
    }
}

/// The matching setter. A non-finite value is the WebIDL `double` conversion's own TypeError, and
/// `volume` additionally rejects out of range the way the spec does -- both messages verbatim from
/// the JS this replaces (`data` is `<member>`).
fn cb_media_num_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = data_str(scope, &args);
    let n = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    if !n.is_finite() {
        throw_type_error(
            scope,
            &format!("Failed to set the '{name}' property on 'HTMLMediaElement': The provided double value is non-finite."),
        );
        return;
    }
    if name == "volume" && !(0.0..=1.0).contains(&n) {
        let msg = format!(
            "Failed to set the 'volume' property on 'HTMLMediaElement': The volume provided ({}) is outside the range [0, 1].",
            fmt_num(n)
        );
        if let Some(e) = make_dom_exc(scope, &msg, "IndexSizeError") {
            scope.throw_exception(e);
        }
        return;
    }
    if name == "currentTime" {
        if let Some(id) = media_load::this_id(scope, args.this()) {
            if media_load::seek(scope, id, n) {
                return;
            }
        }
    }
    let v = v8::Number::new(scope, n);
    slot_write(scope, args.this(), &name, v.into());
}

/// JS `String(n)` for the finite doubles this message can carry -- `2` not `2.0`, `1.5` as written.
fn fmt_num(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

// ---- the settable booleans ----------------------------------------------------------------------

/// `preservesPitch`: `preserves_pitch_ = true` until script says otherwise (`data` is `<member>`).
fn cb_media_bool_true_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = data_str(scope, &args);
    match slot_read(scope, args.this(), &name) {
        Some(v) => rv.set_bool(v.boolean_value(scope)),
        None => rv.set_bool(true),
    }
}

/// `muted` is NOT the `muted` content attribute: that attribute is `defaultMuted`, and the IDL
/// `muted` is per-element state initialised from it (`muted_ = false`, `muted_is_default_ = true`),
/// which is why an unset slot reads the attribute rather than `false`.
fn cb_media_muted_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let m = is_muted(scope, args.this());
    rv.set_bool(m);
}

/// The IDL `muted` of `el` -- what the autoplay policy asks (`core/html/media_load.rs`).
pub(crate) fn is_muted(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    if let Some(v) = slot_read(scope, el, "muted") {
        return v.boolean_value(scope);
    }
    let a = str_val(scope, "muted");
    crate::core::dom::element::attr_call(scope, el.into(), "hasAttribute", &[a]).map(|v| v.boolean_value(scope)).unwrap_or(false)
}

/// The object `srcObject` holds, if any -- the resource selection algorithm's "object" mode.
pub(crate) fn src_object<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    slot_read(scope, el, "srcObject").filter(|v| !v.is_null_or_undefined())
}

/// The load algorithm's "set the playbackRate attribute to the value of the defaultPlaybackRate
/// attribute".
pub(crate) fn reset_playback_rate(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) {
    if let Some(v) = slot_read(scope, el, "defaultPlaybackRate") {
        slot_write(scope, el, "playbackRate", v);
    } else {
        let key = media_slot(scope, "playbackRate");
        el.delete_private(scope, key);
    }
}

/// The setter both booleans share: `!!v` into the slot (`data` is `<member>`).
fn cb_media_bool_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = data_str(scope, &args);
    let b = args.get(0).boolean_value(scope);
    let v = v8::Boolean::new(scope, b);
    slot_write(scope, args.this(), &name, v.into());
}

// ---- srcObject ----------------------------------------------------------------------------------

/// `srcObject` holds whatever script assigns. There is nothing here that can PLAY a MediaStream or a
/// MediaSource, but storing and returning the object is the attribute's whole observable contract,
/// and `null` until then is what a browser reports.
fn cb_media_src_object_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match slot_read(scope, args.this(), "srcObject") {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}
/// `attribute MediaProvider? srcObject` -- `(MediaStream or MediaSourceHandle or Blob)?`: anything else
/// is the setter's TypeError (Chrome 150: `video.srcObject = 1` throws).
fn cb_media_src_object_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let a = args.get(0);
    let ok = a.is_null_or_undefined()
        || ["MediaStream", "MediaSourceHandle", "Blob"].iter().any(|i| crate::binding::protos::implements(scope, a, i));
    if !ok {
        crate::binding::protos::throw_type_error(
            scope,
            "Failed to set the 'srcObject' property on 'HTMLMediaElement': The provided value is not of type '(Blob or MediaSourceHandle or MediaStream)'.",
        );
        return;
    }
    let v: v8::Local<v8::Value> = if a.is_null_or_undefined() { v8::null(scope).into() } else { a };
    slot_write(scope, args.this(), "srcObject", v);
}

/// `captureStream()`: a `MediaStream` of the element's playing tracks -- none, since nothing here
/// decodes media (Chrome 150 answers a MediaStream for a sourceless `<video>`, not null).
fn cb_media_capture_stream(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(s) = crate::core::media::media_stream::mint_stream(scope, &[]) {
        rv.set(s.into());
    }
}

// ---- TimeRanges -----------------------------------------------------------------------------

/// `buffered`, `played` and `seekable`: a fresh, always-empty `TimeRanges`. An element that has never
/// loaded a byte has buffered, played and seeked over nothing, so the truthful list is empty -- and
/// the object is a real `TimeRanges` instance (its prototype is the table's), not a duck.
fn cb_media_empty_ranges(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A resource whose header is read is all here: `buffered` and `seekable` are the
    // whole of it, and `played` is what the playback clock has covered (`data` is the member).
    let which = data_str(scope, &args);
    // an attached MediaSource's buffered frames
    if which != "played" {
        if let Some(ranges) = media_load::this_id(scope, args.this()).and_then(|id| media_load::mse_buffered(scope, id, &which)) {
            if let Some(o) = crate::core::html::media_objects::mint_time_ranges_with(scope, &ranges) {
                rv.set(o.into());
            }
            return;
        }
    }
    let range = media_state(scope, args.this(), None, |m| {
        if m.ready < media_load::HAVE_METADATA || !m.duration.is_finite() {
            return None;
        }
        let end = if which == "played" { media_load::played_end(m) } else { m.duration };
        (end > 0.0).then_some((0.0, end))
    });
    let ranges: Vec<(f64, f64)> = range.into_iter().collect();
    let o = crate::core::html::media_objects::mint_time_ranges_with(scope, &ranges);
    match o {
        Some(o) => rv.set(o.into()),
        None => rv.set_null(),
    }
}

// ---- HTMLVideoElement -------------------------------------------------------------------------

/// `requestVideoFrameCallback(cb)` registers a callback against the next PRESENTED frame. No frame is
/// ever presented here, so the handle is real and monotonic and the callback is never invoked --
/// which is also what a browser does for a video that never plays. The arity and type checks are the
/// observable part and they are real.
///
/// The counter is per-context, in a private slot of the element's own global, so two documents do not
/// share a handle sequence.
fn cb_vfc_request(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        throw_type_error(
            scope,
            "Failed to execute 'requestVideoFrameCallback' on 'HTMLVideoElement': 1 argument required, but only 0 present.",
        );
        return;
    }
    if !args.get(0).is_function() {
        throw_type_error(
            scope,
            "Failed to execute 'requestVideoFrameCallback' on 'HTMLVideoElement': parameter 1 is not of type 'Function'.",
        );
        return;
    }
    let global = scope.get_current_context().global(scope);
    let next = slot_read(scope, global, "vfcNext").and_then(|v| v.number_value(scope)).unwrap_or(1.0);
    let bumped = v8::Number::new(scope, next + 1.0);
    slot_write(scope, global, "vfcNext", bumped.into());
    let n = v8::Number::new(scope, next);
    rv.set(n.into());
}

/// `cancelVideoFrameCallback(handle)`: no callback is ever pending, so there is nothing to remove --
/// but the arity check is the observable part, and it is real.
fn cb_vfc_cancel(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.length() < 1 {
        throw_type_error(
            scope,
            "Failed to execute 'cancelVideoFrameCallback' on 'HTMLVideoElement': 1 argument required, but only 0 present.",
        );
    }
}

/// `requestPictureInPicture()`: PiP needs loaded metadata, and this element has none -- which is the
/// spec's own `InvalidStateError` case, not a device-absence workaround.
fn cb_video_request_pip(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(e) = make_dom_exc(
        scope,
        "Failed to execute 'requestPictureInPicture' on 'HTMLVideoElement': Metadata for the video element are not loaded yet.",
        "InvalidStateError",
    ) else {
        return;
    };
    let p = rejected_promise(scope, e);
    rv.set(p);
}

// ---- the row mounts -----------------------------------------------------------------------------

/// A row whose getter is a shared callback carrying `data`.
fn acc_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
    gdata: &str,
    scb: impl v8::MapFnTo<v8::FunctionCallback>,
    sdata: &str,
) {
    let gd = str_val(scope, gdata);
    let sd = str_val(scope, sdata);
    let g = build_fn_data(scope, gcb, &format!("get {}", m.name), 0, gd);
    let s = build_fn_data(scope, scb, &format!("set {}", m.name), 1, sd);
    crate::install::kits::define_accessor(scope, proto, m, g, Some(s));
}

pub fn install_html_media_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // No resource has ever failed to load, because none has ever been fetched.
        // No key system is ever attached.
        "mediaKeys" => acc1(scope, proto, member, cb_null_get, None),
        "error" => acc1(scope, proto, member, cb_media_error, None),
        "currentSrc" => acc1(scope, proto, member, cb_media_current_src, None),
        // No audio output device has been selected, because there is no device list to select from.
        "sinkId" => acc1(scope, proto, member, cb_const_str_get, Some("")),
        "networkState" => acc1(scope, proto, member, cb_media_network_state, None),
        "buffered" | "played" | "seekable" => acc1(scope, proto, member, cb_media_empty_ranges, Some(member.name)),
        "readyState" => acc1(scope, proto, member, cb_media_ready_state, None),
        // Nothing is decoded.
        "webkitAudioDecodedByteCount" | "webkitVideoDecodedByteCount" => acc1(scope, proto, member, cb_media_zero, None),
        "seeking" => acc1(scope, proto, member, cb_media_false, None),
        "ended" => acc1(scope, proto, member, cb_media_ended, None),
        "duration" => acc1(scope, proto, member, cb_media_duration, None),
        "paused" => acc1(scope, proto, member, cb_media_paused, None),
        "currentTime" => acc_data(scope, proto, member, cb_media_num_get, "currentTime|0", cb_media_num_set, "currentTime"),
        "defaultPlaybackRate" => {
            acc_data(scope, proto, member, cb_media_num_get, "defaultPlaybackRate|1", cb_media_num_set, "defaultPlaybackRate")
        }
        "playbackRate" => acc_data(scope, proto, member, cb_media_num_get, "playbackRate|1", cb_media_num_set, "playbackRate"),
        "volume" => acc_data(scope, proto, member, cb_media_num_get, "volume|1", cb_media_num_set, "volume"),
        "preservesPitch" => {
            acc_data(scope, proto, member, cb_media_bool_true_get, "preservesPitch", cb_media_bool_set, "preservesPitch")
        }
        "muted" => acc_data(scope, proto, member, cb_media_muted_get, "muted", cb_media_bool_set, "muted"),
        "srcObject" => acc2(scope, proto, member, cb_media_src_object_get, None, cb_media_src_object_set, None),
        "captureStream" => crate::install::live::method_row(scope, proto, member, cb_media_capture_stream),
        "textTracks" => acc1(scope, proto, member, cb_media_text_tracks, None),
        "addTextTrack" => method_row(scope, proto, member, cb_media_add_text_track),
        "canPlayType" => method_row(scope, proto, member, cb_can_play_type),
        "load" => method_row(scope, proto, member, cb_media_load),
        "pause" => method_row(scope, proto, member, cb_media_pause),
        "play" => method_row(scope, proto, member, cb_media_play),
        "setSinkId" => method_row(scope, proto, member, cb_media_set_sink_id),
        "setMediaKeys" => method_row(scope, proto, member, cb_media_set_media_keys),
        other => debug_assert!(false, "install_html_media_element: unexpected member {other}"),
    }
}

pub fn install_html_video_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // The intrinsic dimensions of the decoded video: the frame size the resource's header states, or a
        // plugin-fed MediaStream frame's size (fake-media SESSION 2b); 0 before metadata loads or for audio, which
        // is also what a browser reports then. (webkitDecoded/DroppedFrameCount stay 0 -- nothing is dropped.)
        "videoWidth" => acc1(scope, proto, member, cb_video_size, Some("w")),
        "videoHeight" => acc1(scope, proto, member, cb_video_size, Some("h")),
        // Nothing is decoded, presented or dropped.
        "webkitDecodedFrameCount" | "webkitDroppedFrameCount" => acc1(scope, proto, member, cb_media_zero, None),
        "cancelVideoFrameCallback" => method_row(scope, proto, member, cb_vfc_cancel),
        "requestVideoFrameCallback" => method_row(scope, proto, member, cb_vfc_request),
        "getVideoPlaybackQuality" => method_row(scope, proto, member, cb_video_playback_quality),
        "requestPictureInPicture" => method_row(scope, proto, member, cb_video_request_pip),
        other => debug_assert!(false, "install_html_video_element: unexpected member {other}"),
    }
}

webapi::interface! {
    // `[interface]`: html.idl says `// Note: intentionally no [HTMLConstructor]` on this one, and
    // Chrome agrees -- `HTMLMediaElement()` is the bare `Illegal constructor`, unlike its two
    // subclasses below. See `Element`'s note.
    "HTMLMediaElement" : "HTMLElement" [interface, window] {
    ctor            "HTMLMediaElement" len 0                          = stub,
    attr            "error" : Object readonly nullable                = install_html_media_element,
    attr            "src" : Object cfg "src"                          = reflect_url,
    attr            "currentSrc" : Object readonly                    = install_html_media_element,
    attr            "crossOrigin" : Object nullable cfg "crossorigin|anonymous,use-credentials|anonymous" = reflect_nullable_enum,
    attr            "networkState" : Object readonly                  = install_html_media_element,
    attr            "preload" : Object cfg "preload|none,metadata,auto|metadata|metadata" = reflect_enum,
    attr            "buffered" : Object readonly                      = install_html_media_element,
    attr            "readyState" : Object readonly                    = install_html_media_element,
    attr            "seeking" : Object readonly                       = install_html_media_element,
    attr            "currentTime" : Object                            = install_html_media_element,
    attr            "duration" : Object readonly                      = install_html_media_element,
    attr            "paused" : Object readonly                        = install_html_media_element,
    attr            "defaultPlaybackRate" : Object                    = install_html_media_element,
    attr            "playbackRate" : Object                           = install_html_media_element,
    attr            "played" : Object readonly                        = install_html_media_element,
    attr            "seekable" : Object readonly                      = install_html_media_element,
    attr            "ended" : Object readonly                         = install_html_media_element,
    attr            "autoplay" : Object cfg "autoplay"                = reflect_bool,
    attr            "loop" : Object cfg "loop"                        = reflect_bool,
    attr            "preservesPitch" : Object                         = install_html_media_element,
    attr            "controls" : Object cfg "controls"                = reflect_bool,
    attr            "controlsList" : Object cfg "controlslist"        = reflect_tokens,
    attr            "volume" : Object                                 = install_html_media_element,
    attr            "muted" : Object                                  = install_html_media_element,
    attr            "defaultMuted" : Object cfg "muted"               = reflect_bool,
    attr            "textTracks" : Object readonly                    = install_html_media_element,
    attr            "webkitAudioDecodedByteCount" : Object readonly   = install_html_media_element,
    attr            "webkitVideoDecodedByteCount" : Object readonly   = install_html_media_element,
    handler         "onencrypted" nullable                            = event_handler,
    handler         "onwaitingforkey" nullable                        = event_handler,
    attr            "srcObject" : Object nullable                     = install_html_media_element,
    constant        "NETWORK_EMPTY" : Object                          = 0,
    constant        "NETWORK_IDLE" : Object                           = 1,
    constant        "NETWORK_LOADING" : Object                        = 2,
    constant        "NETWORK_NO_SOURCE" : Object                      = 3,
    constant        "HAVE_NOTHING" : Object                           = 0,
    constant        "HAVE_METADATA" : Object                          = 1,
    constant        "HAVE_CURRENT_DATA" : Object                      = 2,
    constant        "HAVE_FUTURE_DATA" : Object                       = 3,
    constant        "HAVE_ENOUGH_DATA" : Object                       = 4,
    method          "addTextTrack" : Object len 1 required 1          = install_html_media_element,
    method          "canPlayType" : Object len 1 required 1           = install_html_media_element,
    method          "captureStream" : Object                          = install_html_media_element,
    method          "load" : Object                                   = install_html_media_element,
    method          "pause" : Object                                  = install_html_media_element,
    method          "play" : Promise                                  = install_html_media_element,
    attr            "loading" : Object cfg "loading|lazy,eager|eager|eager" = reflect_enum,
    attr            "sinkId" : Object readonly                        = install_html_media_element,
    attr            "remote" : Object readonly                        = install_remote,
    attr            "disableRemotePlayback" : Object cfg "disableremoteplayback" = reflect_bool,
    method          "setSinkId" : Promise len 1 required 1            = install_html_media_element,
    attr            "mediaKeys" : Object readonly nullable            = install_html_media_element,
    method          "setMediaKeys" : Promise len 1 required 1         = install_html_media_element,
    static_constant "NETWORK_EMPTY" : Object                          = 0,
    static_constant "NETWORK_IDLE" : Object                           = 1,
    static_constant "NETWORK_LOADING" : Object                        = 2,
    static_constant "NETWORK_NO_SOURCE" : Object                      = 3,
    static_constant "HAVE_NOTHING" : Object                           = 0,
    static_constant "HAVE_METADATA" : Object                          = 1,
    static_constant "HAVE_CURRENT_DATA" : Object                      = 2,
    static_constant "HAVE_FUTURE_DATA" : Object                       = 3,
    static_constant "HAVE_ENOUGH_DATA" : Object                       = 4
    }
}

/// `media.remote`: the element's `[SameObject]` `RemotePlayback`. It was a D2 stub
/// answering `null`, so `video.remote.state` threw where Chrome 150 and Brave 151 answer
/// `"disconnected"` (the Cast sender's first read). One object per element, kept on it, with the
/// interface's brand so its own rows (`state`, `kits::remote_playback`) accept it.
fn cb_media_remote(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let key = {
        let n = v8::String::new(scope, "korvus_remote_playback").unwrap();
        v8::Private::for_api(scope, Some(n))
    };
    if let Some(v) = this.get_private(scope, key).filter(|v| v.is_object()) {
        rv.set(v);
        return;
    }
    let Some(proto) = crate::binding::protos::shell_proto(scope, "RemotePlayback") else { return };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::install::webidl::brand(scope, o, "RemotePlayback");
    this.set_private(scope, key, o.into());
    rv.set(o.into());
}

fn install_remote(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    acc1(scope, proto, member, cb_media_remote, None);
}

webapi::interface! {
    "HTMLVideoElement" : "HTMLMediaElement" [element, window] {
    ctor            "HTMLVideoElement" len 0                          = stub,
    attr            "width" : Object cfg "width|0|"                   = reflect_ulong,
    attr            "height" : Object cfg "height|0|"                 = reflect_ulong,
    attr            "videoWidth" : Object readonly                    = install_html_video_element,
    attr            "videoHeight" : Object readonly                   = install_html_video_element,
    attr            "poster" : Object cfg "poster"                    = reflect_url,
    attr            "webkitDecodedFrameCount" : Object readonly       = install_html_video_element,
    attr            "webkitDroppedFrameCount" : Object readonly       = install_html_video_element,
    attr            "playsInline" : Object cfg "playsinline"          = reflect_bool,
    handler         "onenterpictureinpicture" nullable                = event_handler,
    handler         "onleavepictureinpicture" nullable                = event_handler,
    attr            "disablePictureInPicture" : Object cfg "disablepictureinpicture" = reflect_bool,
    method          "cancelVideoFrameCallback" : Object len 1 required 1 = install_html_video_element,
    method          "getVideoPlaybackQuality" : Object                = install_html_video_element,
    method          "requestPictureInPicture" : Promise              = install_html_video_element,
    method          "requestVideoFrameCallback" : Object len 1 required 1 = install_html_video_element,
    // EDGE'S TWO, LAST, WHICH IS WHERE EDGE 151 PUTS THEM. They are in this table
    // because the interface is, and the family union belongs to whichever table owns the name --
    // `edge-151-linux-20260913T2044`'s capture selects them and the other three bundles do not, so
    // the rows are installed on an Edge device and absent everywhere else, by the one join.
    //
    // MEASURED on the installed Microsoft Edge 151.0.0.0 (`Chrome/151.0.0.0 ... Edg/151.0.0.0`,
    // headed under Xvfb, 2026-09-13) rather than guessed, which is what fixes the two TypeTags:
    //   `v.msVideoProcessing`            -> "default"  (a STRING, and writable -- `v.msVideoProcessing
    //                                      = "x"` is accepted and leaves it "default")
    //   `v.msGetVideoProcessingTypes()`  -> []         (an ARRAY; `.length` is 0)
    // So the types are `DomString` and `Sequence`, and the D2 stub's empty value for each follows
    // from the type: `""` and a fresh `[]`. The array answer is Edge's own on a machine with no
    // video-processing types; the string is NOT ("default" is), and that difference is exactly what
    // a stub row is for -- counted in `tools/status/data/layers.json`, not dressed up as an implementation.
    // This engine has no video-processing pipeline to enumerate, so there is no body to give.
    attr            "msVideoProcessing" : DomString              = stub,
    method          "msGetVideoProcessingTypes" : Sequence len 0 = stub
    }
}

// `HTMLAudioElement` has no members of its own -- every one of them is `HTMLMediaElement`'s, reached
// through the chain. The table exists so the interface has ONE definition site like its two
// siblings, and so the `audio` tag maps onto a prototype this lane owns.
webapi::interface! {
    "HTMLAudioElement" : "HTMLMediaElement" [element, window] {
    ctor "HTMLAudioElement" len 0 = stub
    }
}
