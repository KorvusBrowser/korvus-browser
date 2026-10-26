//! `core/workers/service_worker.rs` — `ServiceWorker`, `ServiceWorkerRegistration` and
//! `ServiceWorkerContainer`, the three tables that close W4.c's named REMAINS.
//!
//! WHAT THIS REPLACES: the 180-line ServiceWorker block of `core/korvus-runtime/src/js/host.js`
//! (`resolveWorkerSource` / `resolveWorkerUrl`, `swRegister`, `swConnectPort`, `setupSwFetch`,
//! `prepSwProtos`, `getSwContainer`), the `op_service_worker_source` SEAM W4.a opened so `Navigator`'s
//! `serviceWorker` row could reach a JS-built container, and the two TRANSITIONAL ops W4.c added to
//! hold the seam open — `op_start_worker_pumps` and `op_sw_fetch_activate`. Both are direct Rust calls
//! now ([`super::shared_worker::start_isolate_pumps`] and
//! [`crate::modules::fetch::fetch_global::activate_sw_fetch`]), and both ops are deleted.
//!
//! # The mechanism this deletes, not just the lines
//!
//! `prepSwProtos` REPLACED the three shell prototypes' D2 stub members at runtime, with
//! `op_nativize`-masked JS closures — thirteen `Object.defineProperty` calls and thirteen native
//! masks, on prototypes the installer had already built from the registry. That is two writers of
//! every one of those slots (invariant I2) and thirteen lies about `[native code]` (I3). The rows
//! below are the ONE definition of each, and the bodies are Rust, so neither mechanism is left.
//!
//! # The shape, measured on `google-chrome --headless=new` 150.0.7871.46 (loopback, 2026-09-12)
//!
//! The JS bodies had NO receiver check at all: every one answered its shape default for a foreign
//! receiver. Chrome splits them, and the split is WebIDL's, not a preference:
//!
//! * a plain attribute or operation THROWS — `controller`, `scope`, `scriptURL`, `state`,
//!   `startMessages` and `postMessage` all answered `TypeError: Illegal invocation`;
//! * a PROMISE-returning one REJECTS instead of throwing — `ready`, `register`, `getRegistration`,
//!   `getRegistrations`, `unregister` and `update` each handed back a `Promise` for `.call({})`.
//!
//! So the promise rows use [`branded_promise_method_row`] and the rest [`branded_getter_row`] /
//! [`branded_method_row`], which is exactly that rule. Also measured:
//! `Object.getOwnPropertyNames(navigator.serviceWorker)` is `[]` (per-instance state is in v8
//! privates here, as it was in the JS's `WeakMap`, so it stays `[]`);
//! `Object.prototype.toString.call(navigator.serviceWorker)` is `[object ServiceWorkerContainer]`;
//! `navigator.serviceWorker.ready === navigator.serviceWorker.ready`; `register.length` is 1 and
//! `postMessage.length` is 1.
//!
//! # The defect the port fixes, beyond the receivers
//!
//! Every `message` from a ServiceWorker fired the page's `navigator.serviceWorker.onmessage` TWICE.
//! The JS read the handler off the container and CALLED it, then also `dispatchEvent`ed the event —
//! and the container's `onmessage` is the realm's event-handler store (`install::kits::event_handler`,
//! the registry's own row), which `dispatchEvent` already runs. Two writers, one handler, two calls.
//! The dispatch is the only delivery here.
//!
//! # What did NOT change, and why
//!
//! `ready` stays PENDING until a registration covering the page exists and then resolves with it
//! (it resolved `undefined` at mint time until then). `getRegistration(scope)` keys
//! on the EXACT scope string rather than matching a client URL against a scope prefix, and
//! `controller` stays `null` — both are carried over verbatim from the JS, because this unit is a
//! PORT and value-identity is the bar. They are honest gaps in the same place they were before, not
//! new ones: a fuller ServiceWorker lifecycle is W5.3/W5.4's, which is where the registration states
//! (`installing` -> `waiting` -> `active`) become real too.

use std::cell::RefCell;

use deno_core::v8;

use crate::binding::protos::{
    build_fn, call_method, get_prop, new_global, rejected_promise, rejected_type_error, resolved_promise, set_prop, str_val,
};
use crate::core::workers::worker::{slot_get, slot_set, worker_source, worker_url};
use crate::core::xmlhttprequest::xml_http_request::deno_op;
use crate::install::webidl::{brand, branded_getter_row, branded_method_row, branded_promise_method_row};
use crate::install::MemberDef;

// ── per-instance state (v8 privates, so an instance owns no property) ────────────────────────────

/// The SW isolate id this `ServiceWorker` object stands for.
const SW_ID: &str = "korvus_sw_worker_id";
const SW_SCRIPT_URL: &str = "korvus_sw_script_url";
const SW_STATE: &str = "korvus_sw_state";
/// The location the worker's realm was spawned with -- the key its lifecycle reports carry:
/// the script URL, or `about:blank` for a literal source (`korvus-runtime`'s `worker_location`).
const SW_LIFE_KEY: &str = "korvus_sw_life_key";
/// The lazily-opened cross-isolate `MessagePort` every `postMessage` on this object rides.
const SW_PORT: &str = "korvus_sw_port";

const REG_SCOPE: &str = "korvus_swr_scope";
/// The three registration states, in the order `register()` fills them (only `installing` today).
const REG_STATES: [&str; 3] = ["korvus_swr_installing", "korvus_swr_waiting", "korvus_swr_active"];

const CONTAINER_CONTROLLER: &str = "korvus_swc_controller";
const CONTAINER_READY: &str = "korvus_swc_ready";

// ── per-realm state ─────────────────────────────────────────────────────────────────────────────

/// The page's ServiceWorker bookkeeping, in `OpState` so it is PER REALM.
///
/// It was three module-scoped closure variables in `host.js` (`swContainer`, `swRegs`, and
/// `__swClientMsgReady`), which died with their realm. `OpState` has the same lifetime; a
/// `thread_local` would not, and `shared_worker::PumpedIsolates` records what that costs.
#[derive(Default)]
pub(crate) struct SwPage {
    /// The one `ServiceWorkerContainer` `navigator.serviceWorker` hands back.
    container: RefCell<Option<v8::Global<v8::Object>>>,
    /// scope -> registration, in REGISTRATION ORDER (`getRegistrations()` answers this sequence).
    regs: RefCell<Vec<(String, v8::Global<v8::Object>)>>,
    /// The `korvus-sw-client-msg` channel, held because nothing else refers to it once
    /// [`setup_client_messages`] returns — and a `BroadcastChannel` with no strong reference is
    /// collectable, and a collected one stops delivering. The JS held it in a function-local `const`,
    /// which is the same defect `fetch_global::SwRespChannel` already documents on the sibling channel.
    client_msg: RefCell<Option<v8::Global<v8::Object>>>,
    /// The resolver behind the container's `ready` promise, held until a registration whose scope
    /// covers this page settles it (and dropped then, so it settles once).
    ready_resolver: RefCell<Option<v8::Global<v8::PromiseResolver>>>,
}

