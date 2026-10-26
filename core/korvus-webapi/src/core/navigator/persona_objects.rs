//! `core/navigator/persona_objects.rs` -- the three Navigator members that hand back an object built
//! from the PERSONA or from a recorded browser answer.
//!
//! WHAT THIS REPLACES: `nav-glue.js`'s `buildUAData` / `buildPermissions` / `buildMediaDevices`, moved BODY
//! FOR BODY -- the same own properties in the same order, the same messages, the same promise shapes, and
//! the same re-pointing onto the real interface prototype. (`buildBattery` became `battery_manager.rs`,
//! A real `BatteryManager` table and one promise per navigator.)
//!
//! THE VALUES ARE THE PERSONA'S, read live from the kernel `Context`'s `spec::PersonaHandle` service
//! ([`super::navigator::client_hints`]). Nothing here hardcodes a brand, a platform or a version:
//! the JS used to read them through `op_persona_hints`, which is the same `client_hints()` call one layer
//! down. What IS written down is the recorded browser answer no persona field carries -- the battery
//! of a desktop on mains power (now in `battery_manager.rs`), a Chromium structural constant identical
//! across builds. Inventing a persona field for it would be worse: it would look captured and not be.
//! The device kinds `enumerateDevices()` lists and the constraints `getSupportedConstraints()`
//! reports were written down the same way and were not constants: the kinds are a
//! fact about the machine and the constraint set grows with the build, so both come from the bundle
//! (`nav.mediaDevices`), with the old lists kept only for a runtime that loads no persona.
//!
//! ONE DELIBERATE DIFFERENCE. The JS built these as object literals, so their methods were plain JS
//! functions whose `toString()` showed their SOURCE: `navigator.userAgentData.getHighEntropyValues`
//! did NOT read `[native code]`. `op_nativize` was applied to the Navigator members, never to the
//! methods of the objects they hand back. Asked directly, headless Chrome 150.0.7871.46 on this
//! machine (loopback, 2026-09-11) reports `[native code]` for `getHighEntropyValues`,
//! `mediaDevices.enumerateDevices` and `permissions.query`. Built here they are native by
//! construction, which CLOSES a first-tier tell rather than opening one.
//!
//! ONE MEASURED GAP LEFT OPEN, recorded rather than quietly kept: Chrome answers
//! `Object.getOwnPropertyNames(navigator.mediaDevices)` as `[]` -- every member is on the interface
//! prototype there, where this carries them as own properties (`userAgentData` and `permissions` own
//! nothing; `BatteryManager` has its own table). Closing it means a real `MediaDevices`
//! table out of the generated registry.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, get_prop, rejected_promise, rejected_type_error, str_val, throw_type_error};
use crate::binding::protos::cb_noop;
use crate::install::live::parent_of;

use super::navigator::{cb_false, client_hints, dom_error, own_prop};

/// A new instance of `iface`, made from its interface's template on the realm's `iface.prototype`,
/// so `navigator.permissions instanceof Permissions` is true and
/// `String(navigator.mediaDevices)` reads `[object MediaDevices]`. The JS `reparent` helper's guard
/// stays: a realm that does not expose the interface gets an object left on `Object.prototype`.
fn mint_persona<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Object> {
    match parent_of(scope, iface) {
        Some((proto, _)) => crate::binding::protos::mint_on(scope, proto.into()),
        None => v8::Object::new(scope),
    }
}

/// `{ brand, version }` per brand -- a new plain object each time, as Blink converts a `NavigatorUABrandVersion`
/// dictionary (its members in their sorted order) on every conversion.
fn brand_items<'s>(scope: &mut v8::PinScope<'s, '_>, brands: &[spec::Brand]) -> Vec<v8::Local<'s, v8::Value>> {
    brands
        .iter()
        .map(|b| {
            let o = v8::Object::new(scope);
            let brand = str_val(scope, &b.brand);
            own_prop(scope, o, "brand", brand);
            let version = str_val(scope, &b.version);
            own_prop(scope, o, "version", version);
            o.into()
        })
        .collect()
}

