//! `core/html/media_objects.rs` -- the seven NON-ELEMENT interfaces of the media family, as seven
//! `interface!` tables.
//!
//! `TimeRanges`, `MediaError`, `MediaCapabilities`, `TextTrack`, `TextTrackList`, `TextTrackCue` and
//! `VTTCue`: the objects a media element hands out, rather than the elements themselves. They share
//! one rule, which is why they share one file -- this engine decodes nothing, so every one of them is
//! either empty or never minted at all, and the shape is the whole of what a page can see.
//!
//! Rows are VERBATIM from the seven blocks of `install/generated_registry.rs` they replace: names,
//! order, kinds, lengths, `required` counts, nullability and descriptor flags.
//!
//! WHAT HAS A BODY AND WHAT DOES NOT, and why the difference is not arbitrary:
//!
//!   * `TimeRanges` is the one this lane implements. `dom/08z-media-element.js` wired its three
//!     members lazily on first access to `buffered`, behind two `KB.__emptyRanges*` bridge slots and
//!     an `Object.defineProperty` pair that ran the first time any media element was asked for a
//!     range list. The rows below are mounted once, by THE installer, like every other row.
//!   * `MediaError` is minted by a media element whose resource failed (`core/html/media_load.rs`):
//! `code` and `message` read what the failure set. Its four constants are what the
//!     bundle's oracle reads, on BOTH holders.
//!   * `MediaCapabilities` HAS BODIES NOW. The claim above -- "promises over a decoder
//!     query this build cannot answer" -- was wrong twice: the rows were `Object` stubs, so they
//!     answered `null` rather than a promise at all, and the query is not a decoder query. It is the
//!     device's recorded codec grid, which this engine already reads for three other members. See the
//!     block above the table for the rules and for what they were measured against.
//!   * `TextTrack` and `TextTrackList` have live bodies. They were `dom/09k-media-rtc.js`'s when this
//!     lane was written -- 09k replaced each row's body IN PLACE after this install -- and W5.1 lane
//!     L4 moved them into `core/html/text_tracks.rs`, which these rows now name directly. The shape
//!     is still this table's and the answers are still one file away, but that file is Rust and the
//!     row points at it, so no later pass has to reach in.
//!   * `TextTrackCue` and `VTTCue` are shape only in this engine today, as they were in the registry:
//!     nothing mints a cue, and a cue with invented timings would be a shape wearing an object's
//!     clothes.

use deno_core::v8;

use crate::binding::protos::{make_dom_exc, throw_type_error};
use crate::core::html::html_input_element::acc1;
// The live bodies of `TextTrack` and `TextTrackList`, which were
// `dom/09k-media-rtc.js`'s when lane L3 wrote these tables.
use crate::core::html::text_tracks::{install_text_track, install_track_list};
use crate::install::MemberDef;

/// The brand every Rust-minted `TimeRanges` wears, so the three rows can tell one of ours from a
/// plain object wearing the prototype. Chrome answers "Illegal invocation" for the latter; the JS
/// this replaces answered `0` and threw the index error, which is a tell -- no browser's
/// `TimeRanges.prototype.length` reads on `{}`.
const BRAND: &str = "korvus_time_ranges";

fn brand_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, BRAND).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// Mint one `TimeRanges` on the table's prototype holding `ranges` (`(start, end)` pairs, in order).
/// `buffered`, `played` and `seekable` each call this; there is no other way to get one, which is what
/// makes the brand check total. The brand IS the list: a private array of the bounds.
pub(crate) fn mint_time_ranges_with<'s>(scope: &mut v8::PinScope<'s, '_>, ranges: &[(f64, f64)]) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "TimeRanges")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    let bounds = v8::Array::new(scope, (ranges.len() * 2) as i32);
    for (i, (start, end)) in ranges.iter().enumerate() {
        let s = v8::Number::new(scope, *start);
        bounds.set_index(scope, (i * 2) as u32, s.into());
        let e = v8::Number::new(scope, *end);
        bounds.set_index(scope, (i * 2 + 1) as u32, e.into());
    }
    let key = brand_key(scope);
    o.set_private(scope, key, bounds.into());
    Some(o)
}

/// The bounds a `TimeRanges` of ours holds, `None` for anything else.
fn ranges_of<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Array>> {
    let key = brand_key(scope);
    v.get_private(scope, key).and_then(|x| v8::Local::<v8::Array>::try_from(x).ok())
}

