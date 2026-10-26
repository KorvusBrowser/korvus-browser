//! `core/workers/worker_global_scope.rs` — the WORKER GLOBAL, as tables (invariants I8/I9).
//!
//! WHAT WAS HERE BEFORE. Four JS files in `core/korvus-runtime/src/js/` assembled this realm:
//! `worker-host.js` assigned `self`, `location`, the five timers and `queueMicrotask` straight onto
//! `globalThis`; `generated_worker_shells.js` built a `WorkerGlobalScope` / `DedicatedWorkerGlobalScope`
//! pair out of the capture with `op_noop_method` stubs (since deleted) for all 29 rows; `worker-finalize.js` then
//! *re-defined twelve of those rows* with `op_nativize`-masked JS bodies closing over what
//! `worker-host.js` had put on the global, reparented `globalThis` onto
//! `DedicatedWorkerGlobalScope.prototype`, and swept the own copies. Three writers per member, in
//! three files, in a load order nothing enforced — and by the time this unit started, THREE MORE Rust
//! installs (`crypto.rs::place_crypto`, `performance.rs::put_singleton`,
//! `navigator.rs::move_navigator_onto_the_scope_prototype`) were each defining one more row on the
//! same prototype from a fourth place.
//!
//! There is ONE definition now: the tables below, mounted by THE installer, once, from
//! [`install_worker_global_scope`] — which also does the reparent, natively, before any script runs
//! (goal ruling R9).
//!
//! THE SINGLETON SLOT is what makes the load order stop mattering. `crypto`, `navigator`,
//! `performance`, `location`, `indexedDB` and `caches` are minted by six different installs that run
//! at six different moments; each one PUBLISHES its object with [`publish_scope_singleton`], and the
//! table's row is a native getter that reads the slot AT CALL TIME. So the row can be mounted before
//! the object exists, the object can be replaced later, and neither is a second writer of the member.
//!
//! THE TRAP (recorded by the W6.1 unit, and it is real): `globalThis.set("crypto", obj)` silently does
//! nothing once `WorkerGlobalScope.prototype` carries a get-only `crypto` accessor, because
//! OrdinarySet walks the chain, finds an accessor with no setter, and returns false. Anything moved
//! onto the prototype must therefore be READ BACK THROUGH THE PROTOTYPE before its own copy is
//! deleted — reading through the global would find the own property and let the check say yes to
//! itself. [`strip_own_copies`] does exactly that.
//!
//! WHERE THE BODIES ARE. A member whose body is a KERNEL SERVICE — the five timers, `importScripts`,
//! `close`, `postMessage`, the two animation-frame methods — is built by `korvus-runtime`'s
//! `op_worker_scope_fn` factory and mounted through [`kernel_row`], the same cross-crate shape the
//! `Worker` table has used for `postMessage`/`terminate` since W4.c. Spawning, fetching, timing and
//! message plumbing are the kernel's (invariant I9); the SHAPE is this table's.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, str_val};
use crate::core::xmlhttprequest::xml_http_request::deno_op;
use crate::install::kits::{define_accessor, define_value};
use crate::install::{installer, MemberDef, MemberKind};

/// Mount a row whose body is a KERNEL SERVICE: ask `korvus-runtime`'s `op_worker_scope_fn` factory for
/// a native function carrying this row's own `name` and `length`, and define it with the row's flags.
///
/// NOT `core/workers/worker.rs`'s `factory_row`, which wraps what it gets in the interface's BRAND: a
/// scope member's receiver is the global object, which carries no brand, so a branded wrapper would
/// make every call throw `Illegal invocation`. The factory's bodies do their own WebIDL argument
/// checks — with the holder's name in the message, which is the one thing a generic guard cannot get
/// right here (`'WorkerGlobalScope'` for `setTimeout`, `'DedicatedWorkerGlobalScope'` for
/// `postMessage`) — so the row is stamped as already-guarded and the arity pass leaves it alone.
///
/// A realm that does not register the factory (the bare, messaging-only worker seam) gets the D2 stub
/// for the row: the one not-implemented mechanism, counted (invariant I7).
fn kernel_row(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    let Some(f) = kernel_fn(scope, member) else {
        installer::put_table_stub(scope, target, member, true);
        return;
    };
    define_value(scope, target, member, f.into(), true);
}

/// The factory's function for a kernel row, stamped as already arity-guarded (`None`: no factory in this
/// realm, or none for this name).
fn kernel_fn<'s>(scope: &mut v8::PinScope<'s, '_>, member: &'static MemberDef) -> Option<v8::Local<'s, v8::Function>> {
    let factory = deno_op(scope, "op_worker_scope_fn")?;
    let name = str_val(scope, member.name);
    let len: v8::Local<v8::Value> = v8::Integer::new(scope, member.length as i32).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let f = factory.call(scope, undef, &[name, len]).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    crate::binding::protos::stamp_arity_guarded(scope, f);
    Some(f)
}

// ── the scope singleton slots ───────────────────────────────────────────────────────────────────

/// The v8 `Private` this realm keeps `crypto` / `navigator` / … in, so the table's row can read it
/// whenever the page asks rather than whenever the installer happened to run.
fn slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Private>> {
    let n = v8::String::new(scope, &format!("korvus.scope.{name}"))?;
    Some(v8::Private::for_api(scope, Some(n)))
}