fn with_page<R>(scope: &mut v8::PinScope, f: impl FnOnce(&SwPage) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<SwPage>().is_none() {
        st.put(SwPage::default());
    }
    f(st.borrow::<SwPage>())
}

/// A fresh instance of `iface`: the table's prototype, the interface's brand, no own property.
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Object> {
    let o = crate::install::live::mint_table(scope, iface);
    brand(scope, o, iface);
    o
}

fn slot_str(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, fallback: &str) -> String {
    match slot_get(scope, this, name) {
        Some(v) => v.to_rust_string_lossy(scope),
        None => fallback.to_string(),
    }
}

// ── ServiceWorker ───────────────────────────────────────────────────────────────────────────────

fn cb_sw_script_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = slot_str(scope, args.this(), SW_SCRIPT_URL, "");
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_sw_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = slot_str(scope, args.this(), SW_STATE, "installing");
    let v = str_val(scope, &s);
    rv.set(v);
}

/// `serviceWorker.postMessage(message, transfer)` — over ONE lazily-opened cross-isolate
/// `MessagePort` per worker object.
///
/// On the worker side each arrival becomes a `message` event whose `source` is the paired port — the
/// Client role — so a reply through `event.source.postMessage(...)` comes back to
/// [`cb_sw_port_message`] below and reaches `navigator.serviceWorker`.
fn cb_sw_post_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let port = match slot_get(scope, this, SW_PORT).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(p) => p,
        None => match open_client_port(scope, this) {
            Some(p) => p,
            None => return,
        },
    };
    let pv: v8::Local<v8::Value> = port.into();
    let message = args.get(0);
    let transfer = args.get(1);
    if transfer.is_undefined() {
        call_method(scope, pv, "postMessage", &[message]);
    } else {
        call_method(scope, pv, "postMessage", &[message, transfer]);
    }
}

/// Open (and remember) this `ServiceWorker` object's client `MessagePort`.
fn open_client_port<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let id = slot_get(scope, this, SW_ID).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    if id <= 0.0 {
        return None;
    }
    // The kernel's FACTORY op, the same shape `Worker.postMessage` reaches `op_worker_post_fn`
    // through: spawning and wiring a worker isolate is a kernel service (invariant I9) and this table
    // calls it. `host.js` called the identical factory with the identical name and length.
    let connect = deno_op(scope, "op_service_worker_connect_fn")?;
    let name = str_val(scope, "__serviceWorkerConnect");
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, 1).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let f = connect.call(scope, undef, &[name, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, id).into();
    let cid = f.call(scope, undef, &[idv]).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    if cid <= 0.0 {
        return None;
    }
    let remote = deno_op(scope, "op_mp_remote")?;
    let cidv: v8::Local<v8::Value> = v8::Number::new(scope, cid).into();
    let port = remote.call(scope, undef, &[cidv]).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let on = build_fn(scope, cb_sw_port_message, "", 1);
    set_prop(scope, port, "onmessage", on.into());
    let pv: v8::Local<v8::Value> = port.into();
    call_method(scope, pv, "start", &[]);
    slot_set(scope, this, SW_PORT, pv);
    Some(port)
}

/// A frame off a ServiceWorker's client port -> a `message` event at `navigator.serviceWorker`.
fn cb_sw_port_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(e) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let data = get_prop(scope, e, "data").unwrap_or_else(|| v8::undefined(scope).into());
    let ports = get_prop(scope, e, "ports").filter(|p| {
        v8::Local::<v8::Array>::try_from(*p).map(|a| a.length() > 0).unwrap_or(false)
    });
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "data", data);
    let list: v8::Local<v8::Value> = match ports {
        Some(p) => p,
        None => v8::Array::new(scope, 0).into(),
    };
    set_prop(scope, init, "ports", list);
    deliver_to_container(scope, init);
}

/// Fire a `message` event built from `init` at the container.
///
/// ONE delivery. The JS called `swContainer.onmessage` by hand AND dispatched, and the container's
/// `onmessage` is the realm's handler store, which the dispatch already runs — so every message ran
/// the page's handler twice. See the module header.
fn deliver_to_container(scope: &mut v8::PinScope, init: v8::Local<v8::Object>) {
    let Some(container) = with_page(scope, |p| p.container.borrow().clone()) else { return };
    let ty = str_val(scope, "message");
    let Some(ev) = new_global(scope, "MessageEvent", &[ty, init.into()]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev); // engine-delivered
    let target: v8::Local<v8::Value> = v8::Local::new(scope, &container).into();
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, target, "dispatchEvent", &[evv]);
}

/// The body [`SERVICEWORKER`]'s two non-stub rows name.
pub fn install_service_worker(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "scriptURL" => branded_getter_row(scope, proto, member, cb_sw_script_url),
        "state" => branded_getter_row(scope, proto, member, cb_sw_state),
        "postMessage" => branded_method_row(scope, proto, member, cb_sw_post_message),
        other => debug_assert!(false, "install_service_worker: unexpected member {other}"),
    }
}

// ── ServiceWorkerRegistration ───────────────────────────────────────────────────────────────────