/// `TimeRanges.length`: how many ranges the list holds -- none for an element that never loaded.
fn cb_ranges_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(bounds) = ranges_of(scope, args.this()) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    rv.set_uint32(bounds.length() / 2);
}

/// `TimeRanges.start(i)` / `.end(i)`: with `length` 0 every index is out of bounds, and the spec's
/// answer for that is `IndexSizeError`. `data` is the member name, so both rows share one body.
///
/// ONE THING THE DELETED JS HAD WRONG, recorded from headless Chrome on this machine (2026-09-11):
/// `dom/08z-media-element.js` hard-coded the index as `(0)` and the comparison as "greater than or
/// equal to" for every call, so `buffered.start(5)` reported the wrong index AND the wrong relation.
/// Chrome has TWO messages and picks by the comparison it actually made:
///
/// ```text
/// v.buffered.start(0) -> "The index provided (0) is greater than or equal to the maximum bound (0)."
/// v.buffered.start(5) -> "The index provided (5) is greater than the maximum bound (0)."
/// ```
///
/// The index is the WebIDL `unsigned long` conversion of the argument, which is why `start(-1)`
/// reports 4294967295.
fn cb_ranges_bound(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(bounds) = ranges_of(scope, args.this()) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let i = args.get(0).uint32_value(scope).unwrap_or(0);
    let len = bounds.length() / 2;
    if i < len {
        let at = i * 2 + u32::from(name == "end");
        if let Some(v) = bounds.get_index(scope, at) {
            rv.set(v);
        }
        return;
    }
    let rel = if i > len { "is greater than" } else { "is greater than or equal to" };
    let msg = format!("Failed to execute '{name}' on 'TimeRanges': The index provided ({i}) {rel} the maximum bound ({len}).");
    if let Some(e) = make_dom_exc(scope, &msg, "IndexSizeError") {
        scope.throw_exception(e);
    }
}

pub fn install_time_ranges(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::binding::protos::{build_fn_data, make_arity_guard, str_val};
    match member.name {
        "length" => acc1(scope, proto, member, cb_ranges_length, None),
        "start" | "end" => {
            // Both rows are `required 1`, so the body wears the arity guard the same way
            // `method_row` puts one on -- built by hand here only because the callback needs its
            // member name as `data`.
            let d = str_val(scope, member.name);
            let f = build_fn_data(scope, cb_ranges_bound, member.name, member.length as i32, d);
            let v: v8::Local<v8::Value> =
                make_arity_guard(scope, "TimeRanges", member.name, member.required as i32, member.length as i32, f.into(), member.ty)
                    .into();
            crate::install::kits::define_value(scope, proto, member, v, true);
        }
        other => debug_assert!(false, "install_time_ranges: unexpected member {other}"),
    }
}

webapi::interface! {
    "TimeRanges" [interface, window] {
    ctor            "TimeRanges" len 0                                = stub,
    attr            "length" : Object readonly                        = install_time_ranges,
    method          "end" : Object len 1 required 1                   = install_time_ranges,
    method          "start" : Object len 1 required 1                 = install_time_ranges
    }
}

/// The brand every Rust-minted `MediaError` wears: a private `[code, message]`.
const ERROR_BRAND: &str = "korvus_media_error";

fn error_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, ERROR_BRAND).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// Mint the `MediaError` a failed media element hands out: `code` and `message` as the
/// failure set them.
pub(crate) fn mint_media_error<'s>(scope: &mut v8::PinScope<'s, '_>, code: u16, message: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "MediaError")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    let pair = v8::Array::new(scope, 2);
    let c = v8::Integer::new(scope, code as i32);
    pair.set_index(scope, 0, c.into());
    let m = crate::binding::protos::str_val(scope, message);
    pair.set_index(scope, 1, m);
    let key = error_key(scope);
    o.set_private(scope, key, pair.into());
    Some(o)
}

/// `MediaError.code` / `.message` (`data` is the member).
fn cb_media_error_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = error_key(scope);
    let Some(pair) = args.this().get_private(scope, key).and_then(|x| v8::Local::<v8::Array>::try_from(x).ok()) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let at = u32::from(crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "message");
    if let Some(v) = pair.get_index(scope, at) {
        rv.set(v);
    }
}

