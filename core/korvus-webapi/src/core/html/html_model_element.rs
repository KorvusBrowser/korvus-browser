//! `core/html/html_model_element.rs` — the `<model>` element (immersive-web/model-element).
//!
//! WebIDL (gen/webref/model-element.idl): 7 members on HTMLModelElement : HTMLElement —
//!   readonly attribute Promise<HTMLModelElement> ready;
//!   readonly attribute DOMPointReadOnly boundingBoxCenter;
//!   readonly attribute DOMPointReadOnly boundingBoxExtents;
//!   attribute DOMMatrixReadOnly entityTransform;
//!   attribute USVString environmentMap;
//!   readonly attribute Promise<undefined> environmentMapReady;
//!   [Reflect=stagemode] attribute DOMString stageMode;
//!
//! All seven are rows of the `interface!` table at the foot of this file, mounted
//! through the ONE row mount (`install::live::{getter_row, pair_row}`) as genuinely-native accessors
//! — `[native code]`, `get x`/`set x` names, getter length 0 / setter length 1, enumerable and
//! configurable. `stageMode` is the shared `reflect_str` kit, because `[Reflect=stagemode] DOMString`
//! IS a reflected string.
//!
//! `js/dom/08j-model-element.js` (57 lines, three `Object.defineProperty` sites, nine op-builder
//! round trips) is deleted, and with it the eight `#[op2] op_model_*` builders: a row names its
//! callback directly, so the `#[op2]` wrapper around `build_fn` had no caller left.
//!
//! NOT INSTALLED IN THIS BUNDLE, deliberately: Brave 151 exposes no `HTMLModelElement` (recorded on
//! this machine — `typeof HTMLModelElement` is "undefined" there, and `createElement("model")` is an
//! `HTMLUnknownElement`), so publishing one would be an invented global. The table is listed in
//! `core::html::UNEXPOSED_BY_THIS_BUNDLE` and `tests/exclusion.rs` asserts that nothing under
//! `install/live/` mounts it — the union-table SELECTOR model, stated where a test can read it
//! (docs/ONE-TABLE-ARCHITECTURE.md D3/D4). Exposing it is adding one line to an install, which is
//! what a bundle that records `<model>` would do.
//!
//! State model. Korvus has no 3D pipeline, so the element's load state machine is degenerate,
//! and each member takes its no-resource value straight from the spec's own language:
//!
//!   * `ready` — "A promise that ... resolves when the model has been processed and is ready to
//!     display", resolving with the element itself. With no model source to process the element
//!     is vacuously processed, so the promise is created already fulfilled. It is cached in a
//!     per-element private slot: the spec speaks of the element's ready promise (one object),
//!     and re-reading `.ready` must return the same promise, not a fresh one.
//!   * `environmentMapReady` — "resolves when an environment map resource has been loaded";
//!     when no environmentMap is specified "the User Agent MUST provide an appropriate map",
//!     which in a no-pipeline engine is trivially available, so it resolves with `undefined`
//!     (Promise<undefined>). Cached per element like `ready`.
//!   * `boundingBoxCenter` / `boundingBoxExtents` — the spec's bounding-box algorithm walks the
//!     scene graph's renderable geometry; with no scene the box is the degenerate empty one —
//!     center and extents both the zero point (`new DOMPointReadOnly()` → x/y/z 0, w 1, the
//!     platform's canonical empty point). A fresh object per read, like every derived
//!     geometry value in the DOM (DOMRect bounds getters).
//!   * `entityTransform` — a stored per-element DOMMatrixReadOnly, per "Updates to the
//!     entityTransform SHOULD be reflected on the next rendered frame" (it is a slot an author
//!     writes, not a derived value). Default: the identity. The setter takes WebIDL interface
//!     conversion: a DOMMatrix (which inherits DOMMatrixReadOnly) is stored as-is; anything
//!     else throws the platform's TypeError.
//!   * `environmentMap` — a plain USVString slot (no [Reflect], so NOT a content attribute):
//!     "" until set; the setter runs ToString then lone-surrogate → U+FFFD, which is exactly
//!     what v8's lossy WTF-16 → UTF-8 conversion does.
//!
//! A rejected `ready` (a model source that fails to load) is unreachable in a no-loader engine
//! and so is not modelled; if a pipeline lands, the private-slot promises are the state to swap.

use deno_core::v8;
use crate::binding::protos::{get_prop, new_global, resolved_promise, str_val, throw_type_error};
use crate::install::live::{getter_row, pair_row};
use crate::install::MemberDef;

// Per-element state, in v8::Private slots on the wrapper (the same mechanism as CSD_EID and the
// EventTarget object store): private names never show up in getOwnPropertyNames / for-in, so the
// element's own-property surface stays exactly HTMLElement's plus nothing.
const READY_PRIV: &str = "korvus_model_ready";
const TRANSFORM_PRIV: &str = "korvus_model_transform";
const ENVMAP_PRIV: &str = "korvus_model_envmap";
const ENVREADY_PRIV: &str = "korvus_model_envready";

fn slot_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let pname = v8::String::new(scope, name)?;
    let key = v8::Private::for_api(scope, Some(pname));
    o.get_private(scope, key).filter(|v| !v.is_undefined())
}
fn slot_set(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    if let Some(pname) = v8::String::new(scope, name) {
        let key = v8::Private::for_api(scope, Some(pname));
        o.set_private(scope, key, v);
    }
}