/// `installing` / `waiting` / `active` — `null` when unset, which is what a nullable IDL attribute
/// answers and what the JS's `v === undefined ? null : v` produced.
fn cb_reg_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    match slot_get(scope, args.this(), &name).filter(|v| !v.is_null_or_undefined()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

fn cb_reg_scope(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = slot_str(scope, args.this(), REG_SCOPE, "/");
    let v = str_val(scope, &s);
    rv.set(v);
}

/// `registration.unregister()` — drops the scope from the page's map and resolves `true`.
fn cb_reg_unregister(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(s) = slot_get(scope, args.this(), REG_SCOPE) {
        let key = s.to_rust_string_lossy(scope);
        with_page(scope, |p| p.regs.borrow_mut().retain(|(k, _)| *k != key));
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    rv.set(resolved_promise(scope, t));
}

/// `registration.update()` — resolves `undefined`. There is no second fetch of the script yet; the
/// JS answered the same, and a real update check is W5.3's.
fn cb_reg_update(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

/// The body [`SERVICEWORKERREGISTRATION`]'s six non-stub rows name.
pub fn install_service_worker_registration(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "installing" | "waiting" | "active" => {
            let slot = match member.name {
                "installing" => REG_STATES[0],
                "waiting" => REG_STATES[1],
                _ => REG_STATES[2],
            };
            let d = str_val(scope, slot);
            branded_getter_row_data(scope, proto, member, cb_reg_state, d);
        }
        "scope" => branded_getter_row(scope, proto, member, cb_reg_scope),
        "unregister" => branded_promise_method_row(scope, proto, member, cb_reg_unregister),
        "update" => branded_promise_method_row(scope, proto, member, cb_reg_update),
        other => debug_assert!(false, "install_service_worker_registration: unexpected member {other}"),
    }
}

/// [`branded_getter_row`] with BOUND DATA, so one callback serves the three registration-state rows.
///
/// `branded_getter_row` builds its getter with `build_fn`, which carries no data; this is the same
/// mount with `build_fn_data`. It lives here rather than in `install::webidl` because this is its only
/// caller — when a second one appears it moves.
fn branded_getter_row_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: v8::Local<'s, v8::Value>,
) {
    use crate::binding::protos::build_fn_data;
    use crate::core::events::ctor::brand_accessor;
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let g = build_fn_data(scope, cb, &format!("get {}", member.name), 0, data);
    let gg = brand_accessor(scope, iface, &format!("get {}", member.name), 0, g);
    crate::install::kits::define_accessor(scope, proto, member, gg, None);
}

// ── ServiceWorkerContainer ──────────────────────────────────────────────────────────────────────

fn cb_container_controller(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match slot_get(scope, args.this(), CONTAINER_CONTROLLER).filter(|v| !v.is_null_or_undefined()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `navigator.serviceWorker.ready` — the SAME promise on every read (`ready === ready` is true in
/// Chrome, measured), stored on the container at mint time.
///
/// A PROMISE-typed attribute, so a foreign receiver gets a REJECTED promise rather than a throw —
/// measured: `Object.getOwnPropertyDescriptor(SWC,"ready").get.call({})` hands back a `Promise`
/// where the sibling `controller` getter throws `Illegal invocation`.
fn cb_container_ready(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match slot_get(scope, args.this(), CONTAINER_READY) {
        Some(v) => rv.set(v),
        None => {
            let p = rejected_type_error(
                scope,
                "Failed to read the 'ready' property from 'ServiceWorkerContainer': Illegal invocation",
            );
            rv.set(p);
        }
    }
}

/// `navigator.serviceWorker.register(scriptURL, options)`.
///
/// Every step is `host.js`'s, in its order: resolve the source (the `__workerScripts` / `blob:` test
/// seam) or the URL against the page, ask the kernel to spawn or reuse the isolate, start the
/// main-side pumps ONCE per new isolate, turn on SW-aware `fetch`, and mint the `ServiceWorker` +
/// `ServiceWorkerRegistration` pair. What is different is that the two "once" calls are direct Rust
/// (`start_isolate_pumps`, `activate_sw_fetch`) rather than the two transitional ops.
fn cb_container_register(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'register' on 'ServiceWorkerContainer': 1 argument required, but only 0 present.",
        );
        rv.set(p);
        return;
    }
    // (USVString or TrustedScriptURL): a TrustedScriptURL read as itself, nothing on it called; a string through
    // the current realm's Trusted Types ("ServiceWorkerContainer register")
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedScriptURL", sample: "ServiceWorkerContainer register", context: "Failed to execute 'register' on 'ServiceWorkerContainer'" };
    let at = crate::core::trusted_types::tables::SinkAt::Realm(scope.get_current_context());
    // a promise-returning operation: what the conversion or the check throws rejects its promise
    let url = {
        v8::tc_scope!(let tc, scope);
        let url = crate::core::trusted_types::tables::compliant_text(tc, args.get(0), &sink, at);
        match url {
            Some(u) => Ok(u),
            None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
        }
    };
    let url = match url {
        Ok(u) => u,
        Err(e) => {
            if let (Some(e), Some(resolver)) = (e, v8::PromiseResolver::new(scope)) {
                let e = v8::Local::new(scope, &e);
                resolver.reject(scope, e);
                rv.set(resolver.get_promise(scope).into());
            }
            return;
        }
    };
    // `RegistrationOptions`, converted as WebIDL converts it (Chrome 150): `scope`, `type` (an
    // enum), `updateViaCache` (an enum), each read once -- a refusal rejects (korvus read `scope` alone)
    let opts = match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(1), "RegistrationOptions", "Failed to execute 'register' on 'ServiceWorkerContainer': ") {
        Ok(o) => o,
        Err(e) => {
            if let Some(resolver) = v8::PromiseResolver::new(scope) {
                resolver.reject(scope, e);
                rv.set(resolver.get_promise(scope).into());
            }
            return;
        }
    };
    let scope_str = match get_prop(scope, opts, "scope") {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => "/".to_string(),
    };
    let src = worker_source(scope, &url);
    // Source-only when the test seam registered a literal; otherwise the isolate fetches `scriptURL`
    // on its own thread, exactly as `new Worker(url)` does.
    let script_url = match &src {
        Some(_) => url.clone(),
        None => worker_url(scope, &url),
    };
    let Some(factory) = deno_op(scope, "op_service_worker_register_fn") else {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'register' on 'ServiceWorkerContainer': service workers are not available in this realm",
        );
        rv.set(p);
        return;
    };
    let name = str_val(scope, "__serviceWorkerRegister");
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, 2).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(register) = factory.call(scope, undef, &[name, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return;
    };
    let a = str_val(scope, src.as_deref().unwrap_or(""));
    let b = str_val(scope, if src.is_some() { "" } else { script_url.as_str() });
    let Some(r) = register.call(scope, undef, &[a, b]).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else {
        return;
    };
    let worker_id = r.get_index(scope, 0).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let is_new = r.get_index(scope, 1).map(|v| v.boolean_value(scope)).unwrap_or(false);
    if is_new {
        super::shared_worker::start_isolate_pumps(scope, worker_id);
    }
    crate::modules::fetch::fetch_global::activate_sw_fetch(scope);
    setup_client_messages(scope);

    let worker = mint(scope, "ServiceWorker");
    let idv: v8::Local<v8::Value> = v8::Number::new(scope, worker_id).into();
    slot_set(scope, worker, SW_ID, idv);
    let su = str_val(scope, &script_url);
    slot_set(scope, worker, SW_SCRIPT_URL, su);
    let lk = str_val(scope, if src.is_some() { "about:blank" } else { script_url.as_str() });
    slot_set(scope, worker, SW_LIFE_KEY, lk);
    let st = str_val(scope, "installing");
    slot_set(scope, worker, SW_STATE, st);

    let reg = mint(scope, "ServiceWorkerRegistration");
    let sc = str_val(scope, &scope_str);
    slot_set(scope, reg, REG_SCOPE, sc);
    slot_set(scope, reg, REG_STATES[0], worker.into());
    let nul: v8::Local<v8::Value> = v8::null(scope).into();
    slot_set(scope, reg, REG_STATES[1], nul);
    slot_set(scope, reg, REG_STATES[2], nul);

    let g = v8::Global::new(scope, reg);
    with_page(scope, |p| {
        let mut regs = p.regs.borrow_mut();
        regs.retain(|(k, _)| *k != scope_str);
        regs.push((scope_str.clone(), g));
    });
    let regv: v8::Local<v8::Value> = reg.into();
    // `ready` settles when this registration has an ACTIVE worker -- the worker realm's `activating`
    // report, [`sw_lifecycle`] -- not here: Chrome 150 (`U-sw-control-*`) resolves it with
    // `registration.active` set, and korvus resolved it at `register()` with only `installing`.
    rv.set(resolved_promise(scope, regv));
}

