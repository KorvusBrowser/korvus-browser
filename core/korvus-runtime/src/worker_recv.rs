//! The worker realm's THREE CROSS-ISOLATE RECEIVE LOOPS, and the connect glue they deliver through
//! (invariant I9).
//!
//! WHAT THIS REPLACES, line for line: the last executable statements of
//! `core/korvus-runtime/src/js/worker-host.js` -- three `(async () => { while (true) { const frame =
//! await O.op_X_self_recv(); … } })().catch(…)` IIFEs, the four `KB.__*` arrows they delivered
//! through (`__reportWorkerError`, `__deliverConnect`, `__bindClientPort`, `__flushConnects`) and the
//! module-scoped queue those arrows shared. With them the file has nothing left and is deleted, which
//! takes `core/korvus-runtime/src/js/` from three files to two.
//!
//! # `await` in a loop IS a `.then` chain, and this file is not the first to say so
//!
//! [`crate::worker_scope::start_pump`] already made this exact substitution for the worker's timer
//! pump and its doc comment records why it is faithful: an `async` IIFE runs synchronously to its
//! first `await`, and each resumption after that is a microtask on the op promise -- which is what a
//! native `.then` continuation is. So each loop here is:
//!
//! ```text
//!   arm:   call the async op, attach a native continuation to the promise it returns
//!   step:  decode the frame, deliver it, and -- unless the frame was empty (the channel closed,
//!          i.e. the worker was terminated) or `close()` has been called -- ARM AGAIN
//! ```
//!
//! The `while (true) { … break … }` and the `if (O.op_worker_scope_closed()) break;` become an early
//! `return` from the step, and the IIFE's `.catch(e => op_kprint("LOOPFAIL: " + e))` becomes the
//! [`note_loop_fail`] each `cb_*_step` calls with whatever its `TryCatch` caught. Two details of the
//! JS are preserved deliberately because they are observable: a frame shorter than 4 bytes stops the
//! loop WITHOUT re-arming (that is how a dropped channel ends the isolate rather than spinning), and
//! the two connect branches `continue`d -- they re-armed WITHOUT consulting the close flag -- so they
//! still do.
//!
//! # WHEN they are armed, and why the new moment is the same moment
//!
//! The IIFEs ran during the extension's ESM evaluation, i.e. before every Rust install of the realm;
//! [`arm_receive_loops`] runs at the END of that Rust block instead. Nothing can be delivered in
//! between: all three ops are `async(lazy)`, their futures are not polled until
//! `runtime.run_event_loop()` is awaited, and that happens after `execute_script`.
//!
//! # No bridge names are left
//!
//! `__deliverConnect` and `__bindClientPort` had exactly one caller each -- the message loop -- so
//! they are plain Rust functions ([`deliver_connect`], [`bind_client_port`]). `__flushConnects` and
//! `__swScriptLoaded` followed: their callers were JS strings `worker.rs`
//! appended to a worker's top-level script, and `worker.rs` now calls [`flush_connects`] itself when
//! that script has been evaluated, so the `Symbol.for("korvus.bridge")` holder they hung on is gone
//! from every realm. (`__reportWorkerError` was deleted together with the JS boot fetch
//! whose catch arm called it: a URL-loaded worker script is fetched from Rust now.)

use deno_core::{v8, JsRuntime};

use crate::worker::build_fn;
use crate::PrintBuffer;

// ── small v8 helpers ────────────────────────────────────────────────────────────────────────────
//
// `korvus-webapi`'s equivalents are private to that crate, and each of these is three lines; the
// same reasoning `worker.rs`'s own `build_fn` records.

fn str_val<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::Value> {
    v8::String::new(scope, s).map(|s| s.into()).unwrap_or_else(|| v8::undefined(scope).into())
}

fn get_prop<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Value>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    let obj = v8::Local::<v8::Object>::try_from(obj).ok()?;
    let k = v8::String::new(scope, key)?;
    obj.get(scope, k.into())
}

fn set_prop<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Object>, key: &str, v: v8::Local<'s, v8::Value>) {
    if let Some(k) = v8::String::new(scope, key) {
        obj.set(scope, k.into(), v);
    }
}

/// [`build_fn`] with a bound `data` value -- the native equivalent of a JS closure's captured
/// variable, and how [`bind_client_port`]'s handler carries the port it answers for.
fn build_fn_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    length: i32,
    data: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Function> {
    let f = v8::Function::builder(webapi::binding::refs::f(cb))
        .length(length)
        .data(data)
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .build(scope)
        .unwrap();
    if let Some(n) = v8::String::new(scope, "") {
        f.set_name(n);
    }
    f
}