/// Publish this realm's `<name>` singleton. Called by whichever install mints the object; the
/// `WorkerGlobalScope` row that exposes it is the table's, and only the table's.
pub fn publish_scope_singleton(scope: &mut v8::PinScope, name: &str, value: v8::Local<v8::Value>) {
    let global = scope.get_current_context().global(scope);
    if let Some(k) = slot(scope, name) {
        global.set_private(scope, k, value);
    }
}

/// This worker realm's own URL (`self.location.href`), off its location's slot so no page code runs, for the
/// call recorder's realm record: a worker is named by its script, as Chrome's tracer names it, not by the page
/// that made it. Empty in a realm that published no location -- every window realm.
#[cfg(feature = "analysis")]
pub(crate) fn worker_href_for_analysis(scope: &mut v8::PinScope) -> String {
    worker_href(scope)
}

/// [`worker_href_for_analysis`]'s read, for the engine's own use (`navigator.connection`'s noise is keyed by
/// the realm's URL): this worker realm's `self.location.href`, empty in every window realm.
pub(crate) fn worker_href(scope: &mut v8::PinScope) -> String {
    let Ok(loc) = v8::Local::<v8::Object>::try_from(scope_singleton(scope, "location")) else { return String::new() };
    let Some(n) = v8::String::new(scope, HREF_SLOT) else { return String::new() };
    let k = v8::Private::for_api(scope, Some(n));
    loc.get_private(scope, k).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}

/// Read it back (`undefined` when nothing published one — a bare worker, or a realm where that
/// install did not run).
pub(crate) fn scope_singleton<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Value> {
    let global = scope.get_current_context().global(scope);
    match slot(scope, name).and_then(|k| global.get_private(scope, k)) {
        Some(v) => v,
        None => v8::undefined(scope).into(),
    }
}

/// `get <singleton>` — the row's body: read the realm's slot. The member NAME travels as the
/// function's call data, so one callback serves all six rows.
fn cb_scope_singleton(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = scope_singleton(scope, &name);
    rv.set(v);
}

/// `get self` — the realm's global object. (`self === globalThis`, and it is NOT an own property of
/// the global in any browser: `Object.getOwnPropertyNames(self)` in Chrome's worker has no `self`.)
fn cb_scope_self(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let g = scope.get_current_context().global(scope);
    rv.set(g.into());
}

// ── WorkerLocation ──────────────────────────────────────────────────────────────────────────────

/// This worker's own script URL, kept on the `WorkerLocation` INSTANCE (a v8 private, not an own
/// property — Chrome's `Object.getOwnPropertyNames(self.location)` is `[]`).
const HREF_SLOT: &str = "korvus.workerLocation.href";

fn location_href(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<String> {
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    let n = v8::String::new(scope, HREF_SLOT)?;
    let k = v8::Private::for_api(scope, Some(n));
    obj.get_private(scope, k).filter(|v| !v.is_undefined()).map(|v| v.to_rust_string_lossy(scope))
}

/// The realm's `location.href`, read off the published `WorkerLocation` (used by `isSecureContext`).
pub(crate) fn scope_href(scope: &mut v8::PinScope) -> String {
    let loc = scope_singleton(scope, "location");
    location_href(scope, loc).unwrap_or_default()
}

macro_rules! loc_get {
    ($cb:ident, |$u:ident| $body:block) => {
        fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let href = location_href(scope, args.this().into()).unwrap_or_default();
            // An unparseable href (the `__workerScripts` / Blob test seam's keys, and "about:blank"
            // before a URL is known) answers the empty string for every component, which is what the
            // two-property object literal this replaced did for the eight it did not have at all.
            let s = match url::Url::parse(&href) {
                Ok($u) => $body,
                Err(_) => String::new(),
            };
            let v = str_val(scope, &s);
            rv.set(v);
        }
    };
}

loc_get!(cb_loc_href, |u| { u.as_str().to_string() });
loc_get!(cb_loc_protocol, |u| { format!("{}:", u.scheme()) });
loc_get!(cb_loc_hostname, |u| { u.host_str().unwrap_or("").to_string() });
loc_get!(cb_loc_port, |u| { u.port().map(|p| p.to_string()).unwrap_or_default() });
loc_get!(cb_loc_origin, |u| { u.origin().ascii_serialization() });
loc_get!(cb_loc_pathname, |u| { u.path().to_string() });
loc_get!(cb_loc_host, |u| {
    match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    }
});
loc_get!(cb_loc_search, |u| {
    match u.query() {
        Some(q) if !q.is_empty() => format!("?{q}"),
        _ => String::new(),
    }
});
loc_get!(cb_loc_hash, |u| {
    match u.fragment() {
        Some(f) if !f.is_empty() => format!("#{f}"),
        _ => String::new(),
    }
});

/// `WorkerLocation.toString()` — the stringifier, which WebIDL defines as `href`.
fn cb_loc_to_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let href = location_href(scope, args.this().into()).unwrap_or_default();
    let s = match url::Url::parse(&href) {
        Ok(u) => u.as_str().to_string(),
        Err(_) => String::new(),
    };
    let v = str_val(scope, &s);
    rv.set(v);
}