pub fn install_media_error(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "code" | "message" => acc1(scope, proto, member, cb_media_error_field, Some(member.name)),
        other => debug_assert!(false, "install_media_error: unexpected member {other}"),
    }
}

webapi::interface! {
    "MediaError" [interface, window] {
    ctor            "MediaError" len 0                                = stub,
    attr            "code" : Object readonly                          = install_media_error,
    attr            "message" : Object readonly                       = install_media_error,
    constant        "MEDIA_ERR_ABORTED" : Object                      = 1,
    constant        "MEDIA_ERR_NETWORK" : Object                      = 2,
    constant        "MEDIA_ERR_DECODE" : Object                       = 3,
    constant        "MEDIA_ERR_SRC_NOT_SUPPORTED" : Object            = 4,
    static_constant "MEDIA_ERR_ABORTED" : Object                      = 1,
    static_constant "MEDIA_ERR_NETWORK" : Object                      = 2,
    static_constant "MEDIA_ERR_DECODE" : Object                       = 3,
    static_constant "MEDIA_ERR_SRC_NOT_SUPPORTED" : Object            = 4
    }
}

// ── MediaCapabilities ─────────────────────────────────────────────────────────────────────────
//
// WHAT WAS WRONG. Both rows were D2 stubs of IDL type `Object`, so the shape default was
// `null` -- `await navigator.mediaCapabilities.decodingInfo(cfg)` was `null` in every realm for every
// configuration, where every browser hands back a `MediaCapabilitiesInfo`. One `await`, no timing, no
// sampling.
//
// THE ANSWER IS THE DEVICE'S GRID, NOT A CONSTANT. `supported` comes from `spec::CodecGrid` -- the
// same recorded container/codec table `canPlayType`, `MediaSource.isTypeSupported` and
// `MediaRecorder.isTypeSupported` read -- through `media_codecs::decoding_supported`, which is where
// the fourth question's rules live and where they are derived from measurement. R7 is untouched: no
// byte is decoded to answer this, and `HTMLMediaElement.readyState` still advances only on what the
// demuxer actually read.
//
// MEASURED 2026-09-18 on real Brave 151.1.93.138 AND stock Chrome 150.0.7871.46 over a loopback page
// -- 56 of 56 cells IDENTICAL between the two browsers, which is why the rules below are in the engine
// and only the grid is in the bundle:
//
//     powerEfficient   supported AND the configuration has no `video` member.
//                      (audio-only supported -> true; anything with video -> false. This engine has
//                      no hardware decoder at all, and neither did either reference browser here.)
//     smooth           == supported, in all 56 cells, including 3840x2160@60 and 1x1@1.
//     keySystemAccess  null, always. A configuration carrying `keySystemConfiguration` answers
//                      {false,false,false,null} -- measured for `com.widevine.alpha` itself, on a
//                      browser that ships Widevine, because a headless profile has no CDM. This
//                      engine has none either, so that is its own answer and not an imitation.
//     key order        powerEfficient, smooth, supported, keySystemAccess.
//
// THE ERRORS ARE PART OF THE SURFACE; a fingerprinter reads `e.message`. They are WebIDL's conversion errors
// (`install::ctor_dicts`) and Blink's own sentences (`media_capabilities.rs`), and every one REJECTS -- the two
// methods return promises (korvus threw them).
//
// `type: "webrtc"` asks about RTP payload names (`video/VP8`, `video/H264`, `audio/opus`) -- a different MIME
// namespace from the file/MSE grid -- and is answered from the device's RTP codec lists, the ones
// `RTCRtpReceiver.getCapabilities()` / `RTCRtpSender.getCapabilities()` report (`media_capabilities::webrtc_support`).

/// The four fields, in Chrome's key order, resolved into a promise.
fn resolve_capabilities_info(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, supported: bool, smooth: bool, power_efficient: bool) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let o = v8::Object::new(scope);
    for (k, v) in [("powerEfficient", power_efficient), ("smooth", smooth), ("supported", supported)] {
        let b = v8::Boolean::new(scope, v);
        crate::binding::protos::set_prop(scope, o, k, b.into());
    }
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    crate::binding::protos::set_prop(scope, o, "keySystemAccess", null);
    resolver.resolve(scope, o.into());
    rv.set(resolver.get_promise(scope).into());
}

