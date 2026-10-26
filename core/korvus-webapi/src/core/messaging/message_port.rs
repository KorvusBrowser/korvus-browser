//! `core/messaging/message_port.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{op2, v8, CppgcBase};
use macros::webapi;
use crate::modules::broadcastchannel::serialize::{bc_deserialize, bc_serialize};
use crate::binding::protos::{build_fn, build_fn_data, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;
use super::transfer;

#[repr(C)]
#[derive(CppgcBase)]
pub struct MessagePort {
    pub(crate) other: std::cell::RefCell<Option<v8::Global<v8::Value>>>, // the paired port (same-isolate)
    pub(crate) started: std::cell::Cell<bool>,
    /// Messages awaiting delivery, each with its transferred ports -- they travel WITH their message,
    /// because that is the only thing that makes them observable in the right `MessageEvent`.
    pub(crate) queue: std::cell::RefCell<Vec<Queued>>,
    /// The messages [`mp_flush`] handed to the event loop, one delivery task queued for each: each task delivers the
    /// FIRST of them ([`cb_mp_deliver`]), so they go out in the order they were posted.
    pub(crate) inflight: std::cell::RefCell<std::collections::VecDeque<Queued>>,
    pub(crate) remote: std::cell::Cell<Option<u32>>, // W3b: cross-isolate channel id; when set, posts route out
    /// This port has been TRANSFERRED away, so it is dead in this agent: posts on it are silent
    /// no-ops and putting it in another transfer list is a `DataCloneError`.
    ///
    /// A field, not `other.is_none()`: a port reconstructed for a cross-isolate channel has no
    /// `other` and is alive, and `close()` leaves a port transferable (measured against Chrome 150).
    pub(crate) detached: std::cell::Cell<bool>,
}

pub(crate) trait MessagePortBehavior {
   fn construct() -> MessagePort where Self: Sized { stub!("MessagePort::constructor") }
}

#[webapi(base, own)]
impl MessagePort {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<MessagePort, ::deno_error::JsErrorBox> { dispatch!("MessagePort::constructor", <MessagePort as MessagePortBehavior>::construct()) }
}

impl MessagePortBehavior for MessagePort {
    fn construct() -> MessagePort { MessagePort { other: std::cell::RefCell::new(None), started: std::cell::Cell::new(false), queue: std::cell::RefCell::new(Vec::new()), inflight: std::cell::RefCell::new(std::collections::VecDeque::new()), remote: std::cell::Cell::new(None), detached: std::cell::Cell::new(false) } }
}
/// One message of a port's queue: the data, and the ports transferred with it (`None`: none were).
pub(crate) struct Queued {
    pub(crate) data: v8::Global<v8::Value>,
    pub(crate) ports: Option<v8::Global<v8::Value>>,
}
impl Queued {
    pub(crate) fn new(scope: &mut v8::PinScope, data: v8::Local<v8::Value>, ports: Option<v8::Local<v8::Value>>) -> Queued {
        Queued { data: v8::Global::new(scope, data), ports: ports.map(|p| v8::Global::new(scope, p)) }
    }
}
fn mp_message_event<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<v8::Value>, ports: Option<v8::Local<v8::Value>>) -> Option<v8::Local<'s, v8::Value>> {
    let mec = crate::binding::protos::iface_object(scope, "MessageEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let ty: v8::Local<v8::Value> = crate::binding::protos::key_str(scope, "message")?.into();
    // The init dictionary made whole in one step, with no prototype and only the members it has: the members it
    // lacks are read as absent, and a message with no transferred port gets the event's own empty frozen `ports`
    // (`MessageEvent`'s constructor makes one). Built a member at a time, it took a map change a member.
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let k_data: v8::Local<v8::Name> = crate::binding::protos::key_str(scope, "data")?.into();
    let init = match ports {
        Some(ports) => {
            let k_ports: v8::Local<v8::Name> = crate::binding::protos::key_str(scope, "ports")?.into();
            v8::Object::with_prototype_and_properties(scope, null, &[k_data, k_ports], &[data, ports])
        }
        None => v8::Object::with_prototype_and_properties(scope, null, &[k_data], &[data]),
    };
    // A message the engine delivers is TRUSTED: Chrome's port messages read `isTrusted`
    // true, and a challenge provider a challenge widget's challenge talks to its worker only through a gate on it.
    let ev = mec.new_instance(scope, &[ty, init.into()])?;
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    Some(ev.into())
}
/// Deliver the first message handed to the event loop for this port (a task of its own, one queued per message by
/// [`mp_flush`]): data is the port.
fn cb_mp_deliver(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let port = crate::binding::protos::body_data(scope, &args);
    let Some(mph) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port) else { return };
    let next = unsafe { mph.as_ref() }.inflight.borrow_mut().pop_front();
    let Some(next) = next else { return };
    let msg = v8::Local::new(scope, &next.data);
    let ports = next.ports.as_ref().map(|p| v8::Local::new(scope, p));
    let ev = match mp_message_event(scope, msg, ports) { Some(e) => e, None => return };
    // `onmessage` is an event handler listener in the same list as addEventListener's: dispatch runs it.
    crate::binding::protos::engine_call(scope, port, "dispatchEvent", &[ev]);
}
/// The port's delivery task: ONE function per port, made the first time it delivers and kept on the port (a private
/// property: a cycle the collector sees through, where a `Global` in the port would root it). It used to be made per
/// message, with an array per message for its data -- a function made from a template every call, with its shared
/// info and its dispatch-table entry. With the message queued as two handles (no `[data, ports]` array, no empty
/// `ports` array, no handle copied and dropped), a `MessageChannel` round trip went from 134,700 user instructions to
/// 112,900 (perf stat, 50,000 round trips, 2026-10-23); what is left is nearly all the `MessageEvent`'s construction
/// (66,000) and dispatch (36,000).
fn mp_deliver_fn<'s>(scope: &mut v8::PinScope<'s, '_>, port: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Function>> {
    let obj = v8::Local::<v8::Object>::try_from(port).ok()?;
    let key = crate::binding::protos::private_key(scope, "mp.deliver");
    if let Some(f) = obj.get_private(scope, key).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        return Some(f);
    }
    // The delivery runs in the PORT's realm, so its `MessageEvent` is that realm's own (`e instanceof
    // MessageEvent` in a frame): the task's callback is made there.
    let realm = obj.get_creation_context(scope);
    let f = match realm.filter(|r| *r != scope.get_current_context()) {
        Some(realm) => {
            let g = {
                let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
                let port = v8::Local::new(scope, port);
                let f = build_fn_data(scope, cb_mp_deliver, "", 0, port);
                v8::Global::new(scope, f)
            };
            v8::Local::new(scope, &g)
        }
        None => build_fn_data(scope, cb_mp_deliver, "", 0, port),
    };
    obj.set_private(scope, key, f.into());
    Some(f)
}
/// Drain a started port's queue, scheduling one delivery task per message.
pub(crate) fn mp_flush(scope: &mut v8::PinScope, port_l: v8::Local<v8::Value>) {
    let port_l = v8::Local::new(scope, port_l);
    let mph = match deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port_l) { Some(p) => p, None => return };
    let p = unsafe { mph.as_ref() };
    if !p.started.get() { return; }
    let n = {
        let mut queue = p.queue.borrow_mut();
        let n = queue.len();
        p.inflight.borrow_mut().extend(queue.drain(..));
        n
    };
    if n == 0 { return; }
    let Some(cb) = mp_deliver_fn(scope, port_l) else { return };
    for _ in 0..n {
        // The engine's own task, never the page's `setTimeout`: a page whose timers
        // are themselves built on a MessageChannel would otherwise wait on itself forever.
        crate::core::intersection_observer::rendering::engine_task(scope, cb, 0.0);
    }
}
/// `MessagePort.postMessage(message, transfer)` — the second argument is real.
///
/// The order is HTML's, and every step of it is observable: the transfer list is read and validated
/// WHOLE first (so a bad list beats an unclonable message, and a throw leaves every port in it
/// alive), then the message is serialized (so a `DataCloneError` still transfers nothing), then the
/// ports are taken and the buffers detached, and only then is anything queued.
fn cb_mp_post_message<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !crate::binding::protos::require_args(scope, &args, 1, "MessagePort", "postMessage") { return; }
    let this = args.this();
    if !mp_recv(scope, this.into()) { return; }
    let this_v: v8::Local<v8::Value> = this.into();
    let Some(list) = transfer::read_port_transfer(scope, &args, this_v) else { return };
    let mph = match deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, this_v) { Some(p) => p, None => return };
    // W3b: a REMOTE (transferred) port's paired endpoint lives in another isolate -- serialize + route out
    // through the PortForward hook (engine ships the bytes to the peer via op_port_deliver_external).
    if let Some(remote_id) = unsafe { mph.as_ref() }.remote.get() {
        // ... and that frame format carries bytes and a channel id, nothing else. A PORT in the list
        // is refused out loud rather than dropped: see `transfer.rs`'s header.
        if list.has_ports() {
            transfer::throw_remote_unsupported(scope);
            return;
        }
        let bytes = bc_serialize(scope, args.get(0));
        {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            if let Some(fwd) = st.try_borrow::<PortForward>() { (fwd.0)(remote_id, bytes); }
        }
        transfer::detach_buffers(scope, &list);
        return;
    }
    // A DETACHED port -- one that has been transferred away -- drops the message silently, and so
    // does an unentangled one. The transfer list has already been validated either way, because
    // Chrome validates it before it looks at where the message is going.
    if unsafe { mph.as_ref() }.detached.get() { return; }
    // The entangled port, read in place (a clone of its `Global` is a handle made and dropped a call).
    fn peer<'s>(scope: &mut v8::PinScope<'s, '_>, p: &MessagePort) -> Option<v8::Local<'s, v8::Value>> {
        p.other.borrow().as_ref().map(|g| v8::Local::new(scope, g))
    }
    // The message is the RECEIVER's: read back, and its ports minted, in the realm of the port it is
    // delivered to -- the entangled port's (read before the transfer, which may re-point it). A port
    // message from the page to a frame was the page's objects there (Chrome 150 measured).
    let receiver = peer(scope, unsafe { mph.as_ref() }).and_then(|l| v8::Local::<v8::Object>::try_from(l).ok()).and_then(|o| o.get_creation_context(scope));
    // A transferred port stays itself in the clone and becomes the port the receiver gets.
    let originals = transfer::port_originals(&list);
    let Some(data) = crate::modules::structured_clone::clone_for_transfer_in(scope, receiver, args.get(0), "MessagePort", "postMessage", &originals) else { return };
    // (no port in the list -- nearly every message: nothing to take, nothing to substitute)
    let (data, ports) = if list.has_ports() {
        let (ports, pairs) = transfer::take_ports_mapped(scope, &list, receiver);
        (crate::modules::structured_clone::substitute(scope, data, &pairs), Some(ports.into()))
    } else {
        (data, None)
    };
    transfer::detach_buffers(scope, &list);
    // THE RECEIVER IS READ AFTER THE TRANSFER, and the order is load-bearing: `p.postMessage(m,
    // [p.peer])` transfers the very port this message is addressed to, and Chrome takes it (only
    // the SOURCE port is refused). The transfer re-points this port's `other` at the fresh
    // replacement, so reading `other` first would queue the message on the port that was just
    // neutered and nobody would ever read it.
    let Some(other_l) = peer(scope, unsafe { mph.as_ref() }) else { return };
    let queued = Queued::new(scope, data, ports);
    if let Some(oh) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, other_l) {
        unsafe { oh.as_ref() }.queue.borrow_mut().push(queued);
    }
    mp_flush(scope, other_l);
}
fn cb_mp_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !mp_recv(scope, this.into()) { return; }
    match deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, this.into()) {
        Some(mph) => { if unsafe { mph.as_ref() }.started.get() { return; } unsafe { mph.as_ref() }.started.set(true); }
        None => return,
    }
    let this_v: v8::Local<v8::Value> = this.into();
    mp_flush(scope, this_v);
}
fn cb_mp_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !mp_recv(scope, args.this().into()) { return; }
    if let Some(mph) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, args.this().into()) { unsafe { mph.as_ref() }.started.set(false); }
}

