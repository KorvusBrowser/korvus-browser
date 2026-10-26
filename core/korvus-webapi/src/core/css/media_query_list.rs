//! `core/css/media_query_list.rs` — `MediaQueryList` and `window.matchMedia`.
//!
//! WHAT THIS REPLACES: `host.js`'s `__wireMQL` block — a closure that, at `INSTALL_DOCUMENT` time,
//! REDEFINED five members of `MediaQueryList.prototype` with `op_nativize`-masked JS bodies, on a
//! prototype the registry install had already built from the capture. Five slots with two writers
//! (invariant I2) and five native masks (I3); `core/korvus-runtime/tests/fixtures/
//! second_writers.manifest` names all five, and they are the largest single group in it.
//!
//! It also replaces the `matchMedia` factory and the `KB.__wireMQL` bridge slot the runtime's
//! navigation code called to force the wiring before any page script ran. There is nothing to force:
//! the rows are installed once, by the one installer, at context creation.
//!
//! # What the JS got wrong, measured on `google-chrome --headless=new` 150.0.7871.46 (loopback,
//! 2026-09-12)
//!
//! * ALL FIVE rows answered their closure's default for a foreign receiver — `media` `""`, `matches`
//!   `false`, `onchange` `null`, and the two listener aliases forwarded to `EventTarget.prototype`
//!   with a `this` that is not an EventTarget. Chrome throws `TypeError: Illegal invocation` for
//!   every one, including `onchange`'s getter, which is why the handler row is the BRANDED
//!   `evt_handler` here and not the registry's plain `event_handler`.
//! * `matchMedia(Symbol("a"))` threw the bare `Cannot convert a Symbol value to a string`. Chrome's
//!   is `Failed to execute 'matchMedia' on 'Window': Cannot convert a Symbol value to a string` —
//!   the member-named form every other WebIDL conversion in this tree produces.
//! * `mql.addListener()` with no argument was accepted (it forwarded `undefined` to
//!   `addEventListener`). Chrome throws `Failed to execute 'addListener' on 'MediaQueryList': 1
//!   argument required, but only 0 present.` — the guard `installer::method_row` puts on a
//!   `required 1` row, which the JS had stripped off by redefining the member.
//!
//! Everything else is carried over exactly: `media` is the SERIALIZED query (so `(MIN-WIDTH: 0PX)`
//! reads `(min-width: 0px)`, measured identical in Chrome), `matches` is the cascade's own
//! evaluator, the two Level-3 aliases forward to `add`/`removeEventListener("change")`, and an
//! instance owns NO property — the query text is a v8 private, where the JS used a `WeakMap` for the
//! same reason.

use deno_core::v8;

use crate::binding::protos::{str_val, throw_type_error};
use crate::core::workers::worker::{slot_get, slot_set};
use crate::install::webidl::{brand, branded_getter_row, has_brand};
use crate::install::MemberDef;

/// The query text this list was constructed over. A v8 PRIVATE, so
/// `Object.getOwnPropertyNames(mql)` is `[]` — which is what Chrome answers (measured) and what the
/// JS's `WeakMap` achieved the same way.
const QUERY_SLOT: &str = "korvus_mql_query";

/// `this`'s query text, or `Illegal invocation`. The brand is the receiver check the JS had none of.
fn this_query(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<String> {
    let this = args.this();
    if !has_brand(scope, this.into(), "MediaQueryList") {
        crate::binding::protos::illegal_invocation(scope);
        return None;
    }
    Some(slot_get(scope, this, QUERY_SLOT).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default())
}

fn cb_mql_media(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(q) = this_query(scope, &args) else { return };
    let s = crate::core::css::cssom::media_serialize(scope, &q);
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_mql_matches(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(q) = this_query(scope, &args) else { return };
    rv.set_bool(crate::core::css::cssom::media_matches_for(scope, &q, args.this()));
}

/// `addListener(fn)` / `removeListener(fn)` — the Level-3 legacy synonyms, forwarded to
/// `EventTarget.prototype.add`/`removeEventListener(this, "change", fn)`.
///
/// The forward goes through the PROTOTYPE's function rather than `this.addEventListener`, so a page
/// that shadows the method on the instance cannot redirect the alias — the same thing the JS's
/// `globalThis.EventTarget.prototype[target].call(this, …)` did.
fn cb_mql_listener(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let target = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    if !has_brand(scope, this.into(), "MediaQueryList") {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let Some(et) = crate::binding::protos::iface_object(scope, "EventTarget")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| crate::binding::protos::get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|p| crate::binding::protos::get_prop(scope, p, &target))
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return;
    };
    let ty = str_val(scope, "change");
    let fnv = args.get(0);
    let thisv: v8::Local<v8::Value> = this.into();
    et.call(scope, thisv, &[ty, fnv]);
}

/// The body [`MEDIAQUERYLIST`]'s four non-handler rows name.
pub fn install_media_query_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "media" => branded_getter_row(scope, proto, member, cb_mql_media),
        "matches" => branded_getter_row(scope, proto, member, cb_mql_matches),
        "addListener" | "removeListener" => {
            // `branded_method_row` with the forwarding TARGET as bound data, so one callback serves
            // both aliases. It builds with `build_fn`, which carries none, so the data goes on
            // through a small local mount rather than a second shared helper.
            let d = str_val(scope, if member.name == "addListener" { "addEventListener" } else { "removeEventListener" });
            branded_method_row_data(scope, proto, member, cb_mql_listener, d);
        }
        other => debug_assert!(false, "install_media_query_list: unexpected member {other}"),
    }
}

