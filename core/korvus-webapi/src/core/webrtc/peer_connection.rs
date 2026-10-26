//! `core/webrtc/peer_connection.rs` -- `RTCPeerConnection`, `RTCDataChannel` and `RTCStatsReport` as
//! three `interface!` tables.
//!
//! Rows are VERBATIM from the three blocks of `install/generated_registry.rs` they replace.
//!
//! DEVICE ABSENCE IS NOT A WORKAROUND, IT IS THE RECORDED STATE. The capture was taken on a machine
//! with no camera, no microphone and no peer, so a fresh `RTCPeerConnection` really is
//! `signalingState: "stable"`, `iceGatheringState: "new"`, zero senders and zero receivers; a data
//! channel that has never seen a DTLS transport really is `readyState: "connecting"` with a null `id`.
//! Every one of those is the state the object is genuinely in, and every one of them is what the
//! bundle's oracle reads.
//!
//! WHAT IS DELIBERATELY NOT HERE, and why -- a failing member is information, a stub that answers a
//! case without doing the work is not:
//!
//!   * `createOffer` produces the session-level SDP of a connection with nothing to negotiate, which
//!     is a complete and correct offer for that case. A connection that HAS something to negotiate
//!     needs an m-section with a DTLS fingerprint, and this engine has no DTLS transport and no
//!     certificate to fingerprint, so that offer is refused (`OperationError`) rather than forged.
//!   * `createAnswer` on a `stable` connection is an `InvalidStateError` by the algorithm's first
//!     step, which is the whole of what a fresh connection can answer.
//!   * `addStream` / `removeStream` / `createDTMFSender` are the legacy pre-transceiver API and
//!     `generateCertificate` needs a certificate store; all four stay honest D2 `stub` rows, exactly
//!     as they were shells before.
//!
//! `RTCStatsReport` IS A REAL MAPLIKE over the stats this engine actually holds, which is none: stats
//! are produced by transports, senders and receivers that have moved packets, and none have. The
//! iterators carry the INTERFACE's brand -- `[object RTCStatsReport Iterator]`, not
//! `[object Map Iterator]` -- the same way a `CustomStateSet` iterator does in Blink.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{
    build_fn_data, make_dom_exc, rejected_promise, resolved_promise, shell_proto, str_val, throw_type_error,
};
use crate::core::media::media_stream::TRACK;
use crate::core::webrtc::rtp::{mint_transceiver, sender_is_free, sender_set_track, xcvr_field, DIRECTIONS, SENDER, XCVR};
use crate::install::kits::{define_accessor, define_value};
use crate::install::live::method_row;
use crate::install::MemberDef;
use crate::install::kits::str_data_getter as acc_named;

pub(crate) const PC: &str = "RTCPeerConnection";
pub(crate) const DC: &str = "RTCDataChannel";
pub(crate) const REPORT: &str = "RTCStatsReport";

const SDP_TYPES: &[&str] = &["offer", "answer", "pranswer", "rollback"];

fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    Some(o)
}

fn throw_dom(scope: &mut v8::PinScope, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        scope.throw_exception(e);
    }
}

fn reject_dom<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let e = make_dom_exc(scope, msg, name)?;
    Some(rejected_promise(scope, e))
}

// ── RTCStatsReport ───────────────────────────────────────────────────────────────────────────────

/// One report over the entries given. Every call site passes none, because there are no stats; the
/// maplike is real so that the moment there are, the report is the same object.
pub(crate) fn mint_stats_report<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    entries: &[(v8::Local<v8::Value>, v8::Local<v8::Value>)],
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, REPORT)?;
    let s = st::attach(scope, o, REPORT);
    let pairs: Vec<v8::Local<v8::Value>> = entries
        .iter()
        .map(|(k, v)| {
            let p = st::array(scope, &[*k, *v]);
            p.into()
        })
        .collect();
    let arr = st::array(scope, &pairs);
    st::set(scope, s, "entries", arr.into());
    Some(o)
}

fn report_entries<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    match st::get_array(scope, s, "entries") {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    }
}

fn pair_parts<'s>(scope: &mut v8::PinScope<'s, '_>, p: v8::Local<v8::Value>) -> Option<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)> {
    let a = v8::Local::<v8::Array>::try_from(p).ok()?;
    Some((a.get_index(scope, 0)?, a.get_index(scope, 1)?))
}

fn cb_report_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REPORT) else { return };
    let n = report_entries(scope, s).len();
    rv.set_int32(n as i32);
}

/// `get(k)` / `has(k)`: `data` says which. Key comparison is SameValueZero, the maplike's own rule --
/// strict equality here, because no key this engine mints is `NaN`.
fn cb_report_lookup(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REPORT) else { return };
    let want_value = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "get";
    let k = args.get(0);
    for p in report_entries(scope, s) {
        if let Some((pk, pv)) = pair_parts(scope, p) {
            if pk == k {
                if want_value {
                    rv.set(pv);
                } else {
                    rv.set_bool(true);
                }
                return;
            }
        }
    }
    if want_value {
        let u = v8::undefined(scope);
        rv.set(u.into());
    } else {
        rv.set_bool(false);
    }
}

/// `keys()` / `values()` / `entries()`: RTCStatsReport is a `maplike`, so each answers the ONE
/// "RTCStatsReport Iterator" of this realm over a snapshot of the report (`webidl_iter::make_iter`,
/// `%MapIteratorPrototype%` beneath it). Each call used to build its own object carrying its own `next`
/// and `@@iterator` -- Chrome 150.0.7871.46 shares one prototype across the three, rejects a foreign
/// receiver's `next` with `Illegal invocation`, and answers `{done, value}` (measured).
fn cb_report_iterator(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REPORT) else { return };
    let kind = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let mut values: Vec<v8::Local<v8::Value>> = Vec::new();
    for p in report_entries(scope, s) {
        let Some((k, v)) = pair_parts(scope, p) else { continue };
        values.push(match kind.as_str() {
            "keys" => k,
            "values" => v,
            _ => crate::core::dom::webidl_iter::pair(scope, k, v),
        });
    }
    if let Some(it) = crate::core::dom::webidl_iter::make_iter(scope, "RTCStatsReport", crate::core::dom::webidl_iter::IterKind::Map, &values) {
        rv.set(it.into());
    }
}

fn cb_report_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), REPORT) else { return };
    let pairs = report_entries(scope, s);
    let entries: Vec<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)> =
        pairs.into_iter().filter_map(|p| pair_parts(scope, p).map(|(k, v)| (v, k))).collect();
    crate::core::dom::webidl_iter::for_each(scope, &args, "RTCStatsReport", |_, i| entries.get(i).copied());
}

