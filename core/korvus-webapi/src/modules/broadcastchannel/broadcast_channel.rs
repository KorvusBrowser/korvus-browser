//! `modules/broadcastchannel/broadcast_channel.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{op2, v8, CppgcBase};
use macros::webapi;
use crate::modules::broadcastchannel::serialize::{bc_deserialize, bc_serialize};
use crate::binding::protos::{build_fn_data, get_prop, global_get, set_prop, str_val, throw_type_error as idb_throw_type};
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;
use crate::modules::indexeddb::backend::{idb_structured_clone, idb_throw};

#[derive(Default)]
pub(crate) struct BcRegistry {
    channels: Vec<(String, v8::Global<v8::Value>)>,
}
#[repr(C)]
#[derive(CppgcBase)]
pub struct BroadcastChannel {
    name: String,
    closed: std::cell::Cell<bool>,
}

pub(crate) trait BroadcastChannelBehavior {
   fn construct(_name: String) -> BroadcastChannel where Self: Sized { stub!("BroadcastChannel::constructor") }
}

#[webapi(base, own)]
impl BroadcastChannel {
    #[constructor]
    #[cppgc]
    fn constructor(#[string] name: String) -> Result<BroadcastChannel, ::deno_error::JsErrorBox> { dispatch!("BroadcastChannel::constructor", <BroadcastChannel as BroadcastChannelBehavior>::construct(name)) }
}

impl BroadcastChannelBehavior for BroadcastChannel {
    fn construct(name: String) -> BroadcastChannel { BroadcastChannel { name, closed: std::cell::Cell::new(false) } }
}
fn bc_message_event<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<v8::Value>, origin: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    let mec = crate::binding::protos::iface_object(scope, "MessageEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let ty = str_val(scope, "message");
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "data", data);
    set_prop(scope, init, "origin", origin);
    // Delivered by the engine, so TRUSTED (Chrome: `isTrusted` true).
    let ev = mec.new_instance(scope, &[ty, init.into()])?;
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    Some(ev.into())
}
fn bc_origin<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let o = global_get(scope, "location")
        .and_then(|l| v8::Local::<v8::Object>::try_from(l).ok())
        .and_then(|l| get_prop(scope, l, "origin"))
        .filter(|o| o.is_string());
    match o { Some(v) => v, None => str_val(scope, "") }
}
/// Deliver one broadcast (setTimeout task): data is `[channel, message, origin]`.
fn cb_bc_deliver(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let arr = match v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) { Ok(a) => a, Err(_) => return };
    let ch = match arr.get_index(scope, 0) { Some(c) => c, None => return };
    let msg = match arr.get_index(scope, 1) { Some(m) => m, None => return };
    let origin = match arr.get_index(scope, 2) { Some(o) => o, None => return };
    if let Some(b) = deno_core::cppgc::try_unwrap_cppgc_object::<BroadcastChannel>(scope, ch) {
        if unsafe { b.as_ref() }.closed.get() { return; }
    }
    let ev = match bc_message_event(scope, msg, origin) { Some(e) => e, None => return };
    // `onmessage` is an event handler listener in the same list as addEventListener's: dispatch runs it.
    crate::binding::protos::engine_call(scope, ch, "dispatchEvent", &[ev]);
}
fn cb_bc_post_message<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    let this_v: v8::Local<v8::Value> = this.into();
    if !bc_recv(scope, this_v) { return; }
    if !crate::binding::protos::require_args(scope, &args, 1, "BroadcastChannel", "postMessage") { return; }
    let (name, closed) = match deno_core::cppgc::try_unwrap_cppgc_object::<BroadcastChannel>(scope, this_v) { Some(b) => (unsafe { b.as_ref() }.name.clone(), unsafe { b.as_ref() }.closed.get()), None => return };
    if closed { idb_throw(scope, "InvalidStateError", "BroadcastChannel is closed"); return; }
    // Clone once, synchronously: an uncloneable value throws DataCloneError HERE (propagated, not swallowed).
    // In BroadcastChannel's own words (Chrome's DataCloneError names 'postMessage' on
    // 'BroadcastChannel'; this called the page's structuredClone, whose message names 'structuredClone' on
    // 'Window').
    let serialized = match crate::modules::structured_clone::clone_for_transfer(scope, args.get(0), "BroadcastChannel", "postMessage", &[]) {
        Some(c) => v8::Global::new(scope, c),
        None => return, // threw -> DataCloneError propagates
    };
    let this_g = v8::Global::new(scope, this_v);
    let peers: Vec<v8::Global<v8::Value>> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<BcRegistry>().channels.iter().filter(|(n, _)| *n == name).map(|(_, g)| g.clone()).collect()
    };
    for peer in peers {
        if peer == this_g { continue; } // never deliver to the sender
        let peer_l = v8::Local::new(scope, &peer);
        if let Some(pb) = deno_core::cppgc::try_unwrap_cppgc_object::<BroadcastChannel>(scope, peer_l) { if unsafe { pb.as_ref() }.closed.get() { continue; } }
        let ser_l = v8::Local::new(scope, &serialized);
        let cloned = idb_structured_clone(scope, ser_l); // each receiver gets an independent copy
        let cloned_l = v8::Local::new(scope, &cloned);
        let origin_v = bc_origin(scope);
        let peer_l2 = v8::Local::new(scope, &peer);
        let arr = v8::Array::new(scope, 3);
        arr.set_index(scope, 0, peer_l2);
        arr.set_index(scope, 1, cloned_l);
        arr.set_index(scope, 2, origin_v);
        let cb = build_fn_data(scope, cb_bc_deliver, "", 0, arr.into());
        // The engine's task, not the page's `setTimeout`.
        crate::core::intersection_observer::rendering::engine_task(scope, cb, 0.0);
    }
    // After the LOCAL fan-out, forward to the OTHER isolate(s) if a cross-isolate `BcForward`
    // hook is installed (worker/main runtimes). Inert in a plain window runtime (no hook) -> BC stays local.
    let need_forward = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let b = op_state.borrow();
        b.try_borrow::<BcForward>().is_some()
    };
    if need_forward {
        let ser_l = v8::Local::new(scope, &serialized);
        let bytes = bc_serialize(scope, ser_l);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let b = op_state.borrow();
        if let Some(fwd) = b.try_borrow::<BcForward>() {
            (fwd.0)(name, bytes);
        }
    }
}
fn cb_bc_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this_v: v8::Local<v8::Value> = args.this().into();
    if !bc_recv(scope, this_v) { return; }
    if let Some(b) = deno_core::cppgc::try_unwrap_cppgc_object::<BroadcastChannel>(scope, this_v) { unsafe { b.as_ref() }.closed.set(true); }
    let this_g = v8::Global::new(scope, this_v);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<BcRegistry>().channels.retain(|(_, g)| *g != this_g);
}