/// Installed by engine in each isolate's OpState: ship a remote port's post to the paired endpoint, keyed by
/// the cross-isolate channel id.
pub struct PortForward(pub Box<dyn Fn(u32, Vec<u8>) + Send>);
/// Per-isolate map: channel id -> the LOCAL MessagePort that RECEIVES for it, plus a `pending` buffer for
/// messages that arrived for an id before its port was reconstructed/registered (the transfer descriptor and
/// the first port message travel on separate channels, so either can land first). Draining `pending` on
/// registration preserves the messages instead of dropping them.
#[derive(Default)]
pub(crate) struct PortRegistry {
    pub(crate) ports: Vec<(u32, v8::Global<v8::Value>)>,
    pub(crate) pending: Vec<(u32, Vec<u8>)>,
}
/// Mark `port` as the cross-isolate endpoint for channel `id` (its posts route out) and register it as the
/// local receiver for `id`. Shared by [`op_mp_remote`] (the JS reconstruction sites in host.js /
/// worker-host.js) and engine's transfer path (`cb_worker_post`).
pub fn mp_register_remote(scope: &mut v8::PinScope, port: v8::Local<v8::Value>, id: u32) {
    match deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port) {
        Some(mph) => unsafe { mph.as_ref() }.remote.set(Some(id)),
        None => return,
    }
    let g = v8::Global::new(scope, port);
    // Register + take any messages buffered for this id before the port existed (init/first-message race).
    let pending: Vec<Vec<u8>> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let reg = st.borrow_mut::<PortRegistry>();
        reg.ports.retain(|(i, _)| *i != id);
        reg.ports.push((id, g));
        let mut drained = Vec::new();
        reg.pending.retain(|(i, b)| if *i == id { drained.push(b.clone()); false } else { true });
        drained
    };
    if !pending.is_empty() {
        let entries: Vec<Queued> = pending
            .iter()
            .map(|bytes| {
                let value = bc_deserialize(scope, bytes);
                Queued::new(scope, value, None)
            })
            .collect();
        if let Some(ph) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port) {
            unsafe { ph.as_ref() }.queue.borrow_mut().extend(entries);
        }
        mp_flush(scope, port);
    }
}


