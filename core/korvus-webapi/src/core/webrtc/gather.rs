//! `core/webrtc/gather.rs` -- an `RTCPeerConnection`'s ICE gathering after `setLocalDescription(offer)`, over the
//! kernel's sockets and STUN (`korvus-runtime/src/ice.rs`).
//!
//! a bot challenge -- in the page, then again in its frame -- creates a connection with its
//! STUN server, a data channel and `offerToReceiveAudio`/`Video`, and reads every candidate's `candidate`
//! string. Chrome's six were three mDNS host candidates and three server-reflexive ones carrying the public
//! address the server saw; korvus's one was a host candidate without `ufrag`, fired as a plain `Event`.
//! What Chrome 150.0.7871.46 does, measured on this box (a fixture STUN server answering 203.0.113.9, and
//! stun.a challenge provider.com live):
//!
//! * one transport per m-section (one under `max-bundle`), gathered on its own UDP socket;
//! * `iceGatheringState` "gathering" as `setLocalDescription` settles;
//! * a host candidate per transport under the DOCUMENT's mDNS name -- one name per address for each window
//!   (two connections of a document share it; a same-site frame, and each of two frames of another site, has its
//!   own -- measured three runs alike) -- a few ms in, once the name is registered;
//! * a server-reflexive candidate per transport and STUN server when its Binding Response arrives (so a
//!   fast server's comes before the host's), `raddr 0.0.0.0 rport 0`, its `url` the server's URL;
//! * every candidate string `candidate:<f> 1 udp <priority> <address> <port> typ <type> [raddr 0.0.0.0 rport 0]
//!   generation 0 ufrag <ufrag> network-cost 999`, the foundation one per connection, type and server --
//!   host 2113937151 and srflx 1677729535 the priorities -- while the local description's `a=candidate`
//!   form carries no `ufrag`;
//! * "complete" and the null candidate once every STUN transaction has ended, and no sooner than ~118 ms
//!   in (a quick server: 118-132 ms; one whose transaction runs out: ~39.9 s).
//!
//! * with `iceCandidatePoolSize` above 0, ONE transport's worth of gathering begins with the connection (its
//!   sockets, its STUN transactions, no event); the description applied later hands that pool to its first
//!   transport, whose candidates gathered so far are reported at once -- while `iceGatheringState` still reads
//!   "new" -- before "gathering" and the other transports' (measured against a fixture STUN server answering after
//!   80 ms, a description applied 300 ms in: `host@1 srflx@1`, then `host@2 host@2`, `srflx@82 srflx@82`; with one
//!   transport all of it, "complete" and the null candidate at once; korvus gathered nothing before the
//!   description and reported every srflx 80 ms after it);
//! * all of it after `setLocalDescription()` has settled (`SLD_SETTLE_MS`).
//!
//! * a UDP TURN server (`turn:`) is asked twice from each IPv4 socket: a Binding Request, whose
//!   answer is a server-reflexive candidate whose `url` is `stun:<host>:<port>`, and an Allocate under its
//!   credentials (`korvus-runtime/src/ice.rs` `op_ice_turn`), whose answer is the relay candidate --
//!   `typ relay raddr <the mapped address> rport <its port>`, priority 50339839, its `url` the TURN URL. Under
//!   `iceTransportPolicy: "relay"` only the relay is gathered, its `raddr 0.0.0.0 rport 0`. An answer whose
//!   MESSAGE-INTEGRITY does not verify (a wrong credential) is dropped and the Allocate runs out: no candidate, the
//!   state still "gathering" 8 s in (Chrome 150, measured against `fixtures/webrtc/turn-server.py`).
//!
//! NOT done here: an IPv6 address's own candidates (no box measured has a routable IPv6 network; the IPv6
//! socket's transactions still run, which is what holds Chrome's `complete`), TURN over TCP or TLS, and
//! `icecandidateerror`.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{build_fn_data, str_val};
use crate::core::webrtc::config;
use crate::core::webrtc::peer_connection::PC;
use crate::core::xmlhttprequest::xml_http_request::deno_op;

