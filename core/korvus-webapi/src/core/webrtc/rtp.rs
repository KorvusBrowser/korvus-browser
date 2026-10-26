//! `core/webrtc/rtp.rs` -- `RTCRtpSender`, `RTCRtpReceiver`, `RTCRtpTransceiver` and `RTCDTMFSender`
//! as four `interface!` tables.
//!
//! Rows are VERBATIM from the four blocks of `install/generated_registry.rs` they replace.
//!
//! `addTransceiver` IS A PURELY LOCAL OPERATION: it creates the transceiver, its sender and its
//! receiver, and the receiver's track, all before a single packet moves. Everything below is the state
//! of that object graph BEFORE negotiation -- `mid` null because no m-section has been assigned one,
//! `currentDirection` null because nothing has been negotiated, `transport` null because no DTLS
//! transport exists, and `canInsertDTMF` false because there is nothing to send tones over. Every one
//! of those is the un-negotiated value, not a placeholder, and every one of them is what the bundle's
//! oracle reads off a real `new RTCPeerConnection().addTransceiver("audio")`.
//!
//! `RTCRtpSender.getCapabilities(kind)` / `RTCRtpReceiver.getCapabilities(kind)` are static queries over the
//! platform's codec lists -- the worn device's (`Persona::webrtc_capabilities`, the bundle's
//! `webrtc-capabilities.json`, measured off the installed browser). They were stub rows answering `null` for every
//! kind, where every Chromium answers its lists (the challenge frame reads both kinds).
//!
//! WHAT IS DELIBERATELY NOT IMPLEMENTED, and why:
//!
//!   * `RTCRtpSender.setParameters` REJECTS. Parameters are applied to a transport, and there is none.
//!     `getParameters` answers the pre-negotiation parameter set, which is a real answer.
//!   * `createEncodedStreams` (both sides) needs an encoded-frame pipeline; the rows stay D2 stubs.
//!   * `RTCRtpReceiver.playoutDelayHint` is a jitter-buffer knob with no jitter buffer behind it.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{
    build_fn_data, make_arity_guard, make_dom_exc, rejected_promise, resolved_promise, shell_proto, str_val,
    throw_type_error,
};
use crate::core::media::media_stream::{mint_track, STREAM, TRACK};
use crate::core::webrtc::peer_connection::mint_stats_report;
use crate::install::kits::define_value;
use crate::install::live::method_row;
use crate::install::MemberDef;
use crate::install::kits::str_data_getter as acc;
use crate::install::kits::str_data_accessor as acc_rw;

pub(crate) const SENDER: &str = "RTCRtpSender";
pub(crate) const RECEIVER: &str = "RTCRtpReceiver";
pub(crate) const XCVR: &str = "RTCRtpTransceiver";
pub(crate) const DTMF: &str = "RTCDTMFSender";

pub(crate) const DIRECTIONS: &[&str] = &["sendrecv", "sendonly", "recvonly", "inactive"];

fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    Some(o)
}

// ---- minting the object graph --------------------------------------------------------------------

fn mint_dtmf<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, DTMF)?;
    let s = st::attach(scope, o, DTMF);
    st::set_str(scope, s, "toneBuffer", "");
    Some(o)
}

fn mint_sender<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    kind: &str,
    track: Option<v8::Local<v8::Value>>,
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, SENDER)?;
    let s = st::attach(scope, o, SENDER);
    st::set_str(scope, s, "kind", kind);
    match track {
        Some(t) => st::set(scope, s, "track", t),
        None => st::set_null(scope, s, "track"),
    }
    let empty = st::array(scope, &[]);
    st::set(scope, s, "streams", empty.into());
    st::set_null(scope, s, "transform");
    // Only an AUDIO sender carries a DTMF sender: tones are an audio-track feature.
    if kind == "audio" {
        match mint_dtmf(scope) {
            Some(d) => st::set(scope, s, "dtmf", d.into()),
            None => st::set_null(scope, s, "dtmf"),
        }
    } else {
        st::set_null(scope, s, "dtmf");
    }
    Some(o)
}

fn mint_receiver<'s>(scope: &mut v8::PinScope<'s, '_>, kind: &str) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, RECEIVER)?;
    let s = st::attach(scope, o, RECEIVER);
    st::set_str(scope, s, "kind", kind);
    // A receiver's track is MUTED until media actually arrives on it, which is the state every
    // receiver here is permanently in.
    match mint_track(scope, TRACK, kind, true) {
        Some(t) => st::set(scope, s, "track", t.into()),
        None => st::set_null(scope, s, "track"),
    }
    st::set_null(scope, s, "transform");
    st::set_null(scope, s, "jitterBufferTarget");
    Some(o)
}