/// `[{ brand, version }, …]` -- a fresh plain-object array per read: the `sequence<NavigatorUABrandVersion>` of a
/// `UADataValues` dictionary (`getHighEntropyValues`) and of `toJSON()`, neither of them frozen in Chrome.
fn brand_list<'s>(scope: &mut v8::PinScope<'s, '_>, brands: &[spec::Brand]) -> v8::Local<'s, v8::Value> {
    let items = brand_items(scope, brands);
    v8::Array::new_with_elements(scope, &items).into()
}

/// The three no-argument listener members every one of these objects carried, in the JS's order.
fn add_event_listener_trio(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) {
    for n in ["addEventListener", "removeEventListener"] {
        let f = build_fn(scope, cb_noop, n, 0);
        own_prop(scope, o, n, f.into());
    }
    let f = build_fn(scope, cb_false, "dispatchEvent", 0);
    own_prop(scope, o, "dispatchEvent", f.into());
}

// ── navigator.userAgentData ──────────────────────────────────────────────────────────────────────

/// The ten keys `getHighEntropyValues` can answer -- the JS `full` object, key for key.
const HIGH_ENTROPY: [&str; 11] = [
    "architecture", "bitness", "model", "platformVersion", "uaFullVersion", "wow64",
    "fullVersionList", "brands", "mobile", "platform", "formFactors",
];

/// One `full[key]`, from the persona's client hints.
fn high_entropy_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ch: &spec::ClientHints,
    key: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    Some(match key {
        "architecture" => str_val(scope, &ch.architecture),
        "bitness" => str_val(scope, &ch.bitness),
        "model" => str_val(scope, &ch.model),
        "platformVersion" => str_val(scope, &ch.platform_version),
        "uaFullVersion" => str_val(scope, &ch.ua_full_version),
        "wow64" => v8::Boolean::new(scope, ch.wow64).into(),
        "fullVersionList" => brand_list(scope, &ch.full_version_list),
        "brands" => brand_list(scope, &ch.brands),
        "mobile" => v8::Boolean::new(scope, ch.mobile).into(),
        "platform" => str_val(scope, &ch.platform),
        "formFactors" => {
            let items: Vec<v8::Local<v8::Value>> = ch.form_factors.iter().map(|f| str_val(scope, f)).collect();
            v8::Array::new_with_elements(scope, &items).into()
        }
        _ => return None,
    })
}

/// `forEach`'s callback: append the first argument to the array bound as `data`.
fn cb_collect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(acc) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let n = acc.length();
    acc.set_index(scope, n, args.get(0));
}

/// The JS `(hints || []).forEach(h => …)`, faithfully: a falsy argument means no hints, an array is
/// walked, and anything else truthy is asked for its own `forEach` -- which is how a caller passing
/// `{}` or a number gets a TypeError instead of a silent empty answer.
///
/// Ruling: the wording of THAT TypeError is this file's (`hints.forEach is not a function`) rather
/// than V8's own `… is not a function` phrasing for the offending expression, which no Rust caller
/// can reproduce without evaluating the source text. Cost if wrong: the message of an error raised
/// only by `getHighEntropyValues(<non-iterable truthy>)` differs; the error, its type and the fact
/// that it is thrown SYNCHRONOUSLY out of the call (not a rejection) are unchanged.
///
/// Returns `None` when a TypeError is pending in `scope` -- the caller must return immediately.
fn requested_hints(scope: &mut v8::PinScope, hints: v8::Local<v8::Value>) -> Option<Vec<String>> {
    let read = |scope: &mut v8::PinScope, arr: v8::Local<v8::Array>| {
        let mut out = Vec::with_capacity(arr.length() as usize);
        for i in 0..arr.length() {
            if let Some(v) = arr.get_index(scope, i) {
                out.push(v.to_rust_string_lossy(scope));
            }
        }
        out
    };
    if !hints.boolean_value(scope) {
        return Some(Vec::new());
    }
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(hints) {
        return Some(read(scope, arr));
    }
    let obj = hints.to_object(scope)?;
    let fe = get_prop(scope, obj, "forEach")
        .filter(|v| v.is_function())
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    let Some(fe) = fe else {
        throw_type_error(scope, "hints.forEach is not a function");
        return None;
    };
    let acc = v8::Array::new(scope, 0);
    let cb = build_fn_data(scope, cb_collect, "", 1, acc.into());
    fe.call(scope, hints, &[cb.into()])?;
    Some(read(scope, acc))
}