/// Whether this device decodes the configuration a `file` / `media-source` question names: a content type of more
/// than one codec is a question about a stream rather than a track -- unsupported, not an error (measured:
/// `video/mp4; codecs="avc1.42E01E, mp4a.40.2"` answers false) -- and the rest is the captured codec table's.
fn decodes(scope: &mut v8::PinScope, conf: v8::Local<v8::Object>, media_source: bool) -> bool {
    let content_type = crate::binding::protos::get_prop(scope, conf, "contentType").filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    match crate::core::dom::media_codecs::content_type_shape(&content_type) {
        Some((_, codecs)) if codecs > 1 => false,
        _ => crate::core::dom::media_codecs::decoding_supported(scope, &content_type, media_source),
    }
}

/// `decodingInfo(configuration)` and `encodingInfo(configuration)`, as Blink 150 answers them
/// (measured: `tests/fixtures/dict-conversion/mediacaps-*`). Promise operations: every refusal REJECTS. The
/// configuration is converted as WebIDL converts it -- `audio`, `video` (each nested configuration read whole, its
/// members sorted), then the derived dictionary's `keySystemConfiguration` and `type` -- then Blink's checks:
/// `record` refused (MediaCapabilitiesEncodingInfo is off), `IsValidMedia*Configuration`
/// ([`super::media_capabilities::invalid_configuration`]). korvus read `type` first and only the members its
/// own checks needed, THREW every refusal, read `width`/`height` as doubles (NaN refused, Chrome converts it to 0),
/// and answered every `webrtc` configuration unsupported.
fn cb_capabilities_info(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let decoding = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "decodingInfo";
    let (method, dict) = if decoding { ("decodingInfo", "MediaDecodingConfiguration") } else { ("encodingInfo", "MediaEncodingConfiguration") };
    let prefix = format!("Failed to execute '{method}' on 'MediaCapabilities': ");
    let config = match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), dict, &prefix) {
        Ok(c) => c,
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let reject = |scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str| {
        let p = crate::binding::protos::rejected_type_error(scope, &format!("{prefix}{msg}"));
        rv.set(p);
    };
    let ty = crate::binding::protos::get_prop(scope, config, "type").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    // `record` IS in the IDL enum and is refused after the conversion -- hence no "Failed to read the 'type'
    // property" in its sentence
    if !decoding && ty == "record" {
        reject(scope, &mut rv, "The provided value 'record' is not a valid enum value of type MediaEncodingType.");
        return;
    }
    let webrtc = ty == "webrtc";
    let video = crate::binding::protos::get_prop(scope, config, "video").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let audio = crate::binding::protos::get_prop(scope, config, "audio").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    if let Some(msg) = super::media_capabilities::invalid_configuration(scope, config, video, audio, decoding, webrtc) {
        reject(scope, &mut rv, msg);
        return;
    }
    let (supported, smooth, power_efficient) = if webrtc {
        super::media_capabilities::webrtc_support(scope, video, audio, decoding)
    } else {
        // A key system this engine cannot provide: nothing about the configuration is supported, and the access
        // object is null. Both reference browsers answer the same on a profile with no CDM.
        let encrypted = crate::binding::protos::get_prop(scope, config, "keySystemConfiguration").is_some_and(|v| v.is_object());
        let media_source = ty == "media-source";
        let video_ok = video.is_none_or(|v| decodes(scope, v, media_source));
        let audio_ok = audio.is_none_or(|a| decodes(scope, a, media_source));
        let supported = !encrypted && video_ok && audio_ok;
        (supported, supported, supported && video.is_none())
    };
    resolve_capabilities_info(scope, &mut rv, supported, smooth, power_efficient);
}

pub fn install_media_capabilities(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "decodingInfo" | "encodingInfo" => {
            let d = crate::binding::protos::str_val(scope, member.name);
            let f = crate::binding::protos::build_fn_data(scope, cb_capabilities_info, member.name, member.length as i32, d);
            let v: v8::Local<v8::Value> = crate::binding::protos::make_arity_guard(
                scope, "MediaCapabilities", member.name, member.required as i32, member.length as i32, f.into(), member.ty,
            )
            .into();
            crate::install::kits::define_value(scope, proto, member, v, true);
        }
        other => debug_assert!(false, "install_media_capabilities: unexpected member {other}"),
    }
}