/// One transceiver and its whole object graph. `dir` has already been validated by the caller, which
/// is where the `addTransceiver` error message belongs.
pub(crate) fn mint_transceiver<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    kind: &str,
    dir: &str,
    track: Option<v8::Local<v8::Value>>,
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, XCVR)?;
    let s = st::attach(scope, o, XCVR);
    st::set_str(scope, s, "kind", kind);
    st::set_null(scope, s, "mid");
    st::set_str(scope, s, "direction", dir);
    st::set_null(scope, s, "currentDirection");
    st::set_bool(scope, s, "stopped", false);
    let sender = mint_sender(scope, kind, track)?;
    st::set(scope, s, "sender", sender.into());
    let receiver = mint_receiver(scope, kind)?;
    st::set(scope, s, "receiver", receiver.into());
    let empty = st::array(scope, &[]);
    st::set(scope, s, "headerExtensions", empty.into());
    Some(o)
}

/// The `kind` a transceiver was created for -- `RTCPeerConnection.addTrack` reads it to find a
/// compatible sender.
pub(crate) fn xcvr_field<'s>(scope: &mut v8::PinScope<'s, '_>, x: v8::Local<v8::Value>, name: &str) -> v8::Local<'s, v8::Value> {
    match st::state(scope, x, XCVR) {
        Some(s) => st::get(scope, s, name),
        None => v8::undefined(scope).into(),
    }
}

/// Give a sender a track, or take its track away -- `addTrack`'s sender reuse and `removeTrack`.
pub(crate) fn sender_set_track(scope: &mut v8::PinScope, sender: v8::Local<v8::Value>, track: Option<v8::Local<v8::Value>>) {
    let Some(s) = st::state(scope, sender, SENDER) else { return };
    match track {
        Some(t) => st::set(scope, s, "track", t),
        None => st::set_null(scope, s, "track"),
    }
}

/// True when the sender has no track -- the condition `addTrack` reuses a sender on.
pub(crate) fn sender_is_free(scope: &mut v8::PinScope, sender: v8::Local<v8::Value>) -> bool {
    match st::state(scope, sender, SENDER) {
        Some(s) => st::get(scope, s, "track").is_null(),
        None => false,
    }
}

// ---- shared row bodies ---------------------------------------------------------------------------

/// A readonly field of the receiver's state, by name. `data` is `"<iface>|<field>"`, so one callback
/// serves all four interfaces and still brands against the right one.
fn cb_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, name) = d.split_once('|').unwrap_or((SENDER, d.as_str()));
    let Some(s) = st::state_or_throw(scope, args.this().into(), iface) else { return };
    let v = st::get(scope, s, name);
    rv.set(v);
}

/// A member whose answer is a constant, after the receiver check. `data` is `"<iface>|<what>"`.
fn cb_const(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, what) = d.split_once('|').unwrap_or((SENDER, d.as_str()));
    if st::state_or_throw(scope, args.this().into(), iface).is_none() {
        return;
    }
    match what {
        "null" => {
            let n = v8::null(scope);
            rv.set(n.into());
        }
        "false" => rv.set_bool(false),
        "array" => {
            let a = st::array(scope, &[]);
            rv.set(a.into());
        }
        _ => {}
    }
}

/// `transform = v` (both sides) and `jitterBufferTarget = v`: plain slots, `undefined` stored as null.
fn cb_nullable_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let d = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, name) = d.split_once('|').unwrap_or((SENDER, d.as_str()));
    let Some(s) = st::state_or_throw(scope, args.this().into(), iface) else { return };
    let v = args.get(0);
    if v.is_undefined() {
        st::set_null(scope, s, name);
    } else if name == "jitterBufferTarget" {
        if v.is_null() {
            st::set_null(scope, s, name);
        } else {
            let n = v.number_value(scope).unwrap_or(f64::NAN);
            st::set_num(scope, s, name, n);
        }
    } else {
        st::set(scope, s, name, v);
    }
}