/// The body every row of [`WORKERLOCATION`] names.
pub fn install_worker_location_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let g = match member.name {
        "href" => build_fn(scope, cb_loc_href, "get href", 0),
        "protocol" => build_fn(scope, cb_loc_protocol, "get protocol", 0),
        "host" => build_fn(scope, cb_loc_host, "get host", 0),
        "hostname" => build_fn(scope, cb_loc_hostname, "get hostname", 0),
        "port" => build_fn(scope, cb_loc_port, "get port", 0),
        "pathname" => build_fn(scope, cb_loc_pathname, "get pathname", 0),
        "search" => build_fn(scope, cb_loc_search, "get search", 0),
        "hash" => build_fn(scope, cb_loc_hash, "get hash", 0),
        "origin" => build_fn(scope, cb_loc_origin, "get origin", 0),
        "toString" => {
            let f = build_fn(scope, cb_loc_to_string, "toString", 0);
            define_value(scope, proto, member, f.into(), true);
            return;
        }
        other => {
            debug_assert!(false, "install_worker_location_row: unexpected member {other}");
            return;
        }
    };
    define_accessor(scope, proto, member, g, None);
}

// Rows VERBATIM from the `WorkerLocation` block of `install/generated_registry.rs`, in the capture's
// order (`worker.surface.json` records the same ten on the singleton). `[interface, worker]`: only the
// worker capture lists it, and it is not constructible.
webapi::interface! {
    "WorkerLocation" [interface, worker] {
        ctor   "WorkerLocation"              len 0            = stub,
        attr   "origin"   : UsvString readonly                 = install_worker_location_row,
        attr   "protocol" : UsvString readonly                 = install_worker_location_row,
        attr   "host"     : UsvString readonly                 = install_worker_location_row,
        attr   "hostname" : UsvString readonly                 = install_worker_location_row,
        attr   "port"     : UsvString readonly                 = install_worker_location_row,
        attr   "pathname" : UsvString readonly                 = install_worker_location_row,
        attr   "search"   : UsvString readonly                 = install_worker_location_row,
        attr   "hash"     : UsvString readonly                 = install_worker_location_row,
        attr   "href"     : UsvString readonly                 = install_worker_location_row,
        method "toString" : UsvString len 0                    = install_worker_location_row
    }
}

// ── WorkerGlobalScope ───────────────────────────────────────────────────────────────────────────

/// `get isSecureContext` — the SAME rule the window applies, over this realm's own script URL.
///
/// A worker whose script URL does not PARSE as an absolute URL has no environment settings object URL
/// of its own: HTML's "local scheme" case, where the worker INHERITS its owner's secure context. This
/// engine's worker cannot read the page's, so it answers `true`, which is the page's answer for every
/// URL that reaches this branch (`about:blank`, a `blob:` handle, the `__workerScripts` test seam's
/// relative keys). Chrome: `isSecureContext` is `true` in a worker of a secure page and `false` in a
/// worker of an insecure one — the parseable branch above gets that right.
fn cb_scope_is_secure_context(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let h = scope_href(scope);
    let secure = match url::Url::parse(&h) {
        Ok(_) => crate::core::html::window::potentially_trustworthy(&h),
        Err(_) => true,
    };
    rv.set_bool(secure);
}

/// `get origin` — this realm's origin, serialized; "null" for an opaque one, exactly as the window's.
fn cb_scope_origin(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let h = scope_href(scope);
    let s = url::Url::parse(&h).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".to_string());
    let v = str_val(scope, &s);
    rv.set(v);
}

/// A `[Replaceable]` readonly attribute of a `[Global]`-adjacent interface: the setter's whole effect is
/// to replace the inherited accessor with an own data property on the GLOBAL. Chrome does this for
/// `origin` and `performance` on `WorkerGlobalScope` (measured: `self.performance = 1` then reads `1`),
/// and without it every one of those assignments is a strict-mode TypeError.
fn cb_scope_replaceable_set(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &a).to_rust_string_lossy(scope);
    let global = scope.get_current_context().global(scope);
    let Some(k) = v8::String::new(scope, &name) else { return };
    let mut d = v8::PropertyDescriptor::new_from_value_writable(a.get(0), true);
    d.set_enumerable(true);
    d.set_configurable(true);
    global.define_property(scope, k.into(), &d);
}

/// The body every non-handler, non-constant row of [`WORKERGLOBALSCOPE`] names.
///
/// `target` is `WorkerGlobalScope.prototype`: these are INHERITED members, not `[Global]` ones, which
/// is why the worker global has no own copy of any of them in any browser.
/// `get trustedTypes` on a worker global: one factory per global, minted on first read.
fn cb_scope_trusted_types(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let global = scope.get_current_context().global(scope);
    if let Some(v) = crate::core::css::css_style_declaration::priv_get(scope, global, "korvus_worker_trusted_types") {
        rv.set(v);
        return;
    }
    let Some(proto) = crate::binding::protos::shell_proto(scope, "TrustedTypePolicyFactory") else {
        rv.set_null();
        return;
    };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::core::dom::element::priv_set(scope, global, "korvus_worker_trusted_types", o.into());
    rv.set(o.into());
}

fn cb_scope_scheduler(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match crate::core::html::scheduler::worker_scheduler_singleton(scope) {
        Some(o) => rv.set(o.into()),
        None => rv.set_null(),
    }
}