/// One op off `Deno.core.ops`, wherever that object currently is. The same route
/// `worker_scope.rs`'s pump takes, and it answers on either side of the `Deno` strip.
fn op<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Function>> {
    webapi::stashed_op(scope, name)
}

fn call_op<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    args: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Local<'s, v8::Value>> {
    let f = op(scope, name)?;
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    f.call(scope, undef, args)
}

/// The frame an `async(lazy)` `#[buffer]` op resolved with, copied out of V8.
///
/// `None` when the promise resolved with something that is not a view -- which cannot happen for
/// these three ops, and is the JS `if (!frame …) break` all the same.
fn frame_of<'s>(v: v8::Local<'s, v8::Value>) -> Option<(v8::Local<'s, v8::ArrayBufferView>, Vec<u8>)> {
    let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
    let mut bytes = vec![0u8; view.byte_length()];
    view.copy_contents(&mut bytes);
    Some((view, bytes))
}

/// `frame.subarray(start)` -- a Uint8Array over the SAME backing store, which is what the ops these
/// bytes are handed to expect (`#[buffer] &[u8]` borrows it rather than copying).
fn subarray<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    view: v8::Local<'s, v8::ArrayBufferView>,
    start: usize,
    len: usize,
) -> Option<v8::Local<'s, v8::Value>> {
    let buf = view.buffer(scope)?;
    v8::Uint8Array::new(scope, buf, view.byte_offset() + start, len).map(|a| a.into())
}

fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The `.catch(e => op_kprint("LOOPFAIL: " + e))` each IIFE carried, written to the buffer
/// `op_kprint` writes to.
///
/// It is the loop's only error policy and it always was: a throw inside a step is reported and the
/// loop stops, rather than taking the isolate down or vanishing silently. Each `cb_*_step` opens a
/// `TryCatch`, runs its step as a PLAIN function -- so that the values it takes off the callback's
/// arguments and the values it makes with the scope carry one lifetime -- and calls this with
/// whatever the `TryCatch` caught.
fn note_loop_fail(scope: &mut v8::PinScope, msg: String) {
    let op_state = JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<PrintBuffer>().0.push(format!("LOOPFAIL: {msg}"));
}

/// `self.close()` has been called: the loops stop and the isolate drains. Read straight off
/// `OpState`, which is where `op_worker_scope_closed` read it too -- that op was deleted once
/// the loops became Rust and stopped needing one to answer their own kernel's flag.
fn worker_closed(scope: &mut v8::PinScope) -> bool {
    let op_state = JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<crate::worker_scope::WorkerClosed>().map(|c| c.get()).unwrap_or(false)
}

/// Which `WorkerGlobalScope` this isolate is, off `OpState` (`op_worker_scope_kind`'s own source).
fn scope_kind(scope: &mut v8::PinScope) -> String {
    let op_state = JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<crate::worker::WorkerScope>().map(|s| s.kind.clone()).unwrap_or_else(|| "dedicated".to_string())
}

// ── the connect queue ───────────────────────────────────────────────────────────────────────────

/// The `connect` (SharedWorker) and client-port (ServiceWorker) events that arrive while the worker's
/// top-level script is still being evaluated.
///
/// `Some(queue)` while the script is loading, `None` once [`flush_connects`] has run -- which is
/// `worker-host.js`'s `let connectQueue = []` and its `connectQueue = null`, moved off a JS module
/// scope and onto the isolate's `OpState`. The buffering is spec ordering, not an optimisation: a
/// connect is a queued task, the event loop runs no tasks while the script is still evaluating, and
/// `onconnect` / `addEventListener("connect")` do not exist until it has.
pub(crate) struct ConnectQueue(Option<Vec<v8::Global<v8::Value>>>);

fn dispatch_on_global<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<'s, v8::Value>) {
    let global = scope.get_current_context().global(scope);
    let Some(f) = get_prop(scope, global.into(), "dispatchEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return;
    };
    v8::tc_scope!(let tc, scope);
    let recv: v8::Local<v8::Value> = global.into();
    f.call(tc, recv, &[ev]);
    tc.reset();
}

/// `queueOrDeliver`: hold it until the script has been evaluated, or dispatch it now.
fn queue_or_deliver<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<'s, v8::Value>) {
    let queued = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.try_borrow_mut::<ConnectQueue>() {
            Some(q) if q.0.is_some() => {
                let g = v8::Global::new(scope, ev);
                q.0.as_mut().expect("checked").push(g);
                true
            }
            _ => false,
        }
    };
    if !queued {
        dispatch_on_global(scope, ev);
    }
}

