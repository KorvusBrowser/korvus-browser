//! `core/navigator/geolocation.rs` -- `Geolocation.getCurrentPosition`, resolved from a loaded plugin.
//!
//! WHAT THIS WIRES (C5 / ABI 3 step 12). `navigator.geolocation` is a shape-only singleton minted on
//! `Geolocation.prototype` (`devices.rs`), and its three methods are the generated registry's stubs: a
//! `getCurrentPosition` that does nothing, which is exactly a browser whose geolocation prompt nobody ever
//! answers. This file replaces that stub with a real body WHEN A PLUGIN IS LOADED (`devices::patch_prototype`
//! calls [`install`] only then), and the body hands the page a position only when a plugin FILLS the
//! geolocation slot: `plugin_host::geolocation(ctx)` returns `{"lat":..,"lng":..,"acc":..}` and the success
//! callback fires with a real `GeolocationPosition`. With no plugin -- or a plugin that passes -- the body
//! does nothing, so the page sees the same unanswered-prompt behaviour as the stub (invariant I9/J2).
//!
//! The `GeolocationPosition` / `GeolocationCoordinates` instances are built the way `persona_objects.rs`
//! builds `PermissionStatus`: an instance minted on the interface's own prototype (so `instanceof` holds),
//! its readonly attributes served by prototype getters that read private slots set here.

use deno_core::v8;

use crate::binding::protos::{body_data, build_fn, build_fn_data, mint_on, str_val};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::core::intersection_observer::rendering::queue_task;
use crate::install::live::parent_of;

use super::navigator::own_prop;

/// The browser delivers `getCurrentPosition` a task later, not inline -- the same shape every other async
/// `navigator` answer here takes.
const DELIVER_MS: f64 = 1.0;

/// Install the real `getCurrentPosition` over `Geolocation.prototype`'s stub. Reached from
/// `devices::patch_prototype` only when a plugin is loaded, so the stubbed engine is byte-for-byte unchanged.
pub(crate) fn install(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>) {
    // `getCurrentPosition(successCallback, errorCallback?, options?)` -- length 1, the registry row's.
    let f = build_fn(scope, cb_get_current_position, "getCurrentPosition", 1);
    // Preserve the registry stub's `required 1` arity guard: `getCurrentPosition()` with no args throws
    // "...1 argument required, but only 0 present." exactly as Chrome does. `build_fn` alone drops the guard,
    // which the Chrome oracle ratchet caught -- the success callback is mandatory, independent of any plugin
    // position (and korvus-stealth makes a plugin always present, so this body is always live).
    let guarded = crate::binding::protos::make_arity_guard(
        scope, "Geolocation", "getCurrentPosition", 1, 1, f.into(), crate::install::TypeTag::Any,
    );
    own_prop(scope, proto, "getCurrentPosition", guarded.into());
    // TODO(1021-verify): `watchPosition` (a watch id + repeated callbacks) and `clearWatch` stay stubs; a
    //      plugin-set position should feed `watchPosition` too, one shot per the same `geolocation(ctx)`.
}

/// `navigator.geolocation.getCurrentPosition(...)`: hand the success callback a plugin-supplied position.
fn cb_get_current_position(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // Consult the plugin FIRST: with no position on offer, do nothing (the unanswered-prompt stub), so a
    // plugin that does not fill this slot leaves the call behaving exactly as the stub did -- for every
    // receiver and every argument shape (I9/J2). `ctx` names the context the position is for.
    // GAP: korvus-plugin-host exposes no public getter for this process's context id (only the C ABI
    //      `context_id()` host call), so `ctx` is "" here. A per-context plugin cannot tell contexts apart;
    //      a plugin that answers one position regardless (the common case) is unaffected. Needs e.g. a
    //      `pub fn context_id() -> String` in korvus-plugin-host.
    let ctx = ""; // TODO(1021-verify): pass the real CDP context id once korvus-plugin-host exposes it.
    // A DevTools client's position (`Emulation.setGeolocationOverride`) first, as Chrome's override answers ahead of
    // the platform's provider; an override with no position leaves the call unanswered.
    let (lat, lng, acc) = match spec::devtools::geolocation() {
        Some(Some(g)) => (g.latitude, g.longitude, g.accuracy),
        Some(None) => return,
        None => {
            let Some(json) = plugin_host::geolocation(ctx) else { return };
            let Some(p) = parse_position(&json) else { return };
            p
        }
    };
    // The success callback is required (`PositionCallback`); a non-callable one is nothing to call.
    let Ok(callback) = v8::Local::<v8::Function>::try_from(args.get(0)) else { return };
    let position = build_position(scope, lat, lng, acc);
    deliver_later(scope, callback, position);
}

/// `{"lat":<deg>,"lng":<deg>,"acc":<metres>}` -> (lat, lng, accuracy). `lat`/`lng` are required; `acc`
/// defaults to 1 m when a plugin omits it (`accuracy` is a non-nullable double).
fn parse_position(json: &str) -> Option<(f64, f64, f64)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let lat = v.get("lat")?.as_f64()?;
    let lng = v.get("lng")?.as_f64()?;
    let acc = v.get("acc").and_then(|a| a.as_f64()).unwrap_or(1.0);
    Some((lat, lng, acc))
}

// ── the position / coordinates instances ───────────────────────────────────────────────────────────