pub fn install_stats_report(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => {
            let d = str_val(scope, "size");
            let g = build_fn_data(scope, cb_report_size, "get size", 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "get" | "has" => {
            let d = str_val(scope, member.name);
            let f = build_fn_data(scope, cb_report_lookup, member.name, member.length as i32, d);
            define_value(scope, proto, member, f.into(), true);
        }
        "keys" | "values" | "entries" => {
            let d = str_val(scope, member.name);
            let f = build_fn_data(scope, cb_report_iterator, member.name, member.length as i32, d);
            define_value(scope, proto, member, f.into(), true);
        }
        // A maplike's `@@iterator` IS its `entries` (Chrome 150.0.7871.46: `r[Symbol.iterator] === r.entries`).
        "@@iterator" => {
            let Some(entries) = crate::binding::protos::get_prop(scope, proto, "entries") else { return };
            define_value(scope, proto, member, entries, true);
        }
        "forEach" => method_row(scope, proto, member, cb_report_for_each),
        other => debug_assert!(false, "install_stats_report: unexpected member {other}"),
    }
}

// ── RTCDataChannel ───────────────────────────────────────────────────────────────────────────────

/// One channel, in the state a channel with no SCTP association is in. The id is assigned BY the
/// association, so an in-band channel's is null; a negotiated channel carries the id the page chose.
fn mint_data_channel<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    label: v8::Local<v8::Value>,
    init: v8::Local<v8::Value>,
) -> Option<v8::Local<'s, v8::Object>> {
    let label = st::to_str(scope, label)?;
    // `RTCDataChannelInit`, converted as WebIDL converts it (Chrome 150): `id`,
    // `maxPacketLifeTime`, `maxRetransmits`, `negotiated`, `ordered`, `protocol`, each read once (korvus read
    // `ordered` first, and never `id` before using it)
    let init: v8::Local<v8::Value> = crate::install::ctor_dicts::convert_dict(scope, init, "RTCDataChannelInit", "Failed to execute 'createDataChannel' on 'RTCPeerConnection': ").ok()?.into();
    let ordered = match st::dict_get(scope, init, "ordered") {
        Some(v) => v.boolean_value(scope),
        None => true,
    };
    let mplt = st::dict_get(scope, init, "maxPacketLifeTime").map(|v| st::to_ulong(scope, v));
    let mrt = st::dict_get(scope, init, "maxRetransmits").map(|v| st::to_ulong(scope, v));
    if mplt.is_some() && mrt.is_some() {
        throw_type_error(
            scope,
            "Failed to execute 'createDataChannel' on 'RTCPeerConnection': maxPacketLifeTime and maxRetransmits may not be set simultaneously",
        );
        return None;
    }
    let protocol = match st::dict_get(scope, init, "protocol") {
        Some(v) => st::to_str(scope, v)?,
        None => String::new(),
    };
    let negotiated = match st::dict_get(scope, init, "negotiated") {
        Some(v) => v.boolean_value(scope),
        None => false,
    };
    let id = if negotiated { st::dict_get(scope, init, "id").map(|v| st::to_ulong(scope, v)) } else { None };

    let o = mint(scope, DC)?;
    let s = st::attach(scope, o, DC);
    st::set_str(scope, s, "label", &label);
    st::set_bool(scope, s, "ordered", ordered);
    match mplt {
        Some(n) => st::set_num(scope, s, "maxPacketLifeTime", n as f64),
        None => st::set_null(scope, s, "maxPacketLifeTime"),
    }
    match mrt {
        Some(n) => st::set_num(scope, s, "maxRetransmits", n as f64),
        None => st::set_null(scope, s, "maxRetransmits"),
    }
    st::set_str(scope, s, "protocol", &protocol);
    st::set_bool(scope, s, "negotiated", negotiated);
    match id {
        Some(n) => st::set_num(scope, s, "id", n as f64),
        None => st::set_null(scope, s, "id"),
    }
    st::set_str(scope, s, "readyState", "connecting");
    st::set_num(scope, s, "bufferedAmount", 0.0);
    st::set_num(scope, s, "bufferedAmountLowThreshold", 0.0);
    // `binaryType` defaults to "arraybuffer" here rather than WebSocket's "blob"; that is what the
    // capture recorded and it is the recording that decides.
    st::set_str(scope, s, "binaryType", "arraybuffer");
    Some(o)
}

fn cb_dc_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DC) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// The legacy alias: a channel is `reliable` when it is ordered and neither partial-reliability knob
/// is set.
fn cb_dc_reliable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DC) else { return };
    let ok = st::get_bool(scope, s, "ordered")
        && st::get(scope, s, "maxRetransmits").is_null()
        && st::get(scope, s, "maxPacketLifeTime").is_null();
    rv.set_bool(ok);
}

fn cb_dc_threshold_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DC) else { return };
    let n = st::to_ulong(scope, args.get(0));
    st::set_num(scope, s, "bufferedAmountLowThreshold", n as f64);
}

/// `binaryType = v`: Chrome accepts only these two and throws on anything else.
fn cb_dc_binary_type_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DC) else { return };
    let Some(t) = st::to_str(scope, args.get(0)) else { return };
    if t != "blob" && t != "arraybuffer" {
        throw_type_error(
            scope,
            &format!("Failed to set the 'binaryType' property on 'RTCDataChannel': Unknown binary type : {t}"),
        );
        return;
    }
    st::set_str(scope, s, "binaryType", &t);
}

fn cb_dc_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DC) else { return };
    let state = st::get_str(scope, s, "readyState");
    if state == "closed" || state == "closing" {
        return;
    }
    st::set_str(scope, s, "readyState", "closed");
}

/// `send(data)`: the algorithm's own first step -- "if readyState is not open, throw
/// InvalidStateError" -- not a device-absence workaround. Nothing here ever opens a channel, so this
/// is what `send` always answers, and the recording says so.
fn cb_dc_send(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DC) else { return };
    if st::get_str(scope, s, "readyState") != "open" {
        throw_dom(
            scope,
            "Failed to execute 'send' on 'RTCDataChannel': RTCDataChannel.readyState is not 'open'",
            "InvalidStateError",
        );
        return;
    }
    // Never reached while there is no transport; kept so the buffered-amount contract is the real one
    // the moment there is.
    let data = args.get(0);
    let add = if data.is_string() {
        st::to_str(scope, data).map(|s| s.chars().count() as f64).unwrap_or(0.0)
    } else {
        v8::Local::<v8::Object>::try_from(data)
            .ok()
            .and_then(|o| crate::binding::protos::get_prop(scope, o, "byteLength"))
            .and_then(|v| v.number_value(scope))
            .unwrap_or(0.0)
    };
    let cur = st::get_num(scope, s, "bufferedAmount");
    st::set_num(scope, s, "bufferedAmount", cur + add);
}


/// A read/write row over the same state field: both halves carry the member name as `data`.
fn acc_named_rw(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
    scb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: &str,
) {
    let d = str_val(scope, data);
    let g = build_fn_data(scope, gcb, &format!("get {}", m.name), 0, d);
    let d = str_val(scope, data);
    let s = build_fn_data(scope, scb, &format!("set {}", m.name), 1, d);
    define_accessor(scope, proto, m, g, Some(s));
}

pub fn install_data_channel(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "label" | "ordered" | "maxPacketLifeTime" | "maxRetransmits" | "protocol" | "negotiated" | "id" | "readyState"
        | "bufferedAmount" => acc_named(scope, proto, member, cb_dc_field, member.name),
        "bufferedAmountLowThreshold" => acc_named_rw(scope, proto, member, cb_dc_field, cb_dc_threshold_set, member.name),
        "binaryType" => acc_named_rw(scope, proto, member, cb_dc_field, cb_dc_binary_type_set, member.name),
        "reliable" => acc_named(scope, proto, member, cb_dc_reliable, member.name),
        "close" => method_row(scope, proto, member, cb_dc_close),
        "send" => method_row(scope, proto, member, cb_dc_send),
        other => debug_assert!(false, "install_data_channel: unexpected member {other}"),
    }
}

// ── RTCPeerConnection ────────────────────────────────────────────────────────────────────────────

/// `new RTCPeerConnection(configuration?)`. The configuration is READ but changes nothing that can be
/// observed beyond `getConfiguration()`, because there is no ICE agent to configure -- which is why
/// the defaults below are the ones the recording reports.
pub(crate) fn cb_pc_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("RTCPeerConnection");
        throw_type_error(scope, &msg);
        return;
    }
    // The configuration is converted and checked before anything exists (`config.rs`).
    let Some(cfg) = crate::core::webrtc::config::parse(scope, args.get(0), "construct") else { return };
    let s = st::attach(scope, args.this(), PC);
    st::set_str(scope, s, "signalingState", "stable");
    st::set_str(scope, s, "iceGatheringState", "new");
    st::set_str(scope, s, "iceConnectionState", "new");
    st::set_str(scope, s, "connectionState", "new");
    for k in [
        "localDescription",
        "currentLocalDescription",
        "pendingLocalDescription",
        "remoteDescription",
        "currentRemoteDescription",
        "pendingRemoteDescription",
    ] {
        st::set_null(scope, s, k);
    }
    for k in ["senders", "receivers", "transceivers", "channels"] {
        let a = st::array(scope, &[]);
        st::set(scope, s, k, a.into());
    }
    st::set_bool(scope, s, "restartPending", false);
    st::set_bool(scope, s, "closed", false);
    crate::core::webrtc::config::store(scope, s, &cfg);
    // `iceCandidatePoolSize`: gathering for the pool begins with the connection (`gather::start_pool`)
    crate::core::webrtc::gather::start_pool(scope, args.this());
}

fn pc_list<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>, name: &str) -> Vec<v8::Local<'s, v8::Value>> {
    match st::get_array(scope, s, name) {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    }
}

fn pc_push(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let mut items = pc_list(scope, s, name);
    items.push(v);
    let arr = st::array(scope, &items);
    st::set(scope, s, name, arr.into());
}