/// One lifecycle report from a service worker's realm, keyed by its script URL:
///
/// * `installed`  -- the install step (and its `waitUntil` promises) finished: installing -> waiting;
/// * `activating` -- the worker becomes the registration's ACTIVE worker, and `ready` settles for a
///   registration whose scope covers this page;
/// * `activated`  -- the activate step's `waitUntil` promises settled;
/// * `claim`      -- `clients.claim()`: the page becomes a CONTROLLED client (its `controller`, a
///   `controllerchange` event) and only from then on do its fetches go to the worker.
///
/// Every state change fires a trusted `statechange` at the `ServiceWorker`, as Chrome's do.
pub(crate) fn sw_lifecycle(scope: &mut v8::PinScope, life: &str, url: &str) {
    let regs: Vec<(String, v8::Global<v8::Object>)> = with_page(scope, |p| p.regs.borrow().clone());
    for (scope_str, rg) in regs {
        let reg = v8::Local::new(scope, &rg);
        let mut worker: Option<v8::Local<v8::Object>> = None;
        for slot in REG_STATES {
            let Some(w) = slot_get(scope, reg, slot).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { continue };
            if slot_get(scope, w, SW_LIFE_KEY).is_some_and(|u| u.to_rust_string_lossy(scope) == url) {
                worker = Some(w);
                break;
            }
        }
        let Some(worker) = worker else { continue };
        let nul: v8::Local<v8::Value> = v8::null(scope).into();
        match life {
            "installed" => {
                slot_set(scope, reg, REG_STATES[0], nul);
                slot_set(scope, reg, REG_STATES[1], worker.into());
                set_worker_state(scope, worker, "installed");
            }
            "activating" => {
                slot_set(scope, reg, REG_STATES[0], nul);
                slot_set(scope, reg, REG_STATES[1], nul);
                slot_set(scope, reg, REG_STATES[2], worker.into());
                set_worker_state(scope, worker, "activating");
                if scope_covers_page(scope, &scope_str) {
                    if let Some(rr) = with_page(scope, |p| p.ready_resolver.borrow_mut().take()) {
                        let r = v8::Local::new(scope, &rr);
                        let regv: v8::Local<v8::Value> = reg.into();
                        r.resolve(scope, regv);
                    }
                }
            }
            "activated" => set_worker_state(scope, worker, "activated"),
            "claim" => {
                if !scope_covers_page(scope, &scope_str) {
                    continue;
                }
                crate::modules::fetch::fetch_global::sw_claimed(scope);
                let Some(cg) = with_page(scope, |p| p.container.borrow().clone()) else { continue };
                let c = v8::Local::new(scope, &cg);
                let already = slot_get(scope, c, CONTAINER_CONTROLLER).is_some_and(|v| v == v8::Local::<v8::Value>::from(worker));
                if already {
                    continue;
                }
                slot_set(scope, c, CONTAINER_CONTROLLER, worker.into());
                let cv: v8::Local<v8::Value> = c.into();
                if let Some(ev) = crate::modules::indexeddb::backend::idb_new_event(scope, "controllerchange", cv, false) {
                    crate::binding::protos::engine_call(scope, cv, "dispatchEvent", &[ev]);
                }
            }
            _ => {}
        }
    }
}

/// `serviceWorker.state = st`, then a trusted `statechange` at it.
fn set_worker_state(scope: &mut v8::PinScope, worker: v8::Local<v8::Object>, st: &str) {
    let sv = str_val(scope, st);
    slot_set(scope, worker, SW_STATE, sv);
    let wv: v8::Local<v8::Value> = worker.into();
    if let Some(ev) = crate::modules::indexeddb::backend::idb_new_event(scope, "statechange", wv, false) {
        crate::binding::protos::engine_call(scope, wv, "dispatchEvent", &[ev]);
    }
}

/// Whether a registration `scope` (resolved against the page) is a prefix of the page's own URL --
/// the "match service worker registration" test. A page whose URL is not hierarchical (a bare test
/// runtime's) has nothing to compare, and every scope covers it.
fn scope_covers_page(scope: &mut v8::PinScope, scope_str: &str) -> bool {
    let resolved = worker_url(scope, scope_str);
    let page = worker_url(scope, "");
    match (url::Url::parse(&page), url::Url::parse(&resolved)) {
        (Ok(p), Ok(s)) if !p.cannot_be_a_base() => p.as_str().starts_with(s.as_str()),
        _ => true,
    }
}

/// `getRegistration(scope)` — the registration under that EXACT scope key, or `undefined`.
///
/// The JS keyed on the string the caller passed (defaulting to "/"); Chrome matches a CLIENT URL
/// against each registration's scope prefix. Carried over verbatim: see the module header.
fn cb_container_get_registration(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = if args.get(0).is_undefined() { "/".to_string() } else { args.get(0).to_rust_string_lossy(scope) };
    let found = with_page(scope, |p| p.regs.borrow().iter().find(|(k, _)| *k == key).map(|(_, g)| g.clone()));
    let value: v8::Local<v8::Value> = match found {
        Some(g) => v8::Local::new(scope, &g).into(),
        None => v8::undefined(scope).into(),
    };
    rv.set(resolved_promise(scope, value));
}

