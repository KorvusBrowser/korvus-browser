//! `modules/websocket/web_socket.rs` — `WebSocket`, as Blink 150's `DOMWebSocket` +
//! `WebSocketCommon` + `WebSocketChannelImpl` (`third_party/blink/renderer/modules/websockets/`).
//!
//! UNTIL THIS FILE, `WebSocket` WAS A REGISTRY STUB: a constructor that brand-stamped `this` and
//! returned, rows answering their shape defaults, and no event ever. A page saw a socket that never
//! opened, never failed and never closed -- BrowserScan's port scan (`new WebSocket('wss://localhost:'
//! + port)`, "open" when `error` fires within 1 s) read every port as N/A, where stock Chrome 150 fires
//! `error` + `close(1006)` within 0-1 ms of a refused connect.
//!
//! WHAT IS CHROME'S, MEASURED. `core/korvus-runtime/tests/fixtures/websocket/ws-probe.js` was run on
//! stock Chrome 150.0.7871.46 (headed, Xvfb, no DevTools client, three recordings identical but for
//! milliseconds) against `ws_server.py`, which also logs the handshake bytes and every client frame.
//! `tests/websocket_live.rs` replays the probe here and compares the rows. The rules, and where they
//! are in Chromium:
//!
//!   * constructor: `KURL(BaseURL, url)`, http(s) -> ws(s), then the SyntaxErrors in
//!     `WebSocketCommon::Connect`'s order (invalid URL, scheme, fragment), the subprotocol checks
//!     (`IsValidSubprotocolString`, duplicates, `EncodeSubprotocolString` in the message), then the
//!     mixed-content SecurityError (`WebSocketChannelImpl::Connect` returning false), then the
//!     per-renderer limit of 255 (`kMaxWebSocketsPerRenderProcess`: "Insufficient resources", failed
//!     asynchronously);
//!   * `send`: InvalidStateError in CONNECTING; after CLOSING it only grows `bufferedAmount`
//!     (`buffered_amount_after_close_`); `bufferedAmount` drops a TASK after the data pipe took the
//!     bytes (`DidConsumeBufferedAmount` -> `PostBufferedAmountUpdateTask`) -- 2 synchronously after
//!     `send('hi')`, 0 after `setTimeout(0)`; 200000 then 69000 for a 200000-byte message, because
//!     the pipe holds 131000;
//!   * `close`: `[Clamp] unsigned short` code, the InvalidAccessError / SyntaxError of
//!     `ValidateCloseCodeAndReason`, then CLOSING; in CONNECTING it FAILS the connection (error, then
//!     close 1006 on the next task);
//!   * events: `open` (Event), `message` (MessageEvent, `origin` the socket URL's origin, data per
//!     `binaryType`), `error` (Event), `close` (CloseEvent with `wasClean` computed as `DidClose`
//!     computes it) -- each delivered in its own task by the receive pump.
//!
//! WHERE THE NETWORK IS. Nothing here opens a socket: the kernel service (`korvus-runtime`
//! `websocket.rs`, ops `op_ws_*`) does, on `transport::websocket`, the persona's wire. The calls go
//! through [`deno_op`], the cross-extension op handle `XMLHttpRequest` uses for `op_fetch`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use deno_core::v8;

use crate::binding::protos::{build_fn_data, global_get, str_val, throw_dom, throw_type_error};
use crate::core::events::event_target::et_dispatch;
use crate::core::html::html_dialog_element::mark_trusted;
use crate::core::xmlhttprequest::xml_http_request::deno_op;
use crate::install::live::{getter_row, method_row, pair_row};
use crate::install::webidl::{brand, has_brand};
use crate::install::MemberDef;

/// `readyState`.
const CONNECTING: u8 = 0;
const OPEN: u8 = 1;
const CLOSING: u8 = 2;
const CLOSED: u8 = 3;

/// `kMaxWebSocketsPerRenderProcess` (websocket_channel_impl.h:95). Process-wide, like Chrome's
/// `g_connection_count`: every realm of this engine (window, frames, workers) is one renderer.
const MAX_SOCKETS: usize = 255;
static CONNECTION_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The data pipe's capacity (`kReceiveDataPipeCapacity`, also used for the send pipe): what a
/// `send()` can hand the network at once, and so what `bufferedAmount` loses a task later.
const PIPE_CAPACITY: u64 = 131_000;

/// `kMaxReasonSizeInBytes`.
const MAX_REASON_BYTES: usize = 123;

const ID_SLOT: &str = "korvus_websocket_id";

/// One `WebSocket`'s state -- the fields of `DOMWebSocket` and `WebSocketCommon` a page can observe,
/// plus the channel's bookkeeping.
struct WsState {
    /// `common_.Url()`, serialised.
    url: String,
    /// `origin_` -- the SOCKET URL's origin, which every `MessageEvent.origin` carries.
    origin: String,
    ready: u8,
    protocol: String,
    extensions: String,
    /// `binaryType == "blob"`.
    blob: bool,
    buffered: u64,
    consumed: u64,
    after_close: u64,
    update_pending: bool,
    /// Bytes in the send pipe the network has not taken yet, and bytes that did not fit it.
    pipe_used: u64,
    waiting: u64,
    /// The kernel service's socket id; 0 when no connection was ever opened.
    rid: u32,
    /// Blink's `channel_ != nullptr`.
    channel: bool,
    /// Whether this socket holds one of the [`MAX_SOCKETS`] slots.
    counted: bool,
}