const HOST_PRIORITY: u32 = 2_113_937_151; // (126 << 24) | (30 << 8) | 255
const SRFLX_PRIORITY: u32 = 1_677_729_535; // (100 << 24) | (30 << 8) | 255
/// A UDP relay's priority over IPv4, as Chrome 150 reports it (`fixtures/webrtc/turn-probe.js`).
const RELAY_PRIORITY: u32 = 50_339_839; // (3 << 24) | (31 << 8) | 255
/// The soonest "complete" comes, from the start of gathering.
const COMPLETE_FLOOR_MS: f64 = 118.0;
/// When the host candidates go, once the mDNS name is "registered".
const HOST_AFTER_MS: f64 = 1.0;
/// `setLocalDescription()` settles in a task this long after the call (`peer_connection::local_description_applied`);
/// gathering is reported after it.
pub(crate) const SLD_SETTLE_MS: f64 = 1.0;

/// The mDNS name of the box's IPv4 address as `pc`'s window registers it: one per document, for its life. Chrome 150
/// keeps the name with the window's `PeerConnectionDependencyFactory` (an `ExecutionContext` supplement, whose
/// `MdnsResponderAdapter` is its own client of the network service's responder, which names an address once per
/// client): the challenge reads its page's candidates and its frame's, and the two carried one name here where
/// Chrome's never do (the pinned-session replay).
fn mdns_name(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>) -> String {
    let Some(ctx) = pc.get_creation_context(scope) else { return format!("{}.local", st::uuid()) };
    let global = ctx.global(scope);
    let k = v8::String::new(scope, "korvus_mdns_name").unwrap();
    let k = v8::Private::for_api(scope, Some(k));
    if let Some(v) = global.get_private(scope, k).filter(|v| v.is_string()) {
        return v.to_rust_string_lossy(scope);
    }
    let name = format!("{}.local", st::uuid());
    let v = v8::String::new(scope, &name).unwrap();
    global.set_private(scope, k, v.into());
    name
}

fn now_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

fn random_u32() -> u32 {
    let b = crate::crypto::random_bytes(4);
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// The connection's foundation for `key` (`host`, or a STUN server's URL), made once.
fn foundation(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, key: &str) -> u32 {
    let k = format!("g_found:{key}");
    let v = st::get(scope, s, &k);
    if v.is_number() {
        return v.uint32_value(scope).unwrap_or(0);
    }
    let f = random_u32();
    st::set_num(scope, s, &k, f64::from(f));
    f
}

/// Queue `cb` with the connection as its data, `ms` from now.
fn step(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, cb: impl v8::MapFnTo<v8::FunctionCallback>, ms: f64) {
    let pc: v8::Local<v8::Value> = v8::Local::new(scope, pc).into();
    let f = build_fn_data(scope, cb, "", 0, pc);
    crate::core::intersection_observer::rendering::queue_task(scope, f, ms);
}

/// The connection a step belongs to, and its state -- unless it has been closed since.
fn live<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<'s, v8::Value>) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>)> {
    let pc = v8::Local::<v8::Object>::try_from(data).ok()?;
    let s = st::state(scope, pc.into(), PC)?;
    (!st::get_bool(scope, s, "closed")).then_some((pc, s))
}