pub fn install_scope(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    let name = member.name;
    match name {
        // The six singletons: one native getter per row, reading this realm's published object.
        "location" | "navigator" | "crypto" | "indexedDB" | "caches" => {
            let d = str_val(scope, name);
            let g = build_fn_data(scope, cb_scope_singleton, &format!("get {name}"), 0, d);
            define_accessor(scope, target, member, g, None);
        }
        // `scheduler`, settable like `performance` (a stub answering `null`).
        // A worker's `trustedTypes` is its own `TrustedTypePolicyFactory` (Chrome 150:
        // `[object TrustedTypePolicyFactory]`, `createPolicy` works and `eval` runs a `TrustedScript`).
        // a challenge provider a challenge widget's challenge worker creates a policy before it evaluates anything.
        "trustedTypes" => {
            let g = build_fn(scope, cb_scope_trusted_types, "get trustedTypes", 0);
            define_accessor(scope, target, member, g, None);
        }
        "scheduler" => {
            let g = build_fn(scope, cb_scope_scheduler, "get scheduler", 0);
            let d2 = str_val(scope, name);
            let s = build_fn_data(scope, cb_scope_replaceable_set, "set scheduler", 1, d2);
            define_accessor(scope, target, member, g, Some(s));
        }
        // …and `performance`, which the capture records as SETTABLE (`[Replaceable]`).
        "performance" => {
            let d = str_val(scope, name);
            let g = build_fn_data(scope, cb_scope_singleton, "get performance", 0, d);
            let d2 = str_val(scope, name);
            let s = build_fn_data(scope, cb_scope_replaceable_set, "set performance", 1, d2);
            define_accessor(scope, target, member, g, Some(s));
        }
        "self" => {
            let g = build_fn(scope, cb_scope_self, "get self", 0);
            define_accessor(scope, target, member, g, None);
        }
        "origin" => {
            let g = build_fn(scope, cb_scope_origin, "get origin", 0);
            let d = str_val(scope, name);
            let s = build_fn_data(scope, cb_scope_replaceable_set, "set origin", 1, d);
            define_accessor(scope, target, member, g, Some(s));
        }
        "isSecureContext" => {
            let g = build_fn(scope, cb_scope_is_secure_context, "get isSecureContext", 0);
            define_accessor(scope, target, member, g, None);
        }
        "crossOriginIsolated" => {
            let g = build_fn(scope, crate::core::html::isolation_policy::cb_worker_cross_origin_isolated, "get crossOriginIsolated", 0);
            define_accessor(scope, target, member, g, None);
        }
        // The four `WindowOrWorkerGlobalScope` bodies this crate already owns. Each does its own
        // WebIDL argument check and is stamped as such, so the arity pass leaves it alone.
        "fetch" => {
            let f = build_fn(scope, crate::modules::fetch::fetch_global::cb_fetch, "fetch", 1);
            crate::binding::protos::stamp_arity_guarded(scope, f);
            define_value(scope, target, member, f.into(), true);
        }
        "structuredClone" => {
            let f = build_fn(scope, crate::modules::structured_clone::cb_structured_clone, "structuredClone", 1);
            crate::binding::protos::stamp_arity_guarded(scope, f);
            define_value(scope, target, member, f.into(), true);
        }
        "atob" | "btoa" => {
            let d = str_val(scope, name);
            let f = build_fn_data(scope, crate::modules::base64::cb_base64, name, 1, d);
            crate::binding::protos::stamp_arity_guarded(scope, f);
            define_value(scope, target, member, f.into(), true);
        }
        "reportError" => {
            let f = build_fn(scope, crate::core::events::report::cb_report_exception, name, member.length as i32);
            let g = crate::binding::protos::make_arity_guard(scope, "WorkerGlobalScope", name, member.required as i32, member.length as i32, f.into(), member.ty);
            define_value(scope, target, member, g.into(), true);
        }
        // The kernel services: the timer quartet + `queueMicrotask` + `importScripts`. The factory op
        // lives in `korvus-runtime` because the scheduler, the module loader and the transport do
        // (invariant I9); a realm that does not register it gets the D2 stub for the row, counted.
        "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval" | "queueMicrotask" | "importScripts" => {
            kernel_row(scope, target, member);
        }
        // Everything else is honestly not implemented in this engine, in either realm: `fonts` and
        // `createImageBitmap` (`trustedTypes` and `scheduler` have arms above). A D2 stub row is what that means (invariant
        // I7) — and it is the SAME shape the generated worker shell gave them, now counted.
        _ => installer::put_table_stub(scope, target, member, true),
    }
}