fn cb_container_get_registrations(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let all = with_page(scope, |p| p.regs.borrow().iter().map(|(_, g)| g.clone()).collect::<Vec<_>>());
    let arr = v8::Array::new(scope, all.len() as i32);
    for (i, g) in all.iter().enumerate() {
        let v: v8::Local<v8::Value> = v8::Local::new(scope, g).into();
        arr.set_index(scope, i as u32, v);
    }
    let av: v8::Local<v8::Value> = arr.into();
    rv.set(resolved_promise(scope, av));
}

/// `startMessages()` — a no-op, as it was in the JS: this container never buffers, so there is
/// nothing to release.
fn cb_container_start_messages(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// The body [`SERVICEWORKERCONTAINER`]'s five non-stub rows name.
pub fn install_service_worker_container(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "controller" => branded_getter_row(scope, proto, member, cb_container_controller),
        // A promise-typed ATTRIBUTE: the row's own body answers the REJECTION for a foreign receiver
        // (Chrome's message verbatim, measured), so it must not be wrapped in the throwing brand
        // accessor -- and it must be STAMPED, or `install::receiver`'s derived pass wraps it from the
        // outside and replaces that message with its own bare "Illegal invocation". Measured here
        // before the stamp: the engine answered `TypeError: Illegal invocation` where Chrome answers
        // `TypeError: Failed to read the 'ready' property from 'ServiceWorkerContainer': Illegal
        // invocation`. Same pairing `install::webidl::stamp` documents for the promise METHOD rows.
        "ready" => {
            let g = build_fn(scope, cb_container_ready, "get ready", 0);
            crate::binding::protos::stamp_receiver_guarded(scope, g);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        "register" => branded_promise_method_row(scope, proto, member, cb_container_register),
        "getRegistration" => branded_promise_method_row(scope, proto, member, cb_container_get_registration),
        "getRegistrations" => branded_promise_method_row(scope, proto, member, cb_container_get_registrations),
        "startMessages" => branded_method_row(scope, proto, member, cb_container_start_messages),
        other => debug_assert!(false, "install_service_worker_container: unexpected member {other}"),
    }
}

// ── the container, and the client-message channel ───────────────────────────────────────────────

/// The `ServiceWorkerContainer` `Navigator`'s `serviceWorker` row hands back, minted once.
///
/// `Navigator`'s row is lazy (`cb_lazy_object`), so this runs on FIRST READ of
/// `navigator.serviceWorker` — the same moment `host.js`'s `getSwContainer` ran, and for the same
/// reason: the three prototypes have to exist. They are this file's tables now, installed by
/// `install::live::install_window_service_workers`, so "exist" is a fact about the install order
/// rather than about a JS file.
///
/// It replaces the W4.a SEAM: `op_service_worker_source` took a JS builder function out of `host.js`
/// and parked it in `OpState` for this row to call. There is no builder and no op; the row calls this.
///
/// A FRAME's is its own: measured 2026-09-28 on Chrome 150.0.7871.46, a same-origin
/// frame's `navigator.serviceWorker` is an instance of the FRAME's `ServiceWorkerContainer` and not the
/// page's container, with `controller` null and `ready` pending while no registration covers it. The
/// client this engine's registrations control, message and settle `ready` for is the page ([`SwPage`]),
/// so a frame's container is minted uncontrolled and stays so -- and is not kept here, because
/// `navigator.rs`'s `cb_lazy_object` keeps each realm's answer on that realm's global.
pub(crate) fn build_container<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let page = crate::core::html::browsing_context::in_page_realm(scope);
    if page {
        if let Some(g) = with_page(scope, |p| p.container.borrow().clone()) {
            return v8::Local::new(scope, &g).into();
        }
    }
    // A realm whose `ServiceWorkerContainer` table is not installed has no container, which is what
    // the JS getter answered (`null`) when `prepSwProtos` could not find the three prototypes.
    if crate::binding::protos::shell_proto(scope, "ServiceWorkerContainer").is_none() {
        return v8::null(scope).into();
    }
    let c = mint(scope, "ServiceWorkerContainer");
    let nul: v8::Local<v8::Value> = v8::null(scope).into();
    slot_set(scope, c, CONTAINER_CONTROLLER, nul);
    // PENDING until a registration covers this page -- Chrome 150, measured on a loopback page with
    // no registration: `Promise.race([navigator.serviceWorker.ready, 300 ms])` answers the timer. It
    // resolved with `undefined` at mint time here, so a page that awaits `ready` before registering
    // (the common "wait for the SW, then use it" shape) ran its continuation with nothing.
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return v8::null(scope).into() };
    let ready: v8::Local<v8::Value> = resolver.get_promise(scope).into();
    slot_set(scope, c, CONTAINER_READY, ready);
    if !page {
        return c.into();
    }
    let rg = v8::Global::new(scope, resolver);
    let g = v8::Global::new(scope, c);
    with_page(scope, |p| {
        *p.container.borrow_mut() = Some(g);
        *p.ready_resolver.borrow_mut() = Some(rg);
    });
    c.into()
}

/// Open the `korvus-sw-client-msg` channel ONCE: a `Client.postMessage` from inside the SW arrives
/// here and becomes a `message` event on `navigator.serviceWorker`.
fn setup_client_messages(scope: &mut v8::PinScope) {
    if with_page(scope, |p| p.client_msg.borrow().is_some()) {
        return;
    }
    let name = str_val(scope, "korvus-sw-client-msg");
    let Some(ch) = new_global(scope, "BroadcastChannel", &[name]) else { return };
    let on = build_fn(scope, cb_client_message, "", 1);
    set_prop(scope, ch, "onmessage", on.into());
    let g = v8::Global::new(scope, ch);
    with_page(scope, |p| *p.client_msg.borrow_mut() = Some(g));
}

fn cb_client_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(e) = v8::Local::<v8::Object>::try_from(args.get(0)) else { return };
    let data = get_prop(scope, e, "data").unwrap_or_else(|| v8::undefined(scope).into());
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "data", data);
    deliver_to_container(scope, init);
}

// ── the tables ──────────────────────────────────────────────────────────────────────────────────

// Rows VERBATIM from the `ServiceWorker` block of `install/generated_registry.rs`, in the capture's
// order: scriptURL, state, onstatechange, postMessage, onerror. `[interface, window]`: the capture
// records `newable: false` and only the window surface lists it.
//
// `event_handler` rather than `evt_handler` for the two handlers, which is the kit the registry row
// named — a foreign receiver answers `null` instead of throwing. That is a difference from Chrome
// this unit did NOT measure (the headless registration never completed, so there was no
// `ServiceWorker` object to read a handler off), and changing it unmeasured would be exactly the
// "edit the test to agree with the engine" this goal forbids. It is the same answer the surface gave
// before this table, so the cutover moves nothing.
webapi::interface! {
    "ServiceWorker" : "EventTarget" [interface, window, secure] {
        ctor    "ServiceWorker"                              len 0 = stub,
        attr    "scriptURL"    : UsvString readonly                = install_service_worker,
        attr    "state"        : Any       readonly                = install_service_worker,
        handler "onstatechange"                                    = event_handler,
        method  "postMessage"  : Undefined len 1 required 1        = install_service_worker,
        handler "onerror"                                          = event_handler
    }
}