/// Every live `WebSocket` of this realm.
#[derive(Default)]
pub(crate) struct WsStates {
    next: f64,
    map: HashMap<u64, WsState>,
}

fn id_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, ID_SLOT).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn ws_id(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<u64> {
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    let k = id_key(scope);
    obj.get_private(scope, k).and_then(|v| v.number_value(scope)).map(|n| n as u64)
}

/// Run `f` over the socket `this` names. The OpState borrow ends with `f`: callers dispatch events
/// (page JS) only AFTER this returns, never inside it.
fn with<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, f: impl FnOnce(&mut WsState) -> R) -> Option<R> {
    let id = ws_id(scope, this)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let states = st.try_borrow_mut::<WsStates>()?;
    states.map.get_mut(&id).map(f)
}

fn new_state(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, s: WsState) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<WsStates>().is_none() {
            st.put(WsStates::default());
        }
        let states = st.borrow_mut::<WsStates>();
        states.next += 1.0;
        let id = states.next as u64;
        states.map.insert(id, s);
        id
    };
    let k = id_key(scope);
    let v: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    this.set_private(scope, k, v);
}

/// The receiver as a `WebSocket`, or `TypeError: Illegal invocation`.
fn recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if has_brand(scope, this, "WebSocket") && ws_id(scope, this).is_some() {
        return true;
    }
    crate::binding::protos::illegal_invocation(scope);
    false
}

/// Chrome prints these to the DevTools console (`AddConsoleMessage`); no page can read them, so this
/// engine writes them to stderr only when asked (`KORVUS_TRACE_WS=1`) rather than into a page sink.
fn console(level: &str, msg: &str) {
    if std::env::var_os("KORVUS_TRACE_WS").is_some() {
        eprintln!("[websocket console {level}] {msg}");
    }
}

// ── URLs ────────────────────────────────────────────────────────────────────────────────────────

/// The realm's base URL: the document's (`<base href>` honoured) in a window, the worker's own script
/// URL in a worker (`ExecutionContext::BaseURL`).
fn realm_base(scope: &mut v8::PinScope) -> String {
    if let Some(f) = deno_op(scope, "op_worker_location_href") {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        if let Some(v) = f.call(scope, undef, &[]) {
            return v.to_rust_string_lossy(scope);
        }
    }
    crate::core::dom::document::base_url(scope)
}

/// `network::IsUrlPotentiallyTrustworthy` for a ws: target, the half of the mixed-content check that
/// lets `ws://localhost` through from an https page.
fn potentially_trustworthy(u: &url::Url) -> bool {
    if u.scheme() == "wss" || u.scheme() == "https" {
        return true;
    }
    match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        None => false,
    }
}

// ── subprotocols (websocket_common.cc) ──────────────────────────────────────────────────────────

/// `IsValidSubprotocolCharacter`: U+0021..U+007E minus RFC 2616's separators.
fn valid_subprotocol(p: &str) -> bool {
    !p.is_empty()
        && p.encode_utf16().all(|c| {
            (0x21..=0x7E).contains(&c)
                && !matches!(c as u8, b'"' | b'(' | b')' | b',' | b'/' | b':'..=b'@' | b'['..=b']' | b'{' | b'}')
        })
}

/// `EncodeSubprotocolString`: a UTF-16 unit outside U+0020..U+007E as `\uXXXX`, a backslash doubled.
fn encode_subprotocol(p: &str) -> String {
    let mut out = String::new();
    for c in p.encode_utf16() {
        if !(0x20..=0x7E).contains(&c) {
            out.push_str(&format!("\\u{:04X}", c));
        } else if c == 0x5C {
            out.push_str("\\\\");
        } else {
            out.push(c as u8 as char);
        }
    }
    out
}

/// `(DOMString or sequence<DOMString>) protocols`: an object with a callable `@@iterator` is a
/// sequence, anything else is ONE string (WebIDL union conversion). `Err` means an exception is
/// pending.
/// One `DOMString` of the constructor's `protocols`, with Chrome's Symbol sentence.
fn ctor_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<String, ()> {
    if v.is_symbol() {
        throw_type_error(scope, "Failed to construct 'WebSocket': Cannot convert a Symbol value to a string");
        return Err(());
    }
    v.to_string(scope).map(|s| s.to_rust_string_lossy(scope)).ok_or(())
}