// Rows VERBATIM from the `WorkerGlobalScope` block of `install/generated_registry.rs`, in the
// capture's order. `[interface, worker]` — the window has `Window` instead. Re-measured against
// `google-chrome --headless=new` 150.0.7871.46 with `tests/fixtures/worker-scope-probe.js`: every one
// of these is an enumerable, configurable member of `WorkerGlobalScope.prototype` there, `origin`,
// `performance` and `scheduler` with a setter and the rest get-only, which is what these rows say.
webapi::interface! {
    "WorkerGlobalScope" : "EventTarget" [interface, worker] {
        ctor     "WorkerGlobalScope"                len 0             = stub,
        attr     "self"                : Object readonly              = install_scope,
        attr     "location"            : Object readonly              = install_scope,
        handler  "onerror"                                            = event_handler,
        handler  "onlanguagechange"                                   = event_handler,
        attr     "navigator"           : Object readonly              = install_scope,
        handler  "onrejectionhandled"                                 = event_handler,
        handler  "onunhandledrejection"                               = event_handler,
        attr     "origin"              : UsvString                    = install_scope,
        attr     "performance"         : Object                       = install_scope,
        attr     "trustedTypes"        : Object readonly              = install_scope,
        attr     "crypto"              : Object readonly              = install_scope,
        attr     "indexedDB"           : Object readonly              = install_scope,
        attr     "fonts"               : Object readonly              = stub,
        method   "createImageBitmap"   : Promise len 1 required 1     = stub,
        method   "fetch"               : Promise len 1 required 1     = install_scope,
        method   "importScripts"       : Undefined len 0              = install_scope,
        attr     "isSecureContext"     : Boolean readonly             = install_scope,
        attr     "crossOriginIsolated" : Boolean readonly             = install_scope,
        attr     "scheduler"           : Object                       = install_scope,
        attr     "caches"              : Object readonly              = install_scope,
        method   "atob"                : DomString len 1 required 1   = install_scope,
        method   "btoa"                : DomString len 1 required 1   = install_scope,
        method   "queueMicrotask"      : Undefined len 1 required 1   = install_scope,
        method   "reportError"         : Undefined len 1 required 1   = install_scope,
        method   "structuredClone"     : Any len 1 required 1 nullable = install_scope,
        method   "clearInterval"       : Undefined len 0              = install_scope,
        method   "clearTimeout"        : Undefined len 0              = install_scope,
        method   "setInterval"         : Integer len 1 required 1     = install_scope,
        method   "setTimeout"          : Integer len 1 required 1     = install_scope
    }
}

// ── the three most-derived scopes ───────────────────────────────────────────────────────────────

/// `get name` on a worker global. Chrome's is an accessor PAIR (measured), and the setter stores —
/// `self.name = "x"` then reads back `"x"` — so this is a slot round-trip, not a replaceable.
fn cb_scope_name_get(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = scope_singleton(scope, "name");
    if v.is_undefined() {
        let s = str_val(scope, "");
        rv.set(s);
        return;
    }
    rv.set(v);
}

fn cb_scope_name_set(scope: &mut v8::PinScope, a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = a.get(0).to_rust_string_lossy(scope);
    let v = str_val(scope, &s);
    publish_scope_singleton(scope, "name", v);
}

/// The body the `[Global]` rows of the three scope tables name. `target` is the GLOBAL OBJECT: a
/// `[Global]` interface's own members live on the global, never on its interface prototype object
/// (the same rule `install_window_window` follows for `Window`).
///
/// EVERY ROW CHECKS ITS RECEIVER IS THE GLOBAL, as `Window`'s do (`window::guarded`,
/// outermost): Chrome 150.0.7871.46 throws `Illegal invocation` from each of them given `{}` or
/// `Object.create(DedicatedWorkerGlobalScope.prototype)` -- `close.call({})`, the `name` pair, the handler
/// pairs, measured in a dedicated and a shared worker -- where every one answered here.
pub fn install_scope_global(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::html::window::guarded;
    match member.name {
        "name" => {
            let g0 = build_fn(scope, cb_scope_name_get, "get name", 0);
            let g = guarded(scope, g0.into(), "get name", 0);
            let s0 = build_fn(scope, cb_scope_name_set, "set name", 1);
            let s = guarded(scope, s0.into(), "set name", 1);
            define_accessor(scope, target, member, g, Some(s));
        }
        "close" | "postMessage" | "requestAnimationFrame" | "cancelAnimationFrame" => {
            let Some(f) = kernel_fn(scope, member) else {
                installer::put_table_stub(scope, target, member, true);
                return;
            };
            let v = guarded(scope, f.into(), member.name, member.length as i32);
            define_value(scope, target, member, v.into(), true);
        }
        n if n.starts_with("on") => global_handler(scope, target, member),
        // The four legacy webkit filesystem entry points. This engine has no filesystem, so they are
        // D2 stub rows — native, counted, and answering nothing, which is the observable result of a
        // callback that never resolves. Their lengths are Chrome's, measured with the scope probe
        // (2/2/2/1 — `worker-finalize.js` had two of the four wrong).
        _ => installer::put_table_stub(scope, target, member, true),
    }
}

/// A `[Global]` scope's `on<type>` row: the event-handler pair over the ONE handler store
/// (`core/events/event_target.rs`), receiver-checked like every other row of the scope (`Window`'s
/// handler arm, in the worker's scopes).
pub(crate) fn global_handler(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::html::window::guarded;
    let n = member.name;
    let ty = n.strip_prefix("on").unwrap_or(n);
    let g0 = crate::core::events::event_target::make_et_on_getter(scope, ty);
    let s0 = crate::core::events::event_target::make_et_on_setter(scope, ty);
    let g = guarded(scope, g0.into(), &format!("get {n}"), 0);
    let s = guarded(scope, s0.into(), &format!("set {n}"), 1);
    define_accessor(scope, target, member, g, Some(s));
}

