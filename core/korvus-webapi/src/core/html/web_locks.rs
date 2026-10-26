//! `core/html/web_locks.rs` -- the Web Locks API, `LockManager` and `Lock`, as `interface!` tables.
//!
//! WHY IT IS NOT A STUB ANY MORE. The registry's D2 rows made `navigator.locks.request(name, cb)`
//! resolve `undefined` WITHOUT EVER CALLING `cb` -- so whatever a page serialises through a lock never
//! ran. supabase-js takes its auth-token refresh through `navigator.locks.request`; the stub-call
//! counter over the failing replay rows found pages calling both members (C3 FIND, `stubhits/`).
//!
//! Measured in Chrome 150 on `tools/realweb/seed/sx`: `request("L", cb)` calls `cb` with a `Lock` whose
//! `name` is "L" and `mode` "exclusive", and resolves with what `cb` returned (42); `query()` resolves
//! an object (`{held, pending}`, the `LockManagerSnapshot` dictionary).
//!
//! SCOPE. One realm's locks: the lock manager of this engine's one origin and one agent, so requests
//! from this realm queue against each other exactly as the spec's per-origin queue orders them. Locks
//! held by ANOTHER realm of the same origin (a worker, a frame) are not seen here -- there is no
//! cross-realm lock registry yet, and a page that coordinates two contexts through a lock would see
//! both granted.

use deno_core::v8;

use crate::binding::protos::{brand_shell_instance, build_fn_data, get_prop, make_dom_exc, shell_proto, str_val, table_receiver_ok, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;

const LOCK_NAME: &str = "korvus.lock.name";
const LOCK_MODE: &str = "korvus.lock.mode";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Exclusive,
    Shared,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Exclusive => "exclusive",
            Mode::Shared => "shared",
        }
    }
}

struct Request {
    id: u64,
    name: String,
    mode: Mode,
    callback: v8::Global<v8::Function>,
    resolver: v8::Global<v8::PromiseResolver>,
}

/// This realm's lock manager: the granted locks and the request queue, in request order.
#[derive(Default)]
struct LockState {
    next: u64,
    held: Vec<(u64, String, Mode)>,
    pending: Vec<Request>,
    /// Granted, waiting for their task to run the callback.
    granted: std::collections::HashMap<u64, Request>,
    /// A request's promise, from the moment its callback ran until its lock is released (H14).
    settling: std::collections::HashMap<u64, v8::Global<v8::PromiseResolver>>,
    client_id: String,
}

fn with_state<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut LockState) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<LockState>().is_none() {
        st.put(LockState { client_id: v4_uuid(), ..LockState::default() });
    }
    f(st.borrow_mut::<LockState>())
}

/// A v4 UUID: this realm's `clientId` in a `query()` snapshot (`crypto.randomUUID`'s construction).
fn v4_uuid() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

/// Could a `mode` lock on `name` be granted now, with the requests queued AHEAD of it considered?
fn grantable(state: &LockState, name: &str, mode: Mode, ahead: &[Request]) -> bool {
    let held_conflict = state.held.iter().any(|(_, n, m)| n == name && (mode == Mode::Exclusive || *m == Mode::Exclusive));
    let queued_before = ahead.iter().any(|r| r.name == name);
    !held_conflict && !queued_before
}

/// Grant every queued request the queue allows, in order, each on a task of its own.
fn process_queue(scope: &mut v8::PinScope) {
    let ready: Vec<u64> = with_state(scope, |s| {
        let mut out = Vec::new();
        let mut i = 0;
        while i < s.pending.len() {
            let (name, mode) = (s.pending[i].name.clone(), s.pending[i].mode);
            if grantable(s, &name, mode, &s.pending[..i]) {
                let r = s.pending.remove(i);
                s.held.push((r.id, r.name.clone(), r.mode));
                out.push(r.id);
                s.granted.insert(r.id, r);
            } else {
                i += 1;
            }
        }
        out
    });
    for id in ready {
        crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_run_granted, id, 0.0);
    }
}

