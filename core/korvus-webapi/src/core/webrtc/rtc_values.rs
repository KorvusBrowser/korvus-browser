//! `core/webrtc/rtc_values.rs` -- `RTCSessionDescription` and `RTCIceCandidate` as two `interface!`
//! tables.
//!
//! The two WebRTC VALUE objects: a page can construct both by hand, and neither of them needs a peer,
//! a transport or a device to be completely correct. They are the half of the WebRTC surface that is
//! pure parsing, which is why they move first and alone -- every other RTC interface in
//! `dom/09k-media-rtc.js` is reachable only through an `RTCPeerConnection` and shares that object
//! graph, so those move together, later.
//!
//! Rows are VERBATIM from the two blocks of `install/generated_registry.rs` they replace: names,
//! order, kinds, lengths, `required` counts, nullability and descriptor flags.
//!
//! WHAT THE CAPTURE DECIDES HERE, and this file does not argue with:
//!
//!   * `RTCSessionDescription`'s `type` and `sdp` are WRITABLE accessors. The modern spec makes them
//!     readonly; Blink still ships the legacy pair, and the capture records a setter on both. The
//!     recording wins.
//!   * `RTCIceCandidate` has SIXTEEN readonly attributes of which only four (`candidate`, `sdpMid`,
//!     `sdpMLineIndex`, `usernameFragment`) come from the dictionary. The other twelve are DERIVED by
//!     parsing the `a=candidate` line (RFC 5245 §15.1), and they are null for a line the grammar does
//!     not accept -- which is what a browser answers, for the same reason.
//!   * `relayProtocol` and `url` describe the ICE server a candidate was GATHERED FROM. A candidate a
//!     page constructed by hand was gathered from nothing, so both are null. That is the recorded
//!     answer, not a gap.
//!
//! INSTANCE STATE is a v8 PRIVATE SLOT on the object -- the mechanism `core/events/event.rs` and
//! `core/html/html_media_element.rs` use -- holding one plain object of fields. It is invisible to
//! `Object.keys`, to `getOwnPropertyNames` and to a page walking the object, so
//! `Object.getOwnPropertyNames(new RTCIceCandidate(...))` is `[]`, which is the instance-tell the
//! quality gate counts. The slot is also the BRAND: a member reached on an object that has none
//! answers Chrome's `Illegal invocation`, where the `WeakMap` this replaces answered for anything the
//! JS had ever minted and threw for everything else. Same answer, one fewer mechanism.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, str_val, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;

// ---- the private state slot ---------------------------------------------------------------------

/// One private key per interface, so a `RTCIceCandidate` body cannot read a `RTCSessionDescription`.
fn state_key<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_rtc_{iface}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn attach_state<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, iface: &str) -> v8::Local<'s, v8::Object> {
    let st = v8::Object::new(scope);
    let key = state_key(scope, iface);
    this.set_private(scope, key, st.into());
    st
}

/// The receiver check every row below shares. A receiver with no state slot is not an instance of the
/// interface -- `Object.create(RTCIceCandidate.prototype)`, the prototype itself, a plain object --
/// and Chrome's binding layer answers `TypeError: Illegal invocation` for exactly that.
fn state_of<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    let key = state_key(scope, iface);
    let v = obj.get_private(scope, key)?;
    v8::Local::<v8::Object>::try_from(v).ok()
}

fn field<'s>(scope: &mut v8::PinScope<'s, '_>, st: v8::Local<v8::Object>, name: &str) -> v8::Local<'s, v8::Value> {
    let k = str_val(scope, name);
    st.get(scope, k).unwrap_or_else(|| v8::null(scope).into())
}

fn set_field(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = str_val(scope, name);
    st.set(scope, k, v);
}

fn set_null(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str) {
    let n: v8::Local<v8::Value> = v8::null(scope).into();
    set_field(scope, st, name, n);
}

fn set_str(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str, s: &str) {
    let v = str_val(scope, s);
    set_field(scope, st, name, v);
}

fn set_num(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str, n: f64) {
    let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
    set_field(scope, st, name, v);
}