// The `[Global]` member rows are `worker.surface.json`'s flat `globals` list (the capture records
// their NAMES and no descriptors, exactly as it does for `Window`'s), with kind/length/shape read off
// `google-chrome --headless=new` 150.0.7871.46 by `tests/fixtures/worker-scope-probe.js`. The two
// `constant` rows are VERBATIM from `install/generated_registry.rs`'s `DedicatedWorkerGlobalScope`
// block, which is the only thing that block ever carried.
webapi::interface! {
    "DedicatedWorkerGlobalScope" : "WorkerGlobalScope" [interface, worker] {
        ctor            "DedicatedWorkerGlobalScope" len 0                = stub,
        attr            "name"                  : DomString               = install_scope_global,
        handler         "onmessage"                                       = install_scope_global,
        handler         "onmessageerror"                                  = install_scope_global,
        method          "cancelAnimationFrame"  : Undefined len 1 required 1 = install_scope_global,
        method          "close"                 : Undefined len 0         = install_scope_global,
        method          "postMessage"           : Undefined len 1 required 1 = install_scope_global,
        method          "requestAnimationFrame" : Integer len 1 required 1 = install_scope_global,
        handler         "onrtctransform"                                  = install_scope_global,
        method          "webkitRequestFileSystem"             : Undefined len 2 required 2 = stub,
        method          "webkitRequestFileSystemSync"         : Object    len 2 required 2 = stub,
        method          "webkitResolveLocalFileSystemSyncURL" : Object    len 1 required 1 = stub,
        method          "webkitResolveLocalFileSystemURL"     : Undefined len 2 required 2 = stub,
        constant        "TEMPORARY"  : Integer = 0,
        constant        "PERSISTENT" : Integer = 1,
        static_constant "TEMPORARY"  : Integer = 0,
        static_constant "PERSISTENT" : Integer = 1
    }
}

// A SharedWorker isolate is a DIFFERENT global scope, and the difference decides whether a
// `.any.sharedworker.html` file can report at all: testharness.js tests
// `global_scope instanceof DedicatedWorkerGlobalScope` FIRST, so a shared worker wearing the dedicated
// scope publishes its results down the dedicated `postMessage` plane, which a page holding a
// `SharedWorker` has no end of. Neither scope is in the bundle (the capture is a dedicated worker), so
// these rows are WebIDL's, which is what `worker-finalize.js` built by hand.
webapi::interface! {
    "SharedWorkerGlobalScope" : "WorkerGlobalScope" [interface, worker] {
        ctor    "SharedWorkerGlobalScope" len 0      = stub,
        attr    "name" : DomString                   = install_scope_global,
        handler "onconnect"                          = install_scope_global
    }
}

/// The three `ServiceWorkerGlobalScope` rows whose bodies are the SW realm's own
/// (`core/workers/service_worker.rs`). A local one-liner because a table row's body ident has to
/// resolve in the file the macro is in.
pub fn install_sw_scope(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    if member.name.starts_with("on") {
        global_handler(scope, target, member);
        return;
    }
    super::service_worker::install_service_worker_scope(scope, target, member);
}

// `clients`, `registration` and `skipWaiting` WERE `SW_PRELUDE`, a Rust string literal in
// `core/korvus-runtime/src/worker.rs` prepended to every ServiceWorker's source. That
// string ALSO wrote `globalThis.oninstall/onactivate/onfetch/onmessage = null` — over the four
// handler rows below, which this installer had already mounted on the global, because the worker
// source runs after every install. Four slots with two writers (I2), and the second one broke the
// mechanism: `self.oninstall = f` set a plain data property instead of the realm's handler store, so
// a `dispatchEvent`ed `install` event would not have run it. The fixture in
// `tests/fixtures/worker/service-worker.js` uses `addEventListener`, which is why nothing noticed.
//
// Neither scope is in the bundle (the capture is a dedicated worker), so these rows are WebIDL's —
// the same footing `SharedWorkerGlobalScope`'s two are on.
webapi::interface! {
    "ServiceWorkerGlobalScope" : "WorkerGlobalScope" [interface, worker] {
        ctor    "ServiceWorkerGlobalScope" len 0     = stub,
        attr    "clients"       : Object readonly    = install_sw_scope,
        attr    "registration"  : Object readonly    = install_sw_scope,
        method  "skipWaiting"   : Promise len 0      = install_sw_scope,
        handler "oninstall"                          = install_sw_scope,
        handler "onactivate"                         = install_sw_scope,
        handler "onfetch"                            = install_sw_scope,
        handler "onsync"                             = install_sw_scope,
        handler "onpush"                             = install_sw_scope,
        handler "onnotificationclick"                = install_sw_scope,
        handler "onnotificationclose"                = install_sw_scope,
        handler "onmessage"                          = install_sw_scope,
        handler "onmessageerror"                     = install_sw_scope
    }
}

// ── the install ─────────────────────────────────────────────────────────────────────────────────

/// Which `WorkerGlobalScope` this isolate IS, from the kernel (`op_worker_scope_kind`).
///
/// `pub(crate)`: the event family asks the same question to decide whether this
/// realm gets the `[Exposed=ServiceWorker]` event chain, and a second way of asking would be a second
/// truth about which scope an isolate is.
pub(crate) fn scope_kind(scope: &mut v8::PinScope) -> String {
    let Some(f) = deno_op(scope, "op_worker_scope_kind") else { return "dedicated".to_string() };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    match f.call(scope, undef, &[]) {
        Some(v) => v.to_rust_string_lossy(scope),
        None => "dedicated".to_string(),
    }
}