/// Start gathering for the local description just applied. Once per connection.
pub(crate) fn start(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>) {
    let Some(s) = st::state(scope, pc.into(), PC) else { return };
    if st::get_bool(scope, s, "g_started") {
        return;
    }
    st::set_bool(scope, s, "g_started", true);
    let sections = local_sdp(scope, s).matches("\r\nm=").count();
    if sections == 0 {
        return;
    }
    let cfg = config::load(scope, s);
    let transports = if cfg.bundle_policy == "max-bundle" { 1 } else { sections };
    st::set_num(scope, s, "g_transports", transports as f64);
    st::set_num(scope, s, "g_start", now_ms());
    st::set_num(scope, s, "g_pending", 0.0);
    if st::get_array(scope, s, "g_sockets").is_none() {
        let sockets = st::array(scope, &[]);
        st::set(scope, s, "g_sockets", sockets.into());
    }
    // the first transport takes the pool: its port, its transactions still out, and -- when it is the only one --
    // the floor of "complete" from when the pool began
    if st::get_bool(scope, s, "p_begun") {
        st::set_bool(scope, s, "p_adopted", true);
        let port = st::get_num(scope, s, "p_port");
        st::set_num(scope, s, "g_port:0", port);
        let pending = st::get_num(scope, s, "p_pending");
        st::set_num(scope, s, "g_pending", pending);
        if transports == 1 {
            let p0 = st::get_num(scope, s, "p_start");
            st::set_num(scope, s, "g_start", p0);
        }
    }
    step(scope, pc, cb_begin, SLD_SETTLE_MS);
}

/// `iceCandidatePoolSize` above 0: the pool's gathering begins with the connection -- one transport's sockets and
/// STUN transactions, reported to nobody until a description takes them ([`start`]). Once per connection, and
/// never after a description began its own.
pub(crate) fn start_pool(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>) {
    let Some(s) = st::state(scope, pc.into(), PC) else { return };
    if st::get_bool(scope, s, "p_started") || st::get_bool(scope, s, "g_started") {
        return;
    }
    if config::load(scope, s).ice_candidate_pool_size == 0 {
        return;
    }
    st::set_bool(scope, s, "p_started", true);
    step(scope, pc, cb_pool_begin, 0.0);
}

fn cb_pool_begin(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Some((pc, s)) = live(scope, data) else { return };
    if st::get_bool(scope, s, "g_started") {
        return;
    }
    st::set_num(scope, s, "p_start", now_ms());
    st::set_num(scope, s, "p_pending", 0.0);
    let f = v8::Boolean::new(scope, false).into();
    let routable = call_op(scope, "op_ice_routable", &[f]).is_some_and(|v| v.is_true());
    st::set_bool(scope, s, "p_routable", routable);
    if st::get_array(scope, s, "g_sockets").is_none() {
        let sockets = st::array(scope, &[]);
        st::set(scope, s, "g_sockets", sockets.into());
    }
    let servers = config::stun_servers(&config::load(scope, s));
    for v6 in [false, true] {
        let Some((id, port)) = open_socket(scope, s, v6) else { continue };
        if !v6 {
            st::set_num(scope, s, "p_port", f64::from(port));
        }
        for (host, port, url) in &servers {
            stun(scope, pc, s, id, 0, v6, host, *port, url, true);
        }
    }
    st::set_bool(scope, s, "p_begun", true);
}

/// A UDP socket of the family (`op_ice_open`), kept with the connection's: `(id, port)`.
fn open_socket(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, v6: bool) -> Option<(u32, u32)> {
    let fam: v8::Local<v8::Value> = v8::Boolean::new(scope, v6).into();
    let pair = call_op(scope, "op_ice_open", &[fam]).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
    let id = pair.get_index(scope, 0).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    let port = pair.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    if id == 0 {
        return None;
    }
    let mut ids = st::get_array(scope, s, "g_sockets").map(|a| st::array_items(scope, a)).unwrap_or_default();
    ids.push(v8::Number::new(scope, f64::from(id)).into());
    let arr = st::array(scope, &ids);
    st::set(scope, s, "g_sockets", arr.into());
    Some((id, port))
}

fn local_sdp(scope: &mut v8::PinScope, s: v8::Local<v8::Object>) -> String {
    let ld = st::get(scope, s, "localDescription");
    crate::core::webrtc::rtc_values::description_sdp(scope, ld).unwrap_or_default()
}