fn make_lock<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, mode: Mode) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, "Lock")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    let n = str_val(scope, name);
    priv_set(scope, o, LOCK_NAME, n);
    let m = str_val(scope, mode.as_str());
    priv_set(scope, o, LOCK_MODE, m);
    Some(o)
}

/// The task a grant queued: run the callback with the `Lock` (or `null`, for an `ifAvailable` request
/// that was not available), settle the request with its result, and release the lock once that
/// result settles.
fn cb_run_granted(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u64) else { return };
    let Some(req) = with_state(scope, |s| s.granted.remove(&id)) else { return };
    let held = with_state(scope, |s| s.held.iter().any(|(h, _, _)| *h == id));
    let lock: v8::Local<v8::Value> = if held {
        match make_lock(scope, &req.name, req.mode) {
            Some(l) => l.into(),
            None => v8::null(scope).into(),
        }
    } else {
        v8::null(scope).into()
    };
    let callback = v8::Local::new(scope, &req.callback);
    let resolver = v8::Local::new(scope, &req.resolver);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let outcome = {
        v8::tc_scope!(let tc, scope);
        let r = callback.call(tc, undef, &[lock]);
        match r {
            Some(v) => Ok(v8::Global::new(tc, v)),
            None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
        }
    };
    // "Upon settlement of the callback's promise: RELEASE the lock, THEN settle the request's promise
    // with the same result" (Web Locks §lock-request-steps). Settling first let an `await request(...)`
    // continue while the lock was still held, so re-requesting it `ifAvailable` read null -- supabase's
    // "Acquiring an exclusive Navigator LockManager lock ... immediately failed".
    let waited = v8::PromiseResolver::new(scope).unwrap();
    match outcome {
        Ok(v) => {
            let v = v8::Local::new(scope, &v);
            waited.resolve(scope, v);
        }
        Err(e) => {
            let e: v8::Local<v8::Value> = match e {
                Some(e) => v8::Local::new(scope, &e),
                None => v8::undefined(scope).into(),
            };
            waited.reject(scope, e);
        }
    }
    let rg = v8::Global::new(scope, resolver);
    with_state(scope, |s| s.settling.insert(id, rg));
    let d = v8::Array::new(scope, 2);
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    d.set_index(scope, 0, idv);
    let heldv = v8::Boolean::new(scope, held);
    d.set_index(scope, 1, heldv.into());
    let on_ok = build_fn_data(scope, cb_release_fulfilled, "", 1, d.into());
    let on_err = build_fn_data(scope, cb_release_rejected, "", 1, d.into());
    waited.get_promise(scope).then2(scope, on_ok, on_err);
}

fn release_and_settle(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, fulfilled: bool) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, args)) else { return };
    let id = d.get_index(scope, 0).and_then(|v| v.number_value(scope)).map(|n| n as u64);
    let held = d.get_index(scope, 1).is_some_and(|v| v.is_true());
    let Some(id) = id else { return };
    if held {
        with_state(scope, |s| s.held.retain(|(h, _, _)| *h != id));
        process_queue(scope);
    }
    let Some(rg) = with_state(scope, |s| s.settling.remove(&id)) else { return };
    let r = v8::Local::new(scope, &rg);
    if fulfilled {
        r.resolve(scope, args.get(0));
    } else {
        r.reject(scope, args.get(0));
    }
}

fn cb_release_fulfilled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    release_and_settle(scope, &args, true);
}

fn cb_release_rejected(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    release_and_settle(scope, &args, false);
}

fn rejected<'s>(scope: &mut v8::PinScope<'s, '_>, e: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    crate::binding::protos::rejected_promise(scope, e)
}