// Rows VERBATIM from the `ServiceWorkerRegistration` block, in the capture's order. `[interface,
// both]`: BOTH captures list the interface — and `cookies` is WINDOW-ONLY on it, which is the row
// that needed the macro's per-member exposure word (before it, this interface could not
// leave the generated registry without losing or gaining a member in one realm).
//
// Eleven rows are honest D2 `stub`s: this engine implements no push, sync, background fetch, payment,
// notification or navigation-preload subsystem, and `updateViaCache` has no HTTP cache to name. They
// were stubs before this table too — `prepSwProtos` replaced only the six below.
webapi::interface! {
    "ServiceWorkerRegistration" : "EventTarget" [interface, both, secure] {
        ctor    "ServiceWorkerRegistration"                   len 0 = stub,
        attr    "installing"        : Any       readonly nullable   = install_service_worker_registration,
        attr    "waiting"           : Any       readonly nullable   = install_service_worker_registration,
        attr    "active"            : Any       readonly nullable   = install_service_worker_registration,
        attr    "navigationPreload" : Any       readonly            = stub,
        attr    "scope"             : UsvString readonly            = install_service_worker_registration,
        attr    "updateViaCache"    : Any       readonly            = stub,
        handler "onupdatefound"                                     = event_handler,
        method  "unregister"        : Promise   len 0               = install_service_worker_registration,
        method  "update"            : Promise   len 0               = install_service_worker_registration,
        attr    "paymentManager"    : Any       readonly            = stub,
        attr    "backgroundFetch"   : Any       readonly            = stub,
        attr    "periodicSync"      : Any       readonly            = stub,
        attr    "sync"              : Any       readonly            = stub,
        attr    "cookies"           : Any       readonly window     = stub,
        attr    "pushManager"       : Any       readonly            = stub,
        method  "getNotifications"  : Promise   len 0               = stub,
        method  "showNotification"  : Promise   len 1 required 1    = stub
    }
}

// Rows VERBATIM from the `ServiceWorkerContainer` block, in the capture's order. `[interface,
// window]`: only the window surface lists it.
webapi::interface! {
    "ServiceWorkerContainer" : "EventTarget" [interface, window, secure] {
        ctor    "ServiceWorkerContainer"                  len 0 = stub,
        attr    "controller"         : Any     readonly nullable = install_service_worker_container,
        attr    "ready"              : Promise readonly          = install_service_worker_container,
        handler "oncontrollerchange"                             = event_handler,
        handler "onmessage"                                      = event_handler,
        handler "onmessageerror"                                 = event_handler,
        method  "getRegistration"    : Promise len 0             = install_service_worker_container,
        method  "getRegistrations"   : Promise len 0             = install_service_worker_container,
        method  "register"           : Promise len 1 required 1  = install_service_worker_container,
        method  "startMessages"      : Undefined len 0           = install_service_worker_container
    }
}

// ── the ServiceWorker REALM's global scope ──────────────────────────────────────────
//
// `clients`, `registration` and `skipWaiting()` were `SW_PRELUDE`, a Rust string literal in
// `core/korvus-runtime/src/worker.rs` prepended to every ServiceWorker's source -- three object
// literals and an arrow function, defining Web API from inside the kernel (invariant I9) and lying
// about `[native code]` by not even trying (`String(skipWaiting)` printed `() => Promise.resolve()`).
// They are `ServiceWorkerGlobalScope` table rows now, with the bodies below.
//
// WHAT THE LITERALS GOT WRONG, beyond being JS:
//   * `registration` was a PLAIN OBJECT: `Object.prototype.toString.call(registration)` read
//     `[object Object]` where a browser reads `[object ServiceWorkerRegistration]`, and its
//     `addEventListener`/`removeEventListener` were `() => {}` -- a method that silently discarded
//     every listener handed to it. It is a real instance of this file's table now, so it inherits
//     `EventTarget` and its `unregister`/`update` are the table's rows.
//   * `clients` and its `Client` were plain objects too, so `Object.getOwnPropertyNames` on either
//     listed the engine's own method names as own keys -- which is what they are.

/// The SW realm's per-scope singletons, minted once.
#[derive(Default)]
pub(crate) struct SwScope {
    clients: RefCell<Option<v8::Global<v8::Object>>>,
    registration: RefCell<Option<v8::Global<v8::Object>>>,
    /// The registering page's client id, made once ([`client_id`]).
    client_id: RefCell<Option<String>>,
}

fn with_scope<R>(scope: &mut v8::PinScope, f: impl FnOnce(&SwScope) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<SwScope>().is_none() {
        st.put(SwScope::default());
    }
    f(st.borrow::<SwScope>())
}