/// [`branded_method_row`] with BOUND DATA. Same guard, same stamp, same descriptor.
///
/// The brand guard COUNTS TOO, after its brand check (`install::webidl::brand_guard_check`: the row's
/// `required 1`, so `mql.addListener()` throws where the JS accepted it). An arity guard wrapped round it
/// here counted FIRST, so `MediaQueryList.prototype.addListener.call({})` answered "1 argument required"
/// where Chrome 150 answers `Illegal invocation` (measured 2026-10-09).
fn branded_method_row_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    data: v8::Local<'s, v8::Value>,
) {
    use crate::binding::protos::{build_fn_data, stamp_receiver_guarded};
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let f = build_fn_data(scope, cb, member.name, member.length as i32, data);
    let v = crate::install::webidl::brand_guard(scope, iface, member, f);
    if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
        stamp_receiver_guarded(scope, f);
    }
    crate::install::kits::define_value(scope, target, member, v, true);
}

// ── window.matchMedia ───────────────────────────────────────────────────────────────────────────

/// `window.matchMedia(CSSOMString query)` — the `Window` table's row body.
///
/// The receiver check is the row's (`core/html/window.rs` wraps this body in `guarded`):
/// `matchMedia.call(null, q)` answers a `MediaQueryList` in Chrome and `matchMedia.call({}, q)` is
/// `Illegal invocation`. The ARITY and the Symbol conversion throw here, and both messages are
/// Chrome's.
pub(crate) fn cb_match_media(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'matchMedia' on 'Window': 1 argument required, but only 0 present.");
        return;
    }
    let arg = args.get(0);
    if arg.is_symbol() {
        // Chrome names the member; the JS threw the bare ES message.
        throw_type_error(scope, "Failed to execute 'matchMedia' on 'Window': Cannot convert a Symbol value to a string");
        return;
    }
    let query = arg.to_rust_string_lossy(scope);
    let mql = crate::install::live::mint_table(scope, "MediaQueryList");
    brand(scope, mql, "MediaQueryList");
    let q = str_val(scope, &query);
    slot_set(scope, mql, QUERY_SLOT, q);
    rv.set(mql.into());
}

/// Fire a `change` event at one `MediaQueryList`. Kept beside the rows because it is the only thing
/// that makes `onchange` reachable, and nothing in this engine calls it yet — the media context does
/// not change after load, so a browser would fire none either.
#[allow(dead_code)]
pub(crate) fn fire_change(scope: &mut v8::PinScope, mql: v8::Local<v8::Object>) {
    let ty = str_val(scope, "change");
    let Some(ev) = crate::binding::protos::new_global(scope, "Event", &[ty]) else { return };
    let target: v8::Local<v8::Value> = mql.into();
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, target, "dispatchEvent", &[evv]);
}

// Rows VERBATIM from the `MediaQueryList` block of `install/generated_registry.rs`, in the capture's
// order: media, matches, onchange, addListener, removeListener — which is also the order real Chrome
// reports, with `constructor` last (measured). `[interface, window]`: the capture records
// `newable: false` and only the window surface lists it.
//
// `evt_handler` rather than the registry's `event_handler`: measured, Chrome throws
// `Illegal invocation` for `Object.getOwnPropertyDescriptor(MediaQueryList.prototype, "onchange")
// .get.call({})`, and the branded kit is what does that. Only an interface whose instances carry a
// brand may name it, which `cb_match_media` now stamps.
webapi::interface! {
    "MediaQueryList" : "EventTarget" [interface, window] {
        ctor    "MediaQueryList"                            len 0 = stub,
        attr    "media"          : DomString readonly             = install_media_query_list,
        attr    "matches"        : Boolean   readonly             = install_media_query_list,
        handler "onchange"                                        = evt_handler,
        method  "addListener"    : Undefined len 1 required 1     = install_media_query_list,
        method  "removeListener" : Undefined len 1 required 1     = install_media_query_list
    }
}