webapi::interface! {
    "MediaCapabilities" [interface, both] {
    ctor            "MediaCapabilities" len 0                         = stub,
    method          "decodingInfo" : Promise len 1 required 1         = install_media_capabilities,
    method          "encodingInfo" : Promise len 1 required 1         = install_media_capabilities
    }
}

webapi::interface! {
    "TextTrackList" : "EventTarget" [interface, window] {
    ctor            "TextTrackList" len 0                             = stub,
    attr            "length" : Object readonly                        = install_track_list,
    handler         "onchange" nullable                               = event_handler,
    handler         "onaddtrack" nullable                             = event_handler,
    handler         "onremovetrack" nullable                          = event_handler,
    method          "getTrackById" : Object len 1 required 1 nullable = install_track_list,
    method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "TextTrackCue" : "EventTarget" [interface, window] {
    ctor            "TextTrackCue" len 0                              = stub,
    attr            "track" : Object readonly nullable                = install_vtt_cue,
    attr            "id" : Object                                     = install_vtt_cue,
    attr            "startTime" : Object                              = install_vtt_cue,
    attr            "endTime" : Object                                = install_vtt_cue,
    attr            "pauseOnExit" : Object                            = install_vtt_cue,
    handler         "onenter" nullable                                = event_handler,
    handler         "onexit" nullable                                 = event_handler
    }
}

webapi::interface! {
    "TextTrack" : "EventTarget" [interface, window] {
    ctor            "TextTrack" len 0                                 = stub,
    attr            "kind" : Object readonly                          = install_text_track,
    attr            "label" : Object readonly                         = install_text_track,
    attr            "language" : Object readonly                      = install_text_track,
    attr            "id" : Object readonly                            = install_text_track,
    attr            "mode" : Object                                   = install_text_track,
    attr            "cues" : Object readonly nullable                 = install_text_track,
    attr            "activeCues" : Object readonly nullable           = install_text_track,
    handler         "oncuechange" nullable                            = event_handler,
    method          "addCue" : Object len 1 required 1                = install_text_track,
    method          "removeCue" : Object len 1 required 1             = install_text_track
    }
}

// ── TextTrackCue / VTTCue members ──────────────────────────────────────────────────
//
// Every member was a D2 stub and the shell constructor kept none of its arguments, so
// `new VTTCue(0, 1, "x").align` read `null` and `.text` nothing. Measured in Chrome 150 and Brave 151:
// a fresh cue is `align` "center", `line` and `position` "auto", `size` 100, `vertical` "",
// `snapToLines` true; `align = "start"` sticks and an invalid keyword is ignored (a WebIDL enum).

fn cue_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_cue_{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn cue_set(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = cue_key(scope, name);
    this.set_private(scope, k, v);
}

/// `new VTTCue(startTime, endTime, text)`, after the shell constructor's own steps (`binding::protos::
/// cb_shell_ctor` calls this for a `VTTCue`).
pub(crate) fn vtt_cue_init(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments) {
    let start = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    let end = args.get(1).number_value(scope).unwrap_or(f64::NAN);
    if !start.is_finite() || !end.is_finite() {
        throw_type_error(scope, "Failed to construct 'VTTCue': The provided double value is non-finite.");
        return;
    }
    let text = args.get(2).to_rust_string_lossy(scope);
    let values: [(&str, v8::Local<v8::Value>); 11] = [
        ("id", v8::String::new(scope, "").unwrap().into()),
        ("startTime", v8::Number::new(scope, start).into()),
        ("endTime", v8::Number::new(scope, end).into()),
        ("pauseOnExit", v8::Boolean::new(scope, false).into()),
        ("vertical", v8::String::new(scope, "").unwrap().into()),
        ("snapToLines", v8::Boolean::new(scope, true).into()),
        ("line", v8::String::new(scope, "auto").unwrap().into()),
        ("position", v8::String::new(scope, "auto").unwrap().into()),
        ("size", v8::Number::new(scope, 100.0).into()),
        ("align", v8::String::new(scope, "center").unwrap().into()),
        ("text", v8::String::new(scope, &text).unwrap().into()),
    ];
    for (name, v) in values {
        cue_set(scope, this, name, v);
    }
}

/// The receiver, when it is a cue this engine minted; `Illegal invocation` otherwise.
fn cue_this<'a>(scope: &mut v8::PinScope, this: v8::Local<'a, v8::Object>) -> Option<v8::Local<'a, v8::Object>> {
    if crate::install::webidl::has_brand(scope, this.into(), "VTTCue") {
        Some(this)
    } else {
        throw_type_error(scope, "Illegal invocation");
        None
    }
}