/// `getStats()` on a sender or a receiver: stats are produced by transports, senders and receivers
/// that have moved packets. None have, so the report is empty -- the same reason `getSenders()` is
/// empty on a fresh connection, and not a placeholder for one.
fn cb_get_stats(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let iface = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if st::state_or_throw(scope, args.this().into(), &iface).is_none() {
        return;
    }
    match mint_stats_report(scope, &[]) {
        Some(r) => rv.set(resolved_promise(scope, r.into())),
        None => {
            let Some(e) = make_dom_exc(scope, &format!("Failed to execute 'getStats' on '{iface}'."), "OperationError") else {
                return;
            };
            rv.set(rejected_promise(scope, e));
        }
    }
}



fn meth_data(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: &str,
    iface: &str,
) {
    let d = str_val(scope, data);
    let f = build_fn_data(scope, cb, m.name, m.length as i32, d);
    let v: v8::Local<v8::Value> = if m.required > 0 {
        make_arity_guard(scope, iface, m.name, m.required as i32, m.length as i32, f.into(), m.ty).into()
    } else {
        f.into()
    };
    define_value(scope, proto, m, v, true);
}

// ---- RTCRtpSender --------------------------------------------------------------------------------

/// `getParameters()`: before negotiation the only parameter that exists is the single default
/// encoding. That is a real answer about a real object, not a placeholder for a negotiated one.
fn cb_sender_get_parameters(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), SENDER).is_none() {
        return;
    }
    let out = v8::Object::new(scope);
    let tid = st::uuid();
    st::set_str(scope, out, "transactionId", &tid);
    let enc = v8::Object::new(scope);
    st::set_bool(scope, enc, "active", true);
    let encs = st::array(scope, &[enc.into()]);
    st::set(scope, out, "encodings", encs.into());
    let he = st::array(scope, &[]);
    st::set(scope, out, "headerExtensions", he.into());
    let rtcp = v8::Object::new(scope);
    st::set(scope, out, "rtcp", rtcp.into());
    let codecs = st::array(scope, &[]);
    st::set(scope, out, "codecs", codecs.into());
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    st::set(scope, out, "degradationPreference", u);
    rv.set(out.into());
}

/// `setParameters()` REJECTS: parameters are applied to a transport, and there is none. A resolved
/// promise here would claim an encoder was reconfigured.
fn cb_sender_set_parameters(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), SENDER).is_none() {
        return;
    }
    let Some(e) = make_dom_exc(
        scope,
        "Failed to execute 'setParameters' on 'RTCRtpSender': cannot apply parameters without a transport.",
        "InvalidStateError",
    ) else {
        return;
    };
    rv.set(rejected_promise(scope, e));
}

/// `replaceTrack(track)`: local, and it really does replace the sender's track -- there is no
/// negotiation to redo, because nothing was negotiated.
fn cb_sender_replace_track(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), SENDER) else { return };
    let t = args.get(0);
    if !t.is_null() && !st::is_a(scope, t, TRACK) {
        let p = crate::binding::protos::rejected_type_error(
            scope,
            "Failed to execute 'replaceTrack' on 'RTCRtpSender': parameter 1 is not of type 'MediaStreamTrack'.",
        );
        rv.set(p);
        return;
    }
    st::set(scope, s, "track", t);
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

/// `setStreams(...streams)`: a variadic of real `MediaStream`s and nothing else.
fn cb_sender_set_streams(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), SENDER) else { return };
    let mut streams = Vec::new();
    for i in 0..args.length() {
        let v = args.get(i);
        if !st::is_a(scope, v, STREAM) {
            throw_type_error(
                scope,
                &format!("Failed to execute 'setStreams' on 'RTCRtpSender': parameter {} is not of type 'MediaStream'.", i + 1),
            );
            return;
        }
        streams.push(v);
    }
    let arr = st::array(scope, &streams);
    st::set(scope, s, "streams", arr.into());
}

/// `getCapabilities(kind)` for `side` ("RTCRtpSender" / "RTCRtpReceiver"), as Blink answers it: the kind is a
/// DOMString (ToString first), anything but "audio" or "video" is `null`; otherwise the device's
/// `RTCRtpCapabilities`, a FRESH dictionary every call (Blink converts a new one: two calls are not `===`), its
/// members in the order a WebIDL dictionary reaches the page -- the order the bundle keeps them in. A persona
/// with no lists answers `null`, as the stub did.
fn get_capabilities(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, side: &str) {
    let Some(kind) = st::to_str(scope, args.get(0)) else { return };
    if kind != "audio" && kind != "video" {
        rv.set_null();
        return;
    }
    let caps = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let state = op_state.borrow();
        crate::binding::protos::persona_handle(&state).and_then(|h| h.0.webrtc_capabilities())
    };
    let Some(text) = caps.and_then(|c| c.by_side_kind.get(&format!("{side}.{kind}")).cloned()) else {
        rv.set_null();
        return;
    };
    let Some(json) = v8::String::new(scope, &text) else { return };
    if let Some(v) = v8::json::parse(scope, json) {
        rv.set(v);
    }
}

