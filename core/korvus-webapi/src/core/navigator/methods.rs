//! `core/navigator/methods.rs` -- the sixteen `Navigator` METHODS.
//!
//! WHAT THIS REPLACES: the `NAV.<method> = method(...)` half of `core/korvus-runtime/src/js/
//! nav-glue.js`, body for body -- the same argument counts, the same TypeError and DOMException
//! wording, the same promise shapes, the same protocol-handler safelist. Each was a JS function
//! wrapped in `op_nativize`; here they are native `v8::Function`s, so `[native code]` is a fact of
//! construction rather than a mask (invariant I3).
//!
//! NO RECEIVER CHECK, and that is a decision rather than an omission. None of the JS bodies looked
//! at `this`, so `Navigator.prototype.sendBeacon.call({}, "/x")` answers `true` today, `javaEnabled`
//! answers `false`, and the accessors answer their values -- measured on this tree, 2026-09-11.
//! Headless Chrome 150.0.7871.46 on this machine throws `TypeError: Illegal invocation` for all
//! four (same probe, same day). Closing that gap is a real fidelity fix and it is a BEHAVIOUR
//! change; this unit's contract is to move where each body is DEFINED without changing what it
//! answers, so the gap is recorded in the W4.a report and not closed here.

use deno_core::v8;

use crate::binding::protos::{
    build_fn_data, get_prop, rejected_promise, rejected_type_error, resolved_promise,
    str_val, throw_type_error,
};
use crate::install::live::method_row;
use crate::install::MemberDef;

use super::navigator::dom_error;

/// The WebIDL "not enough arguments" TypeError, worded as Chrome words it (`need()` in the JS).
fn need(scope: &mut v8::PinScope, member: &str, n: usize, got: usize) {
    let plural = if n == 1 { "argument" } else { "arguments" };
    throw_type_error(
        scope,
        &format!("Failed to execute '{member}' on 'Navigator': {n} {plural} required, but only {got} present."),
    );
}

/// `navigator.getGamepads()` -- four empty slots, which is what a Chrome window with no gamepad
/// connected reports (not an empty array).
fn cb_get_gamepads(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let arr = v8::Array::new(scope, 4);
    for i in 0..4 {
        let n: v8::Local<v8::Value> = v8::null(scope).into();
        arr.set_index(scope, i, n);
    }
    rv.set(arr.into());
}

fn cb_java_enabled(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(v8::Boolean::new(scope, false).into());
}

/// `navigator.vibrate(pattern)` -- a desktop has no vibration motor, so every call answers `false`
/// after the WebIDL arity check, which is what Chrome on this hardware answers.
fn cb_vibrate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        need(scope, "vibrate", 1, args.length() as usize);
        return;
    }
    rv.set(v8::Boolean::new(scope, false).into());
}

/// `navigator.sendBeacon(url, data)` (Blink's `NavigatorBeacon::SendBeaconImpl`, Chrome 150 measured): both arguments
/// converted first -- the URL as a USVString, the data as `(ReadableStream or XMLHttpRequestBodyInit)?`, a platform
/// object of one of those as itself and anything else its string -- then the URL completed against the document's
/// base (an ill-formed one, or one not over HTTP(S), is a TypeError in Blink's words), a ReadableStream refused, and
/// the request queued as a keepalive POST (`fetch_global::beacon`): `false` when the body would take the in-flight
/// keepalive bytes past 64 KiB. Nothing of the page is read: this resolved the URL through the page's `baseURI`,
/// `location.href` and `URL.prototype.href` and read a Blob's `type` through its getter, and a non-HTTP URL or an
/// ill-formed one answered `true` / `false` instead of throwing.
fn cb_send_beacon(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        need(scope, "sendBeacon", 1, args.length() as usize);
        return;
    }
    let Some(raw) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let given = args.get(1);
    let stream = crate::core::streams::readable_stream::as_stream(scope, given).is_some();
    let platform = stream || ["Blob", "FormData", "URLSearchParams"].iter().any(|b| crate::install::webidl::has_brand(scope, given, b));
    let data: v8::Local<v8::Value> = if given.is_null_or_undefined() || platform || given.is_array_buffer() || given.is_array_buffer_view() {
        given
    } else {
        let Some(s) = given.to_string(scope) else { return };
        s.into()
    };
    let base = crate::core::html::browsing_context::realm_base_url(scope);
    let url = match url::Url::parse(&base) {
        Ok(b) => url::Url::options().base_url(Some(&b)).parse(&raw).ok(),
        Err(_) => url::Url::parse(&raw).ok(),
    };
    let Some(url) = url else {
        throw_type_error(scope, "Failed to execute 'sendBeacon' on 'Navigator': The URL argument is ill-formed or unsupported.");
        return;
    };
    if !matches!(url.scheme(), "http" | "https") {
        throw_type_error(scope, "Failed to execute 'sendBeacon' on 'Navigator': Beacons are only supported over HTTP(S).");
        return;
    }
    if stream {
        throw_type_error(scope, "Failed to execute 'sendBeacon' on 'Navigator': sendBeacon cannot have a ReadableStream body.");
        return;
    }
    // The body's CORS-safelisted-ness picks the mode (Blink's `BeaconBlob::Serialize`): every form but a Blob of a
    // type that is not safelisted goes `no-cors`.
    let mode = match crate::core::fileapi::blob::blob_type(scope, data) {
        Some(ty) if !ty.is_empty() && !crate::modules::fetch::fetch_global::no_cors_safelisted("content-type", &ty) => "cors",
        _ => "no-cors",
    };
    let queued = crate::modules::fetch::fetch_global::beacon(scope, url.as_str(), data, mode);
    rv.set(v8::Boolean::new(scope, queued).into());
}

