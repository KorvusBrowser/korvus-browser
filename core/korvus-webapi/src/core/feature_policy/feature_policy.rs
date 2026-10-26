//! `core/feature_policy/feature_policy.rs` — the `FeaturePolicy` interface, as one `interface!`
//! table, and the two members that hand one back.
//!
//! The table's names, order, kinds, lengths and descriptor flags are VERBATIM from the four
//! bundles' `surface.json` (all four record the same five prototype properties in the same order)
//! and from `install/generated_registry.rs`'s `I301`, which this replaces — the interface leaves the
//! generated registry by being defined here (one-table exclusivity).
//!
//! `FeaturePolicy` is NOT constructible: `new FeaturePolicy()` is
//! `TypeError: Failed to construct 'FeaturePolicy': Illegal constructor` and `FeaturePolicy()` is
//! `TypeError: Illegal constructor` in all four browsers, which is what `Model::Interface` plus a
//! `stub` ctor row spells. Instances are minted here and nowhere else.
//!
//! # The brand, and why it is the prototype
//!
//! Every one of the four methods throws `TypeError: Illegal invocation` on a foreign receiver —
//! measured directly: `FeaturePolicy.prototype.allowedFeatures()` throws in all four. The check is
//! "is the interface prototype on this receiver's chain", with the prototype travelling in the
//! function's own `data`, which is the shape `core/animation/document_timeline.rs` established and
//! for the same reason: a page that overwrites `window.FeaturePolicy` must not be able to move the
//! brand. The receiver check runs BEFORE the arity check, which is the order Chrome uses on every
//! method measured for it so far, so these rows mount through `row_method_recv_first`.

use deno_core::v8;

use crate::binding::protos::{require_args, str_val, throw_type_error};
use crate::install::MemberDef;

use super::{allowlist_for, document_origin, Allowlist, REGISTRY};
use crate::install::kits::branded;

/// The private slot an instance minted for an iframe carries: the element itself, so the container
/// policy is read LIVE from the element's `allow` and `src` rather than frozen at mint time.
const CONTAINER_SLOT: &str = "korvus_feature_policy_container";

fn slot<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, CONTAINER_SLOT).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// An instance on the CURRENT realm's `FeaturePolicy.prototype` -- the realm's own interface, never a
/// name the page can write ([`crate::binding::protos::shell_proto`]).
///
/// It was a prototype registered by the install in an `OpState` store, which is per ISOLATE: every
/// frame's instances took the page's prototype. A frame's realm is its own, and Chrome
/// 150.0.7871.46 answers `frame.contentDocument.featurePolicy instanceof frame.contentWindow.FeaturePolicy`
/// true and `=== document.featurePolicy` false (headless, 2026-09-28).
fn new_instance<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "PermissionsPolicy")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    Some(obj)
}

/// The realm slot a realm's `document.featurePolicy` is kept under.
const DOCUMENT_SLOT: &str = "korvus_feature_policy_document";

/// `document.featurePolicy` — one object per realm, so `document.featurePolicy ===
/// document.featurePolicy` (measured true in all four browsers), and a frame's is its own
/// ([`crate::binding::protos::realm_slot`]).
pub(crate) fn document_feature_policy<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    use crate::binding::protos::{realm_slot, set_realm_slot};
    if let Some(o) = realm_slot(scope, DOCUMENT_SLOT).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(o);
    }
    let obj = new_instance(scope)?;
    set_realm_slot(scope, DOCUMENT_SLOT, obj.into());
    Some(obj)
}

/// `iframe.featurePolicy` — the CONTAINER policy: the document's, with the element's `allow`
/// applied. One object per element, remembered on the element so identity is stable across reads,
/// and it reads the attribute live so a later `iframe.allow = "..."` is visible through it.
pub(crate) fn container_feature_policy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    element: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let key = slot(scope);
    let owner = v8::Private::for_api(scope, v8::String::new(scope, "korvus_feature_policy_own"));
    if let Some(existing) = element.get_private(scope, owner).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(existing);
    }
    let obj = new_instance(scope)?;
    obj.set_private(scope, key, element.into());
    element.set_private(scope, owner, obj.into());
    Some(obj)
}


/// The container this instance reports for, as `(allow attribute, frame source origin)`, or `None`
/// for the document's own policy.
fn container_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<(String, String)> {
    let key = slot(scope);
    let el = this.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let read = |scope: &mut v8::PinScope, name: &str| -> String {
        let k = str_val(scope, name);
        el.get(scope, k)
            .filter(|v| !v.is_undefined() && !v.is_null())
            .and_then(|v| v.to_string(scope))
            .map(|s| s.to_rust_string_lossy(scope))
            .unwrap_or_default()
    };
    let allow = read(scope, "allow");
    let src = read(scope, "src");
    // A frame with no `src` inherits its container's origin, which is what `'src'` then resolves to.
    let src_origin = super::origin_token(&src).unwrap_or_else(|| document_origin(scope));
    Some((allow, src_origin))
}

/// The receiver's own allowlist question, shared by all four methods.
fn resolve(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, feature: &str) -> Option<Allowlist> {
    match container_of(scope, this) {
        Some((allow, src)) => allowlist_for(scope, feature, Some((&allow, &src))),
        None => allowlist_for(scope, feature, None),
    }
}