/// A dictionary member: `undefined` and `null` both mean "absent", which is the WebIDL rule for a
/// nullable member with no default.
fn dict_get<'s>(scope: &mut v8::PinScope<'s, '_>, dict: v8::Local<v8::Value>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let obj = v8::Local::<v8::Object>::try_from(dict).ok()?;
    let k = str_val(scope, name);
    obj.get(scope, k).filter(|v| !v.is_null_or_undefined())
}

/// WebIDL `DOMString`: ToString. A value that cannot be stringified (a Symbol) throws, and the throw
/// is already pending when `to_string` answers `None`.
fn to_str(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    v.to_string(scope).map(|s| s.to_rust_string_lossy(scope))
}

/// WebIDL `unsigned long`: ToNumber, truncate, modulo 2^32.
fn to_ulong(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u32 {
    let n = v.number_value(scope).unwrap_or(0.0);
    if !n.is_finite() {
        return 0;
    }
    n.trunc().rem_euclid(4_294_967_296.0) as u32
}

/// A readonly attribute row whose getter reads one field of the state object by name.
fn acc_field(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, m: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let d = str_val(scope, m.name);
    let g = build_fn_data(scope, cb, &format!("get {}", m.name), 0, d);
    define_accessor(scope, proto, m, g, None);
}

// ---- RTCSessionDescription ----------------------------------------------------------------------

const SDP: &str = "RTCSessionDescription";

/// The four `RTCSdpType` values, in the IDL's order.
const SDP_TYPES: &[&str] = &["offer", "pranswer", "answer", "rollback"];

/// `new RTCSessionDescription(descriptionInitDict)`.
///
/// The argument is optional and its members are both optional, so `new RTCSessionDescription()` is a
/// description with a null `type` and an empty `sdp` -- which is what the bundle's oracle records, and
/// why this constructor cannot simply require an argument.
pub(crate) fn cb_sdp_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("RTCSessionDescription");
        throw_type_error(scope, &msg);
        return;
    }
    // `RTCSessionDescriptionInit`, converted as WebIDL converts it (Chrome 150): `sdp`, then
    // `type` (an enum), each read once (korvus read `type` first)
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "RTCSessionDescriptionInit", "Failed to construct 'RTCSessionDescription': ") else { return };
    let init: v8::Local<v8::Value> = init.into();
    let ty = match dict_get(scope, init, "type") {
        Some(v) => {
            let Some(s) = to_str(scope, v) else { return };
            if !SDP_TYPES.contains(&s.as_str()) {
                throw_type_error(
                    scope,
                    &format!(
                        "Failed to construct 'RTCSessionDescription': The provided value '{s}' is not a valid enum value of type RTCSdpType."
                    ),
                );
                return;
            }
            Some(s)
        }
        None => None,
    };
    let sdp = match dict_get(scope, init, "sdp") {
        Some(v) => match to_str(scope, v) {
            Some(s) => s,
            None => return,
        },
        None => String::new(),
    };
    let st = attach_state(scope, args.this(), SDP);
    match ty {
        Some(t) => set_str(scope, st, "type", &t),
        None => set_null(scope, st, "type"),
    }
    set_str(scope, st, "sdp", &sdp);
}

fn cb_sdp_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(st) = state_of(scope, args.this().into(), SDP) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = field(scope, st, &name);
    rv.set(v);
}

/// `type = v`: the enum check is the observable half, and it is the binding layer's, so it runs before
/// anything is stored.
fn cb_sdp_type_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(st) = state_of(scope, args.this().into(), SDP) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let Some(s) = to_str(scope, args.get(0)) else { return };
    if !SDP_TYPES.contains(&s.as_str()) {
        throw_type_error(
            scope,
            &format!(
                "Failed to set the 'type' property on 'RTCSessionDescription': The provided value '{s}' is not a valid enum value of type RTCSdpType."
            ),
        );
        return;
    }
    set_str(scope, st, "type", &s);
}

fn cb_sdp_sdp_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(st) = state_of(scope, args.this().into(), SDP) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let Some(s) = to_str(scope, args.get(0)) else { return };
    set_str(scope, st, "sdp", &s);
}

