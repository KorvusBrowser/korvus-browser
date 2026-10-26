//! `modules/base64.rs` — the `btoa` and `atob` globals.
//!
//! Were a `for` loop in `core/korvus-runtime/src/js/shared-globals.js` building two JS closures behind
//! `op_nativize` masks (two of the call sites invariant I3 deletes). They are native functions now, and
//! the ENCODING ITSELF did not move: `op_btoa` / `op_atob` in `korvus-runtime` already implement the
//! Latin1 check and the infra spec's forgiving-base64 decode with Chrome's exact sentences. What this
//! file adds is the WebIDL wrapper around them.
//!
//! MEASURED on `google-chrome --headless=new` 150.0.7871.46 with
//! `tests/fixtures/base64-console-probe.js`:
//!
//! * both are `{writable, enumerable, configurable}` own data properties of the global holding a
//!   `[native code]` function of `length` 1 and no own tell.
//! * `btoa.call({}, "hi")` is `TypeError: Illegal invocation` — UNPREFIXED, unlike the arity message.
//!   The JS closures had no receiver check at all.
//! * `btoa()` / `atob()` are `TypeError: Failed to execute 'btoa' on 'Window': 1 argument required,
//!   but only 0 present.` — and `'WorkerGlobalScope'` in a worker, which the JS hardcoded as 'Window'
//!   in both realms (measured over a loopback page that posts this probe into a real dedicated
//!   worker). The two ops take the realm's name now for the same reason.
//! * `btoa(123)` is `"MTIz"` (ToString) and `atob(" aGk= ")` is `"hi"` (whitespace stripped from
//!   anywhere) — both already right, and asserted so they stay right.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, global_get, illegal_invocation, str_val, throw_type_error};
use crate::core::xmlhttprequest::xml_http_request::deno_op;

/// The realm's own name, as it appears in every WebIDL message Chrome produces here.
pub(crate) fn realm_name(scope: &mut v8::PinScope) -> &'static str {
    if global_get(scope, "window").map(|v| v.is_object()).unwrap_or(false) {
        "Window"
    } else {
        "WorkerGlobalScope"
    }
}

/// `btoa(DOMString data)` / `atob(DOMString data)` — one body, with the op name bound as data.
pub(crate) fn cb_base64(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // The realm's global OR a frame's window (`atob` inside an iframe was `Illegal
    // invocation`, because a frame's window is not the realm's global here; Chrome answers).
    if !crate::core::html::window_globals::is_global_receiver(scope, args.this().into()) {
        illegal_invocation(scope);
        return;
    }
    let iface = realm_name(scope);
    if args.length() < 1 {
        throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': 1 argument required, but only 0 present."));
        return;
    }
    // `DOMString`: ToString, so a number encodes its decimal text and a Symbol throws.
    let arg = args.get(0);
    if arg.is_symbol() {
        throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': Cannot convert a Symbol value to a string"));
        return;
    }
    let Some(s) = arg.to_string(scope) else { return };
    let op = if member == "btoa" { "op_btoa" } else { "op_atob" };
    let Some(f) = deno_op(scope, op) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let realm = str_val(scope, iface);
    if let Some(r) = f.call(scope, undef, &[s.into(), realm]) {
        rv.set(r);
    }
}

/// Install `globalThis.btoa` and `globalThis.atob`.
pub fn install_base64<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for name in ["btoa", "atob"] {
        let data = str_val(scope, name);
        let f = build_fn_data(scope, cb_base64, name, 1, data);
        // The body does its own WebIDL argument check, naming the realm's own interface. The
        // `generated_order::WINQ` pass must not wrap it in a second one that reports 'Window' in a
        // worker; that pass is deleted, and this stays as the record that the body counts.
        crate::binding::protos::stamp_arity_guarded(scope, f);
        let Some(key) = v8::String::new(scope, name) else { continue };
        let mut pd = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
        pd.set_enumerable(true);
        pd.set_configurable(true);
        global.define_property(scope, key.into(), &pd);
    }
}