fn string_array<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[String]) -> v8::Local<'s, v8::Array> {
    let vals: Vec<v8::Local<v8::Value>> = items.iter().map(|s| str_val(scope, s)).collect();
    v8::Array::new_with_elements(scope, &vals)
}

/// `features()` — every policy-controlled feature this UA has, in the registry's order.
fn cb_features(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let names: Vec<String> = REGISTRY.iter().map(|(n, _)| (*n).to_string()).collect();
    let arr = string_array(scope, &names);
    rv.set(arr.into());
}

/// `allowedFeatures()` — the subsequence of `features()` this policy permits for the document's own
/// origin. Order is preserved: Chrome's `allowedFeatures()` on a plain document is `features()`
/// element for element.
fn cb_allowed_features(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let this = args.this();
    let origin = match container_of(scope, this) {
        Some(_) => document_origin(scope),
        None => super::realm_document_origin(scope),
    };
    let mut out = Vec::new();
    for (name, _) in REGISTRY {
        if resolve(scope, this, name).is_some_and(|l| l.admits(&origin)) {
            out.push((*name).to_string());
        }
    }
    let arr = string_array(scope, &out);
    rv.set(arr.into());
}

/// WebIDL `DOMString` for argument 0, and the origin argument's origin serialisation.
fn arg_string(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> String {
    args.get(i).to_string(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default()
}

/// `getAllowlistForFeature(feature)` — `["*"]`, the list of origins, or `[]` for a feature this UA
/// does not have (measured: an unknown name answers the empty list rather than throwing).
fn cb_get_allowlist(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if !require_args(scope, &args, 1, "PermissionsPolicy", "getAllowlistForFeature") {
        return;
    }
    let feature = arg_string(scope, &args, 0);
    let this = args.this();
    let items = resolve(scope, this, &feature).map(|l| l.serialise()).unwrap_or_default();
    let arr = string_array(scope, &items);
    rv.set(arr.into());
}

/// `allowsFeature(feature, origin?)` — whether the allowlist admits `origin`, defaulting to the
/// document's own. An unknown feature is `false`.
fn cb_allows_feature(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if !require_args(scope, &args, 1, "PermissionsPolicy", "allowsFeature") {
        return;
    }
    let feature = arg_string(scope, &args, 0);
    let asked = if args.length() >= 2 && !args.get(1).is_undefined() {
        let raw = arg_string(scope, &args, 1);
        super::origin_token(&raw).unwrap_or(raw)
    } else if container_of(scope, args.this()).is_some() {
        document_origin(scope)
    } else {
        super::realm_document_origin(scope)
    };
    let this = args.this();
    let ok = resolve(scope, this, &feature).is_some_and(|l| l.admits(&asked));
    rv.set_bool(ok);
}

/// `document.featurePolicy` — the row that replaced the `stub` (L22 E9).
pub(crate) fn cb_document_feature_policy(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match document_feature_policy(scope) {
        Some(o) => rv.set(o.into()),
        None => rv.set_null(),
    }
}

/// `iframe.featurePolicy` / `frame.featurePolicy` — the container policy.
pub(crate) fn cb_element_feature_policy<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue<'s>) {
    let el = args.this();
    match container_feature_policy(scope, el) {
        Some(o) => rv.set(o.into()),
        None => rv.set_null(),
    }
}

/// Mount one `FeaturePolicy` row. Each method carries the interface prototype as its `data`, which
/// is the brand; none of them takes the installer's arity guard, because the receiver check is
/// first.
pub fn install_feature_policy(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let f = match member.name {
        "allowedFeatures" => crate::install::kits::proto_data_fn(scope, proto, cb_allowed_features, member.name, member.length as i32),
        "allowsFeature" => crate::install::kits::proto_data_fn(scope, proto, cb_allows_feature, member.name, member.length as i32),
        "features" => crate::install::kits::proto_data_fn(scope, proto, cb_features, member.name, member.length as i32),
        "getAllowlistForFeature" => crate::install::kits::proto_data_fn(scope, proto, cb_get_allowlist, member.name, member.length as i32),
        other => {
            debug_assert!(false, "install_feature_policy: unexpected member {other}");
            return;
        }
    };
    crate::install::kits::define_value(scope, proto, member, f.into(), true);
}

webapi::interface! {
    // Chrome 152 renamed the interface (`feature_policy.idl`: `interface PermissionsPolicy`,
    // `[LegacyWindowAlias=FeaturePolicy]`): the same four prototype methods, in this order, with these
    // lengths, all enumerable + configurable + writable, and a non-constructible interface object of
    // length 0 named and tagged "PermissionsPolicy". `window.FeaturePolicy` is the same object
    // (`core/html/window_extras.rs`'s legacy window aliases).
    "PermissionsPolicy" [interface, window] {
    ctor   "PermissionsPolicy"     len 0                 = stub,
    method "allowedFeatures"       : Object len 0        = install_feature_policy,
    method "allowsFeature"         : Object len 1        = install_feature_policy,
    method "features"              : Object len 0        = install_feature_policy,
    method "getAllowlistForFeature" : Object len 1       = install_feature_policy
    }
}