fn call_op<'s>(scope: &mut v8::PinScope<'s, '_>, op: &str, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    let f = deno_op(scope, op)?;
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    f.call(scope, undef, args)
}

fn cb_begin(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Some((pc, s)) = live(scope, data) else { return };
    let transports = st::get_num(scope, s, "g_transports") as usize;
    let f = v8::Boolean::new(scope, false).into();
    let routable = call_op(scope, "op_ice_routable", &[f]).is_some_and(|v| v.is_true());
    st::set_bool(scope, s, "g_routable", routable);
    // the pooled transport's candidates first, while the state is still "new" (Chrome 150, measured)
    let adopted = st::get_bool(scope, s, "p_adopted");
    if adopted {
        let p0 = st::get_num(scope, s, "p_start");
        if routable && now_ms() - p0 >= HOST_AFTER_MS {
            emit_host(scope, pc, s, 0);
        }
        let found = st::get_array(scope, s, "p_found").map(|a| st::array_items(scope, a)).unwrap_or_default();
        for item in found {
            let Ok(a) = v8::Local::<v8::Array>::try_from(item) else { continue };
            let ip = a.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let port = a.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0) as u16;
            let url = a.get_index(scope, 2).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            emit_srflx(scope, pc, s, 0, &ip, port, &url);
        }
    }
    st::set_str(scope, s, "iceGatheringState", "gathering");
    crate::core::webrtc::peer_connection::fire_pc_event(scope, pc, "icegatheringstatechange");
    let cfg = config::load(scope, s);
    let relay_only = cfg.ice_transport_policy == "relay";
    st::set_bool(scope, s, "g_relay_only", relay_only);
    let turns = config::turn_servers(&cfg);
    // a UDP TURN server is asked for the socket's reflexive address too, as the STUN server `stun:<host>:<port>`
    let mut servers = if relay_only { Vec::new() } else { config::stun_servers(&cfg) };
    if !relay_only {
        for (host, port, _, _, _) in &turns {
            servers.push((host.clone(), *port, format!("stun:{host}:{port}")));
        }
    }
    for mid in (if adopted { 1 } else { 0 })..transports {
        for v6 in [false, true] {
            let Some((id, port)) = open_socket(scope, s, v6) else { continue };
            if !v6 {
                st::set_num(scope, s, &format!("g_port:{mid}"), f64::from(port));
                for (host, port, url, username, credential) in &turns {
                    turn(scope, pc, s, id, mid, host, *port, url, username, credential);
                }
            }
            for (host, port, url) in &servers {
                stun(scope, pc, s, id, mid, v6, host, *port, url, false);
            }
        }
    }
    if adopted && transports == 1 && st::get_bool(scope, s, "g_host:0") {
        // nothing left to report but what the pool still has out
        st::set_bool(scope, s, "g_hosts_done", true);
        maybe_complete(scope, pc, s);
        return;
    }
    step(scope, pc, cb_hosts, HOST_AFTER_MS);
}

/// One STUN transaction out; its answer comes back to [`cb_stun`].
#[allow(clippy::too_many_arguments)]
fn stun(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>, id: u32, mid: usize, v6: bool, host: &str, port: u16, url: &str, pooled: bool) {
    let a: [v8::Local<v8::Value>; 3] = [v8::Number::new(scope, f64::from(id)).into(), str_val(scope, host), v8::Number::new(scope, f64::from(port)).into()];
    let Some(p) = call_op(scope, "op_ice_stun", &a).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    // A transaction can wait 40 s for an answer; it must not hold the page's event loop open meanwhile.
    crate::core::workers::worker::unref(scope, p.into());
    let key = if pooled { "p_pending" } else { "g_pending" };
    let n = st::get_num(scope, s, key);
    st::set_num(scope, s, key, n + 1.0);
    let m: v8::Local<v8::Value> = v8::Number::new(scope, mid as f64).into();
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, v6).into();
    let u = str_val(scope, url);
    let pl: v8::Local<v8::Value> = v8::Boolean::new(scope, pooled).into();
    let d = st::array(scope, &[pc.into(), m, f, u, pl]);
    let cb = build_fn_data(scope, cb_stun, "", 1, d.into());
    // the answer is a task, not a promise job (`then_task`)
    crate::then_task(scope, p, cb, Some(cb));
}