/// An `RTCSessionDescription` the CONNECTION makes -- what `localDescription` and its pending/current
/// siblings hold (Blink's `RTCPeerConnection` keeps `RTCSessionDescription` objects there, so
/// `pc.localDescription` is `[object RTCSessionDescription]` with a `toJSON`; korvus kept a plain `{type, sdp}`).
/// Built through the realm's own constructor from an engine-made dictionary.
pub(crate) fn mint_session_description<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str, sdp: &str) -> Option<v8::Local<'s, v8::Object>> {
    let init = crate::binding::protos::init_dict(scope);
    set_str(scope, init, "type", ty);
    set_str(scope, init, "sdp", sdp);
    crate::binding::protos::new_global(scope, SDP, &[init.into()])
}

/// A description's `sdp`, read off its state slot rather than through the page-visible accessor.
pub(crate) fn description_sdp(scope: &mut v8::PinScope, desc: v8::Local<v8::Value>) -> Option<String> {
    let st = state_of(scope, desc, SDP)?;
    let v = field(scope, st, "sdp");
    Some(v.to_rust_string_lossy(scope))
}

/// Rewrite a connection-made description's `sdp` (a gathered candidate joins its m-section).
pub(crate) fn set_description_sdp(scope: &mut v8::PinScope, desc: v8::Local<v8::Value>, sdp: &str) {
    if let Some(st) = state_of(scope, desc, SDP) {
        set_str(scope, st, "sdp", sdp);
    }
}

/// `toJSON()`: the dictionary the description was built from, which is the two members that exist.
fn cb_sdp_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(st) = state_of(scope, args.this().into(), SDP) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let out = v8::Object::new(scope);
    for k in ["type", "sdp"] {
        let v = field(scope, st, k);
        set_field(scope, out, k, v);
    }
    rv.set(out.into());
}