/// `navigator.userAgentData.getHighEntropyValues(hints)`.
fn cb_get_high_entropy_values(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'getHighEntropyValues' on 'NavigatorUAData': 1 argument required, but only 0 present.",
        );
        rv.set(p);
        return;
    }
    let Some(wanted) = requested_hints(scope, args.get(0)) else { return };
    let ch = client_hints(scope);
    // The result is a WebIDL dictionary (`UADataValues`), whose members a JS object carries in
    // LEXICOGRAPHIC order -- Chrome answers `architecture, bitness, brands, formFactors, ...`; this
    // put the three low-entropy keys first.
    let mut keys: Vec<&str> = vec!["brands", "mobile", "platform"];
    for h in &wanted {
        if let Some(k) = HIGH_ENTROPY.iter().find(|k| **k == h.as_str()) {
            if !keys.contains(k) {
                keys.push(k);
            }
        }
    }
    keys.sort_unstable();
    let out = v8::Object::new(scope);
    for k in keys {
        if let Some(v) = high_entropy_value(scope, &ch, k) {
            own_prop(scope, out, k, v);
        }
    }
    // settled from a microtask, as Chrome 150's is (`binding::protos::Settle`)
    let p = crate::binding::protos::settle_later(scope, out.into(), false, crate::binding::protos::Settle::Microtask);
    rv.set(p);
}

/// `navigator.userAgentData.toJSON()` -- the low-entropy three, with `mobile` and `platform` read off
/// the RECEIVER as the JS body did (`this.mobile`), not recomputed.
fn cb_ua_data_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ch = client_hints(scope);
    let out = v8::Object::new(scope);
    let brands = brand_list(scope, &ch.brands);
    own_prop(scope, out, "brands", brands);
    let this = args.this();
    for key in ["mobile", "platform"] {
        let v = get_prop(scope, this, key).unwrap_or_else(|| v8::undefined(scope).into());
        own_prop(scope, out, key, v);
    }
    rv.set(out.into());
}

/// The `NavigatorUAData` the JS built: `brands`, `mobile`, `platform`, `getHighEntropyValues`,
/// `toJSON`, in that order, re-pointed at `NavigatorUAData.prototype`.
pub(super) fn build_ua_data<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    // Chrome's `navigator.userAgentData` owns nothing: `brands`/`mobile`/`platform` are
    // NavigatorUAData.prototype accessors and the two methods its operations. This object carried all
    // five as own properties over the prototype's stubs -- `Object.getOwnPropertyNames` said so, and
    // the prototype's `brands` getter called on it answered the stub's `[]`.
    let o = mint_persona(scope, "NavigatorUAData");
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    crate::core::dom::element::priv_set(scope, o, P_UA_DATA, t);
    if let Some((proto, _)) = parent_of(scope, "NavigatorUAData") {
        if first_patch(scope, proto) {
            proto_getter(scope, proto, "brands", cb_ua_brands);
            proto_getter(scope, proto, "mobile", cb_ua_mobile);
            proto_getter(scope, proto, "platform", cb_ua_platform);
            let hev = build_fn(scope, cb_get_high_entropy_values, "getHighEntropyValues", 1);
            own_prop(scope, proto, "getHighEntropyValues", hev.into());
            let tj = build_fn(scope, cb_ua_data_to_json, "toJSON", 0);
            own_prop(scope, proto, "toJSON", tj.into());
        }
    }
    o
}

const P_UA_DATA: &str = "korvus_ua_data";