/// This isolate's scope NAME, from the kernel (`op_worker_scope_name`): the `new SharedWorker(url, name)` name, or a
/// dedicated worker's `WorkerOptions.name`; empty in a service worker.
fn kernel_scope_name(scope: &mut v8::PinScope) -> String {
    let Some(f) = deno_op(scope, "op_worker_scope_name") else { return String::new() };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    match f.call(scope, undef, &[]) {
        Some(v) => v.to_rust_string_lossy(scope),
        None => String::new(),
    }
}

/// Mint this realm's `WorkerLocation` and publish it. The href is the kernel's `HostLocation` — this
/// worker's OWN script URL, never the page's.
fn mint_location<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>) {
    let href = deno_op(scope, "op_worker_location_href")
        .and_then(|f| {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            f.call(scope, undef, &[])
        })
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    if let Some(n) = v8::String::new(scope, HREF_SLOT) {
        let k = v8::Private::for_api(scope, Some(n));
        let hv = str_val(scope, &href);
        obj.set_private(scope, k, hv);
    }
    publish_scope_singleton(scope, "location", obj.into());
}

/// Does the global's PROTOTYPE CHAIN carry an own `name` anywhere above it?
///
/// NOT `proto.get(scope, name)`, which is what this was first written as and what the W6.1 trap note
/// asks for in its own words ("read through the prototype to confirm it answers"). A row of a hand
/// table is mounted through `install::receiver`, so its getter THROWS `Illegal invocation` for a
/// receiver that is not an instance of the interface — and `WorkerGlobalScope.prototype` is not an
/// instance of `WorkerGlobalScope`. The read threw for every row, left a pending exception in the
/// install's scope, and surfaced as one `Uncaught TypeError: Illegal invocation` per worker.
///
/// So the check is the one the trap actually needs and the one a getter cannot refuse: the chain OWNS
/// a descriptor for the name. That is strictly stronger than "answers", because a member whose getter
/// legitimately returns `undefined` (an unpublished singleton in a bare worker) is still inherited.
fn chain_owns<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, key: v8::Local<'s, v8::String>) -> bool {
    let mut cur = global.get_prototype(scope);
    for _ in 0..8 {
        let Some(o) = cur.and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return false };
        if o.get_own_property_descriptor(scope, key.into()).map(|d| !d.is_undefined()).unwrap_or(false) {
            return true;
        }
        cur = o.get_prototype(scope);
    }
    false
}

/// Drop the own copies of members this realm now INHERITS — and only once the chain genuinely carries
/// them. Checking through the GLOBAL would find its own copy and let the check say yes to itself,
/// which is why [`chain_owns`] starts at the global's PROTOTYPE.
fn strip_own_copies<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, names: &[&str]) {
    for n in names {
        let Some(k) = v8::String::new(scope, n) else { continue };
        if !global.has_own_property(scope, k.into()).unwrap_or(false) {
            continue;
        }
        if chain_owns(scope, global, k) {
            global.delete(scope, k.into());
        }
    }
}