fn cb_sender_get_capabilities(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    get_capabilities(scope, &args, &mut rv, SENDER);
}

fn cb_receiver_get_capabilities(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    get_capabilities(scope, &args, &mut rv, RECEIVER);
}

pub fn install_rtp_sender(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getCapabilities" => method_row(scope, proto, member, cb_sender_get_capabilities),
        "track" | "dtmf" => acc(scope, proto, member, cb_field, &format!("{SENDER}|{}", member.name)),
        // No DTLS/RTCP transport exists until a session description is applied.
        "transport" | "rtcpTransport" => acc(scope, proto, member, cb_const, &format!("{SENDER}|null")),
        "transform" => acc_rw(scope, proto, member, cb_field, &format!("{SENDER}|transform"), cb_nullable_set, &format!("{SENDER}|transform")),
        "getParameters" => method_row(scope, proto, member, cb_sender_get_parameters),
        "setParameters" => method_row(scope, proto, member, cb_sender_set_parameters),
        "replaceTrack" => method_row(scope, proto, member, cb_sender_replace_track),
        "setStreams" => method_row(scope, proto, member, cb_sender_set_streams),
        "getStats" => meth_data(scope, proto, member, cb_get_stats, SENDER, SENDER),
        other => debug_assert!(false, "install_rtp_sender: unexpected member {other}"),
    }
}

// ---- RTCRtpReceiver ------------------------------------------------------------------------------

/// `getParameters()` on a receiver: the four lists a receiver reports, all empty before negotiation.
fn cb_receiver_get_parameters(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), RECEIVER).is_none() {
        return;
    }
    let out = v8::Object::new(scope);
    for k in ["headerExtensions", "codecs", "encodings"] {
        let a = st::array(scope, &[]);
        st::set(scope, out, k, a.into());
    }
    let rtcp = v8::Object::new(scope);
    st::set(scope, out, "rtcp", rtcp.into());
    rv.set(out.into());
}

pub fn install_rtp_receiver(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getCapabilities" => method_row(scope, proto, member, cb_receiver_get_capabilities),
        "track" => acc(scope, proto, member, cb_field, &format!("{RECEIVER}|track")),
        "transport" | "rtcpTransport" => acc(scope, proto, member, cb_const, &format!("{RECEIVER}|null")),
        "transform" => acc_rw(scope, proto, member, cb_field, &format!("{RECEIVER}|transform"), cb_nullable_set, &format!("{RECEIVER}|transform")),
        "jitterBufferTarget" => acc_rw(scope, proto, member, cb_field, &format!("{RECEIVER}|jitterBufferTarget"), cb_nullable_set, &format!("{RECEIVER}|jitterBufferTarget")),
        "getParameters" => method_row(scope, proto, member, cb_receiver_get_parameters),
        // Contributing and synchronization sources are read off RTP packets. None have arrived.
        "getContributingSources" | "getSynchronizationSources" => {
            meth_data(scope, proto, member, cb_const, &format!("{RECEIVER}|array"), RECEIVER)
        }
        "getStats" => meth_data(scope, proto, member, cb_get_stats, RECEIVER, RECEIVER),
        other => debug_assert!(false, "install_rtp_receiver: unexpected member {other}"),
    }
}

// ---- RTCRtpTransceiver ---------------------------------------------------------------------------

/// `direction = v`: the enum check is the binding layer's and runs first; a STOPPED transceiver
/// refuses the assignment outright, which is the spec's own step.
fn cb_xcvr_direction_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), XCVR) else { return };
    let Some(d) = st::to_str(scope, args.get(0)) else { return };
    if !DIRECTIONS.contains(&d.as_str()) {
        throw_type_error(
            scope,
            &format!(
                "Failed to set the 'direction' property on 'RTCRtpTransceiver': The provided value '{d}' is not a valid enum value of type RTCRtpTransceiverDirection."
            ),
        );
        return;
    }
    if st::get_bool(scope, s, "stopped") {
        if let Some(e) = make_dom_exc(
            scope,
            "Failed to set the 'direction' property on 'RTCRtpTransceiver': The transceiver is stopped.",
            "InvalidStateError",
        ) {
            scope.throw_exception(e);
        }
        return;
    }
    st::set_str(scope, s, "direction", &d);
}