/// Hook engine installs to forward a local BroadcastChannel post `(name, structured-clone bytes)` to the
/// other isolate(s). Absent in a plain window runtime.
pub struct BcForward(pub Box<dyn Fn(String, Vec<u8>) + Send>);

/// Deliver structured-clone `bytes` to every LOCAL BroadcastChannel named `name`, as if from another origin
/// (another isolate). Engine's cross-isolate BC pump calls this; mirrors `cb_bc_post_message`'s delivery,
/// minus a sender to exclude.
// reentrant: this op schedules delivery via `setTimeout` (JS), which itself invokes `op_timer_set` --
// an op-within-op call deno_core rejects unless the outer op opts in.
#[op2(fast, reentrant)]
pub(crate) fn op_bc_deliver_external<'s>(scope: &mut v8::PinScope<'s, '_>, #[string] name: &str, #[buffer] bytes: &[u8]) {
    let value = bc_deserialize(scope, bytes);
    let value_g = v8::Global::new(scope, value);
    let peers: Vec<v8::Global<v8::Value>> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<BcRegistry>().channels.iter().filter(|(n, _)| n == name).map(|(_, g)| g.clone()).collect()
    };
    for peer in peers {
        let peer_l = v8::Local::new(scope, &peer);
        if let Some(pb) = deno_core::cppgc::try_unwrap_cppgc_object::<BroadcastChannel>(scope, peer_l) {
            if unsafe { pb.as_ref() }.closed.get() { continue; }
        }
        let ser_l = v8::Local::new(scope, &value_g);
        // each receiver gets an independent copy, built in ITS realm: this op is the main realm's, and a channel
        // in a FRAME would otherwise receive the page's objects (as `op_worker_msg_in`)
        let peer_obj = v8::Local::new(scope, &peer);
        let realm = v8::Local::<v8::Object>::try_from(peer_obj).ok().and_then(|o| o.get_creation_context(scope));
        let realm = realm.unwrap_or_else(|| scope.get_current_context());
        let cloned = {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            idb_structured_clone(scope, ser_l)
        };
        let cloned_l = v8::Local::new(scope, &cloned);
        let origin_v = bc_origin(scope);
        let peer_l2 = v8::Local::new(scope, &peer);
        let arr = v8::Array::new(scope, 3);
        arr.set_index(scope, 0, peer_l2);
        arr.set_index(scope, 1, cloned_l);
        arr.set_index(scope, 2, origin_v);
        let cb = build_fn_data(scope, cb_bc_deliver, "", 0, arr.into());
        // The engine's task, not the page's `setTimeout`.
        crate::core::intersection_observer::rendering::engine_task(scope, cb, 0.0);
    }
}