fn cb_stun(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(data) = d.get_index(scope, 0) else { return };
    let Some((pc, s)) = live(scope, data) else { return };
    let mid = d.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0) as usize;
    let v6 = d.get_index(scope, 2).is_some_and(|v| v.is_true());
    let url = d.get_index(scope, 3).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let pooled = d.get_index(scope, 4).is_some_and(|v| v.is_true());
    let answer = v8::Local::<v8::Array>::try_from(args.get(0)).ok();
    if pooled && !st::get_bool(scope, s, "p_adopted") {
        // the pool's answer before any description: kept for the transport that will take the pool
        let n = st::get_num(scope, s, "p_pending");
        st::set_num(scope, s, "p_pending", (n - 1.0).max(0.0));
        if let (Some(a), false) = (answer, v6) {
            let ip = a.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let port = a.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
            if !ip.is_empty() {
                let mut found = st::get_array(scope, s, "p_found").map(|x| st::array_items(scope, x)).unwrap_or_default();
                let (i, n, u) = (str_val(scope, &ip), v8::Number::new(scope, f64::from(port)).into(), str_val(scope, &url));
                let item = st::array(scope, &[i, n, u]);
                found.push(item.into());
                let arr = st::array(scope, &found);
                st::set(scope, s, "p_found", arr.into());
            }
        }
        return;
    }
    let n = st::get_num(scope, s, "g_pending");
    st::set_num(scope, s, "g_pending", (n - 1.0).max(0.0));
    if let (Some(a), false) = (answer, v6) {
        let ip = a.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        let port = a.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0) as u16;
        emit_srflx(scope, pc, s, mid, &ip, port, &url);
    }
    maybe_complete(scope, pc, s);
}

/// One TURN Allocate out from socket `id`; its answer comes back to [`cb_turn`].
#[allow(clippy::too_many_arguments)]
fn turn(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>, id: u32, mid: usize, host: &str, port: u16, url: &str, username: &str, credential: &str) {
    let a: [v8::Local<v8::Value>; 5] =
        [v8::Number::new(scope, f64::from(id)).into(), str_val(scope, host), v8::Number::new(scope, f64::from(port)).into(), str_val(scope, username), str_val(scope, credential)];
    let Some(p) = call_op(scope, "op_ice_turn", &a).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    // an Allocate can wait 40 s for an answer, as a Binding Request can
    crate::core::workers::worker::unref(scope, p.into());
    let n = st::get_num(scope, s, "g_pending");
    st::set_num(scope, s, "g_pending", n + 1.0);
    let m: v8::Local<v8::Value> = v8::Number::new(scope, mid as f64).into();
    let u = str_val(scope, url);
    let d = st::array(scope, &[pc.into(), m, u]);
    let cb = build_fn_data(scope, cb_turn, "", 1, d.into());
    // the answer is a task, not a promise job (`then_task`)
    crate::then_task(scope, p, cb, Some(cb));
}

fn cb_turn(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(data) = d.get_index(scope, 0) else { return };
    let Some((pc, s)) = live(scope, data) else { return };
    let mid = d.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0) as usize;
    let url = d.get_index(scope, 2).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let n = st::get_num(scope, s, "g_pending");
    st::set_num(scope, s, "g_pending", (n - 1.0).max(0.0));
    if let Ok(a) = v8::Local::<v8::Array>::try_from(args.get(0)) {
        let text = |scope: &mut v8::PinScope, i: u32| a.get_index(scope, i).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        let (ip, mapped) = (text(scope, 0), text(scope, 2));
        let port = a.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0) as u16;
        let mapped_port = a.get_index(scope, 3).and_then(|v| v.uint32_value(scope)).unwrap_or(0) as u16;
        emit_relay(scope, pc, s, mid, &ip, port, &mapped, mapped_port, &url);
    }
    maybe_complete(scope, pc, s);
}