/// `stop()`: the transceiver stops sending and receiving. `direction` becomes `"stopped"` -- which is
/// not one of the four settable values, and is what Blink reports for a stopped transceiver.
fn cb_xcvr_stop(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), XCVR) else { return };
    if st::get_bool(scope, s, "stopped") {
        return;
    }
    st::set_bool(scope, s, "stopped", true);
    st::set_str(scope, s, "direction", "stopped");
    st::set_null(scope, s, "currentDirection");
}

/// `setCodecPreferences(codecs)`. The row is `required 0` in the capture, so the arity guard does NOT
/// cover it and the check belongs in the body -- and the recording says the no-argument call is a
/// TypeError. The preference list is then accepted and has nothing to act on, because there is no
/// negotiation to apply it to.
fn cb_xcvr_set_codec_preferences(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), XCVR).is_none() {
        return;
    }
    if args.length() < 1 {
        throw_type_error(
            scope,
            "Failed to execute 'setCodecPreferences' on 'RTCRtpTransceiver': 1 argument required, but only 0 present.",
        );
        return;
    }
    let v = args.get(0);
    let iterable = v8::Local::<v8::Object>::try_from(v)
        .ok()
        .and_then(|o| {
            let sym = v8::Symbol::get_iterator(scope);
            o.get(scope, sym.into())
        })
        .map(|f| f.is_function())
        .unwrap_or(false);
    if !iterable {
        throw_type_error(
            scope,
            "Failed to execute 'setCodecPreferences' on 'RTCRtpTransceiver': The provided value cannot be converted to a sequence.",
        );
    }
}

/// `getHeaderExtensionsToNegotiate()`: the list the page set, copied so it cannot be mutated in place.
fn cb_xcvr_header_extensions(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), XCVR) else { return };
    let items = match st::get_array(scope, s, "headerExtensions") {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    };
    let out = st::array(scope, &items);
    rv.set(out.into());
}

/// `setHeaderExtensionsToNegotiate(list)`: `required 0` in the capture, so the argument check is the
/// body's, like `setCodecPreferences`.
fn cb_xcvr_set_header_extensions(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), XCVR) else { return };
    if args.length() < 1 {
        throw_type_error(
            scope,
            "Failed to execute 'setHeaderExtensionsToNegotiate' on 'RTCRtpTransceiver': 1 argument required, but only 0 present.",
        );
        return;
    }
    let v = args.get(0);
    let items = if v.is_null_or_undefined() {
        Vec::new()
    } else {
        match crate::core::media::media_stream::array_from(scope, v) {
            Some(i) => i,
            None => Vec::new(),
        }
    };
    let arr = st::array(scope, &items);
    st::set(scope, s, "headerExtensions", arr.into());
}

pub fn install_rtp_transceiver(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // `mid` is the m-section identifier the negotiation assigns; nothing has been negotiated.
        "mid" | "sender" | "receiver" | "stopped" | "currentDirection" => {
            acc(scope, proto, member, cb_field, &format!("{XCVR}|{}", member.name))
        }
        "direction" => acc_rw(scope, proto, member, cb_field, &format!("{XCVR}|direction"), cb_xcvr_direction_set, &format!("{XCVR}|direction")),
        "getHeaderExtensionsToNegotiate" => method_row(scope, proto, member, cb_xcvr_header_extensions),
        "getNegotiatedHeaderExtensions" => meth_data(scope, proto, member, cb_const, &format!("{XCVR}|array"), XCVR),
        "setCodecPreferences" => method_row(scope, proto, member, cb_xcvr_set_codec_preferences),
        "setHeaderExtensionsToNegotiate" => method_row(scope, proto, member, cb_xcvr_set_header_extensions),
        "stop" => method_row(scope, proto, member, cb_xcvr_stop),
        other => debug_assert!(false, "install_rtp_transceiver: unexpected member {other}"),
    }
}

// ---- RTCDTMFSender -------------------------------------------------------------------------------