// ── the table ──────────────────────────────────────────────────────────────
//
// WHAT THIS REPLACES. The `MessagePort` block of `dom/08f-crypto.js` (and its copy in
// `core/korvus-runtime/src/js/worker-host.js`): a hand `Object.setPrototypeOf` onto
// `EventTarget.prototype`, five `Object.defineProperty` sites over `op_mp_*` builder round-trips
// onto the cppgc CLASS TEMPLATE's prototype, `globalThis.MessagePort =` the raw class (so
// `new MessagePort()` worked, where the capture records `newable: false`), and the two bridge slots
// `KB.__mpLink` / `KB.__mpSetRemote`.
//
// Those two are gone, not renamed: a port is no longer minted in JS at all. `MessageChannel`'s table
// constructor (`core/events/message_event.rs`) calls [`mp_channel_pair`] for a linked PAIR, and the
// two cross-isolate reconstruction sites ask [`op_mp_remote`] -- one of the four ops anything still
// reaches -- for a port already registered for a channel id. Both mint through [`mp_new`], which
// re-prototypes onto the TABLE's prototype. (`op_mp_channel`, the JS-facing door onto
// `mp_channel_pair`, is deleted: it had no caller of any kind.)

/// Mint a `MessagePort` -- a cppgc value carrying the table's prototype. The one mint site.
fn mp_new<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::class::mint(scope, <MessagePort as MessagePortBehavior>::construct());
    crate::install::live::set_table_proto(scope, obj, "MessagePort");
    obj
}