fn ua_receiver(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    if crate::core::css::css_style_declaration::priv_get(scope, args.this(), P_UA_DATA).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}
/// `readonly attribute FrozenArray<NavigatorUABrandVersion> brands`: the getter returns its `HeapVector`, so
/// Chrome 150 hands out a NEW frozen array per read, of new brand objects (C2, measured in both realms).
fn cb_ua_brands(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !ua_receiver(scope, &args) {
        return;
    }
    let ch = client_hints(scope);
    let answer = crate::install::frozen_array::answer(scope, args.this(), "NavigatorUAData", "brands", |_| 0.0, |scope| Some(brand_items(scope, &ch.brands)));
    if let Some(a) = answer {
        rv.set(a);
    }
}
fn cb_ua_mobile(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !ua_receiver(scope, &args) {
        return;
    }
    let ch = client_hints(scope);
    rv.set_bool(ch.mobile);
}
fn cb_ua_platform(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !ua_receiver(scope, &args) {
        return;
    }
    let ch = client_hints(scope);
    let v = str_val(scope, &ch.platform);
    rv.set(v);
}

// ── navigator.permissions ────────────────────────────────────────────────────────────────────────

/// Chrome 150's `PermissionName` enum with, per name, what `PermissionStatus.name` answers -- the
/// browser's INTERNAL permission type, not the name asked for (`camera` is `video_capture`, the three
/// motion sensors are one `sensors`) -- or `Err` with the message `query()` rejects a name it knows
/// but has switched off with. Every name measured on Chrome 150 and Brave 151 headed,
/// 2026-10-01 (c8/repro/async/perms*.html); ~120 other candidate names are not in the enum.
const PERMISSION_NAMES: &[(&str, Result<&str, &str>)] = &[
    ("geolocation", Ok("geolocation")),
    ("notifications", Ok("notifications")),
    ("push", Ok("notifications")),
    ("midi", Ok("midi")),
    ("camera", Ok("video_capture")),
    ("microphone", Ok("audio_capture")),
    ("speaker-selection", Err("The Speaker Selection API is not enabled.")),
    ("background-fetch", Ok("background_fetch")),
    ("background-sync", Ok("background_sync")),
    ("persistent-storage", Ok("durable_storage")),
    ("ambient-light-sensor", Err("GenericSensorExtraClasses flag is not enabled.")),
    ("accelerometer", Ok("sensors")),
    ("gyroscope", Ok("sensors")),
    ("magnetometer", Ok("sensors")),
    ("screen-wake-lock", Ok("screen_wake_lock")),
    ("system-wake-lock", Err("System Wake Lock is not enabled.")),
    ("nfc", Err("Web NFC is not enabled.")),
    ("display-capture", Ok("display_capture")),
    ("clipboard-read", Ok("clipboard_read")),
    ("clipboard-write", Ok("clipboard_write")),
    ("payment-handler", Ok("payment_handler")),
    ("idle-detection", Ok("idle_detection")),
    ("periodic-background-sync", Ok("periodic_background_sync")),
    ("storage-access", Ok("storage-access")),
    ("top-level-storage-access", Ok("top-level-storage-access")),
    ("window-management", Ok("window-management")),
    ("local-fonts", Ok("local_fonts")),
    ("captured-surface-control", Ok("captured-surface-control")),
    ("keyboard-lock", Ok("keyboard-lock")),
    ("pointer-lock", Ok("pointer-lock")),
    ("fullscreen", Ok("fullscreen")),
    ("web-app-installation", Err("The Web App Install API is not enabled.")),
    ("local-network-access", Ok("local-network-access")),
    ("local-network", Ok("local-network")),
    ("loopback-network", Ok("loopback-network")),
    ("geolocation-approximate", Err("Permission API support for approximate geolocation is not enabled.")),
];

/// States the persona bundle does not record, where Chrome 150 and Brave 151 answer alike -- the
/// engine's defaults, not the persona's choices. `idle-detection` is NOT here: Brave
/// denies it and Chrome prompts, so it is a persona value the bundle's next capture must record
/// (`capture/collect.js` now asks it); until then it answers `prompt`.
const ENGINE_PERMISSION_DEFAULTS: &[(&str, &str)] = &[
    ("background-fetch", "granted"),
    ("periodic-background-sync", "denied"),
    ("keyboard-lock", "granted"),
    ("pointer-lock", "granted"),
    ("fullscreen", "denied"),
];