fn convert_protocols(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<Vec<String>, ()> {
    if let Ok(obj) = v8::Local::<v8::Object>::try_from(v) {
        let sym = v8::Symbol::get_iterator(scope);
        let method = obj.get(scope, sym.into()).ok_or(())?;
        if method.is_function() {
            let method = v8::Local::<v8::Function>::try_from(method).map_err(|_| ())?;
            let it = method.call(scope, obj.into(), &[]).ok_or(())?;
            let Ok(it) = v8::Local::<v8::Object>::try_from(it) else {
                throw_type_error(scope, "Failed to construct 'WebSocket': The object must have a callable @@iterator property.");
                return Err(());
            };
            let next_key = str_val(scope, "next");
            let next = it.get(scope, next_key).ok_or(())?;
            let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
                throw_type_error(scope, "Failed to construct 'WebSocket': The object must have a callable @@iterator property.");
                return Err(());
            };
            let mut out = Vec::new();
            loop {
                let r = next.call(scope, it.into(), &[]).ok_or(())?;
                let Ok(r) = v8::Local::<v8::Object>::try_from(r) else {
                    throw_type_error(scope, "Failed to construct 'WebSocket': The iterator result is not an object.");
                    return Err(());
                };
                let done_key = str_val(scope, "done");
                let done = r.get(scope, done_key).ok_or(())?;
                if done.boolean_value(scope) {
                    break;
                }
                let value_key = str_val(scope, "value");
                let value = r.get(scope, value_key).ok_or(())?;
                out.push(ctor_string(scope, value)?);
            }
            return Ok(out);
        }
        if !method.is_null_or_undefined() {
            throw_type_error(scope, "Failed to construct 'WebSocket': The object must have a callable @@iterator property.");
            return Err(());
        }
    }
    Ok(vec![ctor_string(scope, v)?])
}

// ── events ──────────────────────────────────────────────────────────────────────────────────────

/// A null-prototype dictionary, so building an event's init never reads `Object.prototype`.
fn dict<'s>(scope: &mut v8::PinScope<'s, '_>, pairs: &[(&str, v8::Local<'s, v8::Value>)]) -> v8::Local<'s, v8::Object> {
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let mut names: Vec<v8::Local<v8::Name>> = Vec::with_capacity(pairs.len());
    let mut values: Vec<v8::Local<v8::Value>> = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        names.push(v8::String::new(scope, k).unwrap().into());
        values.push(*v);
    }
    v8::Object::with_prototype_and_properties(scope, null, &names, &values)
}

/// Construct `iface(type, init)` off this realm's global, trusted, and dispatch it at `target`.
fn fire<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<v8::Object>, iface: &str, ty: &str, init: Option<v8::Local<'s, v8::Object>>) {
    let target = v8::Local::new(scope, target);
    let Some(ctor) = global_get(scope, iface).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let t = str_val(scope, ty);
    let ev = match init {
        Some(i) => ctor.new_instance(scope, &[t, i.into()]),
        None => ctor.new_instance(scope, &[t]),
    };
    let Some(ev) = ev else { return };
    mark_trusted(scope, ev);
    et_dispatch(scope, target.into(), ev);
}

/// `ReflectBufferedAmountConsumption`.
fn reflect(s: &mut WsState) {
    s.buffered -= s.consumed.min(s.buffered);
    s.consumed = 0;
}