/// `insertDTMF(tones)`: tones travel over the sender's DTLS transport. There is none, so nothing can
/// be inserted -- the algorithm's own `InvalidStateError` when `canInsertDTMF` is false, and the same
/// reason a fresh sender's `transport` is null.
fn cb_dtmf_insert(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), DTMF).is_none() {
        return;
    }
    if let Some(e) = make_dom_exc(
        scope,
        "Failed to execute 'insertDTMF' on 'RTCDTMFSender': The 'canInsertDTMF' attribute is false: this sender cannot send DTMF.",
        "InvalidStateError",
    ) {
        scope.throw_exception(e);
    }
}

pub fn install_dtmf_sender(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "canInsertDTMF" => acc(scope, proto, member, cb_const, &format!("{DTMF}|false")),
        "toneBuffer" => acc(scope, proto, member, cb_field, &format!("{DTMF}|toneBuffer")),
        "insertDTMF" => method_row(scope, proto, member, cb_dtmf_insert),
        other => debug_assert!(false, "install_dtmf_sender: unexpected member {other}"),
    }
}

// ---- the tables ---------------------------------------------------------------------------------

webapi::interface! {
    "RTCRtpTransceiver" [interface, window] {
    ctor          "RTCRtpTransceiver" len 0                       = stub,
    attr          "mid" : Object readonly nullable                = install_rtp_transceiver,
    attr          "sender" : Object readonly                      = install_rtp_transceiver,
    attr          "receiver" : Object readonly                    = install_rtp_transceiver,
    attr          "stopped" : Object readonly                     = install_rtp_transceiver,
    attr          "direction" : Object                            = install_rtp_transceiver,
    attr          "currentDirection" : Object readonly nullable   = install_rtp_transceiver,
    method        "getHeaderExtensionsToNegotiate" : Object       = install_rtp_transceiver,
    method        "getNegotiatedHeaderExtensions" : Object        = install_rtp_transceiver,
    method        "setCodecPreferences" : Object len 1            = install_rtp_transceiver,
    method        "setHeaderExtensionsToNegotiate" : Object len 1 = install_rtp_transceiver,
    method        "stop" : Object                                 = install_rtp_transceiver
    }
}

webapi::interface! {
    "RTCRtpSender" [interface, window] {
    ctor          "RTCRtpSender" len 0                                     = stub,
    attr          "track" : Object readonly nullable                       = install_rtp_sender,
    attr          "transport" : Object readonly nullable                   = install_rtp_sender,
    attr          "rtcpTransport" : Object readonly                        = install_rtp_sender,
    attr          "dtmf" : Object readonly nullable                        = install_rtp_sender,
    method        "createEncodedStreams" : Object                          = stub,
    method        "getParameters" : Object                                 = install_rtp_sender,
    method        "getStats" : Promise                                     = install_rtp_sender,
    method        "replaceTrack" : Promise len 1 required 1                = install_rtp_sender,
    method        "setParameters" : Promise len 1 required 1               = install_rtp_sender,
    method        "setStreams" : Object                                    = install_rtp_sender,
    attr          "transform" : Object nullable                            = install_rtp_sender,
    static_method "getCapabilities" : Object len 1 required 1 nullable     = install_rtp_sender
    }
}

webapi::interface! {
    "RTCRtpReceiver" [interface, window] {
    ctor          "RTCRtpReceiver" len 0                                   = stub,
    attr          "track" : Object readonly                                = install_rtp_receiver,
    attr          "transport" : Object readonly nullable                   = install_rtp_receiver,
    attr          "rtcpTransport" : Object readonly                        = install_rtp_receiver,
    attr          "playoutDelayHint" : Object                              = stub,
    method        "createEncodedStreams" : Object                          = stub,
    method        "getContributingSources" : Object                        = install_rtp_receiver,
    method        "getParameters" : Object                                 = install_rtp_receiver,
    method        "getStats" : Promise                                     = install_rtp_receiver,
    method        "getSynchronizationSources" : Object                     = install_rtp_receiver,
    attr          "jitterBufferTarget" : Object nullable                   = install_rtp_receiver,
    attr          "transform" : Object nullable                            = install_rtp_receiver,
    static_method "getCapabilities" : Object len 1 required 1 nullable     = install_rtp_receiver
    }
}

webapi::interface! {
    "RTCDTMFSender" : "EventTarget" [interface, window] {
    ctor    "RTCDTMFSender" len 0                        = stub,
    handler "ontonechange" nullable                      = event_handler,
    attr    "canInsertDTMF" : Object readonly            = install_dtmf_sender,
    attr    "toneBuffer" : Object readonly               = install_dtmf_sender,
    method  "insertDTMF" : Object len 1 required 1       = install_dtmf_sender
    }
}