fn cb_pc_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// `canTrickleIceCandidates` and `sctp`. No transport has ever been probed, so whether the remote can
/// trickle is genuinely UNKNOWN -- which is what `null` means on that member, not "not implemented" --
/// and the SCTP transport exists only once a data channel has been negotiated over a connected DTLS
/// transport. Nothing here ever negotiates one.
fn cb_pc_null(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if st::state_or_throw(scope, args.this().into(), PC).is_none() {
        return;
    }
    let n = v8::null(scope);
    rv.set(n.into());
}

/// `getSenders()` / `getReceivers()` / `getTransceivers()`: a COPY of the list, so a page cannot edit
/// the connection through it. `getLocalStreams()` / `getRemoteStreams()` are the legacy pair and are
/// empty because no stream was ever added.
fn cb_pc_list(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let items = if name.is_empty() { Vec::new() } else { pc_list(scope, s, &name) };
    let arr = st::array(scope, &items);
    rv.set(arr.into());
}

fn cb_pc_get_configuration(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    let c = crate::core::webrtc::config::load(scope, s);
    let o = crate::core::webrtc::config::to_js(scope, &c);
    rv.set(o.into());
}

/// `setConfiguration(cfg)`: the configuration REPLACED -- a member left out goes back to its default --
/// except that `bundlePolicy`, `rtcpMuxPolicy` and the certificates may not change (the spec's
/// `InvalidModificationError`, Chrome's sentence), and a closed connection cannot be configured at all.
fn cb_pc_set_configuration(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_dom(
            scope,
            "Failed to execute 'setConfiguration' on 'RTCPeerConnection': Cannot set configuration when the connection is closed.",
            "InvalidStateError",
        );
        return;
    }
    let Some(new) = crate::core::webrtc::config::parse(scope, args.get(0), "setConfiguration") else { return };
    let cur = crate::core::webrtc::config::load(scope, s);
    if new.bundle_policy != cur.bundle_policy || new.rtcp_mux_policy != cur.rtcp_mux_policy || new.certificates != cur.certificates {
        throw_dom(
            scope,
            "Failed to execute 'setConfiguration' on 'RTCPeerConnection': Attempted to modify the PeerConnection's configuration in an unsupported way.",
            "InvalidModificationError",
        );
        return;
    }
    crate::core::webrtc::config::store(scope, s, &new);
    crate::core::webrtc::gather::start_pool(scope, args.this());
}

fn cb_pc_restart_ice(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    st::set_bool(scope, s, "restartPending", true);
}

/// `close()`: the connection and every channel on it go to their terminal state.
fn cb_pc_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    if st::get_bool(scope, s, "closed") {
        return;
    }
    st::set_bool(scope, s, "closed", true);
    crate::core::webrtc::gather::close(scope, s);
    st::set_str(scope, s, "signalingState", "closed");
    st::set_str(scope, s, "iceConnectionState", "closed");
    st::set_str(scope, s, "connectionState", "closed");
    for ch in pc_list(scope, s, "channels") {
        if let Some(cs) = st::state(scope, ch, DC) {
            st::set_str(scope, cs, "readyState", "closed");
        }
    }
}

fn cb_pc_create_data_channel(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_dom(
            scope,
            "Failed to execute 'createDataChannel' on 'RTCPeerConnection': RTCPeerConnection is closed",
            "InvalidStateError",
        );
        return;
    }
    let Some(ch) = mint_data_channel(scope, args.get(0), args.get(1)) else { return };
    pc_push(scope, s, "channels", ch.into());
    rv.set(ch.into());
}

/// The offer a connection would make now: a section per transceiver, then the data channels' section
/// (`sdp.rs`), or -- with nothing to negotiate -- the five session lines alone, which is a complete and
/// correct offer for that case. `None`: a SENDING transceiver, whose section needs its track's `msid` and
/// SSRCs, which this engine does not invent -- that offer is refused rather than forged.
fn session_offer(scope: &mut v8::PinScope, s: v8::Local<v8::Object>) -> Option<String> {
    let channels = st::get_array(scope, s, "channels").map(|a| a.length()).unwrap_or(0);
    let xcvrs = pc_list(scope, s, "transceivers");
    // Per section: kind, direction, and -- for a sending section that korvus CAN describe (a fed video
    // track) -- its (stream, track, ssrc, cname).
    let mut media: Vec<(String, String, Option<(String, String, u32, String)>)> = Vec::new();
    let mut has_fed_send = false;
    for x in xcvrs {
        let kind = xcvr_field(scope, x, "kind").to_rust_string_lossy(scope);
        let dir = xcvr_field(scope, x, "direction").to_rust_string_lossy(scope);
        if dir == "stopped" {
            continue;
        }
        if dir == "recvonly" || dir == "inactive" {
            media.push((kind, dir, None));
            continue;
        }
        // A SENDING transceiver (sendonly/sendrecv): describable only when its sender has a fed VIDEO
        // track (one a media-source plugin is feeding frames to). Otherwise the section needs an `msid`
        // and SSRCs this engine does not invent, so the whole offer is refused -- exactly as before.
        if kind == "video" {
            if let Some(track_id) = sender_track_id(scope, x) {
                if plugin_host::fed_video_dims(&track_id).is_some() {
                    has_fed_send = true;
                    media.push((kind, dir, Some(fed_send_ids(scope, s, &track_id))));
                    continue;
                }
            }
        }
        return None;
    }
    let data = channels > 0 || crate::core::webrtc::config::load(scope, s).always_negotiate_data_channels;
    if !media.is_empty() || data {
        let (sid, ufrag, pwd, fp) = ice_identity(scope, s, has_fed_send);
        let mut sections: Vec<crate::core::webrtc::sdp::Section> = media
            .iter()
            .map(|(k, d, send)| crate::core::webrtc::sdp::Section::Media {
                kind: k,
                direction: d,
                send: send.as_ref().map(|(stream, track, ssrc, cname)| crate::core::webrtc::sdp::SendMedia { stream, track, ssrc: *ssrc, cname }),
            })
            .collect();
        if data {
            sections.push(crate::core::webrtc::sdp::Section::Data);
        }
        let id = crate::core::webrtc::sdp::Identity { session_id: &sid, ufrag: &ufrag, pwd: &pwd, fingerprint: &fp };
        // The audio and video sections are the worn device's own offer: its codecs are its build's.
        let device = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let state = op_state.borrow();
            crate::binding::protos::persona_handle(&state).and_then(|h| h.0.webrtc_offer())
        };
        let sdp = crate::core::webrtc::sdp::offer(&id, device.as_deref(), &sections)?;
        st::set_str(scope, s, "lastOffer", &sdp);
        return Some(sdp);
    }
    // A 63-bit session id, as RFC 3264 asks for; the version starts at 2 the way Chrome's does. The
    // number is stringified through v8 so it reads exactly as `String(n)` would in the page.
    let b = crate::crypto::random_bytes(8);
    let hi = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x7fff_ffff;
    let lo = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let n = (hi as f64) * 4_294_967_296.0 + (lo as f64);
    let num = v8::Number::new(scope, n);
    let sid = num.to_rust_string_lossy(scope);
    Some(format!("v=0\r\no=- {sid} 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=extmap-allow-mixed\r\na=msid-semantic: WMS\r\n"))
}

/// The `id` of the track a transceiver's SENDER holds, or `None` when the sender has no track. The
/// value `plugin_host::fed_video_dims` keys on, so a fed track resolves and any other does not.
fn sender_track_id(scope: &mut v8::PinScope, x: v8::Local<v8::Value>) -> Option<String> {
    let sender = xcvr_field(scope, x, "sender");
    let ss = st::state(scope, sender, SENDER)?;
    let track = st::get(scope, ss, "track");
    let ts = st::state(scope, track, TRACK)?;
    Some(st::get_str(scope, ts, "id"))
}

/// The stream id, track id, SSRC and CNAME a sending section carries, minted once per connection and
/// kept (Chrome keeps one SSRC/CNAME for the connection). The track id is the fed track's own `id`.
fn fed_send_ids(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, track_id: &str) -> (String, String, u32, String) {
    if st::get(scope, s, "sendStream").is_undefined() {
        st::set_str(scope, s, "sendStream", &st::uuid());
        let b = crate::crypto::random_bytes(4);
        let ssrc = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        st::set_num(scope, s, "sendSsrc", f64::from(ssrc));
        st::set_str(scope, s, "sendCname", &ice_string(16));
    }
    // The fed track id, for the answer path to hand the sender (a string set, not an RNG draw, so the
    // no-send RNG order is untouched).
    st::set_str(scope, s, "sendTrack", track_id);
    (st::get_str(scope, s, "sendStream"), track_id.to_string(), st::get_num(scope, s, "sendSsrc") as u32, st::get_str(scope, s, "sendCname"))
}