/// The state for a permission name. Only the notification pair is dynamic -- it follows
/// `Notification.permission`, which a page can have changed since this object was built, so it is
/// read per query exactly as the JS `permState` did.
fn permission_state(scope: &mut v8::PinScope, name: v8::Local<v8::Value>) -> String {
    let n = name.to_rust_string_lossy(scope);
    // THE DOCUMENT'S PERMISSIONS POLICY FIRST: a policy-controlled feature the
    // document may not use is `denied`, whatever the user's setting -- Chrome 150, measured: a page
    // served `camera=()` answers camera `denied`, and its cross-origin frames answer every undelegated
    // `'self'` feature `denied`. And in a nested document Chrome answers two names by where the
    // document sits: `notifications`/`push` are `denied` in a cross-origin frame, and `storage-access`
    // is `prompt` in any frame (`tools/realweb/seed/refperm`).
    let ctx = crate::core::html::browsing_context::current_realm_ctx(scope);
    if crate::core::feature_policy::default_of(&n).is_some() && !crate::core::feature_policy::enabled_in(scope, &n, ctx) {
        return "denied".to_string();
    }
    if let Some(id) = ctx {
        if n == "storage-access" {
            return "prompt".to_string();
        }
        if (n == "notifications" || n == "push") && !crate::core::html::browsing_context::frame_same_origin_with_top(scope, id) {
            return "denied".to_string();
        }
    }
    // A remote frame is always cross-site to the top: the same two answers.
    if ctx.is_none() {
        let embedded = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.has::<crate::core::html::remote_frame::FrameEmbedding>()
        };
        if embedded {
            if n == "storage-access" {
                return "prompt".to_string();
            }
            if n == "notifications" || n == "push" {
                return "denied".to_string();
            }
        }
    }
    // A loaded plugin may answer the prompt dynamically (ABI 3 step 12, `plugin_host::on_permission`):
    // a grant or deny is the live user choice the static persona cannot carry, and it wins over the
    // persona default and `Notification.permission` below. OFF by default -- no plugin, or one that
    // defers (-1), returns None and leaves the state unchanged (I9/J2). The structural denials above
    // (the document's permissions policy, a cross-origin frame) still stand: a plugin cannot grant
    // what the policy forbids, as a user prompt cannot.
    // A permission a DevTools client granted (`Browser.grantPermissions`) is the user's choice made ahead of the
    // prompt, as Chrome's `PermissionController` override holds it.
    if let Some(state) = spec::devtools::permission(&n) {
        return state.to_string();
    }
    if let Some(granted) = plugin_host::on_permission(&n) {
        return (if granted { "granted" } else { "denied" }).to_string();
    }
    // The persona's source browser answered these: Brave denies `background-sync` and
    // the motion sensors, Chrome grants them; then the engine's own defaults; everything else is
    // `prompt`.
    if n != "notifications" && n != "push" {
        let ch = client_hints(scope);
        return ch
            .permissions
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.clone())
            .or_else(|| ENGINE_PERMISSION_DEFAULTS.iter().find(|(k, _)| *k == n).map(|(_, v)| v.to_string()))
            .unwrap_or_else(|| "prompt".to_string());
    }
    let p = crate::binding::protos::iface_object(scope, "Notification")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|o| get_prop(scope, o, "permission"))
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    match p.as_str() {
        "denied" => "denied",
        "granted" => "granted",
        _ => "prompt",
    }
    .to_string()
}

/// Reject `query()` with Chrome's TypeError for `msg`.
fn query_type_error(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str) {
    let p = rejected_type_error(scope, &format!("Failed to execute 'query' on 'Permissions': {msg}"));
    rv.set(p);
}