/// A relay candidate of transport `mid`: related to the mapped address the Allocate's answer carried, which a
/// relay-only connection does not show (`raddr 0.0.0.0 rport 0`).
#[allow(clippy::too_many_arguments)]
fn emit_relay(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>, mid: usize, ip: &str, port: u16, mapped: &str, mapped_port: u16, url: &str) {
    if ip.is_empty() {
        return;
    }
    host_before(scope, pc, s, mid);
    let (raddr, rport) = if st::get_bool(scope, s, "g_relay_only") { ("0.0.0.0", 0) } else { (mapped, mapped_port) };
    let f = foundation(scope, s, &format!("relay {url}"));
    let ufrag = st::get_str(scope, s, "iceUfrag");
    let line = format!("candidate:{f} 1 udp {RELAY_PRIORITY} {ip} {port} typ relay raddr {raddr} rport {rport} generation 0 ufrag {ufrag} network-cost 999");
    emit(scope, pc, s, mid, &line, Some(url));
}

/// A server-reflexive candidate of transport `mid`: one per transport and address, whichever server reported it first.
fn emit_srflx(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>, mid: usize, ip: &str, port: u16, url: &str) {
    host_before(scope, pc, s, mid);
    let seen = format!("g_srflx:{mid}:{ip}:{port}");
    if ip.is_empty() || st::get_bool(scope, s, &seen) {
        return;
    }
    st::set_bool(scope, s, &seen, true);
    let f = foundation(scope, s, url);
    let ufrag = st::get_str(scope, s, "iceUfrag");
    let line = format!("candidate:{f} 1 udp {SRFLX_PRIORITY} {ip} {port} typ srflx raddr 0.0.0.0 rport 0 generation 0 ufrag {ufrag} network-cost 999");
    emit(scope, pc, s, mid, &line, Some(url));
}

/// A socket's host candidate is signalled when the socket is bound, before its STUN or TURN request goes out
/// (libwebrtc's `UDPPort::OnLocalAddressReady`), so a transport's reflexive or relayed candidate never comes first --
/// however fast a nearby server answers (a loopback server's answer, now delivered as a task of its
/// own, beat the host step's 1 ms).
fn host_before(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>, mid: usize) {
    if st::get_bool(scope, s, "g_routable") && !st::get_bool(scope, s, "g_relay_only") {
        emit_host(scope, pc, s, mid);
    }
}

/// Transport `mid`'s host candidate under the document's mDNS name, once.
fn emit_host(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>, mid: usize) {
    let done = format!("g_host:{mid}");
    let port = st::get_num(scope, s, &format!("g_port:{mid}")) as u32;
    if port == 0 || st::get_bool(scope, s, &done) {
        return;
    }
    st::set_bool(scope, s, &done, true);
    let f = foundation(scope, s, "host");
    let ufrag = st::get_str(scope, s, "iceUfrag");
    let line = format!("candidate:{f} 1 udp {HOST_PRIORITY} {} {port} typ host generation 0 ufrag {ufrag} network-cost 999", mdns_name(scope, pc));
    emit(scope, pc, s, mid, &line, Some(""));
}

fn cb_hosts(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Some((pc, s)) = live(scope, data) else { return };
    // under `iceTransportPolicy: "relay"` there are relays only
    if st::get_bool(scope, s, "g_routable") && !st::get_bool(scope, s, "g_relay_only") {
        let transports = st::get_num(scope, s, "g_transports") as usize;
        for mid in 0..transports {
            emit_host(scope, pc, s, mid);
        }
    }
    st::set_bool(scope, s, "g_hosts_done", true);
    maybe_complete(scope, pc, s);
}