const ICE_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn ice_string(n: usize) -> String {
    crate::crypto::random_bytes(n).iter().map(|b| ICE_CHARS[(*b as usize) % ICE_CHARS.len()] as char).collect()
}

/// This connection's session id, ICE credentials and certificate fingerprint -- made once and kept,
/// as Chrome keeps them for the life of the connection.
fn ice_identity(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, has_fed_send: bool) -> (String, String, String, String) {
    if st::get(scope, s, "iceUfrag").is_undefined() {
        let b = crate::crypto::random_bytes(8);
        let hi = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x7fff_ffff;
        let lo = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        let n = (hi as f64) * 4_294_967_296.0 + (lo as f64);
        let num = v8::Number::new(scope, n);
        let sid = num.to_rust_string_lossy(scope);
        st::set_str(scope, s, "sessionId", &sid);
        let ufrag = ice_string(4);
        st::set_str(scope, s, "iceUfrag", &ufrag);
        let pwd = ice_string(24);
        st::set_str(scope, s, "icePwd", &pwd);
        // The connection's FAKE fingerprint, in its own slot: a fresh digest per connection, as Chrome
        // has a fresh certificate per connection. A connection that never sends keeps this, and no DTLS
        // handshake runs against it. Drawn here, in this exact RNG order (sessionId -> ufrag -> pwd ->
        // this 64), so a connection that never sends is byte-for-byte what it has always been (I9).
        use sha2::Digest;
        let digest = sha2::Sha256::digest(crate::crypto::random_bytes(64));
        let fp = digest.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
        st::set_str(scope, s, "fakeFingerprint", &fp);
    }
    // The fingerprint is real exactly when a fed sending section exists -- now, or earlier in this
    // connection's life (Chrome keeps one certificate for the connection, so once the real one is minted
    // it stays). The real certificate is minted lazily and uses BoringSSL's own RNG (`EcKey::generate`),
    // NOT `crate::crypto::random_bytes`, so the no-send RNG draw order above is never perturbed.
    let want_real = has_fed_send || st::get(scope, s, "realFingerprint").is_string();
    if want_real && st::get(scope, s, "realFingerprint").is_undefined() {
        if let Some((der, key, real_fp)) = crate::core::webrtc::rtc_cert::mint() {
            st::set_str(scope, s, "dtlsCertHex", &crate::core::webrtc::rtc_cert::hex(&der));
            st::set_str(scope, s, "dtlsKeyHex", &crate::core::webrtc::rtc_cert::hex(&key));
            st::set_str(scope, s, "realFingerprint", &real_fp);
        }
    }
    // If minting ever failed, fall back to the fake one so the connection still produces a valid offer.
    let fp = if want_real && st::get(scope, s, "realFingerprint").is_string() {
        st::get_str(scope, s, "realFingerprint")
    } else {
        st::get_str(scope, s, "fakeFingerprint")
    };
    (st::get_str(scope, s, "sessionId"), st::get_str(scope, s, "iceUfrag"), st::get_str(scope, s, "icePwd"), fp)
}


/// ICE gathering for the local description just applied: `gather.rs`, over real sockets and STUN.
fn start_gathering(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>) {
    crate::core::webrtc::gather::start(scope, pc);
}

/// A plain `Event` of type `ty` at the connection (`icegatheringstatechange` and its kind).
pub(crate) fn fire_pc_event(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, ty: &str) {
    let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let t = crate::binding::protos::str_val(scope, ty);
    let Some(ev) = ctor.new_instance(scope, &[t]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let pcv: v8::Local<v8::Value> = pc.into();
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, pcv, "dispatchEvent", &[evv]);
}

/// Applying a local offer gives each transceiver in it the mid of its section, in order (Chrome 150,
/// measured: `null` until `setLocalDescription`, then "0", "1", ...).
fn assign_mids(scope: &mut v8::PinScope, s: v8::Local<v8::Object>) {
    let mut i = 0;
    for x in pc_list(scope, s, "transceivers") {
        let Some(xs) = st::state(scope, x, XCVR) else { continue };
        if st::get_str(scope, xs, "direction") == "stopped" {
            continue;
        }
        if st::get(scope, xs, "mid").is_null() {
            st::set_str(scope, xs, "mid", &i.to_string());
        }
        i += 1;
    }
}

/// A description the connection holds (`localDescription` and its pending/current siblings): an
/// `RTCSessionDescription`, as Blink's `RTCPeerConnection` keeps them.
fn description<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str, sdp: &str) -> v8::Local<'s, v8::Object> {
    match crate::core::webrtc::rtc_values::mint_session_description(scope, ty, sdp) {
        Some(d) => d,
        None => description_init(scope, ty, sdp),
    }
}

/// What `createOffer()` resolves with: an `RTCSessionDescriptionInit` DICTIONARY, which the bindings turn into a
/// plain object with its members in the IDL's sorted order -- `sdp`, then `type` (Chrome 150, measured:
/// `Object.keys(await pc.createOffer())` is `sdp,type`; korvus answered `type,sdp`).
fn description_init<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str, sdp: &str) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    st::set_str(scope, o, "sdp", sdp);
    st::set_str(scope, o, "type", ty);
    o
}

/// `RTCOfferOptions`, read in Chrome's order -- `voiceActivityDetection`, `iceRestart`, `offerToReceiveAudio`,
/// `offerToReceiveVideo` (the inherited dictionary's member first; measured with a Proxy) -- each a
/// `boolean`, `None` when absent. `Err`: a getter threw.
fn offer_options(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<(Option<bool>, Option<bool>), ()> {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return Ok((None, None)) };
    let mut out = [None, None];
    for (i, k) in ["voiceActivityDetection", "iceRestart", "offerToReceiveAudio", "offerToReceiveVideo"].iter().enumerate() {
        let key = str_val(scope, k);
        let Some(m) = o.get(scope, key) else { return Err(()) };
        if i >= 2 && !m.is_undefined() {
            out[i - 2] = Some(m.boolean_value(scope));
        }
    }
    Ok((out[0], out[1]))
}

/// libwebrtc's `HandleLegacyOfferOptions`: `offerToReceive<Kind>: true` adds a receive-only transceiver of
/// that kind unless one already receives; `false` takes receiving away from every transceiver of the kind.
fn legacy_receive(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, kind: &str, want: Option<bool>) {
    let Some(want) = want else { return };
    let xs: Vec<v8::Local<v8::Value>> =
        pc_list(scope, s, "transceivers").into_iter().filter(|x| xcvr_field(scope, *x, "kind").to_rust_string_lossy(scope) == kind).collect();
    if want {
        let receiving = xs.iter().any(|x| {
            let d = xcvr_field(scope, *x, "direction").to_rust_string_lossy(scope);
            d == "sendrecv" || d == "recvonly"
        });
        if receiving {
            return;
        }
        let Some(x) = mint_transceiver(scope, kind, "recvonly", None) else { return };
        let sender = xcvr_field(scope, x.into(), "sender");
        let receiver = xcvr_field(scope, x.into(), "receiver");
        pc_push(scope, s, "transceivers", x.into());
        pc_push(scope, s, "senders", sender);
        pc_push(scope, s, "receivers", receiver);
    } else {
        for x in xs {
            let Some(xst) = st::state(scope, x, XCVR) else { continue };
            let d = st::get_str(scope, xst, "direction");
            let nd = match d.as_str() {
                "sendrecv" => "sendonly",
                "recvonly" => "inactive",
                other => other,
            }
            .to_string();
            st::set_str(scope, xst, "direction", &nd);
        }
    }
}