pub fn install_rtc_session_description(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "type" => {
            let d = str_val(scope, "type");
            let g = build_fn_data(scope, cb_sdp_get, "get type", 0, d);
            let d = str_val(scope, "type");
            let s = build_fn_data(scope, cb_sdp_type_set, "set type", 1, d);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "sdp" => {
            let d = str_val(scope, "sdp");
            let g = build_fn_data(scope, cb_sdp_get, "get sdp", 0, d);
            let d = str_val(scope, "sdp");
            let s = build_fn_data(scope, cb_sdp_sdp_set, "set sdp", 1, d);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "toJSON" => method_row(scope, proto, member, cb_sdp_to_json),
        other => debug_assert!(false, "install_rtc_session_description: unexpected member {other}"),
    }
}

// ---- RTCIceCandidate ----------------------------------------------------------------------------

const ICE: &str = "RTCIceCandidate";

const CAND_TYPES: &[&str] = &["host", "srflx", "prflx", "relay"];
const TCP_TYPES: &[&str] = &["active", "passive", "so"];

/// The twelve DERIVED fields, in the order the capture lists them, all null until the line parses.
const DERIVED: &[&str] = &[
    "foundation",
    "component",
    "priority",
    "address",
    "protocol",
    "port",
    "type",
    "tcpType",
    "relatedAddress",
    "relatedPort",
    "usernameFragment",
    "relayProtocol",
];

/// Parse one SDP `a=candidate` line into the twelve derived fields (RFC 5245 §15.1):
///
/// ```text
/// candidate:<foundation> <component> <transport> <priority> <addr> <port> typ <type> [<k> <v>]*
/// ```
///
/// A line that does not match answers all-null, which is what a browser does with the same grammar --
/// the constructor does NOT reject it, because the dictionary member is a plain DOMString.
fn parse_candidate_line(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, line: &str) {
    for k in DERIVED {
        set_null(scope, st, k);
    }
    let mut t = line.trim();
    if t.len() >= 2 && t[..2].eq_ignore_ascii_case("a=") {
        t = &t[2..];
    }
    if t.len() >= 10 && t[..10].eq_ignore_ascii_case("candidate:") {
        t = &t[10..];
    }
    let p: Vec<&str> = t.split_ascii_whitespace().collect();
    if p.len() < 8 || !p[6].eq_ignore_ascii_case("typ") {
        return;
    }
    set_str(scope, st, "foundation", p[0]);
    match p[1].parse::<f64>() {
        Ok(c) if c == 1.0 => set_str(scope, st, "component", "rtp"),
        Ok(c) if c == 2.0 => set_str(scope, st, "component", "rtcp"),
        _ => {}
    }
    let tr = p[2].to_ascii_lowercase();
    if tr == "udp" || tr == "tcp" {
        set_str(scope, st, "protocol", &tr);
    }
    if let Ok(pri) = p[3].parse::<f64>() {
        if pri.is_finite() {
            set_num(scope, st, "priority", pri);
        }
    }
    set_str(scope, st, "address", p[4]);
    if let Ok(port) = p[5].parse::<f64>() {
        if port.is_finite() {
            set_num(scope, st, "port", port);
        }
    }
    let ty = p[7].to_ascii_lowercase();
    if CAND_TYPES.contains(&ty.as_str()) {
        set_str(scope, st, "type", &ty);
    }
    let mut i = 8;
    while i + 1 < p.len() {
        let (k, v) = (p[i].to_ascii_lowercase(), p[i + 1]);
        match k.as_str() {
            "raddr" => set_str(scope, st, "relatedAddress", v),
            "rport" => {
                if let Ok(n) = v.parse::<f64>() {
                    if n.is_finite() {
                        set_num(scope, st, "relatedPort", n);
                    }
                }
            }
            "tcptype" => {
                let lv = v.to_ascii_lowercase();
                if TCP_TYPES.contains(&lv.as_str()) {
                    set_str(scope, st, "tcpType", &lv);
                }
            }
            // NOT `ufrag`: Blink's `RTCIceCandidatePlatform::PopulateFields` takes `usernameFragment` from the
            // dictionary and never from the line (Chrome 150, measured: a constructed candidate whose line says
            // `ufrag Xy12` answers `usernameFragment` null). A gathered candidate's init names its ufrag.
            _ => {}
        }
        i += 2;
    }
}

/// `new RTCIceCandidate(candidateInitDict)`.
///
/// The constructor's own first step: a candidate that names neither an m-section by id nor by index
/// cannot be added to any connection, so it is not constructible -- which is why
/// `new RTCIceCandidate()` is a TypeError and is recorded as one.
pub(crate) fn cb_ice_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("RTCIceCandidate");
        throw_type_error(scope, &msg);
        return;
    }
    // `RTCIceCandidateInit`, converted as WebIDL converts it (Chrome 150): `candidate`,
    // `sdpMLineIndex`, `sdpMid`, `usernameFragment`, each read once (korvus read `sdpMid` before `sdpMLineIndex`,
    // and `usernameFragment` only after its check)
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "RTCIceCandidateInit", "Failed to construct 'RTCIceCandidate': ") else { return };
    let init: v8::Local<v8::Value> = init.into();
    let candidate = match dict_get(scope, init, "candidate") {
        Some(v) => match to_str(scope, v) {
            Some(s) => s,
            None => return,
        },
        None => String::new(),
    };
    let sdp_mid = match dict_get(scope, init, "sdpMid") {
        Some(v) => match to_str(scope, v) {
            Some(s) => Some(s),
            None => return,
        },
        None => None,
    };
    // `unsigned short`: the WebIDL type is 16-bit, so the 32-bit conversion is taken modulo 2^16.
    let sdp_mline = dict_get(scope, init, "sdpMLineIndex").map(|v| to_ulong(scope, v) % 65536);
    if sdp_mid.is_none() && sdp_mline.is_none() {
        throw_type_error(scope, "Failed to construct 'RTCIceCandidate': sdpMid and sdpMLineIndex are both null.");
        return;
    }
    let ufrag = match dict_get(scope, init, "usernameFragment") {
        Some(v) => match to_str(scope, v) {
            Some(s) => Some(s),
            None => return,
        },
        None => None,
    };

    let st = attach_state(scope, args.this(), ICE);
    parse_candidate_line(scope, st, &candidate);
    set_str(scope, st, "candidate", &candidate);
    match &sdp_mid {
        Some(m) => set_str(scope, st, "sdpMid", m),
        None => set_null(scope, st, "sdpMid"),
    }
    match sdp_mline {
        Some(i) => set_num(scope, st, "sdpMLineIndex", i as f64),
        None => set_null(scope, st, "sdpMLineIndex"),
    }
    if let Some(u) = &ufrag {
        set_str(scope, st, "usernameFragment", u);
    }
    // `relayProtocol` and `url` describe the ICE server a candidate was gathered FROM. A candidate a
    // page constructed by hand was gathered from nothing.
    set_null(scope, st, "relayProtocol");
    set_null(scope, st, "url");
}