/// `ReleaseChannel` + `WebSocketChannelImpl::Dispose`: the slot is freed, and the caller hands the
/// returned id to [`abort_rid`] so the kernel service forgets the socket (a no-op when it already has).
fn release(s: &mut WsState) -> Option<u32> {
    s.channel = false;
    if s.counted {
        s.counted = false;
        CONNECTION_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
    (s.rid != 0).then_some(s.rid)
}

fn abort_rid(scope: &mut v8::PinScope, rid: Option<u32>) {
    let Some(rid) = rid else { return };
    let Some(f) = deno_op(scope, "op_ws_abort") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let r: v8::Local<v8::Value> = v8::Number::new(scope, rid as f64).into();
    f.call(scope, undef, &[r]);
}

/// Queue `cb` as a task with `this` as its data (the setTimeout(0) hop every deferred delivery in
/// this crate takes -- `message_port.rs`, `broadcast_channel.rs`, `idb_pump`).
fn post_task(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let Some(st) = global_get(scope, "setTimeout").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let data: v8::Local<v8::Value> = v8::Local::new(scope, this).into();
    let f = build_fn_data(scope, cb, "", 0, data);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let zero: v8::Local<v8::Value> = v8::Number::new(scope, 0.0).into();
    st.call(scope, undef, &[f.into(), zero]);
}

/// `BufferedAmountUpdateTask`.
fn cb_buffered_update(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.data();
    with(scope, this, |s| {
        s.update_pending = false;
        reflect(s);
    });
}

/// `DidConsumeBufferedAmount(n)`: ignored once CLOSED; otherwise counted, and the reflection posted.
fn did_consume(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, n: u64) {
    if n == 0 {
        return;
    }
    let post = with(scope, this.into(), |s| {
        if s.ready == CLOSED {
            return false;
        }
        s.consumed += n;
        if s.update_pending {
            return false;
        }
        s.update_pending = true;
        true
    })
    .unwrap_or(false);
    if post {
        post_task(scope, this, cb_buffered_update);
    }
}

/// Queue the `OnWritable` step for `n` drained bytes (see `cb_step`'s DRAINED arm).
fn post_drained(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, n: u64) {
    let Some(st) = global_get(scope, "setTimeout").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let d = v8::Array::new(scope, 2);
    let tv: v8::Local<v8::Value> = v8::Local::new(scope, this).into();
    d.set_index(scope, 0, tv);
    let nv: v8::Local<v8::Value> = v8::Number::new(scope, n as f64).into();
    d.set_index(scope, 1, nv);
    let dv: v8::Local<v8::Value> = d.into();
    let f = build_fn_data(scope, cb_drained, "", 0, dv);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let zero: v8::Local<v8::Value> = v8::Number::new(scope, 0.0).into();
    st.call(scope, undef, &[f.into(), zero]);
}

/// `OnWritable` -> `ProcessSendQueue` -> `ProduceData`: the drained bytes leave the pipe, waiting bytes
/// enter it, and what entered is `DidConsumeBufferedAmount`'d.
fn cb_drained(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(args.data()) else { return };
    let Some(this) = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(n) = d.get_index(scope, 1).and_then(|v| v.number_value(scope)).map(|n| n as u64) else { return };
    let consumed = with(scope, this.into(), |s| {
        let from_pipe = n.min(s.pipe_used);
        s.pipe_used -= from_pipe;
        let direct = (n - from_pipe).min(s.waiting);
        s.waiting -= direct;
        let refill = s.waiting.min(PIPE_CAPACITY - s.pipe_used.min(PIPE_CAPACITY));
        s.waiting -= refill;
        s.pipe_used += refill;
        direct + refill
    })
    .unwrap_or(0);
    did_consume(scope, this, consumed);
}

/// `TearDownFailedConnection`: `DidError` (error event), then `DidClose(false, 1006, "")`.
fn teardown_failed(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) {
    let go = with(scope, this.into(), |s| {
        if !s.channel {
            return false;
        }
        reflect(s);
        s.ready = CLOSED;
        true
    })
    .unwrap_or(false);
    if !go {
        return;
    }
    fire(scope, this, "Event", "error", None);
    did_close(scope, this, false, 1006, String::new());
}

/// A connection the document's CSP refused: `error` with the state CLOSED, and NO `close` -- Chrome 150 fires none
/// (measured: `error` at 7 ms, readyState 3, nothing more in 3.5 s; the browser's refusal reaches `DidError` and
/// the channel is never closed). The channel is released without one.
fn cb_csp_refused(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(this) = v8::Local::<v8::Object>::try_from(args.data()) else { return };
    let go = with(scope, this.into(), |s| {
        if !s.channel {
            return false;
        }
        reflect(s);
        s.ready = CLOSED;
        let _ = release(s);
        true
    })
    .unwrap_or(false);
    if go {
        fire(scope, this, "Event", "error", None);
    }
}

fn cb_teardown(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(this) = v8::Local::<v8::Object>::try_from(args.data()) else { return };
    teardown_failed(scope, this);
}

/// `DOMWebSocket::DidClose`.
fn did_close(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, handshake_complete: bool, code: u16, reason: String) {
    let r = with(scope, this.into(), |s| {
        reflect(s);
        if !s.channel {
            return None;
        }
        let all_consumed = s.buffered == s.consumed;
        let was_clean = s.ready == CLOSING && all_consumed && handshake_complete && code != 1006;
        s.ready = CLOSED;
        Some((was_clean, release(s)))
    });
    let Some(Some((was_clean, rid))) = r else { return };
    abort_rid(scope, rid);
    let wc: v8::Local<v8::Value> = v8::Boolean::new(scope, was_clean).into();
    let cv: v8::Local<v8::Value> = v8::Number::new(scope, code as f64).into();
    let rv = str_val(scope, &reason);
    let init = dict(scope, &[("wasClean", wc), ("code", cv), ("reason", rv)]);
    fire(scope, this, "CloseEvent", "close", Some(init));
}

// ── the receive pump ────────────────────────────────────────────────────────────────────────────

/// Await the kernel service's next event for `rid`, then [`cb_step`]. NOT unref'd -- see the runtime
/// file's header: a live socket is work the realm is still waiting on.
fn arm(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, rid: u32) {
    let Some(f) = deno_op(scope, "op_ws_next") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let r: v8::Local<v8::Value> = v8::Number::new(scope, rid as f64).into();
    let Some(p) = f.call(scope, undef, &[r]) else { return };
    let Ok(p) = v8::Local::<v8::Promise>::try_from(p) else { return };
    let d = v8::Array::new(scope, 2);
    d.set_index(scope, 0, this.into());
    d.set_index(scope, 1, r);
    let dv: v8::Local<v8::Value> = d.into();
    let step = build_fn_data(scope, cb_step, "", 1, dv);
    // `Promise::then` is V8's own reaction, not a lookup of `Promise.prototype.then` a page could
    // have replaced.
    // ...run as a task of its own (`then_task`): a socket's events are tasks, not promise jobs.
    let _ = crate::then_task(scope, p, step, None);
}

fn bytes_of(v: v8::Local<v8::Value>) -> Vec<u8> {
    let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) else { return Vec::new() };
    let mut buf = vec![0u8; view.byte_length()];
    view.copy_contents(&mut buf);
    buf
}