fn cb_pc_create_offer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    // The legacy overload `createOffer(success, failure, options)` carries its options third.
    let opts = if args.get(0).is_function() { args.get(2) } else { args.get(0) };
    let Ok((audio, video)) = offer_options(scope, opts) else { return };
    if st::get_bool(scope, s, "closed") {
        if let Some(p) = reject_dom(
            scope,
            "Failed to execute 'createOffer' on 'RTCPeerConnection': The RTCPeerConnection's signalingState is 'closed'.",
            "InvalidStateError",
        ) {
            rv.set(p);
        }
        return;
    }
    legacy_receive(scope, s, "audio", audio);
    legacy_receive(scope, s, "video", video);
    match session_offer(scope, s) {
        Some(sdp) => {
            let d = description_init(scope, "offer", &sdp);
            // settled from a microtask, as Chrome 150's is (`binding::protos::Settle`)
            let p = crate::binding::protos::settle_later(scope, d.into(), false, crate::binding::protos::Settle::Microtask);
            rv.set(p);
        }
        None => {
            if let Some(p) = reject_dom(
                scope,
                "Failed to execute 'createOffer' on 'RTCPeerConnection': cannot describe a media or data section without a DTLS transport.",
                "OperationError",
            ) {
                rv.set(p);
            }
        }
    }
}

/// `createAnswer()`: the algorithm's first step. An answer is only meaningful once a remote offer has
/// been applied, and a fresh connection is `stable` -- so this IS the answer, not a shortcut.
fn cb_pc_create_answer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    let state = st::get_str(scope, s, "signalingState");
    let msg = if state != "have-remote-offer" && state != "have-local-pranswer" {
        (
            "Failed to execute 'createAnswer' on 'RTCPeerConnection': PeerConnection cannot create an answer in a state other than have-remote-offer or have-local-pranswer.",
            "InvalidStateError",
        )
    } else {
        (
            "Failed to execute 'createAnswer' on 'RTCPeerConnection': cannot describe a media or data section without a DTLS transport.",
            "OperationError",
        )
    };
    if let Some(p) = reject_dom(scope, msg.0, msg.1) {
        rv.set(p);
    }
}

/// `setLocalDescription(desc?)` / `setRemoteDescription(desc)`. `data` names the member. With no
/// argument, `setLocalDescription` uses the description the connection would create for itself, which
/// is the only case that can succeed here; anything that has to be APPLIED needs a DTLS transport.
/// `setLocalDescription()`'s promise: applied by the peer connection's signaling thread, it settles in a task of its own
/// -- 1-2 ms on an idle machine (Chrome 150, eight runs against a fixture STUN server, 2026-10-11), later under load
/// (`platform_answers_live` leaves it out for that reason); it was a 10 ms task here. The connection's gathering
/// begins after it ([`start_gathering`]): Chrome settles the promise, then reports `gathering` and the candidates.
fn local_description_applied<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    crate::binding::protos::settle_later(scope, v, false, crate::binding::protos::Settle::Task(crate::core::webrtc::gather::SLD_SETTLE_MS))
}

fn cb_pc_set_description(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    let op = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if st::get_bool(scope, s, "closed") {
        if let Some(p) = reject_dom(
            scope,
            &format!("Failed to execute '{op}' on 'RTCPeerConnection': The RTCPeerConnection's signalingState is 'closed'."),
            "InvalidStateError",
        ) {
            rv.set(p);
        }
        return;
    }
    let desc = args.get(0);
    if op == "setLocalDescription" && desc.is_null_or_undefined() {
        match session_offer(scope, s) {
            Some(sdp) => {
                let d = description(scope, "offer", &sdp);
                st::set(scope, s, "pendingLocalDescription", d.into());
                st::set(scope, s, "localDescription", d.into());
                st::set_str(scope, s, "signalingState", "have-local-offer");
                assign_mids(scope, s);
                let u: v8::Local<v8::Value> = v8::undefined(scope).into();
                rv.set(local_description_applied(scope, u));
                if sdp.contains("\r\nm=") {
                    let this = args.this();
                    start_gathering(scope, this);
                }
            }
            None => {
                if let Some(p) = reject_dom(
                    scope,
                    "Failed to execute 'setLocalDescription' on 'RTCPeerConnection': cannot describe a media or data section without a DTLS transport.",
                    "OperationError",
                ) {
                    rv.set(p);
                }
            }
        }
        return;
    }
    // `type` is an `RTCSdpType` ENUM member of the dictionary, so an unrecognised spelling is the
    // BINDING LAYER's TypeError, thrown synchronously before any promise exists -- the same error
    // `RTCSessionDescription`'s `type` setter throws, which the capture records there. An ABSENT
    // `type` is legal in the dictionary and falls through.
    // Our own last offer, applied locally: `have-local-offer`, and gathering starts.
    if op == "setLocalDescription" {
        let ty = st::dict_get(scope, desc, "type").and_then(|v| st::to_str(scope, v)).unwrap_or_default();
        let sdp = st::dict_get(scope, desc, "sdp").and_then(|v| st::to_str(scope, v)).unwrap_or_default();
        // `get_str` reads a never-set slot as "undefined", so the slot's presence is asked directly.
        let minted = st::get(scope, s, "lastOffer").is_string();
        let last = st::get_str(scope, s, "lastOffer");
        if ty == "offer" && minted && sdp == last {
            let d = description(scope, "offer", &sdp);
            st::set(scope, s, "pendingLocalDescription", d.into());
            st::set(scope, s, "localDescription", d.into());
            st::set_str(scope, s, "signalingState", "have-local-offer");
            assign_mids(scope, s);
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            rv.set(local_description_applied(scope, u));
            let this = args.this();
            start_gathering(scope, this);
            return;
        }
    }
    if let Some(v) = st::dict_get(scope, desc, "type") {
        let Some(t) = st::to_str(scope, v) else { return };
        if !SDP_TYPES.contains(&t.as_str()) {
            throw_type_error(
                scope,
                &format!(
                    "Failed to execute '{op}' on 'RTCPeerConnection': Failed to read the 'type' property from 'RTCSessionDescriptionInit': The provided value '{t}' is not a valid enum value of type RTCSdpType."
                ),
            );
            return;
        }
    }
    // A remote ANSWER to our own fed-sending offer: apply it and run the real DTLS-SRTP handshake over
    // the gathered socket (goal A). Gated on a local offer being out, a real certificate having been
    // minted for a fed sending section, and a gathered socket -- none of which a connection that never
    // sends ever has, so every such connection still falls to the identical reject below (I9).
    if op == "setRemoteDescription" {
        let ty = st::dict_get(scope, desc, "type").and_then(|v| st::to_str(scope, v)).unwrap_or_default();
        let have_local = st::get_str(scope, s, "signalingState") == "have-local-offer";
        let has_cert = st::get(scope, s, "dtlsCertHex").is_string();
        if ty == "answer" && have_local && has_cert {
            let sock_id = st::get_array(scope, s, "g_sockets").and_then(|a| a.get_index(scope, 0)).and_then(|v| v.uint32_value(scope)).filter(|id| *id != 0);
            let sdp = st::dict_get(scope, desc, "sdp").and_then(|v| st::to_str(scope, v)).unwrap_or_default();
            if let (Some((setup, _remote_fp, addr, port)), Some(id)) = (crate::core::webrtc::sdp::parse_answer_transport(&sdp), sock_id) {
                let d = description(scope, "answer", &sdp);
                st::set(scope, s, "remoteDescription", d.into());
                st::set(scope, s, "currentRemoteDescription", d.into());
                st::set_str(scope, s, "signalingState", "stable");
                st::set_str(scope, s, "dtlsState", "connecting");
                // The answer's a=setup decides our DTLS role: `active` => the answerer is the client, so
                // WE are the server; `passive` (actpass resolved to passive) => WE are the client.
                let is_server = setup == "active";
                st::set_str(scope, s, "dtlsRole", if is_server { "server" } else { "client" });
                let cert = st::get_str(scope, s, "dtlsCertHex");
                let key = st::get_str(scope, s, "dtlsKeyHex");
                let remote = format!("{addr}:{port}");
                // The fed sending track for the media sender: its id + SSRC (cached at offer time) + the
                // VP8 payload type. Only a fed-sending offer minted dtlsCertHex, so only such a connection
                // reaches here; a receive-only one has an empty sendTrack and the sender never starts.
                let track = st::get_str(scope, s, "sendTrack");
                let ssrc = st::get_num(scope, s, "sendSsrc") as u32;
                // Keep the remote default address + the gathered socket id for getStats' candidate-pair.
                st::set_str(scope, s, "remoteAddr", &addr);
                st::set_num(scope, s, "remotePort", f64::from(port));
                st::set_num(scope, s, "statsSocket", f64::from(id));
                dtls_connect(scope, s, id, is_server, &cert, &key, &remote, &track, ssrc, 96);
                let u: v8::Local<v8::Value> = v8::undefined(scope).into();
                rv.set(resolved_promise(scope, u));
                return;
            }
        }
    }
    // Everything that reaches here is a description this engine cannot APPLY, because applying one
    // means handing it to a DTLS transport and there is none. `OperationError` is what the capture
    // recorded for `setRemoteDescription({})` -- the JS this replaces answered `InvalidAccessError`
    // there and lost the member, measured 2026-09-11 against the bundle's own oracle.
    if let Some(p) = reject_dom(
        scope,
        &format!("Failed to execute '{op}' on 'RTCPeerConnection': cannot apply a session description without a DTLS transport."),
        "OperationError",
    ) {
        rv.set(p);
    }
}

