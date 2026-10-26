//! `core/workers/shared_worker.rs` — `SharedWorker`.
//!
//! Was `class SharedWorker extends EventTarget` in `core/korvus-runtime/src/js/host.js`, with two
//! `op_nativize`-masked JS accessors on its prototype and a `startSharedWorkerPumps` closure. It is an
//! `interface!` table with a native constructor now — the LAST of the three ESM-time
//! `extends EventTarget` declarations W4.b named as the reason `events.js` still exists.
//!
//! A shared worker is a keyed SINGLETON isolate (by url + name). Each `new SharedWorker(url, name)`
//! opens a `MessagePort` (`.port`) entangled to it over the W3b cross-isolate port channel, and the
//! worker gets a `connect` event carrying the worker-side port. The main-side pumps are started ONCE
//! per isolate, because a second connection reuses it — [`start_isolate_pumps`] is that "once", and the
//! ServiceWorker container calls it directly for the same reason -- through `op_start_worker_pumps`
//! until that transitional op was deleted.
//!
//! WHAT THE JS CLASS GOT WRONG, measured with `tests/fixtures/shared-worker-probe.js` on
//! `google-chrome --headless=new` 150.0.7871.46:
//!
//! * `new SharedWorker()` CONSTRUCTED over the string "undefined"; Chrome throws "1 argument required,
//!   but only 0 present." (`SharedWorker.length` was already forced to 1 by hand.)
//! * `SharedWorker("x")` said "Class constructor SharedWorker cannot be invoked without 'new'".
//! * `Object.getOwnPropertyNames(new SharedWorker(u))` was `["__port","__onerror"]`; Chrome's is `[]`.
//! * `Symbol.toStringTag` was a GETTER; Chrome's is a non-writable data property.
//! * `port` and `onerror` answered their JS closure's default for a foreign receiver where Chrome
//!   throws `Illegal invocation` — and both were `op_nativize` MASKS, two of the call sites invariant
//!   I3 deletes.
//! * `onerror` was a plain `__onerror` property pair, so it was NOT in the realm's one event-handler
//!   store: an `error` event dispatched at a SharedWorker would not have run it. It is the branded
//!   `evt_handler` row now, the same store `Worker.onerror` uses.

use deno_core::v8;

use crate::binding::protos::{get_prop, str_val};
use crate::core::workers::worker::{arm_pump, local_source, resolve_script_url, seam_source, slot_get, slot_set, worker_url};
use crate::core::xmlhttprequest::xml_http_request::deno_op;
use crate::install::webidl::{arg_string, brand, branded_getter_row, require_args, require_new};
use crate::install::MemberDef;

/// The `[SameObject]` `MessagePort` this connection opened.
const PORT_SLOT: &str = "korvus_shared_worker_port";

/// Shared- and service-worker isolate ids whose MAIN-SIDE pumps are already running: the JS
/// `swPumped` Set, moved.
///
/// IT LIVES IN `OpState`, which is PER REALM, and a thread_local is what it must not be. The JS Set
/// was a module-scoped closure variable, so it died with its realm; a thread_local outlives the realm
/// because one test thread builds several `Engine`s in a row and each new realm restarts its
/// `WorkerRegistry` ids at 1. A stale id then reads as "already pumped" and the new realm's worker
/// gets NO pumps — which is exactly how `worker::worker_realms_are_their_own_global_scopes` failed:
/// its ServiceWorker case ran after its SharedWorker case, drew the same id, and never received a
/// single frame.
#[derive(Default)]
pub(crate) struct PumpedIsolates(std::cell::RefCell<std::collections::HashSet<u32>>);

/// Start the main-side receive pumps for a shared (or service) worker isolate, ONCE.
///
/// Two of the four channels, and only two: a remote port's post leaving the worker -> the page's
/// paired port, and a worker `BroadcastChannel` post -> the page's channels of that name. A shared
/// worker has no `Worker` object to fire `message`/`error` at, which is why the other two are absent
/// here and were absent in the JS.
pub fn start_isolate_pumps(scope: &mut v8::PinScope, id: f64) {
    let key = id as u32;
    let fresh = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<PumpedIsolates>().is_none() {
            st.put(PumpedIsolates::default());
        }
        let pumped = st.borrow::<PumpedIsolates>();
        let fresh = pumped.0.borrow_mut().insert(key);
        fresh
    };
    if !fresh {
        return;
    }
    // `arm_pump`'s dispatch target is unused for these two channels (both deliver through an op
    // rather than firing an event), so the global stands in for it.
    let global = scope.get_current_context().global(scope);
    arm_pump(scope, global, id, 2); // op_worker_port_recv
    arm_pump(scope, global, id, 1); // op_worker_bc_recv
}

// `op_start_worker_pumps` IS GONE. It was W4.c's second transitional op: the
// ServiceWorker container was still JS in `host.js` and registered an isolate the same way a
// `SharedWorker` does, so it needed a way into [`start_isolate_pumps`] and an op was the only one.
// The container's `register` is a table row now
// (`core/workers/service_worker.rs::cb_container_register`) and calls the function directly.
//
// Its `reentrant` attribute went with it, and the reason is worth keeping written down: arming a pump
// CALLS `op_worker_port_recv` and `op_worker_bc_recv`, and deno_core aborts the process on a
// re-entrant op that did not declare itself ("op X was not marked as #[op2(reentrant)], but
// re-entrantly invoked op Y"). A plain Rust call is not an op, so the hazard does not arise --
// `cb_shared_worker_ctor` has always reached `start_isolate_pumps` this way.