/// One event off the pump (the encoding is `korvus-runtime/src/websocket.rs::encode`), then re-arm
/// unless it was the last.
fn cb_step(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(args.data()) else { return };
    let Some(this) = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(rid) = d.get_index(scope, 1).and_then(|v| v.number_value(scope)).map(|n| n as u32) else { return };
    let ev = bytes_of(args.get(0));
    if ev.is_empty() {
        return;
    }
    // A socket whose channel is gone (closed, failed, torn down after `close()` in CONNECTING) hears
    // nothing more -- `WebSocketChannelImpl::Dispose` reset every pipe.
    let live = with(scope, this.into(), |s| s.channel && s.rid == rid).unwrap_or(false);
    if !live {
        return;
    }
    let body = &ev[1..];
    let mut terminal = false;
    match ev[0] {
        // OPEN -> `DidConnect`.
        1 if body.len() >= 4 => {
            let n = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            let protocol = String::from_utf8_lossy(&body[4..(4 + n).min(body.len())]).into_owned();
            let extensions = String::from_utf8_lossy(&body[(4 + n).min(body.len())..]).into_owned();
            let open = with(scope, this.into(), |s| {
                if s.ready != CONNECTING {
                    return false;
                }
                s.ready = OPEN;
                s.protocol = protocol;
                s.extensions = extensions;
                true
            })
            .unwrap_or(false);
            if open {
                fire(scope, this, "Event", "open", None);
            }
        }
        // TEXT / BINARY -> `DidReceiveTextMessage` / `DidReceiveBinaryMessage`.
        2 | 3 => {
            let r = with(scope, this.into(), |s| {
                reflect(s);
                (s.ready == OPEN).then(|| (s.blob, s.origin.clone()))
            });
            if let Some(Some((blob, origin))) = r {
                let data: v8::Local<v8::Value> = if ev[0] == 2 {
                    str_val(scope, &String::from_utf8_lossy(body))
                } else if blob {
                    match crate::core::fileapi::blob::mint_blob(scope, body.to_vec(), "") {
                        Some(b) => b.into(),
                        None => v8::null(scope).into(),
                    }
                } else {
                    let store = v8::ArrayBuffer::new_backing_store_from_vec(body.to_vec()).make_shared();
                    v8::ArrayBuffer::with_backing_store(scope, &store).into()
                };
                let o = str_val(scope, &origin);
                let init = dict(scope, &[("data", data), ("origin", o)]);
                fire(scope, this, "MessageEvent", "message", Some(init));
            }
        }
        // DRAINED -> the pipe moved; whatever enters it now is consumed -- on a TASK of its own, as
        // Chrome's is: the bytes that did not fit are written by `OnWritable`, a mojo watcher
        // callback (itself a task), and only then does `DidConsumeBufferedAmount` post its reflection.
        // Handled inline here, a fast loopback drain could be counted BEFORE the reflection task
        // `send()` already posted ran, and that task would then reflect the whole message: Chrome's
        // three recordings read 69000 a task after a 200000-byte send, never 0 (a race the gate's
        // loaded run hit).
        4 if body.len() >= 8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&body[..8]);
            let n = u64::from_le_bytes(b);
            post_drained(scope, this, n);
        }
        // CLOSING -> `DidStartClosingHandshake`.
        5 => {
            with(scope, this.into(), |s| {
                reflect(s);
                s.ready = CLOSING;
            });
        }
        // CLOSED -> `DidClose`.
        6 if body.len() >= 3 => {
            let clean = body[0] != 0;
            let code = u16::from_le_bytes([body[1], body[2]]);
            let reason = String::from_utf8_lossy(&body[3..]).into_owned();
            did_close(scope, this, clean, code, reason);
            terminal = true;
        }
        // FAILED -> `OnConnectionError` -> `FailAsError`: the console line, then the teardown.
        7 => {
            let url = with(scope, this.into(), |s| s.url.clone()).unwrap_or_default();
            console("error", &format!("WebSocket connection to '{url}' failed: {}", String::from_utf8_lossy(body)));
            teardown_failed(scope, this);
            terminal = true;
        }
        _ => {}
    }
    if !terminal {
        arm(scope, this, rid);
    }
}

// ── the constructor ─────────────────────────────────────────────────────────────────────────────