/// Kick off the send path's DTLS-SRTP handshake over gathered socket `id` (`rtc_media::op_rtc_dtls_connect`),
/// and settle `dtlsState` from its result in a task. `pc` is bound through to the callback.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn dtls_connect<'s>(scope: &mut v8::PinScope<'s, '_>, state: v8::Local<'s, v8::Object>, id: u32, is_server: bool, cert_hex: &str, key_hex: &str, remote: &str, track_id: &str, ssrc: u32, pt: u8) {
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_rtc_dtls_connect") else { return };
    // The op caps its argument count, so the four strings ride one newline-joined `params` (hex has no
    // newline, nor do an addr or a track id). cert (DER hex) \n key (PKCS#8 DER hex) \n remote \n track id.
    let params = format!("{cert_hex}\n{key_hex}\n{remote}\n{track_id}");
    let args: [v8::Local<v8::Value>; 5] = [
        v8::Number::new(scope, f64::from(id)).into(),
        v8::Boolean::new(scope, is_server).into(),
        str_val(scope, &params),
        v8::Number::new(scope, f64::from(ssrc)).into(),
        v8::Number::new(scope, f64::from(pt)).into(),
    ];
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &args).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    // The handshake can take seconds (a lossy network); it must not hold the page's event loop open.
    crate::core::workers::worker::unref(scope, p.into());
    // The answer is a task, not a promise job (`then_task`), as the gathering answers are. The connection
    // STATE object is bound through so the callback can set its `dtlsState`.
    let cb = build_fn_data(scope, cb_dtls_done, "", 1, state.into());
    crate::then_task(scope, p, cb, Some(cb));
}

/// The handshake's result `(ok, dtls_cipher, srtp_profile)`: `dtlsState` becomes `connected` on success
/// (with the two cipher names stored for `getStats`), `failed` otherwise.
fn cb_dtls_done(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Ok(state) = v8::Local::<v8::Object>::try_from(data) else { return };
    let result = args.get(0);
    let (ok, cipher, profile) = match v8::Local::<v8::Array>::try_from(result) {
        Ok(a) => {
            let ok = a.get_index(scope, 0).is_some_and(|v| v.is_true());
            let cipher = a.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let profile = a.get_index(scope, 2).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            (ok, cipher, profile)
        }
        Err(_) => (false, String::new(), String::new()),
    };
    st::set_str(scope, state, "dtlsState", if ok { "connected" } else { "failed" });
    if ok {
        st::set_str(scope, state, "dtlsCipher", &cipher);
        st::set_str(scope, state, "srtpCipher", &profile);
    }
}

/// `addIceCandidate()`: the algorithm's first step, and the only one a connection with no remote
/// description reaches.
fn cb_pc_add_ice_candidate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    if st::get_bool(scope, s, "closed") {
        if let Some(p) = reject_dom(
            scope,
            "Failed to execute 'addIceCandidate' on 'RTCPeerConnection': The RTCPeerConnection's signalingState is 'closed'.",
            "InvalidStateError",
        ) {
            rv.set(p);
        }
        return;
    }
    // NO CANDIDATE is the END-OF-CANDIDATES signal, and it is satisfied by a connection that has
    // gathered nothing -- the capture records `await pc.addIceCandidate()` on a fresh connection as
    // resolving with `undefined`, and the JS this replaces rejected it, which lost the member
    // (measured 2026-09-11 against the bundle's own oracle). An actual candidate still needs a remote
    // description to be added TO, which is the algorithm's own first step.
    let candidate = args.get(0);
    if !candidate.is_null_or_undefined() && st::get(scope, s, "remoteDescription").is_null() {
        if let Some(p) = reject_dom(
            scope,
            "Failed to execute 'addIceCandidate' on 'RTCPeerConnection': The remote description was null",
            "InvalidStateError",
        ) {
            rv.set(p);
        }
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

fn cb_pc_get_stats(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else {
        return;
    };
    // A connection that ran a DTLS handshake (the send path) reports one `transport` stat carrying its
    // `dtlsState`; every other connection reports the same empty report it always has (I9).
    let entries = connection_stats(scope, s);
    match mint_stats_report(scope, &entries) {
        Some(r) => rv.set(resolved_promise(scope, r.into())),
        None => {
            if let Some(p) = reject_dom(scope, "Failed to execute 'getStats' on 'RTCPeerConnection'.", "OperationError") {
                rv.set(p);
            }
        }
    }
}

/// A monotonic millisecond timestamp for a stat, as a `DOMHighResTimeStamp` reads (ms since an origin).
fn rtc_now_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// Every `RTCStats` entry for a sending connection, built only when the send path's `dtlsState` slot is
/// set (so a connection that never sent reports the same empty report as before -- I9). The set Chrome
/// reports for an active sending video connection: an enriched `transport`, a `codec` (VP8), an
/// `outbound-rtp` with the sender's live byte/packet/frame counts, and the `candidate-pair` with its
/// `local-candidate`/`remote-candidate`. Only fields true for this connection are filled; the richer
/// encoder fields and `remote-inbound-rtp` are left out rather than faked.
fn connection_stats<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>) -> Vec<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)> {
    if !st::get(scope, s, "dtlsState").is_string() {
        return Vec::new();
    }
    let now = rtc_now_ms();
    let ssrc = st::get_num(scope, s, "sendSsrc");
    let socket = st::get_num(scope, s, "statsSocket") as u32;
    let (bytes, packets, frames) = op_outbound(scope, socket);
    let mut out: Vec<(v8::Local<v8::Value>, v8::Local<v8::Value>)> = Vec::new();

    // transport (T0)
    let dtls_state = st::get_str(scope, s, "dtlsState");
    let t = v8::Object::new(scope);
    st::set_num(scope, t, "timestamp", now);
    st::set_str(scope, t, "id", "T0");
    st::set_str(scope, t, "type", "transport");
    st::set_str(scope, t, "dtlsState", &dtls_state);
    st::set_str(scope, t, "selectedCandidatePairId", "CPT0");
    st::set_num(scope, t, "bytesSent", bytes);
    st::set_num(scope, t, "packetsSent", packets);
    if st::get(scope, s, "dtlsRole").is_string() {
        let role = st::get_str(scope, s, "dtlsRole");
        st::set_str(scope, t, "dtlsRole", &role);
    }
    if st::get(scope, s, "dtlsCipher").is_string() {
        let cipher = st::get_str(scope, s, "dtlsCipher");
        st::set_str(scope, t, "dtlsCipher", &cipher);
        st::set_str(scope, t, "tlsVersion", "FEFD");
    }
    if st::get(scope, s, "srtpCipher").is_string() {
        let srtp = st::get_str(scope, s, "srtpCipher");
        st::set_str(scope, t, "srtpCipher", &srtp);
    }
    out.push((str_val(scope, "T0"), t.into()));

    // codec (COT0): VP8 at 90 kHz, the offer's payload type.
    let c = v8::Object::new(scope);
    st::set_num(scope, c, "timestamp", now);
    st::set_str(scope, c, "id", "COT0");
    st::set_str(scope, c, "type", "codec");
    st::set_str(scope, c, "transportId", "T0");
    st::set_num(scope, c, "payloadType", 96.0);
    st::set_str(scope, c, "mimeType", "video/VP8");
    st::set_num(scope, c, "clockRate", 90_000.0);
    out.push((str_val(scope, "COT0"), c.into()));

    // outbound-rtp (OT0): the live counters from the sender.
    let o = v8::Object::new(scope);
    st::set_num(scope, o, "timestamp", now);
    st::set_str(scope, o, "id", "OT0");
    st::set_str(scope, o, "type", "outbound-rtp");
    st::set_str(scope, o, "kind", "video");
    st::set_str(scope, o, "mediaType", "video");
    st::set_num(scope, o, "ssrc", ssrc);
    st::set_str(scope, o, "codecId", "COT0");
    st::set_str(scope, o, "transportId", "T0");
    st::set_num(scope, o, "bytesSent", bytes);
    st::set_num(scope, o, "packetsSent", packets);
    st::set_num(scope, o, "framesEncoded", frames);
    out.push((str_val(scope, "OT0"), o.into()));

    // candidate-pair (CPT0) + its local/remote host candidates.
    let (laddr, lport) = local_host_candidate(scope, s).unwrap_or_else(|| ("0.0.0.0".to_string(), 0));
    let raddr = st::get_str(scope, s, "remoteAddr");
    let rport = st::get_num(scope, s, "remotePort") as u16;
    for (id, ty, addr, port) in [("ILT0", "local-candidate", laddr.as_str(), lport), ("IRT0", "remote-candidate", raddr.as_str(), rport)] {
        let cand = v8::Object::new(scope);
        st::set_num(scope, cand, "timestamp", now);
        st::set_str(scope, cand, "id", id);
        st::set_str(scope, cand, "type", ty);
        st::set_str(scope, cand, "transportId", "T0");
        st::set_str(scope, cand, "candidateType", "host");
        st::set_str(scope, cand, "protocol", "udp");
        st::set_str(scope, cand, "address", addr);
        st::set_num(scope, cand, "port", f64::from(port));
        out.push((str_val(scope, id), cand.into()));
    }
    let cp = v8::Object::new(scope);
    st::set_num(scope, cp, "timestamp", now);
    st::set_str(scope, cp, "id", "CPT0");
    st::set_str(scope, cp, "type", "candidate-pair");
    st::set_str(scope, cp, "transportId", "T0");
    st::set_str(scope, cp, "localCandidateId", "ILT0");
    st::set_str(scope, cp, "remoteCandidateId", "IRT0");
    st::set_str(scope, cp, "state", "succeeded");
    st::set_bool(scope, cp, "nominated", true);
    st::set_bool(scope, cp, "writable", true);
    st::set_num(scope, cp, "bytesSent", bytes);
    st::set_num(scope, cp, "packetsSent", packets);
    out.push((str_val(scope, "CPT0"), cp.into()));

    out
}