/// `navigator.permissions.query(descriptor)`.
fn cb_permissions_query(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'query' on 'Permissions': 1 argument required, but only 0 present.",
        );
        rv.set(p);
        return;
    }
    let raw = args.get(0);
    // `desc === null || typeof desc !== "object"` -- a function is not an object to WebIDL either.
    let desc = v8::Local::<v8::Object>::try_from(raw).ok().filter(|_| !raw.is_function());
    let Some(desc) = desc else {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'query' on 'Permissions': parameter 1 is not of type 'object'.",
        );
        rv.set(p);
        return;
    };
    let name = get_prop(scope, desc, "name").unwrap_or_else(|| v8::undefined(scope).into());
    if name.is_undefined() {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from \
             'PermissionDescriptor': Required member is undefined.",
        );
        rv.set(p);
        return;
    }
    // The enum, then the per-name rules, in Chrome 150's order and words.
    let asked = name.to_rust_string_lossy(scope);
    let Some((_, internal)) = PERMISSION_NAMES.iter().find(|(k, _)| *k == asked) else {
        query_type_error(scope, &mut rv, &format!("Failed to read the 'name' property from 'PermissionDescriptor': The provided value '{asked}' is not a valid enum value of type PermissionName."));
        return;
    };
    let internal = match internal {
        Ok(i) => *i,
        Err(msg) => {
            query_type_error(scope, &mut rv, msg);
            return;
        }
    };
    let member_true = |scope: &mut v8::PinScope, key: &str| get_prop(scope, desc, key).is_some_and(|v| v.boolean_value(scope));
    match asked.as_str() {
        "push" if !member_true(scope, "userVisibleOnly") => {
            let Some(e) = super::navigator::dom_error(
                scope,
                "Failed to execute 'query' on 'Permissions': Push Permission without userVisibleOnly:true isn't supported yet.",
                "NotSupportedError",
            ) else {
                return;
            };
            let p = rejected_promise(scope, e);
            rv.set(p);
            return;
        }
        "fullscreen" if !member_true(scope, "allowWithoutGesture") => {
            query_type_error(scope, &mut rv, "Fullscreen Permission only supports allowWithoutGesture:true.");
            return;
        }
        "top-level-storage-access" => {
            let origin = get_prop(scope, desc, "requestedOrigin").filter(|v| !v.is_undefined()).map(|v| v.to_rust_string_lossy(scope));
            if !origin.is_some_and(|o| url::Url::parse(&o).is_ok_and(|u| u.origin().is_tuple())) {
                query_type_error(scope, &mut rv, "The requested origin is invalid.");
                return;
            }
        }
        _ => {}
    }
    let state = permission_state(scope, name);
    // A PermissionStatus owns NOTHING (Chrome: `Object.getOwnPropertyNames(status)` is `[]`):
    // `name`, `state` and `onchange` are its prototype's, `addEventListener` EventTarget's. It was a
    // plain object carrying all six as own properties, and not an instance of PermissionStatus.
    let status = mint_persona(scope, "PermissionStatus");
    patch_permission_status_proto(scope);
    let nv = str_val(scope, internal);
    crate::core::dom::element::priv_set(scope, status, P_PERM_NAME, nv);
    let sv = str_val(scope, &state);
    crate::core::dom::element::priv_set(scope, status, P_PERM_STATE, sv);
    // the browser's answer arrives a task later (Chrome 150: after a posted message and a 0 ms timer queued right
    // after the call, before a 1 ms one -- `binding::protos::Settle`)
    let p = crate::binding::protos::settle_later(scope, status.into(), false, crate::binding::protos::Settle::Task(1.0));
    rv.set(p);
}

const P_PERM_NAME: &str = "korvus_perm_name";
const P_PERM_STATE: &str = "korvus_perm_state";
const P_PATCHED: &str = "korvus_proto_patched";

/// Whether `proto` was already given its real members (a private marker, once per realm).
fn first_patch(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>) -> bool {
    if crate::core::css::css_style_declaration::priv_get(scope, proto, P_PATCHED).is_some() {
        return false;
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    crate::core::dom::element::priv_set(scope, proto, P_PATCHED, t);
    true
}

/// A WebIDL attribute getter on `proto`: `{get, set: undefined, enumerable, configurable}`.
fn proto_getter(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, name: &str, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let g = build_fn(scope, cb, &format!("get {name}"), 0);
    let Some(k) = v8::String::new(scope, name) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), undef);
    d.set_enumerable(true);
    d.set_configurable(true);
    proto.define_property(scope, k.into(), &d);
}