/// `new WebSocket(url, protocols)` -- `DOMWebSocket::Create` + `Connect`.
pub fn cb_ws_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !crate::install::webidl::require_new(scope, &args, "WebSocket") || !crate::install::webidl::require_args(scope, &args, "WebSocket", 1) {
        return;
    }
    // The bindings convert EVERY argument before the algorithm runs.
    let Some(url_arg) = crate::install::webidl::arg_string(scope, &args, 0, "WebSocket") else { return };
    let protocols = if args.length() > 1 && !args.get(1).is_undefined() {
        match convert_protocols(scope, args.get(1)) {
            Ok(p) => p,
            Err(()) => return,
        }
    } else {
        Vec::new()
    };
    let syntax = |scope: &mut v8::PinScope, m: String| throw_dom(scope, &format!("Failed to construct 'WebSocket': {m}"), "SyntaxError");

    // `url_ = KURL(execution_context->BaseURL(), url)`, then http(s) -> ws(s).
    let base = realm_base(scope);
    let parsed = match url::Url::parse(&base) {
        Ok(b) => b.join(&url_arg),
        Err(_) => url::Url::parse(&url_arg),
    };
    let mut u = match parsed {
        Ok(u) => u,
        Err(_) => return syntax(scope, format!("The URL '{url_arg}' is invalid.")),
    };
    match u.scheme() {
        "http" => {
            let _ = u.set_scheme("ws");
        }
        "https" => {
            let _ = u.set_scheme("wss");
        }
        _ => {}
    }
    if u.scheme() != "ws" && u.scheme() != "wss" {
        return syntax(scope, format!("The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '{}' is not allowed.", u.scheme()));
    }
    if let Some(frag) = u.fragment() {
        return syntax(
            scope,
            format!("The URL contains a fragment identifier ('{frag}'). Fragment identifiers are not allowed in WebSocket URLs."),
        );
    }
    for p in &protocols {
        if !valid_subprotocol(p) {
            return syntax(scope, format!("The subprotocol '{}' is invalid.", encode_subprotocol(p)));
        }
    }
    for (i, p) in protocols.iter().enumerate() {
        if protocols[..i].contains(p) {
            return syntax(scope, format!("The subprotocol '{}' is duplicated.", encode_subprotocol(p)));
        }
    }
    // `ShouldBlockWebSocketByMixedContentCheck`: a secure page may not open an insecure, untrustworthy
    // socket -- a synchronous SecurityError.
    let page_secure = url::Url::parse(&base).map(|b| b.scheme() == "https").unwrap_or(false);
    if page_secure && !potentially_trustworthy(&u) {
        throw_dom(
            scope,
            "Failed to construct 'WebSocket': An insecure WebSocket connection may not be initiated from a page loaded over HTTPS.",
            "SecurityError",
        );
        return;
    }

    let this = args.this();
    brand(scope, this, "WebSocket");
    let url_s = u.to_string();
    let origin = u.origin().ascii_serialization();
    new_state(
        scope,
        this,
        WsState {
            url: url_s.clone(),
            origin,
            ready: CONNECTING,
            protocol: String::new(),
            extensions: String::new(),
            blob: true,
            buffered: 0,
            consumed: 0,
            after_close: 0,
            update_pending: false,
            pipe_used: 0,
            waiting: 0,
            rid: 0,
            channel: true,
            counted: false,
        },
    );

    // `connect-src`: a connection the document's CSP refuses is never made; it fails with `error`
    // alone ([`cb_csp_refused`]), and each refusing policy reports a violation.
    if crate::core::html::csp::realm_request_blocked(scope, &url_s, "") {
        post_task(scope, this, cb_csp_refused);
        rv.set(this.into());
        return;
    }
    // The per-renderer limit, counted like `g_connection_count`: over it, the connection fails
    // asynchronously with "Insufficient resources".
    let old = CONNECTION_COUNT.fetch_add(1, Ordering::Relaxed);
    with(scope, this.into(), |s| s.counted = true);
    if old >= MAX_SOCKETS {
        console("error", &format!("WebSocket connection to '{url_s}' failed: Insufficient resources"));
        post_task(scope, this, cb_teardown);
        rv.set(this.into());
        return;
    }

    let Some(open) = deno_op(scope, "op_ws_open") else {
        // A realm with no kernel service (a bare test runtime): the connection cannot be made, so it
        // fails the way a refused connection does.
        post_task(scope, this, cb_teardown);
        rv.set(this.into());
        return;
    };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let uv = str_val(scope, &url_s);
    let pv = str_val(scope, &protocols.join(", "));
    let rid = open.call(scope, undef, &[uv, pv]).and_then(|v| v.number_value(scope)).map(|n| n as u32).unwrap_or(0);
    if rid == 0 {
        post_task(scope, this, cb_teardown);
        rv.set(this.into());
        return;
    }
    with(scope, this.into(), |s| s.rid = rid);
    arm(scope, this, rid);
    rv.set(this.into());
}

// ── the rows ────────────────────────────────────────────────────────────────────────────────────

fn cb_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    if let Some(u) = with(scope, args.this().into(), |s| s.url.clone()) {
        rv.set(str_val(scope, &u));
    }
}

fn cb_ready_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    if let Some(r) = with(scope, args.this().into(), |s| s.ready) {
        rv.set_uint32(r as u32);
    }
}

fn cb_buffered_amount(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    if let Some(n) = with(scope, args.this().into(), |s| s.after_close + s.buffered) {
        rv.set_double(n as f64);
    }
}