/// `new MessageChannel()`'s two entangled ports. The Rust-callable core: the `MessageChannel` table's
/// native constructor (`core/events/message_event.rs`) calls this directly, so the port pair is minted
/// by one function whether the caller is Rust or the remaining `#[op2]` seam.
pub(crate) fn mp_channel_pair<'s>(scope: &mut v8::PinScope<'s, '_>) -> (v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>) {
    let a = mp_new(scope);
    let b = mp_new(scope);
    let (av, bv): (v8::Local<v8::Value>, v8::Local<v8::Value>) = (a.into(), b.into());
    let (ag, bg) = (v8::Global::new(scope, av), v8::Global::new(scope, bv));
    if let Some(pa) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, av) { *unsafe { pa.as_ref() }.other.borrow_mut() = Some(bg); }
    if let Some(pb) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, bv) { *unsafe { pb.as_ref() }.other.borrow_mut() = Some(ag); }
    (av, bv)
}


/// A port that is the local endpoint of cross-isolate channel `id`: its posts route out through the
/// `PortForward` hook and messages buffered for `id` before it existed are drained into it. Replaces
/// `new MessagePort(); KB.__mpSetRemote(port, cid)`.
// reentrant: `mp_register_remote` may flush a buffered message, which schedules delivery via
// `setTimeout` (op_timer_set) -- an op-within-op deno_core rejects unless the outer op opts in.
#[op2(reentrant)]
pub(crate) fn op_mp_remote<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> v8::Local<'s, v8::Object> {
    mp_remote_port(scope, id)
}