/// The worker's top-level script has been evaluated (`worker.rs`'s `after_top_level`), so every buffered
/// event is delivered now and everything after it is delivered immediately.
pub(crate) fn flush_connects(scope: &mut v8::PinScope) {
    let pending = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.try_borrow_mut::<ConnectQueue>() {
            Some(q) => q.0.take(),
            None => None,
        }
    };
    for g in pending.unwrap_or_default() {
        let ev = v8::Local::new(scope, &g);
        dispatch_on_global(scope, ev);
    }
}

// ── the events the loops deliver ────────────────────────────────────────────────────────────────

/// `new globalThis.MessageEvent(type, { data, ports, source })`, through the realm's own interface
/// object -- the same constructor `message_port.rs` and `broadcast_channel.rs` mint through.
///
/// `source` goes in the INIT DICT, where `MessageEventInit.source` accepts a `MessagePort`. The JS
/// this replaces could not do that -- it built the event and then wrote an OWN `source` property over
/// the prototype's accessor with `Object.defineProperty`, which is both an own-property tell on an
/// event instance and a second way to fill a slot the table already owns. One writer now: the
/// constructor's own `init_message_event`.
fn new_message_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ty: &str,
    data: v8::Local<'s, v8::Value>,
    ports: v8::Local<'s, v8::Value>,
    source: Option<v8::Local<'s, v8::Value>>,
) -> Option<v8::Local<'s, v8::Value>> {
    // The realm's own `MessageEvent`, not a global the worker script replaced.
    let ctor = webapi::binding::protos::intrinsic_fn(scope, "MessageEvent")?;
    let t = str_val(scope, ty);
    let init = webapi::binding::protos::init_dict(scope);
    set_prop(scope, init, "data", data);
    set_prop(scope, init, "ports", ports);
    if let Some(s) = source {
        set_prop(scope, init, "source", s);
    }
    // Delivered by the engine, so TRUSTED: Chrome's worker-side `message` and `connect`
    // events read `isTrusted` true, and a challenge widget's worker evaluates nothing from an untrusted one.
    let ev = ctor.new_instance(scope, &[t, init.into()])?;
    webapi::input::mark_event_trusted(scope, ev);
    Some(ev.into())
}

/// A `SharedWorker` connection: the page's `new SharedWorker(url, name)` ships one
/// `__korvus_connect__` frame carrying the worker-side end of the W3b MessagePort channel, and this
/// realm turns it into the `connect` MessageEvent a `SharedWorkerGlobalScope` fires.
///
/// `MessageEvent.source` is the connecting port, and testharness.js reads exactly that
/// (`SharedWorkerTestEnvironment`: `this_obj._add_message_port(message_event.source)`).
fn deliver_connect<'s>(scope: &mut v8::PinScope<'s, '_>, ports: v8::Local<'s, v8::Array>, first: v8::Local<'s, v8::Value>) {
    let empty = str_val(scope, "");
    let Some(ev) = new_message_event(scope, "connect", empty, ports.into(), Some(first)) else { return };
    queue_or_deliver(scope, ev);
}

/// One arrival on a `ServiceWorker`'s client channel, re-dispatched on the global as a `message`
/// event whose `source` is the paired port -- the role a `Client` plays in a real
/// `ServiceWorkerGlobalScope`, and what testharness.js's `ServiceWorkerTestEnvironment` posts its
/// results back through (`_add_message_port(event.source)`).
fn cb_client_port_message<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let port = webapi::binding::protos::body_data(scope, &args);
    let incoming = args.get(0);
    let data = get_prop(scope, incoming, "data").unwrap_or_else(|| v8::undefined(scope).into());
    let ports = match get_prop(scope, incoming, "ports").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        Some(a) if a.length() > 0 => a,
        _ => v8::Array::new(scope, 0),
    };
    let Some(ev) = new_message_event(scope, "message", data, ports.into(), Some(port)) else { return };
    queue_or_deliver(scope, ev);
}

/// The page opens one `MessagePort` per `serviceWorker.postMessage` target (`worker.rs`'s
/// `__serviceWorkerConnect`); everything it sends arrives here.
fn bind_client_port<'s>(scope: &mut v8::PinScope<'s, '_>, port: v8::Local<'s, v8::Value>) {
    // `Client.postMessage` answers on this same port from now on, so the two ways a
    // service worker talks to its page stay in one order.
    {
        let g = v8::Global::new(scope, port);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().put(webapi::SwClientPort(g));
    }
    let handler = build_fn_data(scope, cb_client_port_message, 1, port);
    if let Ok(obj) = v8::Local::<v8::Object>::try_from(port) {
        set_prop(scope, obj, "onmessage", handler.into());
    }
    v8::tc_scope!(let tc, scope);
    if let Some(start) = get_prop(tc, port, "start").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        start.call(tc, port, &[]);
    }
    tc.reset();
}