fn cb_extensions(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    if let Some(e) = with(scope, args.this().into(), |s| s.extensions.clone()) {
        rv.set(str_val(scope, &e));
    }
}

fn cb_protocol(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    if let Some(p) = with(scope, args.this().into(), |s| s.protocol.clone()) {
        rv.set(str_val(scope, &p));
    }
}

fn cb_binary_type_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    if let Some(b) = with(scope, args.this().into(), |s| s.blob) {
        rv.set(str_val(scope, if b { "blob" } else { "arraybuffer" }));
    }
}

/// `attribute BinaryType binaryType`: an enum attribute IGNORES a value outside the enum (Chrome
/// prints "The provided value 'foo' is not a valid enum value of type BinaryType." and keeps the old
/// one); a Symbol cannot be converted and throws.
fn cb_binary_type_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    let v = args.get(0);
    if v.is_symbol() {
        throw_type_error(scope, "Failed to set the 'binaryType' property on 'WebSocket': Cannot convert a Symbol value to a string");
        return;
    }
    let Some(s) = v.to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    match s.as_str() {
        "blob" => {
            with(scope, args.this().into(), |st| st.blob = true);
        }
        "arraybuffer" => {
            with(scope, args.this().into(), |st| st.blob = false);
        }
        other => console("warning", &format!("The provided value '{other}' is not a valid enum value of type BinaryType.")),
    }
}

/// `[Clamp] unsigned short`.
fn clamp_u16(n: f64) -> u16 {
    if n.is_nan() {
        return 0;
    }
    let c = n.clamp(0.0, 65535.0);
    let f = c.floor();
    let diff = c - f;
    let r = if diff > 0.5 {
        f + 1.0
    } else if diff < 0.5 {
        f
    } else if f % 2.0 == 0.0 {
        f
    } else {
        f + 1.0
    };
    r as u16
}

/// `close(optional [Clamp] unsigned short code, optional USVString reason)` -- `CloseInternal`.
fn cb_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    let fail = |scope: &mut v8::PinScope, m: &str, name: &str| throw_dom(scope, &format!("Failed to execute 'close' on 'WebSocket': {m}"), name);
    // Argument conversion first, in order.
    let mut code: Option<u16> = None;
    if args.length() >= 1 && !args.get(0).is_undefined() {
        let v = args.get(0);
        if v.is_symbol() {
            throw_type_error(scope, "Failed to execute 'close' on 'WebSocket': Cannot convert a Symbol value to a number");
            return;
        }
        let Some(n) = v.number_value(scope) else { return };
        code = Some(clamp_u16(n));
    }
    let mut reason = String::new();
    if args.length() >= 2 && !args.get(1).is_undefined() {
        let v = args.get(1);
        if v.is_symbol() {
            throw_type_error(scope, "Failed to execute 'close' on 'WebSocket': Cannot convert a Symbol value to a string");
            return;
        }
        let Some(s) = v.to_string(scope) else { return };
        reason = s.to_rust_string_lossy(scope);
    }
    // `ValidateCloseCodeAndReason`.
    if let Some(c) = code {
        if !(c == 1000 || (3000..=4999).contains(&c)) {
            return fail(
                scope,
                &format!("The close code must be either 1000, or between 3000 and 4999. {c} is neither."),
                "InvalidAccessError",
            );
        }
    } else if !reason.is_empty() {
        code = Some(1000);
    }
    if reason.len() > MAX_REASON_BYTES {
        return fail(scope, &format!("The close reason must not be greater than {MAX_REASON_BYTES} UTF-8 bytes."), "SyntaxError");
    }
    let this = args.this();
    let (ready, rid, url) = match with(scope, this.into(), |s| (s.ready, s.rid, s.url.clone())) {
        Some(v) => v,
        None => return,
    };
    if ready == CLOSING || ready == CLOSED {
        return;
    }
    with(scope, this.into(), |s| s.ready = CLOSING);
    if ready == CONNECTING {
        // `channel->Fail("WebSocket is closed before the connection is established.", warning)`:
        // `Fail` only logs and POSTS `TearDownFailedConnection`; the channel -- and with it the
        // network side's handshake -- lives until that task runs (error, close 1006, `Dispose`). A
        // handshake that completes meanwhile is dropped with Close 1001 when the channel goes, which
        // Chrome 150 did for the fixture's first such socket in all three recordings.
        let _ = rid;
        console("warning", &format!("WebSocket connection to '{url}' failed: WebSocket is closed before the connection is established."));
        post_task(scope, this, cb_teardown);
        return;
    }
    let Some(f) = deno_op(scope, "op_ws_close") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let r: v8::Local<v8::Value> = v8::Number::new(scope, rid as f64).into();
    // `kCloseEventCodeNotSpecified` goes out as 1005, "a Close frame with no payload".
    let c: v8::Local<v8::Value> = v8::Number::new(scope, code.unwrap_or(1005) as f64).into();
    let rs = str_val(scope, &reason);
    f.call(scope, undef, &[r, c, rs]);
}