fn patch_permission_status_proto(scope: &mut v8::PinScope) {
    let Some((proto, _)) = parent_of(scope, "PermissionStatus") else { return };
    if !first_patch(scope, proto) {
        return;
    }
    proto_getter(scope, proto, "name", cb_perm_status_name);
    proto_getter(scope, proto, "state", cb_perm_status_state);
}

fn perm_slot(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, key: &str, rv: &mut v8::ReturnValue) {
    match crate::core::css::css_style_declaration::priv_get(scope, args.this(), key) {
        Some(v) => rv.set(v),
        None => throw_type_error(scope, "Illegal invocation"),
    }
}
fn cb_perm_status_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    perm_slot(scope, &args, P_PERM_NAME, &mut rv);
}
fn cb_perm_status_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    perm_slot(scope, &args, P_PERM_STATE, &mut rv);
}

/// The `Permissions` the JS built: one method, re-pointed at `Permissions.prototype`.
pub(super) fn build_permissions<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    // `query` is Permissions.prototype's (a WebIDL operation), not the instance's own.
    let o = mint_persona(scope, "Permissions");
    if let Some((proto, _)) = parent_of(scope, "Permissions") {
        if first_patch(scope, proto) {
            let q = build_fn(scope, cb_permissions_query, "query", 1);
            own_prop(scope, proto, "query", q.into());
        }
    }
    o
}

// ── navigator.mediaDevices ───────────────────────────────────────────────────────────────────────

/// The device kinds an unprompted `enumerateDevices()` lists when no persona carries its own --
/// one per kind, every identifying field blank. Which kinds appear is a fact about the MACHINE:
/// the persona's `media_device_kinds` answers it whenever a bundle is loaded.
const DEVICE_KINDS: [&str; 3] = ["audioinput", "videoinput", "audiooutput"];

/// The constraints `getSupportedConstraints()` reports when no persona carries its own, all `true`.
/// The set grows with the build (Chrome 150 reports 36), so a loaded bundle's
/// `supported_constraints` answers it.
const SUPPORTED_CONSTRAINTS: [&str; 16] = [
    "aspectRatio", "autoGainControl", "brightness", "channelCount", "deviceId", "echoCancellation",
    "facingMode", "frameRate", "groupId", "height", "latency", "noiseSuppression", "resizeMode",
    "sampleRate", "sampleSize", "width",
];

/// The persona's list, or the fallback when the persona has none.
fn persona_or(list: Vec<String>, fallback: &[&str]) -> Vec<String> {
    if list.is_empty() { fallback.iter().map(|s| s.to_string()).collect() } else { list }
}

/// `enumerateDevices()`: one `MediaDeviceInfo` per device -- an `InputDeviceInfo` for an input -- with every
/// identifying field blank, the page holding no media permission (PHASE 3 F7: they were plain objects with four
/// own data properties, `[object Object]` with no `toJSON`; Chrome 150's are interface instances with none).
/// A fresh list of fresh objects per call, as Blink's `MediaDevices::DevicesEnumerated` builds one per answer.
/// The answer comes from the browser process (`MediaDevicesDispatcherHost`), so the promise settles in a task --
/// after a posted message and the 0 ms and 1 ms timers the page queued right after the call, about with a 2 ms one
/// (Chrome 150, 15 calls measured: after the 2 ms timer 9 times, before it 6); korvus answered with an already
/// resolved promise, ahead of all of them (`binding::protos::Settle`).
fn cb_enumerate_devices(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let kinds = persona_or(client_hints(scope).media_device_kinds, &DEVICE_KINDS);
    let arr = v8::Array::new(scope, kinds.len() as i32);
    for (i, kind) in kinds.iter().enumerate() {
        let Some(o) = crate::core::media::media_device_info::mint_device(scope, kind, "", "", "") else { continue };
        arr.set_index(scope, i as u32, o.into());
    }
    let p = crate::binding::protos::settle_later(scope, arr.into(), false, crate::binding::protos::Settle::Task(2.0));
    rv.set(p);
}