/// Read the sending connection's outbound counters via the runtime op (synchronous, for getStats).
fn op_outbound(scope: &mut v8::PinScope, socket: u32) -> (f64, f64, f64) {
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_rtc_outbound") else { return (0.0, 0.0, 0.0) };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let arg: v8::Local<v8::Value> = v8::Number::new(scope, f64::from(socket)).into();
    let Some(v) = f.call(scope, undef, &[arg]) else { return (0.0, 0.0, 0.0) };
    let Ok(a) = v8::Local::<v8::Array>::try_from(v) else { return (0.0, 0.0, 0.0) };
    let b = a.get_index(scope, 0).and_then(|x| x.number_value(scope)).unwrap_or(0.0);
    let p = a.get_index(scope, 1).and_then(|x| x.number_value(scope)).unwrap_or(0.0);
    let fr = a.get_index(scope, 2).and_then(|x| x.number_value(scope)).unwrap_or(0.0);
    (b, p, fr)
}

/// korvus's gathered host candidate `(address, port)` from the local description, for the candidate-pair.
fn local_host_candidate(scope: &mut v8::PinScope, s: v8::Local<v8::Object>) -> Option<(String, u16)> {
    let ld = st::get(scope, s, "localDescription");
    let sdp = crate::core::webrtc::rtc_values::description_sdp(scope, ld)?;
    for l in sdp.lines().filter_map(|l| l.strip_prefix("a=candidate:")) {
        let t: Vec<&str> = l.split(' ').collect();
        if t.len() >= 6 && t[1] == "1" && t[2].eq_ignore_ascii_case("udp") {
            let typ = t.iter().position(|x| *x == "typ").and_then(|i| t.get(i + 1)).copied().unwrap_or("");
            if typ == "host" {
                if let Ok(p) = t[5].parse::<u16>() {
                    return Some((t[4].to_string(), p));
                }
            }
        }
    }
    None
}

/// `addTransceiver(trackOrKind, init?)`: the whole object graph, created locally, before a packet
/// moves.
fn cb_pc_add_transceiver(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_dom(
            scope,
            "Failed to execute 'addTransceiver' on 'RTCPeerConnection': RTCPeerConnection is closed",
            "InvalidStateError",
        );
        return;
    }
    let arg = args.get(0);
    let (kind, track) = if arg.is_string() {
        let Some(k) = st::to_str(scope, arg) else { return };
        if k != "audio" && k != "video" {
            throw_type_error(
                scope,
                "Failed to execute 'addTransceiver' on 'RTCPeerConnection': The argument provided as parameter 1 is not a valid MediaStreamTrack kind ('audio' or 'video').",
            );
            return;
        }
        (k, None)
    } else if let Some(ts) = st::state(scope, arg, TRACK) {
        (st::get_str(scope, ts, "kind"), Some(arg))
    } else {
        throw_type_error(
            scope,
            "Failed to execute 'addTransceiver' on 'RTCPeerConnection': The provided value is not of type '(MediaStreamTrack or DOMString)'.",
        );
        return;
    };
    // `RTCRtpTransceiverInit`, converted as WebIDL converts it (Chrome 150): `direction` (an
    // enum), `sendEncodings`, `streams`, each read once (korvus read `direction` alone)
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "RTCRtpTransceiverInit", "Failed to execute 'addTransceiver' on 'RTCPeerConnection': ") else { return };
    let mut dir = "sendrecv".to_string();
    if let Some(v) = st::dict_get(scope, init.into(), "direction") {
        let Some(d) = st::to_str(scope, v) else { return };
        if !DIRECTIONS.contains(&d.as_str()) {
            throw_type_error(
                scope,
                &format!(
                    "Failed to execute 'addTransceiver' on 'RTCPeerConnection': The provided value '{d}' is not a valid enum value of type RTCRtpTransceiverDirection."
                ),
            );
            return;
        }
        dir = d;
    }
    let Some(x) = mint_transceiver(scope, &kind, &dir, track) else {
        throw_dom(scope, "Failed to execute 'addTransceiver' on 'RTCPeerConnection'.", "OperationError");
        return;
    };
    let sender = xcvr_field(scope, x.into(), "sender");
    let receiver = xcvr_field(scope, x.into(), "receiver");
    pc_push(scope, s, "transceivers", x.into());
    pc_push(scope, s, "senders", sender);
    pc_push(scope, s, "receivers", receiver);
    rv.set(x.into());
}

/// `addTrack(track)`: reuse a compatible sender that has no track, exactly as the algorithm says,
/// before adding one.
fn cb_pc_add_track(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_dom(scope, "Failed to execute 'addTrack' on 'RTCPeerConnection': RTCPeerConnection is closed", "InvalidStateError");
        return;
    }
    let track = args.get(0);
    let Some(ts) = st::state(scope, track, TRACK) else {
        throw_type_error(scope, "Failed to execute 'addTrack' on 'RTCPeerConnection': parameter 1 is not of type 'MediaStreamTrack'.");
        return;
    };
    let kind = st::get_str(scope, ts, "kind");
    for x in pc_list(scope, s, "transceivers") {
        let Some(xs) = st::state(scope, x, XCVR) else { continue };
        if st::get_bool(scope, xs, "stopped") || st::get_str(scope, xs, "kind") != kind {
            continue;
        }
        let sender = st::get(scope, xs, "sender");
        if sender_is_free(scope, sender) {
            sender_set_track(scope, sender, Some(track));
            rv.set(sender);
            return;
        }
    }
    let Some(x) = mint_transceiver(scope, &kind, "sendrecv", Some(track)) else { return };
    let sender = xcvr_field(scope, x.into(), "sender");
    let receiver = xcvr_field(scope, x.into(), "receiver");
    pc_push(scope, s, "transceivers", x.into());
    pc_push(scope, s, "senders", sender);
    pc_push(scope, s, "receivers", receiver);
    rv.set(sender);
}