/// The four `send` overloads, resolved as the generated binding resolves them: an ArrayBuffer, an
/// ArrayBufferView, a Blob, else a USVString.
fn cb_send(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !recv(scope, args.this().into()) {
        return;
    }
    let v = args.get(0);
    // (text?, bytes, is_blob)
    let msg: (bool, Vec<u8>, bool) = if v.is_array_buffer() {
        let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) else { return };
        let len = ab.byte_length();
        let mut buf = vec![0u8; len];
        if len > 0 {
            if let Some(view) = v8::Uint8Array::new(scope, ab, 0, len) {
                view.copy_contents(&mut buf);
            }
        }
        (false, buf, false)
    } else if v.is_array_buffer_view() {
        let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) else { return };
        if view.buffer(scope).map(|b| { let bv: v8::Local<v8::Value> = b.into(); bv.is_shared_array_buffer() }).unwrap_or(false) {
            throw_type_error(scope, "Failed to execute 'send' on 'WebSocket': The provided ArrayBufferView value must not be shared.");
            return;
        }
        let mut buf = vec![0u8; view.byte_length()];
        view.copy_contents(&mut buf);
        (false, buf, false)
    } else if let Some(bytes) = crate::core::fileapi::blob::blob_bytes(scope, v) {
        (false, bytes, true)
    } else {
        if v.is_symbol() {
            throw_type_error(scope, "Failed to execute 'send' on 'WebSocket': Cannot convert a Symbol value to a string");
            return;
        }
        let Some(s) = v.to_string(scope) else { return };
        (true, s.to_rust_string_lossy(scope).into_bytes(), false)
    };
    let (text, data, is_blob) = msg;
    let len = data.len() as u64;
    let this = args.this();
    let Some((ready, rid)) = with(scope, this.into(), |s| (s.ready, s.rid)) else { return };
    if ready == CONNECTING {
        throw_dom(scope, "Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", "InvalidStateError");
        return;
    }
    if ready == CLOSING || ready == CLOSED {
        // `UpdateBufferedAmountAfterClose`.
        with(scope, this.into(), |s| s.after_close += len);
        console("error", "WebSocket is already in CLOSING or CLOSED state.");
        return;
    }
    // `buffered_amount_ += size`, then the data pipe takes what fits; a Blob is read first
    // (`BlobLoader`) and enters the pipe only behind whatever is already waiting.
    let took = with(scope, this.into(), |s| {
        s.buffered += len;
        if is_blob || s.waiting > 0 {
            s.waiting += len;
            0
        } else {
            let take = len.min(PIPE_CAPACITY - s.pipe_used.min(PIPE_CAPACITY));
            s.pipe_used += take;
            s.waiting += len - take;
            take
        }
    })
    .unwrap_or(0);
    let op = if text { "op_ws_send_text" } else { "op_ws_send_binary" };
    if let Some(f) = deno_op(scope, op) {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        let r: v8::Local<v8::Value> = v8::Number::new(scope, rid as f64).into();
        let payload: v8::Local<v8::Value> = if text {
            str_val(scope, &String::from_utf8_lossy(&data))
        } else {
            let n = data.len();
            let store = v8::ArrayBuffer::new_backing_store_from_vec(data).make_shared();
            let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
            match v8::Uint8Array::new(scope, ab, 0, n) {
                Some(a) => a.into(),
                None => return,
            }
        };
        f.call(scope, undef, &[r, payload]);
    }
    did_consume(scope, this, took);
}

/// The body of every `WebSocket` row that has one -- the REGISTRY rows `tools/sync/gen_shells.py`
/// claims for the `websocket` kit (`install::kits::websocket`). The handlers stay the
/// `event_handler` kit and the constants stay constants.
///
/// WHY THE INTERFACE STAYS A REGISTRY ROW and not an `interface!` table: it is
/// `[Exposed=(Window,Worker)]` and sits in BOTH captures' own-global orders, and a realm's
/// `globalThis` insertion order is fingerprint surface (`tests/global_order.rs` ratchets it). A table
/// is installed by its own lane, out of that order; a claimed row is created by `install_registry` in
/// the capture's position. `OffscreenCanvas` and `CSSAnimation` made the same choice (gen_shells.py).
/// The constructor body is joined to the row in `install::ctors::row_ctor`.
pub fn install_websocket(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "url" => getter_row(scope, proto, member, cb_url),
        "readyState" => getter_row(scope, proto, member, cb_ready_state),
        "bufferedAmount" => getter_row(scope, proto, member, cb_buffered_amount),
        "extensions" => getter_row(scope, proto, member, cb_extensions),
        "protocol" => getter_row(scope, proto, member, cb_protocol),
        "binaryType" => pair_row(scope, proto, member, cb_binary_type_get, cb_binary_type_set),
        "close" => method_row(scope, proto, member, cb_close),
        "send" => method_row(scope, proto, member, cb_send),
        other => debug_assert!(false, "install_websocket: unexpected member {other}"),
    }
}