/// "complete" once the hosts are out and no transaction is, never before [`COMPLETE_FLOOR_MS`].
fn maybe_complete(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>) {
    if !st::get_bool(scope, s, "g_hosts_done") || st::get_num(scope, s, "g_pending") > 0.0 || st::get_bool(scope, s, "g_completing") {
        return;
    }
    st::set_bool(scope, s, "g_completing", true);
    let start = st::get_num(scope, s, "g_start");
    let wait = (start + COMPLETE_FLOOR_MS - now_ms()).max(0.0);
    step(scope, pc, cb_complete, wait);
}

fn cb_complete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Some((pc, s)) = live(scope, data) else { return };
    st::set_str(scope, s, "iceGatheringState", "complete");
    crate::core::webrtc::peer_connection::fire_pc_event(scope, pc, "icegatheringstatechange");
    fire_candidate_event(scope, pc, None);
}

/// One gathered candidate: the `RTCIceCandidate`, its line in the local description, and its event.
fn emit(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, s: v8::Local<v8::Object>, mid: usize, line: &str, url: Option<&str>) {
    let init = crate::binding::protos::init_dict(scope);
    st::set_str(scope, init, "candidate", line);
    st::set_str(scope, init, "sdpMid", &mid.to_string());
    st::set_num(scope, init, "sdpMLineIndex", mid as f64);
    // A gathered candidate's `usernameFragment` is the ICE ufrag it was gathered under (Blink's
    // `RTCIceCandidatePlatform` gets it from `webrtc::Candidate::username()`); the line carries it.
    let words: Vec<&str> = line.split(' ').collect();
    if let Some(ufrag) = words.windows(2).find(|w| w[0] == "ufrag").map(|w| w[1]) {
        st::set_str(scope, init, "usernameFragment", ufrag);
    }
    let Some(cand) = crate::binding::protos::new_global(scope, "RTCIceCandidate", &[init.into()]) else { return };
    if let Some(u) = url {
        crate::core::webrtc::rtc_values::set_gathered_url(scope, cand.into(), u);
    }
    let ld = st::get(scope, s, "localDescription");
    if let Some(sdp) = crate::core::webrtc::rtc_values::description_sdp(scope, ld) {
        let sdp_line = match line.find(" ufrag ") {
            Some(i) => {
                let rest = &line[i + 7..];
                let after = rest.find(' ').map(|j| &rest[j..]).unwrap_or("");
                format!("{}{after}", &line[..i])
            }
            None => line.to_string(),
        };
        let with = crate::core::webrtc::sdp::add_candidate(&sdp, mid, &sdp_line);
        crate::core::webrtc::rtc_values::set_description_sdp(scope, ld, &with);
    }
    fire_candidate_event(scope, pc, Some(cand.into()));
}

fn fire_candidate_event(scope: &mut v8::PinScope, pc: v8::Local<v8::Object>, cand: Option<v8::Local<v8::Value>>) {
    let init = crate::binding::protos::init_dict(scope);
    let c = cand.unwrap_or_else(|| v8::null(scope).into());
    crate::binding::protos::set_prop(scope, init, "candidate", c);
    let ty = str_val(scope, "icecandidate");
    let Some(ev) = crate::binding::protos::new_global(scope, "RTCPeerConnectionIceEvent", &[ty, init.into()]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let pcv: v8::Local<v8::Value> = pc.into();
    crate::binding::protos::engine_call(scope, pcv, "dispatchEvent", &[ev.into()]);
}

/// `close()`: every socket of the connection goes, its keepalives with it.
pub(crate) fn close(scope: &mut v8::PinScope, s: v8::Local<v8::Object>) {
    let ids = st::get_array(scope, s, "g_sockets").map(|a| st::array_items(scope, a)).unwrap_or_default();
    for id in ids {
        let _ = call_op(scope, "op_ice_close", &[id]);
    }
    let empty = st::array(scope, &[]);
    st::set(scope, s, "g_sockets", empty.into());
}