/// `Promise.resolve(undefined)` -- `setAppBadge()` and `clearAppBadge()`, which have no badge to set
/// outside an installed PWA and resolve either way.
fn cb_resolved_undefined(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let p = resolved_promise(scope, u);
    rv.set(p);
}

fn cb_get_installed_related_apps(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let arr = v8::Array::new(scope, 0);
    let p = resolved_promise(scope, arr.into());
    rv.set(p);
}

/// `navigator.requestMIDIAccess(options)` -- MIDI asks first (`midi` is `prompt`, with or without
/// `sysex`), and Brave 151 leaves the promise pending behind that prompt (measured
/// 2026-10-01); this rejected `NotAllowedError` at once, which no browser a person sits at does. Its `MIDIOptions`
/// are converted first, as Blink's binding converts them (korvus read none).
fn cb_request_midi_access(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Err(e) = crate::install::ctor_dicts::convert_method_args_caught(scope, "Navigator::requestMIDIAccess", &args, "Failed to execute 'requestMIDIAccess' on 'Navigator': ") {
        rv.set(rejected_promise(scope, e));
        return;
    }
    let p = crate::binding::protos::pending_promise(scope);
    rv.set(p);
}

/// `navigator.requestMediaKeySystemAccess(keySystem, supportedConfigurations)`, Blink 150's steps
/// (NavigatorRequestMediaKeySystemAccess; 2026-10-11, measured): the arguments converted first -- each
/// configuration a `MediaKeySystemConfiguration`, a refusal a rejection -- then an empty key system ("The keySystem
/// parameter is empty.") and an empty list ("The supportedConfigurations parameter is empty.") refused at once; support
/// is determined asynchronously, and this build ships no CDM, so the answer is NotSupportedError a task later. korvus
/// read no configuration, refused an empty list in a sentence of its own, and rejected at once.
fn cb_request_media_key_system_access(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 2 {
        need(scope, "requestMediaKeySystemAccess", 2, args.length() as usize);
        return;
    }
    if let Err(e) = crate::install::ctor_dicts::convert_method_args_caught(scope, "Navigator::requestMediaKeySystemAccess", &args, "Failed to execute 'requestMediaKeySystemAccess' on 'Navigator': ") {
        rv.set(rejected_promise(scope, e));
        return;
    }
    let key_system = args.get(0).to_rust_string_lossy(scope);
    let empty_list = v8::Local::<v8::Object>::try_from(args.get(1))
        .ok()
        .and_then(|o| get_prop(scope, o, "length"))
        .and_then(|v| v.number_value(scope))
        .is_some_and(|n| n == 0.0);
    let refusal = if key_system.is_empty() {
        Some("Failed to execute 'requestMediaKeySystemAccess' on 'Navigator': The keySystem parameter is empty.")
    } else if empty_list {
        Some("Failed to execute 'requestMediaKeySystemAccess' on 'Navigator': The supportedConfigurations parameter is empty.")
    } else {
        None
    };
    if let Some(msg) = refusal {
        rv.set(rejected_type_error(scope, msg));
        return;
    }
    let Some(e) = dom_error(scope, "Unsupported keySystem or supportedConfigurations.", "NotSupportedError") else { return };
    let p = crate::binding::protos::settle_later(scope, e, true, crate::binding::protos::Settle::Task(0.0));
    rv.set(p);
}

/// The microtask the legacy `getUserMedia` queues: call the page's error callback, swallowing
/// anything it throws (the JS body's `try { error(err); } catch (e) {}`). `data` is
/// `[errorCallback, domException]`.
fn cb_invoke_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(pair) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(f) = pair.get_index(scope, 0).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let Some(err) = pair.get_index(scope, 1) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    v8::tc_scope!(let tc, scope);
    f.call(tc, undef, &[err]);
    tc.reset();
}