fn cb_cue_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(this) = cue_this(scope, args.this()) else { return };
    if name == "track" {
        rv.set_null();
        return;
    }
    let k = cue_key(scope, &name);
    match this.get_private(scope, k).filter(|v| !v.is_undefined()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `(double or AutoKeyword)`: `Some(None)` for `"auto"`, `Some(Some(n))` for a finite number, and
/// `None` once the TypeError for anything else is thrown.
fn cue_line_setting(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str, name: &str) -> Option<Option<f64>> {
    if v.is_string() {
        let s = v.to_rust_string_lossy(scope);
        if s == "auto" {
            return Some(None);
        }
        throw_type_error(scope, &format!("Failed to set the '{name}' property on '{iface}': The provided value '{s}' is not a valid enum value of type AutoKeyword."));
        return None;
    }
    let n = v.number_value(scope).unwrap_or(f64::NAN);
    if !n.is_finite() {
        throw_type_error(scope, &format!("Failed to set the '{name}' property on '{iface}': The provided double value is non-finite."));
        return None;
    }
    Some(Some(n))
}

fn cb_cue_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(this) = cue_this(scope, args.this()) else { return };
    let iface = if matches!(name.as_str(), "id" | "startTime" | "endTime" | "pauseOnExit") { "TextTrackCue" } else { "VTTCue" };
    let v = args.get(0);
    let out: v8::Local<v8::Value> = match name.as_str() {
        "id" | "text" => {
            let s = v.to_rust_string_lossy(scope);
            v8::String::new(scope, &s).unwrap().into()
        }
        "pauseOnExit" | "snapToLines" => v8::Boolean::new(scope, v.boolean_value(scope)).into(),
        "startTime" | "endTime" => {
            let n = v.number_value(scope).unwrap_or(f64::NAN);
            if !n.is_finite() {
                throw_type_error(scope, &format!("Failed to set the '{name}' property on '{iface}': The provided double value is non-finite."));
                return;
            }
            v8::Number::new(scope, n).into()
        }
        "vertical" | "align" => {
            // An enum attribute: an unknown keyword is IGNORED, not an error.
            let s = v.to_rust_string_lossy(scope);
            let ok = if name == "vertical" { ["", "rl", "lr"].contains(&s.as_str()) } else { ["start", "center", "end", "left", "right"].contains(&s.as_str()) };
            if !ok {
                return;
            }
            v8::String::new(scope, &s).unwrap().into()
        }
        "line" | "position" | "size" => {
            let setting = if name == "size" {
                let n = v.number_value(scope).unwrap_or(f64::NAN);
                if !n.is_finite() {
                    throw_type_error(scope, &format!("Failed to set the '{name}' property on '{iface}': The provided double value is non-finite."));
                    return;
                }
                Some(n)
            } else {
                match cue_line_setting(scope, v, iface, &name) {
                    Some(x) => x,
                    None => return,
                }
            };
            match setting {
                None => v8::String::new(scope, "auto").unwrap().into(),
                Some(n) if name != "line" && !(0.0..=100.0).contains(&n) => {
                    if let Some(e) = make_dom_exc(scope, &format!("Failed to set the '{name}' property on '{iface}': The value provided ({n}) is outside the range [0, 100]."), "IndexSizeError") {
                        scope.throw_exception(e);
                    }
                    return;
                }
                Some(n) => v8::Number::new(scope, n).into(),
            }
        }
        _ => return,
    };
    cue_set(scope, this, &name, out);
}

// ── getCueAsHTML: the WebVTT cue text DOM construction rules ────────────────────────────────────────
//
// `cue.getCueAsHTML()` is a NEW DocumentFragment per call (Chrome 150, measured) holding the cue text
// parsed by WebVTT's cue text parsing rules (Blink `VTTTokenizer` / `VTTTreeBuilder`): `<c.a.b>` is a
// `<span class="a b">`, `<i>` `<b>` `<u>` `<ruby>` `<rt>` keep their names (with the classes),
// `<v Bob>` is `<span title="Bob">`, `<lang en>` `<span lang="en">`, a `<00:00:01.000>` timestamp a
// `timestamp` processing instruction, a character reference its character; anything else is dropped.
// Measured: `<b>bold</b> <i>it</i> <c.cls>c</c> <v Bob>hi</v> &amp; ...` reads back exactly that way.