/// Re-bake `this` with the scope's own lifetime (args.this() is tied to FunctionCallbackArguments,
/// not to the scope -- the same round-trip cb_dialog_close uses for build_fn_data's bound data).
fn reown<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>) -> v8::Local<'s, v8::Object> {
    let g = v8::Global::new(scope, o);
    v8::Local::new(scope, &g)
}

// ready: the element's (single, cached) ready promise — already fulfilled with the element itself.
fn cb_model_ready_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = reown(scope, args.this());
    if let Some(p) = slot_get(scope, this, READY_PRIV) { rv.set(p); return; }
    let tv: v8::Local<v8::Value> = this.into();
    let p = resolved_promise(scope, tv);
    slot_set(scope, this, READY_PRIV, p);
    rv.set(p);
}

// environmentMapReady: cached, fulfilled with undefined (Promise<undefined>).
fn cb_model_env_ready_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = reown(scope, args.this());
    if let Some(p) = slot_get(scope, this, ENVREADY_PRIV) { rv.set(p); return; }
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let p = resolved_promise(scope, undef);
    slot_set(scope, this, ENVREADY_PRIV, p);
    rv.set(p);
}

// boundingBoxCenter / boundingBoxExtents: the degenerate empty-scene box, as a fresh
// DOMPointReadOnly(0, 0, 0, 1) per read (derived geometry values are new objects, like DOMRect's).
fn cb_model_bb_center_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = args;
    if let Some(p) = new_global(scope, "DOMPointReadOnly", &[]) { rv.set(p.into()); }
}
fn cb_model_bb_extents_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = args;
    if let Some(p) = new_global(scope, "DOMPointReadOnly", &[]) { rv.set(p.into()); }
}

// entityTransform: a stored per-element DOMMatrixReadOnly (default identity). The setter is
// WebIDL interface conversion — DOMMatrix (a subclass) is accepted and stored as-is.
fn cb_model_transform_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = reown(scope, args.this());
    if let Some(t) = slot_get(scope, this, TRANSFORM_PRIV) { rv.set(t); return; }
    if let Some(t) = new_global(scope, "DOMMatrixReadOnly", &[]) {
        let tv: v8::Local<v8::Value> = t.into();
        slot_set(scope, this, TRANSFORM_PRIV, tv);
        rv.set(tv);
    }
}
fn cb_model_transform_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let v = args.get(0);
    let global = scope.get_current_context().global(scope);
    let ctor = get_prop(scope, global, "DOMMatrixReadOnly").and_then(|c| v8::Local::<v8::Object>::try_from(c).ok());
    let is_matrix = v.is_object() && ctor.map(|c| v.instance_of(scope, c).unwrap_or(false)).unwrap_or(false);
    if !is_matrix {
        throw_type_error(scope, "Failed to set the 'entityTransform' property on 'HTMLModelElement': The provided value is not of type 'DOMMatrixReadOnly'.");
        return;
    }
    let this = reown(scope, args.this());
    slot_set(scope, this, TRANSFORM_PRIV, v);
}

// environmentMap: a USVString slot — "" until set, ToString + lone-surrogate→U+FFFD on set.
fn cb_model_env_map_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = reown(scope, args.this());
    match slot_get(scope, this, ENVMAP_PRIV) {
        Some(s) => rv.set(s),
        None => { let e = str_val(scope, ""); rv.set(e); }
    }
}
fn cb_model_env_map_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // to_rust_string_lossy is ToString + WTF-16→UTF-8 with lone surrogates as U+FFFD — WebIDL's
    // USVString conversion exactly. A throwing toString (or a Symbol) leaves the pending exception
    // set, which propagates out of the raw callback.
    let usv = args.get(0).to_rust_string_lossy(scope);
    let sv = str_val(scope, &usv);
    let this = reown(scope, args.this());
    slot_set(scope, this, ENVMAP_PRIV, sv);
}

pub fn install_html_model_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "ready" => getter_row(scope, proto, member, cb_model_ready_get),
        "boundingBoxCenter" => getter_row(scope, proto, member, cb_model_bb_center_get),
        "boundingBoxExtents" => getter_row(scope, proto, member, cb_model_bb_extents_get),
        "entityTransform" => pair_row(scope, proto, member, cb_model_transform_get, cb_model_transform_set),
        "environmentMap" => pair_row(scope, proto, member, cb_model_env_map_get, cb_model_env_map_set),
        "environmentMapReady" => getter_row(scope, proto, member, cb_model_env_ready_get),
        other => debug_assert!(false, "install_html_model_element: unexpected member {other}"),
    }
}

webapi::interface! {
    // WebIDL source order (gen/webref/model-element.idl). Enumeration order is itself a fingerprint
    // signal, so it is the table's order and not a convenience order.
    // `[interface]`: model-element.idl declares no constructor at all, `[HTMLConstructor]`
    // included, and Chrome 150 does not ship the interface. See `Element`'s note.
    "HTMLModelElement" : "HTMLElement" [interface, window] {
        ctor "HTMLModelElement"                      len 0      = stub,
        attr "ready"               : Promise   readonly         = install_html_model_element,
        attr "boundingBoxCenter"   : Object    readonly         = install_html_model_element,
        attr "boundingBoxExtents"  : Object    readonly         = install_html_model_element,
        attr "entityTransform"     : Object                     = install_html_model_element,
        attr "environmentMap"      : UsvString                  = install_html_model_element,
        attr "environmentMapReady" : Promise   readonly         = install_html_model_element,
        // [Reflect=stagemode] attribute DOMString -- a reflected string, so the shared kit, which is
        // the very pair the deleted JS reached for through `op_reflect_str_get`/`_set`.
        attr "stageMode"           : DomString cfg "stagemode"  = reflect_str
    }
}