/// Build the worker global scope from its tables and CHAIN the global onto it.
///
/// Runs from `core/korvus-runtime/src/worker.rs` after the realm's other installs and before the
/// worker's own script — natively, once, in one place (goal ruling R9: the bisected timeout came from
/// re-chaining a LIVE global, so the chain is built once and then scripts run).
pub fn install_worker_global_scope<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::install::live::parent_of;
    use crate::install::installer::Flavor;

    // 0. ADOPT whatever the ESM layer left as an OWN global for a name this table is about to make an
    //    inherited row. `worker-host.js` still mints the `navigator` cppgc instance (a deno_core
    //    `objects =` class is reachable only from ESM), and an embedder may provide its own; either
    //    way the OBJECT is kept and only its reachability changes. Nothing is overwritten: a name
    //    something already published wins, because that publisher minted it deliberately.
    for name in ["navigator", "crypto", "performance", "indexedDB", "caches"] {
        if !scope_singleton(scope, name).is_undefined() {
            continue;
        }
        let Some(k) = v8::String::new(scope, name) else { continue };
        if !global.has_own_property(scope, k.into()).unwrap_or(false) {
            continue;
        }
        if let Some(v) = global.get(scope, k.into()).filter(|v| !v.is_undefined()) {
            publish_scope_singleton(scope, name, v);
        }
    }

    let mut ledger = installer::take_ledger(scope);

    // 1. `WorkerLocation` + this realm's instance. The `location` ROW reads the slot at call time, so
    //    this could equally run after the scope install; it runs first because nothing needs it to.
    let loc = installer::create_interface(scope, global, &WORKERLOCATION, "WorkerLocation", None, None, Flavor::Table, &mut ledger);
    mint_location(scope, loc.proto);

    // 2. `WorkerGlobalScope` itself, spliced onto `EventTarget` (its table's `parent`), so the global
    //    reaches `addEventListener` through the chain rather than owning a copy.
    let et = parent_of(scope, "EventTarget");
    let wgs = installer::create_interface(scope, global, &WORKERGLOBALSCOPE, "WorkerGlobalScope", et, None, Flavor::Table, &mut ledger);
    for g in &wgs.gaps {
        eprintln!("[korvus-worker] scope install gap: {}.{}", g.interface, g.member);
    }

    // 3. The most-derived scope. `[Global]` moves its own members off the prototype and onto the
    //    global object, so the create walks a selector of just the two constants and the rest are
    //    mounted by hand — the same split `install_window_window` makes for `Window`.
    // The scope NAME, before the rows: `SharedWorkerGlobalScope.name` is the `new SharedWorker(url,
    // name)` name and a page's `.any.sharedworker.html` reads it, so the slot has to be seeded rather
    // than left to whatever the first assignment puts there. A dedicated worker's is its `WorkerOptions.name`
    // (Chrome 150, 2026-10-11: `new Worker(u, {name: 'blob'})` reads `self.name` "blob"; korvus read "").
    let kind = scope_kind(scope);
    if kind == "shared" || kind == "dedicated" {
        let n = kernel_scope_name(scope);
        let nv = str_val(scope, &n);
        publish_scope_singleton(scope, "name", nv);
    }
    let def: &'static crate::install::InterfaceDef = match kind.as_str() {
        "shared" => &SHAREDWORKERGLOBALSCOPE,
        "service" => &SERVICEWORKERGLOBALSCOPE,
        _ => &DEDICATEDWORKERGLOBALSCOPE,
    };
    const CONSTS: &[&str] = &["TEMPORARY", "PERSISTENT"];
    let selector: Option<&[&str]> = if def.members.iter().any(|m| matches!(m.kind, MemberKind::Const)) { Some(CONSTS) } else { Some(&[]) };
    // `create_global_scope`, not `create_interface`: this realm's own scope interface is published by
    // NAME whatever the capture says, because the rig captures a window and a DEDICATED worker and so
    // can never record `SharedWorkerGlobalScope` or `ServiceWorkerGlobalScope`. Without
    // it a shared worker's global carried only `WorkerGlobalScope`, and `testharness.js` — which picks
    // its environment by name before `instanceof` — ran every shared-worker WPT file as a DEDICATED
    // one and published its results down a plane the page has no end of.
    // The static half of the two constants was mounted here by hand; `installer::create_interface`
    // places both halves of a two-row name now, so this realm has one writer too. See the window
    // twin's note in `core/html/window.rs` for the ledger collision that found it.
    //
    // THE CREATE MOUNTS THE `[Global]` ROWS, right after it puts the interface on the
    // global -- the order this function's own loop had -- so they are recorded with the interface and a
    // realm that instantiates its template takes them from it, as `Window`'s do. Mounted by hand here,
    // outside the recording, every worker built its own with `build_fn`: its first realm kept them.
    let rows: Vec<&'static MemberDef> = def.members.iter().filter(|m| !matches!(m.kind, MemberKind::Const)).collect();
    let derived = installer::create_global_scope_with_members(scope, global, def, Some((wgs.proto, wgs.ctor)), selector, &rows, &mut ledger);
    installer::put_ledger(scope, ledger);

    // 4. THE CHAIN: globalThis -> <Derived>.prototype -> WorkerGlobalScope.prototype ->
    //    EventTarget.prototype -> Object.prototype.
    // (with the scope interface's brand: the global is an instance of it)
    let protov: v8::Local<v8::Value> = derived.proto.into();
    crate::binding::protos::adopt_as(scope, global, protov);

    // 5. …and only then the sweep. `self`, `location` and the timers were own properties of the global
    //    while the ESM layer ran; every one of them is an inherited `WorkerGlobalScope` row now, and a
    //    real worker's `Object.getOwnPropertyNames(self)` contains none of them.
    if global.get_prototype(scope).map(|p| p == protov).unwrap_or(false) {
        let names: Vec<&str> = WORKERGLOBALSCOPE.members.iter().map(|m| m.name).collect();
        strip_own_copies(scope, global, &names);
        strip_own_copies(scope, global, &["addEventListener", "removeEventListener", "dispatchEvent"]);
    }

    // 6. Chrome gates `SharedArrayBuffer` behind cross-origin isolation (COOP+COEP) and the default
    //    worker does not expose it; V8 exposes it unconditionally, which is a tell. (This was the last
    //    statement of `generated_worker_shells.js`.) HELD, not deleted: an isolated
    //    document's blob: worker gets it back from `isolation_policy::apply_worker_isolation`.
    let _ = global;
    crate::core::html::isolation_policy::hold_worker_sab(scope);
}

// `scope_proto` — "`WorkerGlobalScope.prototype` in this realm, the holder every
// `WindowOrWorkerGlobalScope` member belongs on" — lived here, with `install::live::rows::
// worker_global_scope_proto` as its only caller. Both are deleted.
//
// THEY WERE THE PRE-W6.2 MECHANISM: a lane that wanted to mount a `fetch`/`atob`/`performance` body
// in the worker asked for the holder and defined the member itself, over the top of whatever the
// registry shell had already put there. W6.2 made `WorkerGlobalScope` a table in THIS file, so the
// rows name their bodies and `install/installer.rs` picks the holder from the row -- one writer, no
// holder lookup, no install order (invariants I2, I8). The pair had been unreferenced ever since.
// Nothing regressed with them: `worker_surface` is 294/294 interfaces, 3127 members, MISSING-GLOB 0
// before and after, which is the measurement that would see a member land on the wrong holder.