/// `new SharedWorker(USVString scriptURL, optional (DOMString or WorkerOptions) options)` — `length` 1.
pub(crate) fn cb_shared_worker_ctor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    if !require_new(scope, &args, "SharedWorker") || !require_args(scope, &args, "SharedWorker", 1) {
        return;
    }
    // (USVString or TrustedScriptURL): a TrustedScriptURL read as itself, nothing on it called
    // a string through the current realm's Trusted Types ("SharedWorker constructor")
    let url = if crate::core::trusted_types::tables::is_trusted(scope, args.get(0), "TrustedScriptURL") {
        crate::core::trusted_types::tables::sink_text(scope, args.get(0), "TrustedScriptURL")
    } else {
        arg_string(scope, &args, 0, "SharedWorker").and_then(|u| {
            let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedScriptURL", sample: "SharedWorker constructor", context: "Failed to construct 'SharedWorker'" };
            let at = crate::core::trusted_types::tables::SinkAt::Realm(scope.get_current_context());
            crate::core::trusted_types::tables::check_text(scope, u, &sink, at)
        })
    };
    let Some(url) = url else { return };
    // The second argument is `(DOMString or SharedWorkerOptions)`, converted as WebIDL converts the union (Chrome 150):
    // `undefined`, `null` and every object are the dictionary -- `credentials`, `name`, `type`,
    // then `extendedLifetime`, `sameSiteCookies`, each read once and converted as it is read -- and anything else is the
    // name, as a string. korvus took every value for a string: a dictionary was `ToString`ed (its `toString` called,
    // its `name` never read), and a bad `type` passed. This engine keeps the NAME, which keys the singleton.
    let opt = args.get(1);
    let name = if opt.is_null_or_undefined() || opt.is_object() {
        let Ok(d) = crate::install::ctor_dicts::convert_dict(scope, opt, "SharedWorkerOptions", "Failed to construct 'SharedWorker': ") else { return };
        get_prop(scope, d, "name").filter(|n| n.is_string()).map(|n| n.to_rust_string_lossy(scope)).unwrap_or_default()
    } else if opt.is_symbol() {
        crate::binding::protos::throw_type_error(scope, "Failed to construct 'SharedWorker': Cannot convert a Symbol value to a string");
        return;
    } else {
        match opt.to_string(scope) {
            Some(s) => s.to_rust_string_lossy(scope),
            None => return,
        }
    };
    // the script's URL, completed and checked against this realm's origin (`SharedWorker::CreateImpl` ->
    // `AbstractWorker::ResolveURL`, shared_worker.cc:155-157) -- unless a harness registered the string itself.
    // Chrome 150, measured: 'http://[' is a SyntaxError, another origin's script or about:blank a SecurityError;
    // korvus fired nothing at all for either.
    let seam = seam_source(scope, &url);
    let resolved = match &seam {
        Some(_) => None,
        None => match resolve_script_url(scope, &url, "SharedWorker") {
            Some(u) => Some(u),
            None => return,
        },
    };
    let this = args.this();
    brand(scope, this, "SharedWorker");
    rv.set(this.into());
    let Some(connect) = deno_op(scope, "op_shared_worker_connect_fn") else { return };
    // The factory op hands back the native `__sharedWorkerConnect(src, name, url)`.
    let fname = str_val(scope, "__sharedWorkerConnect");
    let flen: v8::Local<v8::Value> = v8::Integer::new(scope, 3).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(f) = connect.call(scope, undef, &[fname, flen]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let src = seam.or_else(|| local_source(scope, &url).map(|(text, _)| text));
    let (src_s, url_s) = match &src {
        Some(s) => (s.clone(), url.clone()),
        None => (String::new(), resolved.clone().unwrap_or_else(|| worker_url(scope, &url))),
    };
    let a = str_val(scope, &src_s);
    let b = str_val(scope, &name);
    let c = str_val(scope, &url_s);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(r) = f.call(scope, undef, &[a, b, c]).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return };
    let cid = r.get_index(scope, 0).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let worker_id = r.get_index(scope, 1).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let is_new = r.get_index(scope, 2).map(|v| v.boolean_value(scope)).unwrap_or(false);
    if is_new {
        start_isolate_pumps(scope, worker_id);
    }
    let Some(remote) = deno_op(scope, "op_mp_remote") else { return };
    let cidv: v8::Local<v8::Value> = v8::Number::new(scope, cid).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(port) = remote.call(scope, undef, &[cidv]) else { return };
    slot_set(scope, this, PORT_SLOT, port);
}

/// `SharedWorker.port` — the `[SameObject]` MessagePort, read back off the slot.
fn cb_shared_worker_port(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(p) = slot_get(scope, args.this(), PORT_SLOT) {
        rv.set(p);
    }
}

/// The body [`SHAREDWORKER`]'s one non-handler row names.
pub fn install_shared_worker(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "port" => branded_getter_row(scope, proto, member, cb_shared_worker_port),
        other => debug_assert!(false, "install_shared_worker: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `SharedWorker` block of `install/generated_registry.rs`, in the capture's
// order: `port` then `onerror`. `[value, window]`: only the WINDOW capture lists `SharedWorker`.
webapi::interface! {
    "SharedWorker" : "EventTarget" [value, window] {
        ctor    "SharedWorker"             len 1 required 1 = stub,
        attr    "port" : Object readonly                    = install_shared_worker,
        handler "onerror"                                   = evt_handler
    }
}