/// The legacy callback-style `getUserMedia` / `webkitGetUserMedia`: three required arguments, then
/// the error callback is invoked in a microtask with a `NotAllowedError` and the call itself returns
/// `undefined`. `data` is the member name, so one callback serves both rows and each message names
/// the member the page actually called.
fn cb_legacy_get_user_media(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if args.length() < 3 {
        need(scope, &member, 3, args.length() as usize);
        return;
    }
    let error = args.get(2);
    if !error.is_function() {
        return;
    }
    let Some(e) = dom_error(scope, "Permission denied", "NotAllowedError") else { return };
    let pair = v8::Array::new(scope, 2);
    pair.set_index(scope, 0, error);
    pair.set_index(scope, 1, e);
    let cb = build_fn_data(scope, cb_invoke_error, "", 0, pair.into());
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let p = resolved_promise(scope, u);
    crate::binding::protos::promise_then(scope, p, cb, None);
}

// ── registerProtocolHandler / unregisterProtocolHandler ──────────────────────────────────────────

/// The HTML standard's protocol-handler safelist, verbatim from `nav-glue.js` (which took it from
/// the spec's "normalize protocol handler parameters" table).
const SAFELIST: [&str; 43] = [
    "bitcoin", "bitcoincash", "cabal", "dat", "did", "doi", "dweb", "ed2k", "ethereum", "facetime",
    "feed", "fish", "ftp", "ftps", "geo", "gemini", "git", "gopher", "hyper", "im", "ipfs", "ipns",
    "irc", "ircs", "magnet", "mailto", "matrix", "mms", "news", "nntp", "openpgp4fpr", "sftp", "sip",
    "sms", "smsto", "ssb", "ssh", "svn", "tel", "urn", "webcal", "wtai", "xmpp",
];

/// `normalizeHandler` from the JS: the arity check, the scheme safelist (or a `web+<lowercase>`
/// custom scheme), and the `%s` requirement -- each failure with the message Chrome prints. The
/// normalised URL the JS returned was discarded by both callers; this engine registers no handler.
fn normalize_handler(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) {
    let argc = args.length() as usize;
    if argc < 2 {
        need(scope, member, 2, argc);
        return;
    }
    let raw = args.get(0).to_rust_string_lossy(scope);
    let s = raw.to_lowercase();
    let web_plus = s.starts_with("web+") && s.len() > 4 && s[4..].bytes().all(|b| b.is_ascii_lowercase());
    if !web_plus && !SAFELIST.contains(&s.as_str()) {
        let msg = format!(
            "Failed to execute '{member}' on 'Navigator': The scheme '{raw}' doesn't belong to the \
             schemes allowed for a protocol handler; see \
             https://html.spec.whatwg.org/multipage/system-state.html#normalize-protocol-handler-parameters."
        );
        if let Some(e) = dom_error(scope, &msg, "SecurityError") {
            scope.throw_exception(e);
        }
        return;
    }
    let u = args.get(1).to_rust_string_lossy(scope);
    if !u.contains("%s") {
        let msg = format!("Failed to execute '{member}' on 'Navigator': The url provided ('{u}') does not contain '%s'.");
        if let Some(e) = dom_error(scope, &msg, "SyntaxError") {
            scope.throw_exception(e);
        }
    }
}

fn cb_register_protocol_handler(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    normalize_handler(scope, &args, "registerProtocolHandler");
}

fn cb_unregister_protocol_handler(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    normalize_handler(scope, &args, "unregisterProtocolHandler");
}

// ── the table bodies ─────────────────────────────────────────────────────────────────────────────

/// The table body for every `Navigator` method whose callback needs no bound data.
pub fn install_method(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getGamepads" => method_row(scope, proto, member, cb_get_gamepads),
        "javaEnabled" => method_row(scope, proto, member, cb_java_enabled),
        "sendBeacon" => method_row(scope, proto, member, cb_send_beacon),
        "vibrate" => method_row(scope, proto, member, cb_vibrate),
        "clearAppBadge" | "setAppBadge" => method_row(scope, proto, member, cb_resolved_undefined),
        "getBattery" => method_row(scope, proto, member, super::battery_manager::cb_get_battery),
        "requestMIDIAccess" => method_row(scope, proto, member, cb_request_midi_access),
        "requestMediaKeySystemAccess" => method_row(scope, proto, member, cb_request_media_key_system_access),
        "getInstalledRelatedApps" => method_row(scope, proto, member, cb_get_installed_related_apps),
        "registerProtocolHandler" => method_row(scope, proto, member, cb_register_protocol_handler),
        "unregisterProtocolHandler" => method_row(scope, proto, member, cb_unregister_protocol_handler),
        other => debug_assert!(false, "install_method: unexpected Navigator member {other}"),
    }
}

/// The table body for the two legacy `getUserMedia` aliases, which share one callback and differ
/// only in the member name their TypeError prints.
pub fn install_legacy_get_user_media(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let data = str_val(scope, member.name);
    let f = build_fn_data(scope, cb_legacy_get_user_media, member.name, member.length as i32, data);
    crate::install::kits::define_value(scope, proto, member, f.into(), true);
}