/// A GATHERED candidate's `url`: the STUN server it came from (Chrome 150: `"stun:host:port"` as configured;
/// a host candidate's is `""`, set here too). A candidate a page constructs keeps its null.
pub(crate) fn set_gathered_url(scope: &mut v8::PinScope, cand: v8::Local<v8::Value>, url: &str) {
    if let Some(st) = state_of(scope, cand, "RTCIceCandidate") {
        set_str(scope, st, "url", url);
    }
}

fn cb_ice_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(st) = state_of(scope, args.this().into(), ICE) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = field(scope, st, &name);
    rv.set(v);
}

/// `toJSON()`: the members of `RTCIceCandidateInit`, which is the dictionary the candidate can be rebuilt
/// from -- not the twelve derived ones -- built as Blink's `RTCIceCandidate::toJSONForBinding` builds them:
/// `candidate`, `sdpMid` and `usernameFragment` through `V8ObjectBuilder::AddString`, which writes a null
/// string as `""`, and `sdpMLineIndex` only when the candidate has one (Chrome 150, measured:
/// `new RTCIceCandidate({candidate: '', sdpMid: '0'}).toJSON()` is `{candidate: "", sdpMid: "0",
/// usernameFragment: ""}`; korvus wrote `sdpMLineIndex: null` and `usernameFragment: null`).
fn cb_ice_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(st) = state_of(scope, args.this().into(), ICE) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let out = v8::Object::new(scope);
    for k in ["candidate", "sdpMid", "sdpMLineIndex", "usernameFragment"] {
        let v = field(scope, st, k);
        if k == "sdpMLineIndex" {
            if v.is_number() {
                set_field(scope, out, k, v);
            }
        } else if v.is_null_or_undefined() {
            set_str(scope, out, k, "");
        } else {
            set_field(scope, out, k, v);
        }
    }
    rv.set(out.into());
}

pub fn install_rtc_ice_candidate(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" => method_row(scope, proto, member, cb_ice_to_json),
        // Every other row is one readonly field of the parsed candidate, read by name.
        _ => acc_field(scope, proto, member, cb_ice_get),
    }
}

// ---- the tables ---------------------------------------------------------------------------------

webapi::interface! {
    "RTCSessionDescription" [value, window] {
    ctor   "RTCSessionDescription" len 0                = stub,
    attr   "type" : Object                              = install_rtc_session_description,
    attr   "sdp" : Object                               = install_rtc_session_description,
    method "toJSON" : Object                            = install_rtc_session_description
    }
}

webapi::interface! {
    "RTCIceCandidate" [value, window] {
    ctor   "RTCIceCandidate" len 0                       = stub,
    attr   "candidate" : Object readonly                 = install_rtc_ice_candidate,
    attr   "sdpMid" : Object readonly nullable           = install_rtc_ice_candidate,
    attr   "sdpMLineIndex" : Object readonly nullable    = install_rtc_ice_candidate,
    attr   "foundation" : Object readonly nullable       = install_rtc_ice_candidate,
    attr   "component" : Object readonly nullable        = install_rtc_ice_candidate,
    attr   "priority" : Object readonly nullable         = install_rtc_ice_candidate,
    attr   "address" : Object readonly nullable          = install_rtc_ice_candidate,
    attr   "protocol" : Object readonly nullable         = install_rtc_ice_candidate,
    attr   "port" : Object readonly nullable             = install_rtc_ice_candidate,
    attr   "type" : Object readonly nullable             = install_rtc_ice_candidate,
    attr   "tcpType" : Object readonly nullable          = install_rtc_ice_candidate,
    attr   "relatedAddress" : Object readonly nullable   = install_rtc_ice_candidate,
    attr   "relatedPort" : Object readonly nullable      = install_rtc_ice_candidate,
    attr   "usernameFragment" : Object readonly nullable = install_rtc_ice_candidate,
    attr   "relayProtocol" : Object readonly nullable    = install_rtc_ice_candidate,
    attr   "url" : Object readonly nullable              = install_rtc_ice_candidate,
    method "toJSON" : Object                             = install_rtc_ice_candidate
    }
}