// ── the three loops ─────────────────────────────────────────────────────────────────────────────

/// Call an async receive op and attach `step` to the promise it hands back. This is one turn of a
/// `while (true) { await … }`, and the step re-calls it to take the next.
fn arm(scope: &mut v8::PinScope, op_name: &str, step: impl v8::MapFnTo<v8::FunctionCallback>) {
    let Some(f) = op(scope, op_name) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    v8::tc_scope!(let tc, scope);
    let Some(p) = f.call(tc, undef, &[]) else {
        tc.reset();
        return;
    };
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(p) else {
        tc.reset();
        return;
    };
    let cont = build_fn(tc, step, "", 1);
    // each delivery a task of its own, not a promise job
    webapi::then_task(tc, promise, cont, None);
    tc.reset();
}

fn arm_message(scope: &mut v8::PinScope) {
    arm(scope, "op_worker_self_recv", cb_message_step);
}
fn arm_broadcast(scope: &mut v8::PinScope) {
    arm(scope, "op_bc_self_recv", cb_broadcast_step);
}
fn arm_port(scope: &mut v8::PinScope) {
    arm(scope, "op_port_self_recv", cb_port_step);
}

/// The `postMessage` plane. A W3b frame is `[u32 n_ports][u32 channel_id * n_ports][structured-clone
/// data]`: reconstruct a `MessagePort` per id (marked remote, so its posts route back to the peer),
/// delivered as `MessageEvent.ports`.
fn cb_message_step<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let frame = args.get(0);
    v8::tc_scope!(let tc, scope);
    message_step(tc, frame);
    if tc.has_caught() {
        let msg = tc.exception().map(|e| e.to_rust_string_lossy(tc)).unwrap_or_else(|| "?".to_string());
        tc.reset();
        note_loop_fail(tc, msg);
    }
}

fn message_step<'s>(scope: &mut v8::PinScope<'s, '_>, frame: v8::Local<'s, v8::Value>) {
    let Some((_view, bytes)) = frame_of(frame) else { return };
    if bytes.len() < 4 {
        return; // channel closed = terminated
    }
    let Some(n_ports) = u32_le(&bytes, 0) else { return };
    let mut off = 4usize;
    let ports = v8::Array::new(scope, n_ports as i32);
    let mut first: Option<v8::Local<'s, v8::Value>> = None;
    let mut port_objs: Vec<v8::Local<v8::Object>> = Vec::new();
    for i in 0..n_ports as usize {
        let Some(cid) = u32_le(&bytes, off) else { break };
        off += 4;
        let num: v8::Local<v8::Value> = v8::Number::new(scope, cid as f64).into();
        let Some(p) = call_op(scope, "op_mp_remote", &[num]) else { continue };
        ports.set_index(scope, i as u32, p);
        if let Ok(o) = v8::Local::<v8::Object>::try_from(p) {
            port_objs.push(o);
        }
        if first.is_none() {
            first = Some(p);
        }
    }
    // A port inside the message is the port received at its index.
    let data = webapi::wire_deserialize(scope, &bytes[off.min(bytes.len())..], &port_objs);
    let marker = data.is_string().then(|| data.to_rust_string_lossy(scope));
    let kind = scope_kind(scope);
    // The SharedWorker / ServiceWorker connect markers are not messages: they become the `connect`
    // event and the client-port binding, and never reach `onmessage` (a SharedWorkerGlobalScope has
    // no `onmessage` for one to reach). Both branches `continue`d in the JS -- they re-armed WITHOUT
    // consulting the close flag -- and they still do.
    if kind == "shared"
        && marker.as_deref() == Some("__korvus_connect__")
        && n_ports > 0
        && webapi::binding::protos::intrinsic(scope, "MessageEvent").is_some()
    {
        if let Some(f) = first {
            deliver_connect(scope, ports, f);
        }
        arm_message(scope);
        return;
    }
    if kind == "service" && marker.as_deref() == Some("__korvus_sw_connect__") && n_ports > 0 {
        if let Some(f) = first {
            bind_client_port(scope, f);
        }
        arm_message(scope);
        return;
    }
    // `self.onmessage` is an event handler listener in the same list as addEventListener's: one
    // dispatch runs both, in registration order, and a throwing handler is reported (report.rs) to
    // this realm's `error` event and then to the parent `Worker`'s `onerror`.
    if let Some(ev) = new_message_event(scope, "message", data, ports.into(), None) {
        dispatch_on_global(scope, ev);
    }
    if worker_closed(scope) {
        return;
    }
    arm_message(scope);
}