// ── the table ──────────────────────────────────────────────────────────────
//
// WHAT THIS REPLACES. The `BroadcastChannel` block of `dom/08f-crypto.js` (and its copy in
// `core/korvus-runtime/src/js/worker-host.js`): a hand `Object.setPrototypeOf`, three
// `Object.defineProperty` sites over `op_bc_*` builder round-trips onto the cppgc CLASS TEMPLATE's
// prototype, the bridge slot `KB.__bcRegister`, and a JS constructor WRAPPER whose only job was to
// call it after `new NativeBroadcastChannel(name)` -- because an op2 constructor cannot register its
// own `this`. [`cb_bc_ctor`] does all three in Rust: construct, re-prototype, register. The wrapper
// and the bridge slot are gone from both realms.

/// The receiver as a `BroadcastChannel`, or the WebIDL `TypeError` a foreign one gets.
fn bc_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<BroadcastChannel>(scope, this).is_some() {
        return true;
    }
    idb_throw_type(scope, "Illegal invocation");
    false
}

fn cb_bc_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(b) = deno_core::cppgc::try_unwrap_cppgc_object::<BroadcastChannel>(scope, this) else {
        idb_throw_type(scope, "Illegal invocation");
        return;
    };
    let n = unsafe { b.as_ref() }.name.clone();
    let v = str_val(scope, &n);
    rv.set(v);
}

/// `new BroadcastChannel(name)` -- the `ctor` row's body, and the reason `KB.__bcRegister` existed.
/// Construct the cppgc value, put it on the TABLE's prototype (or the subclass's, for
/// `class C extends BroadcastChannel {}`), then register it in the realm's open-channel registry so a
/// peer's `postMessage` can find it. The arity message is Blink's, recorded in the bundle's oracle
/// (`new BroadcastChannel()` -> TypeError).
pub fn cb_bc_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        idb_throw_type(scope, &crate::binding::protos::use_new_msg("BroadcastChannel"));
        return;
    }
    if args.length() < 1 {
        idb_throw_type(scope, "Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present.");
        return;
    }
    let name = args.get(0).to_rust_string_lossy(scope);
    let obj = crate::binding::class::mint(scope, <BroadcastChannel as BroadcastChannelBehavior>::construct(name.clone()));
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => crate::install::live::set_table_proto(scope, obj, "BroadcastChannel"),
    }
    let ov: v8::Local<v8::Value> = obj.into();
    let g = v8::Global::new(scope, ov);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().borrow_mut::<BcRegistry>().channels.push((name, g));
    }
    rv.set(ov);
}

/// The body every non-handler row of [`BROADCASTCHANNEL`] names.
pub fn install_broadcast_channel(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "name" => getter_row(scope, proto, member, cb_bc_name),
        "close" => method_row(scope, proto, member, cb_bc_close),
        "postMessage" => method_row(scope, proto, member, cb_bc_post_message),
        other => debug_assert!(false, "install_broadcast_channel: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `BroadcastChannel` blocks of `install/generated_registry.rs` +
// `generated_worker_registry.rs` (identical member lists in both realms), in the capture's order.
// `[value, both]` with `ctor len 1`: the capture's `newable` flag is `false` only because its probe
// calls `new BroadcastChannel()` with no argument, which Chrome rejects -- the bundle's oracle
// records `new BroadcastChannel("x")` -> `[object BroadcastChannel]` and the bare call as TypeError,
// and the registry's own `Model::Value` + `CtorDef { length: 1 }` says the same.
webapi::interface! {
    "BroadcastChannel" : "EventTarget" [value, both] {
        ctor    "BroadcastChannel"                       len 1 = stub,
        attr    "name"        : Object readonly                = install_broadcast_channel,
        handler "onmessage"                                    = event_handler,
        handler "onmessageerror"                               = event_handler,
        method  "close"       : Object len 0                   = install_broadcast_channel,
        method  "postMessage" : Object len 1 required 1        = install_broadcast_channel
    }
}

// The native ops this file owns — declared here, beside their definitions, so that adding one
// touches no shared file (binding/exposure.rs). build.rs collects every such declaration into the
// extension's op table on each build.
crate::webapi_ops![
    op_bc_deliver_external
];