enum VttNode {
    Text(String),
    Pi(String),
    El { tag: &'static str, kind: &'static str, class: String, title: Option<String>, lang: Option<String>, kids: Vec<VttNode> },
}

fn vtt_char_ref(name: &str) -> Option<String> {
    if let Some(n) = name.strip_prefix('#') {
        let cp = match n.strip_prefix(['x', 'X']) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => n.parse::<u32>().ok()?,
        };
        return char::from_u32(cp).map(|c| c.to_string());
    }
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "nbsp" => "\u{a0}",
        "lrm" => "\u{200e}",
        "rlm" => "\u{200f}",
        "quot" => "\"",
        "apos" => "'",
        _ => return None,
    }.to_string())
}

fn vtt_timestamp(t: &str) -> bool {
    let parts: Vec<&str> = t.split(':').collect();
    let ok_num = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let (h, m, rest) = match parts.len() {
        3 => (Some(parts[0]), parts[1], parts[2]),
        2 => (None, parts[0], parts[1]),
        _ => return false,
    };
    if let Some(h) = h {
        if h.len() < 2 || !h.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    let Some((sec, ms)) = rest.split_once('.') else { return false };
    ok_num(m, 2) && ok_num(sec, 2) && ok_num(ms, 3) && m < "60" && sec < "60"
}

fn vtt_parse(text: &str) -> Vec<VttNode> {
    // A stack of open elements; index 0 is the fragment.
    let mut stack: Vec<VttNode> = vec![VttNode::El { tag: "", kind: "", class: String::new(), title: None, lang: None, kids: Vec::new() }];
    fn push(stack: &mut [VttNode], n: VttNode) {
        if let Some(VttNode::El { kids, .. }) = stack.last_mut() {
            if let (VttNode::Text(t), Some(VttNode::Text(prev))) = (&n, kids.last_mut()) {
                prev.push_str(t);
                return;
            }
            kids.push(n);
        }
    }
    fn close(stack: &mut Vec<VttNode>) {
        if stack.len() > 1 {
            let n = stack.pop().unwrap();
            push(stack, n);
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut buf = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '&' {
            let rest: String = chars[i + 1..].iter().take(32).collect();
            if let Some(end) = rest.find(';') {
                if let Some(r) = vtt_char_ref(&rest[..end]) {
                    buf.push_str(&r);
                    i += end + 2;
                    continue;
                }
            }
            buf.push('&');
            i += 1;
            continue;
        }
        if c != '<' {
            buf.push(c);
            i += 1;
            continue;
        }
        // A tag: up to the next `>` (or the end of the text).
        let close_at = chars[i + 1..].iter().position(|&x| x == '>').map(|p| i + 1 + p);
        let body: String = match close_at {
            Some(e) => chars[i + 1..e].iter().collect(),
            None => chars[i + 1..].iter().collect(),
        };
        i = close_at.map(|e| e + 1).unwrap_or(chars.len());
        if !buf.is_empty() {
            push(&mut stack, VttNode::Text(std::mem::take(&mut buf)));
        }
        if let Some(name) = body.strip_prefix('/') {
            let name = name.trim();
            let top = match stack.last() {
                Some(VttNode::El { kind, .. }) => *kind,
                _ => "",
            };
            if !name.is_empty() && top == name {
                close(&mut stack);
            } else if name == "ruby" && top == "rt" {
                close(&mut stack);
                close(&mut stack);
            }
            continue;
        }
        if body.starts_with(|c: char| c.is_ascii_digit()) {
            if vtt_timestamp(body.trim()) {
                push(&mut stack, VttNode::Pi(body.trim().to_string()));
            }
            continue;
        }
        let (head, annotation) = match body.find([' ', '\t', '\n', '\x0c', '\r']) {
            Some(p) => (&body[..p], Some(body[p..].trim().to_string())),
            None => (body.as_str(), None),
        };
        let mut parts = head.split('.');
        let name = parts.next().unwrap_or("");
        let classes: Vec<&str> = parts.filter(|c| !c.is_empty()).collect();
        let (tag, kind): (&'static str, &'static str) = match name {
            "c" => ("span", "c"),
            "i" => ("i", "i"),
            "b" => ("b", "b"),
            "u" => ("u", "u"),
            "ruby" => ("ruby", "ruby"),
            "rt" => match stack.last() {
                Some(VttNode::El { kind: "ruby", .. }) => ("rt", "rt"),
                _ => continue,
            },
            "v" => ("span", "v"),
            "lang" => ("span", "lang"),
            _ => continue,
        };
        let title = if kind == "v" { Some(annotation.clone().unwrap_or_default()) } else { None };
        let lang = if kind == "lang" { Some(annotation.unwrap_or_default()) } else { None };
        stack.push(VttNode::El { tag, kind, class: classes.join(" "), title, lang, kids: Vec::new() });
    }
    if !buf.is_empty() {
        push(&mut stack, VttNode::Text(buf));
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    match stack.pop() {
        Some(VttNode::El { kids, .. }) => kids,
        _ => Vec::new(),
    }
}

fn vtt_build<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<'s, v8::Value>, parent: v8::Local<'s, v8::Value>, nodes: &[VttNode]) -> Option<()> {
    use crate::binding::protos::{call_method, str_val};
    for n in nodes {
        let child = match n {
            VttNode::Text(t) => {
                let tv = str_val(scope, t);
                call_method(scope, doc, "createTextNode", &[tv])?
            }
            VttNode::Pi(t) => {
                let target = str_val(scope, "timestamp");
                let data = str_val(scope, t);
                call_method(scope, doc, "createProcessingInstruction", &[target, data])?
            }
            VttNode::El { tag, class, title, lang, kids, .. } => {
                let tn = str_val(scope, tag);
                let el = call_method(scope, doc, "createElement", &[tn])?;
                for (k, v) in [("class", (!class.is_empty()).then(|| class.clone())), ("title", title.clone()), ("lang", lang.clone())] {
                    if let Some(v) = v {
                        let kv = str_val(scope, k);
                        let vv = str_val(scope, &v);
                        call_method(scope, el, "setAttribute", &[kv, vv])?;
                    }
                }
                vtt_build(scope, doc, el, kids)?;
                el
            }
        };
        call_method(scope, parent, "appendChild", &[child])?;
    }
    Some(())
}

fn cb_cue_as_html(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = cue_this(scope, args.this()) else { return };
    let k = cue_key(scope, "text");
    let text = this.get_private(scope, k).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let nodes = vtt_parse(&text);
    let Some(doc) = crate::binding::protos::global_get(scope, "document") else { return };
    let Some(frag) = crate::binding::protos::call_method(scope, doc, "createDocumentFragment", &[]) else { return };
    if vtt_build(scope, doc, frag, &nodes).is_some() {
        rv.set(frag);
    }
}

fn install_vtt_cue_html(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::install::live::method_row(scope, proto, member, cb_cue_as_html);
}

fn install_vtt_cue(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::html::html_input_element::acc2;
    if member.name == "track" {
        acc1(scope, proto, member, cb_cue_get, Some(member.name));
    } else {
        acc2(scope, proto, member, cb_cue_get, Some(member.name), cb_cue_set, Some(member.name));
    }
}

// The one NEWABLE interface of the seven: `new VTTCue(0, 1, "text")` mints an object in Chrome, and
// the registry's `Model::Value` shell constructor is what did that here. `install_window_media` keeps
// that exact constructor (`binding::protos::cb_shell_ctor`) over the table's prototype, so the
// observable behaviour of `new VTTCue()` -- and of calling it without `new` -- is unchanged.
webapi::interface! {
    "VTTCue" : "TextTrackCue" [value, window] {
    ctor            "VTTCue" len 3                                    = stub,
    attr            "vertical" : Object                               = install_vtt_cue,
    attr            "snapToLines" : Object                            = install_vtt_cue,
    attr            "line" : Object                                   = install_vtt_cue,
    attr            "position" : Object                               = install_vtt_cue,
    attr            "size" : Object                                   = install_vtt_cue,
    attr            "align" : Object                                  = install_vtt_cue,
    attr            "text" : Object                                   = install_vtt_cue,
    method          "getCueAsHTML" : Object                           = install_vtt_cue_html
    }
}