/// `skipWaiting()` — resolves `undefined`. There is no waiting worker to skip past in this engine
/// (the lifecycle fires `install` then `activate` unconditionally), and the arrow it replaces
/// resolved the same value.
///
/// A receiver that is not the global REJECTS, as a promise-returning `[Global]` member does: Chrome
/// 150.0.7871.46 answers `skipWaiting.call({})` and `.call(Object.create(ServiceWorkerGlobalScope.prototype))`
/// with a promise rejected by "Failed to execute 'skipWaiting' on 'ServiceWorkerGlobalScope': Illegal
/// invocation" (measured) -- it resolved here.
fn cb_skip_waiting(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::core::html::window_globals::is_global_receiver(scope, args.this().into()) {
        rv.set(crate::binding::protos::rejected_type_error(scope, "Failed to execute 'skipWaiting' on 'ServiceWorkerGlobalScope': Illegal invocation"));
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

/// THE CLIENTS API. `self.clients` is a `Clients`, each client a `WindowClient : Client`,
/// every member a row of the three tables below on the prototypes -- as Chrome 150.0.7871.46 has them
/// (measured): `clients` and its clients were PLAIN objects, whose `claim`/`matchAll`/`get` and
/// `postMessage` were own functions built per realm and per call, the clients' `url` "", and the three
/// interfaces absent from the realm. The one client is the registering page (the kernel hands its URL to
/// the worker: `op_worker_client_url`); its id is made once per worker (`op_uuid`). A client is a NEW
/// object per `matchAll`/`get`, as Chrome's are; `postMessage` rides the `korvus-sw-client-msg`
/// BroadcastChannel `ServiceWorkerContainer`'s side of this file opens.
fn client_url(scope: &mut v8::PinScope) -> String {
    let Some(op) = deno_op(scope, "op_worker_client_url") else { return String::new() };
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    op.call(scope, u, &[]).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}

fn client_id(scope: &mut v8::PinScope) -> String {
    if let Some(id) = with_scope(scope, |s| s.client_id.borrow().clone()) {
        return id;
    }
    let id = deno_op(scope, "op_uuid")
        .and_then(|op| {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            op.call(scope, u, &[]).map(|v| v.to_rust_string_lossy(scope))
        })
        .unwrap_or_else(|| "korvus-client".to_string());
    with_scope(scope, |s| *s.client_id.borrow_mut() = Some(id.clone()));
    id
}

/// A `WindowClient` for the registering page: the table's prototype and both interfaces' brands.
fn mint_window_client<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let o = mint(scope, "WindowClient");
    brand(scope, o, "Client");
    o
}

fn cb_client_url(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let u = client_url(scope);
    let v = str_val(scope, &u);
    rv.set(v);
}
fn cb_client_id(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = client_id(scope);
    let v = str_val(scope, &id);
    rv.set(v);
}
fn cb_client_type(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = str_val(scope, "window");
    rv.set(v);
}
fn cb_client_frame_type(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = str_val(scope, "top-level");
    rv.set(v);
}

/// The port the registering page opened to this service worker (`serviceWorker.postMessage`), once
/// it has: the same channel `event.source.postMessage` answers on. `korvus-runtime` records it when it
/// binds the port.
pub struct SwClientPort(pub v8::Global<v8::Value>);

fn cb_client_post_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // ONE ordered channel per client when there is one: Chrome queues `client.postMessage` and
    // `event.source.postMessage` on the same client message queue, in call order. Two channels (a
    // BroadcastChannel here, the port there) raced -- `F-sw-clients` read its `'x'` after the JSON
    // posted later, once the SW's lifecycle reports shared the broadcast pump.
    let port = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<SwClientPort>().map(|p| p.0.clone())
    };
    if let Some(pg) = port {
        let pv = v8::Local::new(scope, &pg);
        call_method(scope, pv, "postMessage", &[args.get(0)]);
        return;
    }
    let name = str_val(scope, "korvus-sw-client-msg");
    let Some(ch) = new_global(scope, "BroadcastChannel", &[name]) else { return };
    let chv: v8::Local<v8::Value> = ch.into();
    let message = args.get(0);
    call_method(scope, chv, "postMessage", &[message]);
}

/// `visibilityState`: "visible" -- the page is the one being loaded (headless Chrome answers the same).
fn cb_wclient_visibility(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = str_val(scope, "visible");
    rv.set(v);
}
fn cb_wclient_focused(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(false);
}

/// A promise rejected with a `DOMException` of `name`.
fn rejected_dom<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str, name: &str) -> v8::Local<'s, v8::Value> {
    match crate::binding::protos::make_dom_exc(scope, msg, name) {
        Some(e) => rejected_promise(scope, e),
        None => rejected_type_error(scope, msg),
    }
}

/// A promise rejected with WebIDL's arity sentence, when `args` has fewer than `need`.
fn rejected_arity<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, need: i32, member: &str, iface: &str) -> Option<v8::Local<'s, v8::Value>> {
    if args.length() >= need {
        return None;
    }
    let msg = format!("Failed to execute '{member}' on '{iface}': {need} argument required, but only {} present.", args.length());
    Some(rejected_type_error(scope, &msg))
}

/// `focus()`: this engine opens no window to focus -- Chrome's answer without a user activation.
fn cb_wclient_focus(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(rejected_dom(scope, "Not allowed to focus a window.", "InvalidAccessError"));
}

/// `navigate(url)`: an unparseable or `about:` URL is a TypeError naming it; otherwise this worker does
/// not control the page (nothing here controls one), which is Chrome's answer for an uncontrolled client.
fn cb_wclient_navigate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(p) = rejected_arity(scope, &args, 1, "navigate", "WindowClient") {
        rv.set(p);
        return;
    }
    let raw = args.get(0).to_rust_string_lossy(scope);
    let base = worker_url(scope, "");
    let parsed = url::Url::parse(&base).and_then(|b| b.join(&raw)).or_else(|_| url::Url::parse(&raw));
    let msg = match parsed {
        Ok(u) if u.scheme() != "about" => "This service worker is not the client's active service worker.".to_string(),
        _ => format!("'{raw}' is not a valid URL."),
    };
    rv.set(rejected_type_error(scope, &msg));
}

/// `clients.claim()`: tell the registering page it is now controlled -- before this, the
/// page never got a `controller` and its fetches reached the worker from `register()` on.
fn cb_clients_claim(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    crate::core::workers::service_worker_realm::report_lifecycle(scope, "claim");
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

/// `clients.matchAll(options)` -- the registering page, a window client, in a list; a `type` that
/// is not a `ClientType` rejects with WebIDL's enum sentence, and "worker"/"sharedworker" match nothing.
fn cb_clients_match_all(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut kind = "window".to_string();
    if let Ok(o) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        if let Some(t) = get_prop(scope, o, "type").filter(|v| !v.is_undefined()) {
            kind = t.to_rust_string_lossy(scope);
        }
    }
    if !["window", "worker", "sharedworker", "all"].contains(&kind.as_str()) {
        let msg = format!("Failed to execute 'matchAll' on 'Clients': Failed to read the 'type' property from 'ClientQueryOptions': The provided value '{kind}' is not a valid enum value of type ClientType.");
        rv.set(rejected_type_error(scope, &msg));
        return;
    }
    let list: Vec<v8::Local<v8::Value>> = if kind == "window" || kind == "all" { vec![mint_window_client(scope).into()] } else { Vec::new() };
    // A plain array: `Object.isFrozen(await clients.matchAll())` is false in Chrome 150.0.7871.46 (measured).
    let arr = v8::Array::new_with_elements(scope, &list);
    let av: v8::Local<v8::Value> = arr.into();
    rv.set(resolved_promise(scope, av));
}

/// `clients.get(id)` -- a new client for the page's id, `undefined` for any other.
fn cb_clients_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(p) = rejected_arity(scope, &args, 1, "get", "Clients") {
        rv.set(p);
        return;
    }
    let want = args.get(0).to_rust_string_lossy(scope);
    let v: v8::Local<v8::Value> = if want == client_id(scope) { mint_window_client(scope).into() } else { v8::undefined(scope).into() };
    rv.set(resolved_promise(scope, v));
}