/// `request(name, [options,] callback)`.
fn cb_lm_request<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let recv_ok = shell_proto(scope, "LockManager").is_some_and(|p| table_receiver_ok(scope, args.this().into(), p.into()));
    if !recv_ok {
        let m = v8::String::new(scope, "Illegal invocation").unwrap();
        let e = v8::Exception::type_error(scope, m);
        let p = rejected(scope, e);
        rv.set(p);
        return;
    }
    let Some(name) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let (options, callback) = if args.length() >= 3 { (args.get(1), args.get(2)) } else { (v8::undefined(scope).into(), args.get(1)) };
    let Ok(callback) = v8::Local::<v8::Function>::try_from(callback) else {
        let which = if args.length() >= 3 { 3 } else { 2 };
        // Blink's context applied after the error is made
        let e = deno_core::error::native_error(scope, "TypeError", &format!("Failed to execute 'request' on 'LockManager': parameter {which} is not of type 'Function'."));
        let p = rejected(scope, e);
        rv.set(p);
        return;
    };
    // `LockOptions`, converted as WebIDL converts it (Chrome 150): `ifAvailable`, `mode` (an
    // enum), `signal` (an AbortSignal), `steal`, each read once -- a refusal rejects (korvus read `mode` first)
    let options = match crate::install::ctor_dicts::convert_dict_caught(scope, options, "LockOptions", "Failed to execute 'request' on 'LockManager': ") {
        Ok(o) => v8::Local::<v8::Value>::from(o),
        Err(e) => {
            let p = rejected(scope, e);
            rv.set(p);
            return;
        }
    };
    let mut mode = Mode::Exclusive;
    let mut if_available = false;
    if let Ok(o) = v8::Local::<v8::Object>::try_from(options) {
        if let Some(m) = get_prop(scope, o, "mode").filter(|v| !v.is_undefined()) {
            match m.to_rust_string_lossy(scope).as_str() {
                "exclusive" => mode = Mode::Exclusive,
                "shared" => mode = Mode::Shared,
                other => {
                    let e = deno_core::error::native_error(scope, "TypeError", &format!("Failed to execute 'request' on 'LockManager': Failed to read the 'mode' property from 'LockOptions': The provided value '{other}' is not a valid enum value of type LockMode."));
                    let p = rejected(scope, e);
                    rv.set(p);
                    return;
                }
            }
        }
        if_available = get_prop(scope, o, "ifAvailable").is_some_and(|v| v.boolean_value(scope));
        let steal = get_prop(scope, o, "steal").is_some_and(|v| v.boolean_value(scope));
        let signal = get_prop(scope, o, "signal").filter(|v| !v.is_null_or_undefined());
        // The option conflicts Chrome 150 rejects with NotSupportedError (measured).
        let conflict = if steal && if_available {
            Some("The 'steal' and 'ifAvailable' options cannot be used together.")
        } else if steal && mode == Mode::Shared {
            Some("The 'steal' option may only be used with 'exclusive' locks.")
        } else if signal.is_some() && (steal || if_available) {
            Some("The 'signal' and 'steal' or 'ifAvailable' options cannot be used together.")
        } else {
            None
        };
        if let Some(msg) = conflict {
            let Some(e) = make_dom_exc(scope, &format!("Failed to execute 'request' on 'LockManager': {msg}"), "NotSupportedError") else { return };
            let p = rejected(scope, e);
            rv.set(p);
            return;
        }
        // An already-aborted signal rejects with its reason (an AbortError when it gave none).
        if let Some(sig) = signal.and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            if get_prop(scope, sig, "aborted").is_some_and(|v| v.is_true()) {
                let reason = get_prop(scope, sig, "reason").filter(|v| !v.is_undefined());
                let e = match reason {
                    Some(r) => r,
                    None => match make_dom_exc(scope, "signal is aborted without reason", "AbortError") { Some(e) => e, None => return },
                };
                let p = rejected(scope, e);
                rv.set(p);
                return;
            }
        }
    }
    if name.starts_with('-') {
        let Some(e) = make_dom_exc(scope, "Failed to execute 'request' on 'LockManager': Names cannot start with '-'.", "NotSupportedError") else { return };
        let p = rejected(scope, e);
        rv.set(p);
        return;
    }
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    let req_callback = v8::Global::new(scope, callback);
    let req_resolver = v8::Global::new(scope, resolver);
    let available = with_state(scope, |s| grantable(s, &name, mode, &s.pending));
    if if_available && !available {
        // Not available: the callback runs with `null`, on a task, and nothing is held.
        let id = with_state(scope, |s| {
            s.next += 1;
            let id = s.next;
            s.granted.insert(id, Request { id, name, mode, callback: req_callback, resolver: req_resolver });
            id
        });
        crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_run_granted, id, 0.0);
    } else {
        with_state(scope, |s| {
            s.next += 1;
            let id = s.next;
            s.pending.push(Request { id, name, mode, callback: req_callback, resolver: req_resolver });
        });
        process_queue(scope);
    }
    rv.set(promise.into());
}