fn cb_supported_constraints(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let o = v8::Object::new(scope);
    for name in persona_or(client_hints(scope).supported_constraints, &SUPPORTED_CONSTRAINTS) {
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        own_prop(scope, o, &name, t);
    }
    rv.set(o.into());
}

/// `navigator.mediaDevices.getUserMedia(constraints)` -- the two WebIDL rejections, then the
/// permission answer this build gives (there is no capture device behind it).
fn cb_media_get_user_media(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let raw = args.get(0);
    let constraints = v8::Local::<v8::Object>::try_from(raw)
        .ok()
        .filter(|_| !raw.is_function() && args.length() >= 1);
    let Some(constraints) = constraints else {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'getUserMedia' on 'MediaDevices': 1 argument required, but only 0 present.",
        );
        rv.set(p);
        return;
    };
    let want = ["audio", "video"]
        .iter()
        .any(|k| get_prop(scope, constraints, k).map(|v| v.boolean_value(scope)).unwrap_or(false));
    if !want {
        let p = rejected_type_error(
            scope,
            "Failed to execute 'getUserMedia' on 'MediaDevices': At least one of audio and video must be requested",
        );
        rv.set(p);
        return;
    }
    // A media-source plugin (ABI 3, fake-media / D-media) may answer what getUserMedia resolves to. Serialize the
    // constraints and ask; on a usable answer, resolve a task later (as Blink delivers a grant) with a MediaStream
    // of the described synthetic tracks. With no plugin -- or none that answers, or an unusable answer -- fall
    // through to the device-absent rejection below, exactly as with no plugin loaded (I9/J2).
    let constraints_json = v8::json::stringify(scope, constraints.into())
        .map(|j| j.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "{}".to_string());
    if let Some(desc) = plugin_host::on_user_media("user", &constraints_json) {
        if let Some(stream) = crate::core::media::media_stream::mint_fed_stream(scope, &desc) {
            let p = crate::binding::protos::settle_later(scope, stream.into(), false, crate::binding::protos::Settle::Task(1.0));
            rv.set(p);
            return;
        }
    }
    reject_not_allowed(scope, rv);
}

/// `getDisplayMedia(options)`: its `DisplayMediaStreamOptions` converted as Blink's binding converts them (a refusal a
/// rejection), then the screen picker the browser shows -- Chrome 150 (measured) leaves the
/// promise pending behind it; korvus read no option and refused at once.
fn cb_media_get_display_media(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Err(e) = crate::install::ctor_dicts::convert_method_args_caught(scope, "MediaDevices::getDisplayMedia", &args, "Failed to execute 'getDisplayMedia' on 'MediaDevices': ") {
        rv.set(rejected_promise(scope, e));
        return;
    }
    let p = crate::binding::protos::pending_promise(scope);
    rv.set(p);
}

/// `Promise.reject(new DOMException("Permission denied", "NotAllowedError"))`.
fn reject_not_allowed(scope: &mut v8::PinScope, mut rv: v8::ReturnValue) {
    let Some(e) = dom_error(scope, "Permission denied", "NotAllowedError") else { return };
    let p = rejected_promise(scope, e);
    rv.set(p);
}

/// The `MediaDevices` the JS built, member for member and in its order, re-pointed at
/// `MediaDevices.prototype`.
pub(super) fn build_media_devices<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let o = mint_persona(scope, "MediaDevices");
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    own_prop(scope, o, "ondevicechange", null);
    let f = build_fn(scope, cb_enumerate_devices, "enumerateDevices", 0);
    own_prop(scope, o, "enumerateDevices", f.into());
    let f = build_fn(scope, cb_supported_constraints, "getSupportedConstraints", 0);
    own_prop(scope, o, "getSupportedConstraints", f.into());
    let f = build_fn(scope, cb_media_get_user_media, "getUserMedia", 1);
    own_prop(scope, o, "getUserMedia", f.into());
    let f = build_fn(scope, cb_media_get_display_media, "getDisplayMedia", 0);
    own_prop(scope, o, "getDisplayMedia", f.into());
    add_event_listener_trio(scope, o);
    o
}