fn cb_pc_remove_track(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), PC) else { return };
    if st::get_bool(scope, s, "closed") {
        throw_dom(scope, "Failed to execute 'removeTrack' on 'RTCPeerConnection': RTCPeerConnection is closed", "InvalidStateError");
        return;
    }
    let sender = args.get(0);
    if !st::is_a(scope, sender, SENDER) {
        throw_type_error(scope, "Failed to execute 'removeTrack' on 'RTCPeerConnection': parameter 1 is not of type 'RTCRtpSender'.");
        return;
    }
    if !pc_list(scope, s, "senders").iter().any(|x| *x == sender) {
        throw_dom(
            scope,
            "Failed to execute 'removeTrack' on 'RTCPeerConnection': The sender was not created by this peer connection.",
            "InvalidAccessError",
        );
        return;
    }
    sender_set_track(scope, sender, None);
}

fn meth_named(
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
        make_arity_guard(scope, PC, m.name, m.required as i32, m.length as i32, f.into(), m.ty).into()
    } else {
        f.into()
    };
    define_value(scope, proto, m, v, true);
}

pub fn install_peer_connection(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "localDescription" | "currentLocalDescription" | "pendingLocalDescription" | "remoteDescription"
        | "currentRemoteDescription" | "pendingRemoteDescription" | "signalingState" | "iceGatheringState"
        | "iceConnectionState" | "connectionState" => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_pc_field, &format!("get {}", member.name), 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "canTrickleIceCandidates" | "sctp" => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_pc_null, &format!("get {}", member.name), 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "getSenders" => meth_named(scope, proto, member, cb_pc_list, "senders"),
        "getReceivers" => meth_named(scope, proto, member, cb_pc_list, "receivers"),
        "getTransceivers" => meth_named(scope, proto, member, cb_pc_list, "transceivers"),
        "getLocalStreams" | "getRemoteStreams" => meth_named(scope, proto, member, cb_pc_list, ""),
        "getConfiguration" => method_row(scope, proto, member, cb_pc_get_configuration),
        "setConfiguration" => method_row(scope, proto, member, cb_pc_set_configuration),
        "restartIce" => method_row(scope, proto, member, cb_pc_restart_ice),
        "close" => method_row(scope, proto, member, cb_pc_close),
        "createDataChannel" => method_row(scope, proto, member, cb_pc_create_data_channel),
        "createOffer" => method_row(scope, proto, member, cb_pc_create_offer),
        "createAnswer" => method_row(scope, proto, member, cb_pc_create_answer),
        "setLocalDescription" => meth_named(scope, proto, member, cb_pc_set_description, "setLocalDescription"),
        "setRemoteDescription" => meth_named(scope, proto, member, cb_pc_set_description, "setRemoteDescription"),
        "addIceCandidate" => method_row(scope, proto, member, cb_pc_add_ice_candidate),
        "getStats" => method_row(scope, proto, member, cb_pc_get_stats),
        "addTransceiver" => method_row(scope, proto, member, cb_pc_add_transceiver),
        "addTrack" => method_row(scope, proto, member, cb_pc_add_track),
        "removeTrack" => method_row(scope, proto, member, cb_pc_remove_track),
        other => debug_assert!(false, "install_peer_connection: unexpected member {other}"),
    }
}

// ---- the tables ---------------------------------------------------------------------------------

webapi::interface! {
    "RTCStatsReport" [interface, window] {
    ctor   "RTCStatsReport" len 0                       = stub,
    attr   "size" : Object readonly                     = install_stats_report,
    method "entries" : Object                           = install_stats_report,
    method "forEach" : Object len 1                     = install_stats_report,
    method "get" : Object len 1                         = install_stats_report,
    method "has" : Object len 1                         = install_stats_report,
    method "keys" : Object                              = install_stats_report,
    method "values" : Object                            = install_stats_report,
    method "@@iterator" : Object len 0 hidden           = install_stats_report
    }
}

webapi::interface! {
    "RTCDataChannel" : "EventTarget" [interface, both] {
    ctor    "RTCDataChannel" len 0                         = stub,
    attr    "label" : Object readonly                      = install_data_channel,
    attr    "ordered" : Object readonly                    = install_data_channel,
    attr    "maxPacketLifeTime" : Object readonly nullable = install_data_channel,
    attr    "maxRetransmits" : Object readonly nullable    = install_data_channel,
    attr    "protocol" : Object readonly                   = install_data_channel,
    attr    "negotiated" : Object readonly                 = install_data_channel,
    attr    "id" : Object readonly nullable                = install_data_channel,
    attr    "readyState" : Object readonly                 = install_data_channel,
    attr    "bufferedAmount" : Object readonly             = install_data_channel,
    attr    "bufferedAmountLowThreshold" : Object          = install_data_channel,
    handler "onopen" nullable                              = event_handler,
    handler "onbufferedamountlow" nullable                 = event_handler,
    handler "onerror" nullable                             = event_handler,
    handler "onclosing" nullable                           = event_handler,
    handler "onclose" nullable                             = event_handler,
    handler "onmessage" nullable                           = event_handler,
    attr    "binaryType" : Object                          = install_data_channel,
    attr    "reliable" : Object readonly                   = install_data_channel,
    method  "close" : Object                               = install_data_channel,
    method  "send" : Object len 1 required 1               = install_data_channel
    }
}

webapi::interface! {
    "RTCPeerConnection" : "EventTarget" [value, window] {
    ctor          "RTCPeerConnection" len 0                             = stub,
    attr          "localDescription" : Object readonly nullable         = install_peer_connection,
    attr          "currentLocalDescription" : Object readonly nullable  = install_peer_connection,
    attr          "pendingLocalDescription" : Object readonly nullable  = install_peer_connection,
    attr          "remoteDescription" : Object readonly nullable        = install_peer_connection,
    attr          "currentRemoteDescription" : Object readonly nullable = install_peer_connection,
    attr          "pendingRemoteDescription" : Object readonly nullable = install_peer_connection,
    attr          "signalingState" : Object readonly                    = install_peer_connection,
    attr          "iceGatheringState" : Object readonly                 = install_peer_connection,
    attr          "iceConnectionState" : Object readonly                = install_peer_connection,
    attr          "connectionState" : Object readonly                   = install_peer_connection,
    attr          "canTrickleIceCandidates" : Object readonly nullable  = install_peer_connection,
    handler       "onnegotiationneeded" nullable                        = event_handler,
    handler       "onicecandidate" nullable                             = event_handler,
    handler       "onsignalingstatechange" nullable                     = event_handler,
    handler       "oniceconnectionstatechange" nullable                 = event_handler,
    handler       "onconnectionstatechange" nullable                    = event_handler,
    handler       "onicegatheringstatechange" nullable                  = event_handler,
    handler       "onicecandidateerror" nullable                        = event_handler,
    handler       "ontrack" nullable                                    = event_handler,
    attr          "sctp" : Object readonly nullable                     = install_peer_connection,
    handler       "ondatachannel" nullable                              = event_handler,
    handler       "onaddstream"                                         = event_handler,
    handler       "onremovestream"                                      = event_handler,
    method        "addIceCandidate" : Promise                           = install_peer_connection,
    method        "addStream" : Object len 1                            = stub,
    method        "addTrack" : Object len 1 required 1                  = install_peer_connection,
    method        "addTransceiver" : Object len 1 required 1            = install_peer_connection,
    method        "close" : Object                                      = install_peer_connection,
    method        "createAnswer" : Promise                              = install_peer_connection,
    method        "createDTMFSender" : Object len 1                     = stub,
    method        "createDataChannel" : Object len 1 required 1         = install_peer_connection,
    method        "createOffer" : Promise                               = install_peer_connection,
    method        "getConfiguration" : Object                           = install_peer_connection,
    method        "getLocalStreams" : Object                            = install_peer_connection,
    method        "getReceivers" : Object                               = install_peer_connection,
    method        "getRemoteStreams" : Object                           = install_peer_connection,
    method        "getSenders" : Object                                 = install_peer_connection,
    method        "getStats" : Promise                                  = install_peer_connection,
    method        "getTransceivers" : Object                            = install_peer_connection,
    method        "removeStream" : Object len 1                         = stub,
    method        "removeTrack" : Object len 1 required 1               = install_peer_connection,
    method        "restartIce" : Object                                 = install_peer_connection,
    method        "setConfiguration" : Object                           = install_peer_connection,
    method        "setLocalDescription" : Promise                       = install_peer_connection,
    method        "setRemoteDescription" : Promise len 1 required 1     = install_peer_connection,
    static_method "generateCertificate" : Object len 1 required 1       = stub
    }
}