/// A fresh port whose other end is in another isolate under channel `id` (a port a worker transferred to
/// this realm).
pub fn mp_remote_port<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> v8::Local<'s, v8::Object> {
    let port = mp_new(scope);
    let pv: v8::Local<v8::Value> = port.into();
    mp_register_remote(scope, pv, id);
    port
}

/// The receiver as a `MessagePort`, or the WebIDL `TypeError` a foreign one gets.
fn mp_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, this).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

/// `onmessage` is the one handler row with a side effect: assigning it STARTS the port (HTML's
/// "first time onmessage is set"). It writes and reads THE handler store -- `et_on_get` / `et_on_set`,
/// the same listener list `addEventListener` writes and `mp_flush`'s `dispatchEvent` runs -- so there
/// is no second store; the setter only adds the start in front. `onmessageerror` has no side effect
/// and takes the shared `event_handler` kit.
fn cb_mp_onmessage_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if !mp_recv(scope, this) { return; }
    let v = crate::core::events::event_target::et_on_get(scope, this, "message");
    rv.set(v);
}

fn cb_mp_onmessage_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this_v: v8::Local<v8::Value> = args.this().into();
    if !mp_recv(scope, this_v) { return; }
    let fn_arg = args.get(0);
    crate::core::events::event_target::et_on_set(scope, this_v, "message", fn_arg);
    let mut do_flush = false;
    if let Some(mph) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, this_v) {
        if !unsafe { mph.as_ref() }.started.get() { unsafe { mph.as_ref() }.started.set(true); do_flush = true; } // setter starts the port
    }
    if do_flush { mp_flush(scope, this_v); }
}

/// The `onmessage` row's body: the handler pair plus the start.
pub fn install_mp_onmessage(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    debug_assert_eq!(member.name, "onmessage");
    let g = build_fn(scope, cb_mp_onmessage_get, "get onmessage", 0);
    let s = build_fn(scope, cb_mp_onmessage_set, "set onmessage", 1);
    define_accessor(scope, proto, member, g, Some(s));
}

/// The body the three method rows of [`MESSAGEPORT`] name.
pub fn install_message_port(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "close" => method_row(scope, proto, member, cb_mp_close),
        "postMessage" => method_row(scope, proto, member, cb_mp_post_message),
        "start" => method_row(scope, proto, member, cb_mp_start),
        other => debug_assert!(false, "install_message_port: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `MessagePort` blocks of `install/generated_registry.rs` +
// `generated_worker_registry.rs` (identical member lists in both realms), in the capture's order.
// `[interface, both]`: the capture records `newable: false` and the bundle's oracle
// `new MessagePort()` -> TypeError, in both realms. `required` is the WINDOW capture's (the worker
// registry records 0 for `postMessage`; the oracle's `port.postMessage()` -> TypeError is the truth).
webapi::interface! {
    "MessagePort" : "EventTarget" [interface, both] {
        ctor    "MessagePort"                          len 0 = stub,
        handler "onmessage"                                  = install_mp_onmessage,
        handler "onmessageerror"                             = event_handler,
        method  "close"       : Object len 0                 = install_message_port,
        method  "postMessage" : Object len 1 required 1      = install_message_port,
        method  "start"       : Object len 0                 = install_message_port
    }
}

// The native ops this file owns — declared here, beside their definitions, so that adding one
// touches no shared file (binding/exposure.rs). build.rs collects every such declaration into the
// extension's op table on each build.
crate::webapi_ops![
    op_mp_remote, op_port_deliver_external
];


// ── moved out of bindings.rs (STEP 3 dissolution) ──
/// Is `value` a native MessagePort? (Engine scans a transfer list without seeing the private cppgc type.)
pub fn is_message_port(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> bool {
    deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, value).is_some()
}
/// Take `port`'s same-isolate peer (its `other`) and NEUTER `port` (clear its link, so it can no longer
/// deliver locally) -- the transferred port is dead in the source agent; its peer becomes the cross-isolate
/// endpoint. Returns the peer (None if the port was unentangled).
pub fn mp_take_peer<'s>(scope: &mut v8::PinScope<'s, '_>, port: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    let mph = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port)?;
    let peer_g = unsafe { mph.as_ref() }.other.borrow_mut().take();
    // The port is dead in this agent from here on: a second transfer of it is a `DataCloneError`
    // and a post on it is a silent no-op.
    unsafe { mph.as_ref() }.detached.set(true);
    peer_g.map(|g| v8::Local::new(scope, &g))
}

/// Has `port` been transferred away? `MessagePort`-typed callers only; false for anything else.
pub(crate) fn mp_is_detached(scope: &mut v8::PinScope, port: v8::Local<v8::Value>) -> bool {
    match deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port) {
        Some(mph) => unsafe { mph.as_ref() }.detached.get(),
        None => false,
    }
}