/// `clients.openWindow(url)`: this engine opens no window -- Chrome's answer without a user activation.
fn cb_clients_open_window(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(p) = rejected_arity(scope, &args, 1, "openWindow", "Clients") {
        rv.set(p);
        return;
    }
    rv.set(rejected_dom(scope, "Not allowed to open a window.", "InvalidAccessError"));
}

/// The `clients` singleton, minted once per realm: a `Clients`.
fn sw_clients<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    if let Some(g) = with_scope(scope, |s| s.clients.borrow().clone()) {
        return v8::Local::new(scope, &g).into();
    }
    let o = mint(scope, "Clients");
    let g = v8::Global::new(scope, o);
    with_scope(scope, |s| *s.clients.borrow_mut() = Some(g));
    o.into()
}

pub fn install_clients(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "claim" => branded_promise_method_row(scope, proto, member, cb_clients_claim),
        "get" => branded_promise_method_row(scope, proto, member, cb_clients_get),
        "matchAll" => branded_promise_method_row(scope, proto, member, cb_clients_match_all),
        "openWindow" => branded_promise_method_row(scope, proto, member, cb_clients_open_window),
        other => debug_assert!(false, "install_clients: unexpected member {other}"),
    }
}

pub fn install_client(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "url" => branded_getter_row(scope, proto, member, cb_client_url),
        "id" => branded_getter_row(scope, proto, member, cb_client_id),
        "type" => branded_getter_row(scope, proto, member, cb_client_type),
        "frameType" => branded_getter_row(scope, proto, member, cb_client_frame_type),
        "postMessage" => branded_method_row(scope, proto, member, cb_client_post_message),
        other => debug_assert!(false, "install_client: unexpected member {other}"),
    }
}

pub fn install_window_client(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "visibilityState" => branded_getter_row(scope, proto, member, cb_wclient_visibility),
        "focused" => branded_getter_row(scope, proto, member, cb_wclient_focused),
        "focus" => branded_promise_method_row(scope, proto, member, cb_wclient_focus),
        "navigate" => branded_promise_method_row(scope, proto, member, cb_wclient_navigate),
        other => debug_assert!(false, "install_window_client: unexpected member {other}"),
    }
}

/// The three interfaces, made in a ServiceWorker realm only (`install::live::event_family`'s gate).
pub(crate) fn install_sw_client_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    use crate::install::installer::{create_interface, Flavor};
    create_interface(scope, global, &CLIENTS, "Clients", None, None, Flavor::Table, ledger);
    let client = create_interface(scope, global, &CLIENT, "Client", None, None, Flavor::Table, ledger);
    create_interface(scope, global, &WINDOWCLIENT, "WindowClient", Some((client.proto, client.ctor)), None, Flavor::Table, ledger);
}

// Rows in Chrome 150.0.7871.46's prototype order (measured; no capture records a ServiceWorker realm).
// `[Exposed=ServiceWorker]`: `worker`, the SERVICE-realm gate at the install site, as ExtendableEvent's.
webapi::interface! {
    "Clients" [interface, worker] {
        ctor   "Clients"                              len 0 = stub,
        method "claim"      : Promise len 0                 = install_clients,
        method "get"        : Promise len 1 required 1      = install_clients,
        method "matchAll"   : Promise len 0                 = install_clients,
        method "openWindow" : Promise len 1 required 1      = install_clients
    }
}

webapi::interface! {
    "Client" [interface, worker] {
        ctor   "Client"                               len 0 = stub,
        attr   "url"         : UsvString readonly           = install_client,
        attr   "id"          : DomString readonly           = install_client,
        attr   "type"        : Any       readonly           = install_client,
        attr   "frameType"   : Any       readonly           = install_client,
        method "postMessage" : Undefined len 1 required 1   = install_client
    }
}

webapi::interface! {
    "WindowClient" : "Client" [interface, worker] {
        ctor   "WindowClient"                         len 0 = stub,
        attr   "visibilityState" : Any     readonly         = install_window_client,
        attr   "focused"         : Boolean readonly         = install_window_client,
        method "focus"           : Promise len 0            = install_window_client,
        method "navigate"        : Promise len 1 required 1 = install_window_client
    }
}

/// The `registration` singleton: a REAL `ServiceWorkerRegistration`, on the table's prototype, with
/// the same three null states and the same "/" scope the JS literal carried.
///
/// `ServiceWorkerRegistration` is `[Exposed=(Window,Worker)]` and
/// `install::live::install_worker_service_workers` mounts it in this realm, so the prototype is here.
fn sw_registration<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    if let Some(g) = with_scope(scope, |s| s.registration.borrow().clone()) {
        return v8::Local::new(scope, &g).into();
    }
    if crate::binding::protos::shell_proto(scope, "ServiceWorkerRegistration").is_none() {
        return v8::undefined(scope).into();
    }
    let r = mint(scope, "ServiceWorkerRegistration");
    let s = str_val(scope, "/");
    slot_set(scope, r, REG_SCOPE, s);
    let nul: v8::Local<v8::Value> = v8::null(scope).into();
    for name in REG_STATES {
        slot_set(scope, r, name, nul);
    }
    let g = v8::Global::new(scope, r);
    with_scope(scope, |s| *s.registration.borrow_mut() = Some(g));
    r.into()
}

fn cb_scope_clients(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = sw_clients(scope);
    rv.set(v);
}

fn cb_scope_registration(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = sw_registration(scope);
    rv.set(v);
}

/// The body the three `ServiceWorkerGlobalScope` rows name.
///
/// The two attributes check their receiver is the GLOBAL OBJECT, their holder -- which is not an instance
/// of anything a brand could check -- as every `[Global]` row does (`window::guarded`; Chrome 150.0.7871.46
/// throws `Illegal invocation` from both getters given `{}` or `Object.create(ServiceWorkerGlobalScope
/// .prototype)`, measured).
/// A readonly `[Global]` attribute on the global object, receiver-checked ([`install_service_worker_scope`]).
fn global_getter(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let name = format!("get {}", member.name);
    let g0 = build_fn(scope, cb, &name, 0);
    let g = crate::core::html::window::guarded(scope, g0.into(), &name, 0);
    crate::install::kits::define_accessor(scope, target, member, g, None);
}

pub(crate) fn install_service_worker_scope(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::rows::method_row;
    match member.name {
        "clients" => global_getter(scope, target, member, cb_scope_clients),
        "registration" => global_getter(scope, target, member, cb_scope_registration),
        "skipWaiting" => method_row(scope, target, member, cb_skip_waiting),
        other => debug_assert!(false, "install_service_worker_scope: unexpected member {other}"),
    }
}
