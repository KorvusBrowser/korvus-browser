//! `core/navigator/context_apis.rs` -- APIs whose answer depends on the DOCUMENT they run in: focus,
//! top-levelness, visibility (the Chrome oracle ratchet).
//!
//! - `Clipboard`: Blink's `ClipboardPromise` refuses an unfocused document first, NotAllowedError
//!   "Document is not focused." (an iframe nothing focused is one); a focused one reads behind the
//!   `clipboard-read` prompt (pending) and writes (`clipboard-write` is granted to a
//!   focused document). C5 (ABI 3 step 12): a loaded plugin may answer `readText` through
//!   `plugin_host::on_clipboard_read`; without one the read stays pending, as before.
//! - `MediaDevices.setCaptureHandleConfig`: InvalidStateError "Can only be called from the top-level
//!   document." in a frame (`media_devices.cc`), nothing at the top.
//! - `WakeLock.request(type)`: a `WakeLockSentinel` of that type while the page is visible; `release()`
//!   resolves and flips `released`.

use deno_core::v8;

use crate::binding::protos::{make_dom_exc, str_val, throw_dom, throw_type_error};
use crate::install::MemberDef;

fn key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_ctx_{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn promise_with<'s>(scope: &mut v8::PinScope<'s, '_>, ok: Option<v8::Local<v8::Value>>, err: Option<(&str, &str)>) -> Option<v8::Local<'s, v8::Value>> {
    let p = v8::PromiseResolver::new(scope)?;
    if let Some((name, msg)) = err {
        let e = make_dom_exc(scope, msg, name)?;
        p.reject(scope, e);
    } else if let Some(v) = ok {
        p.resolve(scope, v);
    }
    Some(p.get_promise(scope).into())
}

fn cb_clipboard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let method = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if !crate::binding::protos::implements(scope, args.this().into(), "Clipboard") {
        let p = crate::binding::protos::rejected_type_error(scope, &format!("Failed to execute '{method}' on 'Clipboard': Illegal invocation"));
        rv.set(p);
        return;
    }
    let out = if !crate::core::html::browsing_context::realm_document_focused(scope) {
        promise_with(scope, None, Some(("NotAllowedError", &format!("Failed to execute '{method}' on 'Clipboard': Document is not focused."))))
    } else if method == "readText" {
        // C5 (ABI 3 step 12): a loaded plugin may answer the PASTE. `plugin_host::on_clipboard_read`
        // returns the text the page reads; OFF by default -- with no plugin, or one that passes (NULL),
        // it returns None and the read stays behind the `clipboard-read` prompt that nothing answers,
        // exactly as before (I9/J2).
        match plugin_host::on_clipboard_read() {
            Some(text) => {
                // Resolve str_val first: both it and promise_with take `&mut scope`, so they cannot be nested.
                let v = str_val(scope, &text);
                promise_with(scope, Some(v), None)
            }
            None => Some(crate::binding::protos::pending_promise(scope)),
        }
    } else if method == "read" {
        // `read()` resolves with a `[ClipboardItem]`, not a bare string, so the plugin's text has to be
        // wrapped in a text/plain item. Left pending as before; the text paste rides `readText` above.
        // TODO(1021-verify): resolve read() with `[new ClipboardItem({'text/plain': <blob of on_clipboard_read()>})]`.
        Some(crate::binding::protos::pending_promise(scope))
    } else {
        // `write` / `writeText` -- a page COPY. C5: the copied text should reach the clipboard the page
        // loop owns -- the buffer `host->clipboard_write` feeds (`plugin_host::take_clipboard_writes`,
        // drained in korvus-cdp's message loop). The promise resolves (as before), but the text goes
        // nowhere yet: there is no page-side entry to that buffer.
        // GAP: korvus-plugin-host exposes no public page-side writer for the clipboard-write queue
        //      (`push_clipboard_write` is private and there is no engine clipboard store webapi can
        //      reach), so a page copy cannot feed `take_clipboard_writes` from here. Needs e.g. a
        //      `pub fn page_clipboard_write(text: &str)` in korvus-plugin-host (outside this crate).
        // TODO(1021-verify): once that exists, enqueue `args.get(0)` for `writeText` here (and the
        //      text/plain item of the `ClipboardItem` sequence for `write`).
        let u = v8::undefined(scope).into();
        promise_with(scope, Some(u), None)
    };
    if let Some(p) = out {
        rv.set(p);
    }
}

fn cb_capture_handle(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !crate::binding::protos::implements(scope, args.this().into(), "MediaDevices") {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if !crate::core::html::browsing_context::realm_is_top_document(scope) {
        throw_dom(scope, "Failed to execute 'setCaptureHandleConfig' on 'MediaDevices': Can only be called from the top-level document.", "InvalidStateError");
    }
}

fn cb_wake_lock_request(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::binding::protos::implements(scope, args.this().into(), "WakeLock") {
        let p = crate::binding::protos::rejected_type_error(scope, "Illegal invocation");
        rv.set(p);
        return;
    }
    let ty = if args.get(0).is_undefined() { "screen".to_string() } else { args.get(0).to_rust_string_lossy(scope) };
    if ty != "screen" {
        let p = crate::binding::protos::rejected_type_error(scope, &format!("Failed to execute 'request' on 'WakeLock': The provided value '{ty}' is not a valid enum value of type WakeLockType."));
        rv.set(p);
        return;
    }
    let Some(ctor) = crate::binding::protos::iface_object(scope, "WakeLockSentinel").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(proto) = crate::binding::protos::get_prop(scope, ctor, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let s = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, s);
    crate::install::webidl::brand(scope, s, "WakeLockSentinel");
    let k = key(scope, "type");
    let tv = str_val(scope, &ty);
    s.set_private(scope, k, tv);
    if let Some(p) = promise_with(scope, Some(s.into()), None) {
        rv.set(p);
    }
}

fn cb_sentinel(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    if !crate::install::webidl::has_brand(scope, this.into(), "WakeLockSentinel") {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let rk = key(scope, "released");
    let released = this.get_private(scope, rk).is_some_and(|v| v.is_true());
    match member.as_str() {
        "type" => {
            let k = key(scope, "type");
            let v = this.get_private(scope, k).unwrap_or_else(|| str_val(scope, "screen"));
            rv.set(v);
        }
        "released" => rv.set_bool(released),
        _ => {
            if !released {
                let t = v8::Boolean::new(scope, true).into();
                this.set_private(scope, rk, t);
                crate::core::html::browsing_context::fire_lifecycle(scope, this.into(), "release", false, None);
            }
            let u = v8::undefined(scope).into();
            if let Some(p) = promise_with(scope, Some(u), None) {
                rv.set(p);
            }
        }
    }
}

/// The table body of the rows this file implements.
pub fn install_context_apis(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = member.label.split("::").next().unwrap_or("");
    let name = str_val(scope, member.name);
    match iface {
        "Clipboard" => crate::core::dom::shared::row_method_data(scope, proto, member, cb_clipboard, name),
        "MediaDevices" => crate::core::dom::shared::row_method_data(scope, proto, member, cb_capture_handle, name),
        "WakeLock" => crate::core::dom::shared::row_method_data(scope, proto, member, cb_wake_lock_request, name),
        _ if member.name == "release" => crate::core::dom::shared::row_method_data(scope, proto, member, cb_sentinel, name),
        _ => crate::core::dom::shared::row_getter_data(scope, proto, member, cb_sentinel, name),
    }
}