/// The cross-isolate `BroadcastChannel` plane: `[u32 LE name_len][name][structured-clone bytes]`,
/// forwarded from the page (or a peer worker), delivered to THIS worker's local channels of that
/// name by the native op.
///
/// The name was decoded by a `new TextDecoder()` the JS kept alive across the whole loop; UTF-8
/// decoding is `String::from_utf8_lossy` here, which is the same lossy replacement behaviour
/// `TextDecoder`'s default gives.
fn cb_broadcast_step<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let frame = args.get(0);
    v8::tc_scope!(let tc, scope);
    broadcast_step(tc, frame);
    if tc.has_caught() {
        let msg = tc.exception().map(|e| e.to_rust_string_lossy(tc)).unwrap_or_else(|| "?".to_string());
        tc.reset();
        note_loop_fail(tc, msg);
    }
}

fn broadcast_step<'s>(scope: &mut v8::PinScope<'s, '_>, frame: v8::Local<'s, v8::Value>) {
    let Some((view, bytes)) = frame_of(frame) else { return };
    if bytes.len() < 4 {
        return; // channel closed = terminated
    }
    let Some(name_len) = u32_le(&bytes, 0) else { return };
    let name_end = (4 + name_len as usize).min(bytes.len());
    let name = String::from_utf8_lossy(&bytes[4..name_end]).into_owned();
    let name_v = str_val(scope, &name);
    let Some(msg) = subarray(scope, view, name_end, bytes.len() - name_end) else { return };
    call_op(scope, "op_bc_deliver_external", &[name_v, msg]);
    if worker_closed(scope) {
        return;
    }
    arm_broadcast(scope);
}

/// The W3b cross-isolate `MessagePort` plane: a post routed INTO this worker for a transferred port
/// arrives as `[u32 LE channel_id][structured-clone bytes]`; deliver it to the local port registered
/// for that id.
fn cb_port_step<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let frame = args.get(0);
    v8::tc_scope!(let tc, scope);
    port_step(tc, frame);
    if tc.has_caught() {
        let msg = tc.exception().map(|e| e.to_rust_string_lossy(tc)).unwrap_or_else(|| "?".to_string());
        tc.reset();
        note_loop_fail(tc, msg);
    }
}

fn port_step<'s>(scope: &mut v8::PinScope<'s, '_>, frame: v8::Local<'s, v8::Value>) {
    let Some((view, bytes)) = frame_of(frame) else { return };
    if bytes.len() < 4 {
        return; // channel closed = terminated
    }
    let Some(cid) = u32_le(&bytes, 0) else { return };
    let num: v8::Local<v8::Value> = v8::Number::new(scope, cid as f64).into();
    let Some(rest) = subarray(scope, view, 4, bytes.len() - 4) else { return };
    call_op(scope, "op_port_deliver_external", &[num, rest]);
    if worker_closed(scope) {
        return;
    }
    arm_port(scope);
}

// ── what runs when the worker's top-level script has been evaluated ───────────────────────────────

// THE BRIDGE HOLDER IS GONE. `globalThis[Symbol.for("korvus.bridge")]` held two native
// functions, `__flushConnects` and -- in a ServiceWorker realm -- `__swScriptLoaded`, for one caller: the JS
// strings `worker.rs` appended to a worker's top-level script (`CONNECT_FLUSH`, `SW_FLUSH`) so they ran the
// moment that script had been evaluated. Rust runs at that moment too -- `execute_script` has just
// returned, and no microtask has run (deno_core's policy is explicit) -- so `worker.rs`'s
// `after_top_level` calls [`flush_connects`] and `webapi::sw_script_loaded` itself, and nothing is left for
// a page to find on its global: Chrome's worker global has no symbol-keyed member at all (measured, all three
// kinds). The load-failure reporter that sat here went earlier: a URL-loaded worker script is
// fetched and run from Rust, which sends a load failure down the error channel directly.

/// Seed the connect queue and arm all three receive loops.
///
/// Called from `worker.rs` at the end of the realm's Rust block, for a REAL WorkerGlobalScope only:
/// the bare messaging-only isolate registers none of these ops and drives `op_worker_self_recv` from
/// its own source.
pub fn arm_receive_loops(scope: &mut v8::PinScope) {
    {
        let op_state = JsRuntime::op_state_from(scope);
        op_state.borrow_mut().put(ConnectQueue(Some(Vec::new())));
    }
    arm_message(scope);
    arm_broadcast(scope);
    arm_port(scope);
}