/// SAME-AGENT TRANSFER, the whole of it: `port` is neutered and a FRESH port takes its place, with
/// its entanglement, its cross-isolate registration and its queued messages.
///
/// The queue moves because a transferred port keeps the messages already sent to it — measured
/// against Chrome 150, which delivers them to the receiver in order once the new port is started.
/// The new port starts UNSTARTED whatever the old one was, which is what makes that observable.
/// It is minted in `realm`, the receiver's (`None`: the current realm) -- see [`transfer::take_ports`].
pub(crate) fn mp_adopt_transferred<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    port: v8::Local<'s, v8::Value>,
    realm: Option<v8::Local<'s, v8::Context>>,
) -> v8::Local<'s, v8::Value> {
    let fresh_v: v8::Local<v8::Value> = match realm.filter(|r| *r != scope.get_current_context()) {
        Some(realm) => {
            let g = {
                let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
                let fresh: v8::Local<v8::Value> = mp_new(scope).into();
                v8::Global::new(scope, fresh)
            };
            v8::Local::new(scope, &g)
        }
        None => mp_new(scope).into(),
    };
    let (peer_g, remote, queue) = match deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port) {
        Some(src) => {
            let peer = unsafe { src.as_ref() }.other.borrow_mut().take();
            let remote = unsafe { src.as_ref() }.remote.take();
            let queue: Vec<Queued> = unsafe { src.as_ref() }.queue.borrow_mut().drain(..).collect();
            unsafe { src.as_ref() }.started.set(false);
            unsafe { src.as_ref() }.detached.set(true);
            (peer, remote, queue)
        }
        None => return fresh_v,
    };
    if let Some(peer_g) = &peer_g {
        // The peer must point BACK at the new port, or its posts would still queue on the dead one.
        let peer_l = v8::Local::new(scope, peer_g);
        if let Some(ph) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, peer_l) {
            *unsafe { ph.as_ref() }.other.borrow_mut() = Some(v8::Global::new(scope, fresh_v));
        }
    }
    if let Some(fh) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, fresh_v) {
        *unsafe { fh.as_ref() }.other.borrow_mut() = peer_g;
        *unsafe { fh.as_ref() }.queue.borrow_mut() = queue;
    }
    // A port that was the local end of a cross-isolate channel hands that role over whole: the
    // registry entry for the id is REPLACED, so the peer isolate's posts land on the new port.
    if let Some(id) = remote {
        mp_register_remote(scope, fresh_v, id);
    }
    fresh_v
}
/// Deliver structured-clone `bytes` to the LOCAL port registered for channel `id`, as a MessageEvent (queue +
/// flush -> onmessage + dispatch). Engine's cross-isolate port pump calls this.
// reentrant: mp_flush schedules delivery via setTimeout (op_timer_set) -- an op-within-op deno_core
// rejects unless the outer op opts in.
#[op2(fast, reentrant)]
pub(crate) fn op_port_deliver_external<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, #[buffer] bytes: &[u8]) {
    let port_g = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<PortRegistry>().and_then(|r| r.ports.iter().find(|(i, _)| *i == id).map(|(_, g)| g.clone()))
    };
    let port_g = match port_g {
        Some(g) => g,
        None => {
            // The port for this id has not been reconstructed/registered yet -- buffer the message; the
            // pending queue is drained (in order) when `mp_register_remote` registers the port.
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            st.borrow_mut::<PortRegistry>().pending.push((id, bytes.to_vec()));
            return;
        }
    };
    // Built in the PORT's realm: this op is the main realm's, and a port living in a FRAME would otherwise
    // receive the page's objects (as `op_worker_msg_in`).
    let port_l = v8::Local::new(scope, &port_g);
    let realm = v8::Local::<v8::Object>::try_from(port_l).ok().and_then(|o| o.get_creation_context(scope));
    let realm = realm.unwrap_or_else(|| scope.get_current_context());
    let queued = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let value = bc_deserialize(scope, bytes);
        Queued::new(scope, value, None)
    };
    if let Some(ph) = deno_core::cppgc::try_unwrap_cppgc_object::<MessagePort>(scope, port_l) {
        unsafe { ph.as_ref() }.queue.borrow_mut().push(queued);
    }
    mp_flush(scope, port_l);
}