const P_LAT: &str = "korvus_geo_lat";
const P_LNG: &str = "korvus_geo_lng";
const P_ACC: &str = "korvus_geo_acc";
const P_COORDS: &str = "korvus_geo_coords";
const P_TS: &str = "korvus_geo_timestamp";
const P_PATCHED: &str = "korvus_geo_patched";

/// A `GeolocationPosition` whose `coords` is a `GeolocationCoordinates` of `(lat, lng, acc)`, both real
/// instances of their interfaces so `position instanceof GeolocationPosition` and `.coords instanceof
/// GeolocationCoordinates` hold, as a real one does.
fn build_position<'s>(scope: &mut v8::PinScope<'s, '_>, lat: f64, lng: f64, acc: f64) -> v8::Local<'s, v8::Value> {
    let coords = mint_on_iface(scope, "GeolocationCoordinates");
    patch_coords_proto(scope);
    set_num(scope, coords, P_LAT, lat);
    set_num(scope, coords, P_LNG, lng);
    set_num(scope, coords, P_ACC, acc);

    let position = mint_on_iface(scope, "GeolocationPosition");
    patch_position_proto(scope);
    priv_set(scope, position, P_COORDS, coords.into());
    // `timestamp` is a `DOMTimeStamp`: Unix epoch milliseconds (unlike `performance.now()`, it is wall-clock).
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0);
    set_num(scope, position, P_TS, ms);
    position.into()
}

/// An instance minted on `iface.prototype`, or a bare object when the realm does not expose the interface
/// (mirrors `persona_objects::mint_persona`).
fn mint_on_iface<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Object> {
    match parent_of(scope, iface) {
        Some((proto, _)) => mint_on(scope, proto.into()),
        None => v8::Object::new(scope),
    }
}

fn set_num(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, n: f64) {
    let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
    priv_set(scope, o, key, v);
}

/// Give `proto` its real getters once per realm (the members are `configurable:true` stubs; a second pass
/// would re-define them needlessly). A private marker on the prototype records it, as `persona_objects` does.
fn first_patch(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>) -> bool {
    if priv_get(scope, proto, P_PATCHED).is_some() {
        return false;
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, proto, P_PATCHED, t);
    true
}

fn patch_coords_proto(scope: &mut v8::PinScope) {
    let Some((proto, _)) = parent_of(scope, "GeolocationCoordinates") else { return };
    if !first_patch(scope, proto) {
        return;
    }
    // The three a plugin supplies; the rest are null (`altitude`, `altitudeAccuracy`, `heading`, `speed` are
    // nullable and read null when the plugin gives no slot -- `priv_get` returns None, the getter answers null).
    slot_getter(scope, proto, "latitude", P_LAT);
    slot_getter(scope, proto, "longitude", P_LNG);
    slot_getter(scope, proto, "accuracy", P_ACC);
    for name in ["altitude", "altitudeAccuracy", "heading", "speed"] {
        slot_getter(scope, proto, name, "korvus_geo_absent");
    }
    // TODO(1021-verify): `GeolocationCoordinates.toJSON` stays a stub.
}

fn patch_position_proto(scope: &mut v8::PinScope) {
    let Some((proto, _)) = parent_of(scope, "GeolocationPosition") else { return };
    if !first_patch(scope, proto) {
        return;
    }
    slot_getter(scope, proto, "coords", P_COORDS);
    slot_getter(scope, proto, "timestamp", P_TS);
    // TODO(1021-verify): `GeolocationPosition.toJSON` stays a stub.
}

/// A readonly WebIDL attribute getter on `proto` that reads the private slot `key` of its receiver, answering
/// `null` when the slot is absent (a nullable coordinate the plugin did not give). `{get, enumerable, configurable}`.
fn slot_getter(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, name: &str, key: &str) {
    let data = str_val(scope, key);
    let g = build_fn_data(scope, cb_slot, &format!("get {name}"), 0, data);
    let Some(k) = v8::String::new(scope, name) else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), undef);
    d.set_enumerable(true);
    d.set_configurable(true);
    proto.define_property(scope, k.into(), &d);
}

fn cb_slot(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = body_data(scope, &args).to_rust_string_lossy(scope);
    match priv_get(scope, args.this(), &key) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

// ── delivery ─────────────────────────────────────────────────────────────────────────────────────

/// Queue `callback(position)` a task from now (an undefined `this`), as Blink delivers a geolocation fix.
fn deliver_later(scope: &mut v8::PinScope, callback: v8::Local<v8::Function>, position: v8::Local<v8::Value>) {
    let d = v8::Array::new(scope, 2);
    d.set_index(scope, 0, callback.into());
    d.set_index(scope, 1, position);
    let f = build_fn_data(scope, cb_deliver, "", 0, d.into());
    queue_task(scope, f, DELIVER_MS);
}

fn cb_deliver(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(body_data(scope, &args)) else { return };
    let Some(callback) = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let position = match d.get_index(scope, 1) {
        Some(v) => v,
        None => v8::undefined(scope).into(),
    };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    // A throw in a page callback is reported at the realm, never propagated (Blink's `InvokeAndReportException`).
    // TODO(1021-verify): report it like `devices::invoke_and_report` rather than swallow it.
    v8::tc_scope!(let tc, scope);
    callback.call(tc, undef, &[position]);
    tc.reset();
}