/// `query()`: `{held, pending}`, each a list of `{name, mode, clientId}`.
fn cb_lm_query<'s>(scope: &mut v8::PinScope<'s, '_>, _args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let (held, pending, client) = with_state(scope, |s| {
        (
            s.held.iter().map(|(_, n, m)| (n.clone(), *m)).collect::<Vec<_>>(),
            s.pending.iter().map(|r| (r.name.clone(), r.mode)).collect::<Vec<_>>(),
            s.client_id.clone(),
        )
    });
    let list = |scope: &mut v8::PinScope<'_, '_>, items: &[(String, Mode)]| -> v8::Global<v8::Array> {
        let arr = v8::Array::new(scope, items.len() as i32);
        for (i, (n, m)) in items.iter().enumerate() {
            let o = v8::Object::new(scope);
            for (k, v) in [("name", n.as_str()), ("mode", m.as_str()), ("clientId", client.as_str())] {
                let key = str_val(scope, k);
                let val = str_val(scope, v);
                o.set(scope, key, val);
            }
            arr.set_index(scope, i as u32, o.into());
        }
        v8::Global::new(scope, arr)
    };
    let h = list(scope, &held);
    let p = list(scope, &pending);
    let snapshot = v8::Object::new(scope);
    let hk = str_val(scope, "held");
    let hv: v8::Local<v8::Value> = v8::Local::new(scope, &h).into();
    snapshot.set(scope, hk, hv);
    let pk = str_val(scope, "pending");
    let pv: v8::Local<v8::Value> = v8::Local::new(scope, &p).into();
    snapshot.set(scope, pk, pv);
    let promise = crate::binding::protos::resolved_promise(scope, snapshot.into());
    rv.set(promise);
}

fn lock_slot(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, slot: &str, mut rv: v8::ReturnValue) {
    let this = args.this();
    let ok = shell_proto(scope, "Lock").is_some_and(|p| table_receiver_ok(scope, this.into(), p.into()));
    if !ok {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if let Some(v) = priv_get(scope, this, slot) {
        rv.set(v);
    }
}

fn cb_lock_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    lock_slot(scope, &args, LOCK_NAME, rv);
}

fn cb_lock_mode(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    lock_slot(scope, &args, LOCK_MODE, rv);
}

pub fn install_lock_manager(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "query" => method_row(scope, proto, member, cb_lm_query),
        "request" => method_row(scope, proto, member, cb_lm_request),
        other => debug_assert!(false, "install_lock_manager: unexpected member {other}"),
    }
}

pub fn install_lock(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "name" => getter_row(scope, proto, member, cb_lock_name),
        "mode" => getter_row(scope, proto, member, cb_lock_mode),
        other => debug_assert!(false, "install_lock: unexpected member {other}"),
    }
}

// Rows from the generated registry's `Lock` / `LockManager` blocks, in the capture's order, with their
// lengths and `required` counts; `[SecureContext]` as `tools/sync/data/secure_context.json` measured it.
webapi::interface! {
    "Lock" [interface, both, secure] {
        ctor "Lock"                         len 0 = stub,
        attr "name" : DomString readonly          = install_lock,
        attr "mode" : Any readonly                = install_lock
    }
}

webapi::interface! {
    "LockManager" [interface, both, secure] {
        ctor   "LockManager"                           len 0 = stub,
        method "query"   : Promise len 0                     = install_lock_manager,
        method "request" : Promise len 2 required 2          = install_lock_manager
    }
}
