//! The shared member BODIES a table names by word.
//!
//! A row like `attr "alt" : DomString = reflect_str` or `handler "onload" = event_handler` names one
//! of these `InstallFn`s; the `interface!` macro resolves the word to `::webapi::install::kits::<word>`
//! so a table imports nothing. Each kit mounts ONE member from its row -- name, kind, descriptor flags
//! and the `cfg` string -- using the same native callbacks the pre-table glue used, so a member built
//! here is byte- and value-identical to the one it replaces:
//!
//! * `reflect_*`     -- a `[Reflect]` content-attribute accessor (`core/dom/element.rs` callbacks). The
//!                     content attribute is `cfg`'s first field, or the ASCII-lowercase IDL name when
//!                     `cfg` is empty; the numeric kinds read `attr|default|mode|min:max`, `reflect_enum`
//!                     reads `attr|keywords|missing|invalid`, `reflect_nullable_enum`
//!                     `attr|keywords|invalid`.
//! * `event_handler` -- an `on<type>` accessor pair over the native EventTarget handler store.
//! * `slot_*`        -- an accessor over the instance's private slot (`core/events/event.rs`), the
//!                     shape a slot-backed interface's constructor fills (`MessageEvent.data`).
//!
//! Every kit honours the row's descriptor flags (`enumerable`/`configurable`) and, for a method row
//! with `required > 0`, wraps the function in the WebIDL arity guard at install time (W1.4) -- there is
//! no separate arity pass for a table-owned member.

use deno_core::v8;

use super::{MemberDef, MemberKind};
use crate::binding::protos::{build_fn_data, global_get, member_key, str_val};
use crate::core::dom::element::{
    cb_reflect_bool_get, cb_reflect_bool_set, cb_reflect_element_get, cb_reflect_element_set, cb_reflect_enum_get,
    cb_reflect_long_get, cb_reflect_long_set, cb_reflect_nullable_enum_get, cb_reflect_nullable_str_get,
    cb_reflect_nullable_str_set, cb_reflect_str_get, cb_reflect_str_set, cb_reflect_ulong_get, cb_reflect_ulong_set,
    cb_reflect_url_get,
};
use crate::core::events::event::{
    make_evt_get, make_evt_get_null, make_evt_set, make_evt_slot_get, make_evt_slot_get_null, make_evt_slot_setter,
};
use crate::core::events::event_target::{make_et_on_getter, make_et_on_setter};

/// The full config a reflect kit hands its getter: `cfg` verbatim, or the lowercase IDL name.
fn reflect_cfg(member: &MemberDef) -> String {
    if member.cfg.is_empty() { member.name.to_ascii_lowercase() } else { member.cfg.to_string() }
}

/// The bare content-attribute name (the first `|` field of the config) -- what a setter writes.
fn reflect_attr(member: &MemberDef) -> String {
    reflect_cfg(member).split('|').next().unwrap_or("").to_string()
}

fn is_readonly(member: &MemberDef) -> bool {
    matches!(member.kind, MemberKind::Attr { readonly: true } | MemberKind::StaticAttr { readonly: true })
}

/// Define an accessor with the row's descriptor flags. `None` setter = readonly.
pub fn define_accessor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &MemberDef,
    getter: v8::Local<'s, v8::Function>,
    setter: Option<v8::Local<'s, v8::Function>>,
) {
    let setter_v: v8::Local<v8::Value> = match setter {
        Some(s) => s.into(),
        None => v8::undefined(scope).into(),
    };
    let mut d = v8::PropertyDescriptor::new_from_get_set(getter.into(), setter_v);
    d.set_enumerable(member.enumerable);
    d.set_configurable(member.configurable);
    let k = member_key(scope, member.name);
    proto.define_property(scope, k, &d);
}

/// A `[PutForwards=forward]` attribute: `getter` as given, and WebIDL's forwarding setter (WebIDL
/// §3.7.6, "the attribute setter", steps for `[PutForwards]`): after the receiver check, `Q = ? Get(O,
/// id)` -- a property GET, through whatever the page has put on the prototype -- a TypeError when Q is
/// not an object, then `? Set(Q, forward, V)`. Both steps are observable and Chrome 150 makes both
/// (measured 2026-10-08: a wrapped `HTMLIFrameElement.prototype.sandbox` getter and a wrapped
/// `DOMTokenList.prototype.value` setter each fire once for `iframe.sandbox = "..."`; a challenge provider's
/// challenge frame reads the first). The setter writing the content attribute directly made neither.
pub fn put_forwards<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &MemberDef,
    getter: v8::Local<'s, v8::Function>,
    forward: &str,
) {
    // `id` and `forward` as ONE string: a function's data is recorded into the isolate's templates,
    // so it must be a value every realm can share -- never an object of this realm. The
    // receiver check is the row's own template guard, as for every other setter.
    let d = str_val(scope, &format!("{}\u{0}{forward}", member.name));
    let setter = build_fn_data(scope, cb_put_forwards, &format!("set {}", member.name), 1, d);
    define_accessor(scope, proto, member, getter, Some(setter));
}

fn cb_put_forwards(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some((id, fwd)) = data.split_once('\u{0}') else { return };
    let this = args.this();
    let Some(k) = v8::String::new(scope, id) else { return };
    // An exception from the getter is pending when `get` answers None: it propagates as is.
    let Some(q) = this.get(scope, k.into()) else { return };
    let Ok(q) = v8::Local::<v8::Object>::try_from(q) else {
        crate::binding::protos::throw_type_error(scope, &format!("Failed to set the '{id}' property: the forwarded value is not an object."));
        return;
    };
    let Some(f) = v8::String::new(scope, fwd) else { return };
    q.set(scope, f.into(), args.get(0));
}

/// Define a data property with the row's descriptor flags (`writable` as given).
pub fn define_value<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &MemberDef, value: v8::Local<'s, v8::Value>, writable: bool) {
    let mut d = v8::PropertyDescriptor::new_from_value_writable(value, writable);
    d.set_enumerable(member.enumerable);
    d.set_configurable(member.configurable);
    let k = member_key(scope, member.name);
    proto.define_property(scope, k, &d);
}

/// The SAME function object as `Array.prototype[cfg]`, as a writable data property. WebIDL gives an
/// interface with an indexed property getter `Array.prototype`'s value iterator (`@@iterator ===
/// Array.prototype.values`), and an iterable declaration its `entries`/`keys`/`values`/`forEach`
/// (Chrome: `NodeList.prototype.forEach === Array.prototype.forEach`). `cfg` is the `Array.prototype`
/// key; empty = the row's own name.
pub fn array_proto_fn(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let key = if member.cfg.is_empty() { member.name } else { member.cfg };
    let Some(array) = global_get(scope, "Array").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let pk = v8::String::new(scope, "prototype").unwrap();
    let Some(ap) = array.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let k = member_key(scope, key);
    let Some(f) = ap.get(scope, k.into()) else { return };
    define_value(scope, proto, member, f, true);
}

/// `@@iterator` as the SAME function object as this prototype's own `cfg` member (`values` for a setlike,
/// `entries` for a maplike or a pair iterable) -- measured on Brave 151:
/// `FontFaceSet.prototype[Symbol.iterator] === FontFaceSet.prototype.values`,
/// `AudioParamMap.prototype[Symbol.iterator] === AudioParamMap.prototype.entries`. The row is the table's
/// last, so `cfg` already holds the function the page sees (its receiver-check wrapper, which the installer
/// leaves alone the second time).
pub fn own_alias(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let k = member_key(scope, member.cfg);
    let Some(f) = proto.get(scope, k.into()).filter(|v| v.is_function()) else { return };
    define_value(scope, proto, member, f, true);
}

/// `@@unscopables`: a null-prototype object whose own keys are the comma-separated names in `cfg`, each `true`,
/// on a { non-writable, non-enumerable, configurable } slot -- measured on Brave 151:
/// `Element.prototype[Symbol.unscopables]` names `after,append,before,prepend,remove,replaceChildren,
/// replaceWith,slot`. It was missing on the five DOM prototypes that have one, so a `with (element)` block saw
/// `append` as the element's. A template realm makes it per realm (`install::templates`, `Entry::Unscopables`).
pub fn unscopables(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let mut ks: Vec<v8::Local<v8::Name>> = Vec::new();
    for name in member.cfg.split(',').filter(|n| !n.is_empty()) {
        ks.push(v8::String::new(scope, name).unwrap().into());
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    let vs = vec![t; ks.len()];
    let o = v8::Object::with_prototype_and_properties(scope, null, &ks, &vs);
    define_value(scope, proto, member, o.into(), false);
}

/// One reflected accessor: `getter_cb` gets the FULL config as data, `setter_cb` the bare attribute
/// (unless `setter_full_cfg`, for the numeric kinds whose setter parses the same `attr|default|mode`).
fn reflect_pair(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    getter_cb: impl v8::MapFnTo<v8::FunctionCallback>,
    setter_cb: impl v8::MapFnTo<v8::FunctionCallback>,
    setter_full_cfg: bool,
) {
    let cfg = reflect_cfg(member);
    let gdata = str_val(scope, &cfg);
    let getter = build_fn_data(scope, getter_cb, &format!("get {}", member.name), 0, gdata);
    let setter = if is_readonly(member) {
        None
    } else {
        let sd = if setter_full_cfg { cfg.clone() } else { reflect_attr(member) };
        let sdata = str_val(scope, &sd);
        Some(build_fn_data(scope, setter_cb, &format!("set {}", member.name), 1, sdata))
    };
    define_accessor(scope, proto, member, getter, setter);
}

/// `[Reflect] attribute DOMString`: `getAttribute(attr) ?? ""` / `setAttribute(attr, String(v))`.
pub fn reflect_str(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    // HTMLOrSVGElement's `nonce` is no reflection: it reads and writes the element's `[[CryptographicNonce]]`, and the
    // content attribute only feeds it (`korvus_dom::Dom::nonce`). The challenge page set
    // `script.nonce` on two scripts and walked their attributes: Chrome 150 lists none, this engine listed `nonce`.
    if member.name == "nonce" && reflect_attr(member) == "nonce" {
        reflect_pair(scope, proto, member, crate::core::dom::element::cb_nonce_get, crate::core::dom::element::cb_nonce_set, false);
        return;
    }
    reflect_pair(scope, proto, member, cb_reflect_str_get, cb_reflect_str_set, false);
}
/// `[Reflect] attribute DOMString?` (nullable string): absent reads `null`.
pub fn reflect_nullable_str(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_nullable_str_get, cb_reflect_nullable_str_set, false);
}
/// `[Reflect] readonly attribute DOMTokenList` (`relList`, `sandbox`, `sizes`, `blocking`, `htmlFor`):
/// the getter hands back the SAME list every read (a `[SameObject]` view of `cfg`'s attribute) and the
/// setter writes that attribute, which is what `list.value = x` does (`[PutForwards=value]`).
///
/// A kit rather than a per-interface body because a registry ROW needs one too: `SVGAElement.relList` is
/// backfilled from the registry, and when `<a>`'s own table took the member the SVG borrow pass
/// (`install::live::svg`, `dom/09k-svg.js` until W9.1) -- which copies the donor's descriptor at the
/// top of the pre-bootstrap block, before the element lanes below it install their tables -- started
/// copying the D2 stub instead of a working accessor. Measured: SVGAElement lost a `verified` member
/// when W5.1 L1 landed (2026-09-11).
pub fn reflect_tokens(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    // Every `[Reflect]` DOMTokenList is `[PutForwards=value]` (Blink's IDL, all of them).
    let cfg = reflect_cfg(member);
    let gdata = str_val(scope, &cfg);
    let getter = build_fn_data(scope, crate::core::dom::element::cb_el_token_list, &format!("get {}", member.name), 0, gdata);
    put_forwards(scope, proto, member, getter, "value");
}
/// `[ReflectURL]`: the getter parses against the document base URL; the setter is the plain string one.
pub fn reflect_url(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_url_get, cb_reflect_str_set, false);
}
/// `[Reflect] attribute boolean`: `hasAttribute` / toggle presence.
pub fn reflect_bool(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_bool_get, cb_reflect_bool_set, false);
}
/// `[Reflect] attribute unsigned long` (cfg `attr|default|mode|min:max`).
pub fn reflect_ulong(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_ulong_get, cb_reflect_ulong_set, true);
}
/// `[Reflect] attribute long` with a signed default (cfg `attr|default|mode`).
pub fn reflect_long(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_long_get, cb_reflect_long_set, true);
}
/// An ENUMERATED reflection (cfg `attr|keywords|missing|invalid`): the getter limits to the keywords.
pub fn reflect_enum(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_enum_get, cb_reflect_str_set, false);
}
/// A NULLABLE enumerated reflection (cfg `attr|keywords|invalid`): absent reads `null`.
///
/// THE SETTER IS THE NULLABLE ONE. Paired with `cb_reflect_str_set` -- which does
/// `args.get(0).to_rust_string_lossy(scope)` -- `el.crossOrigin = null` wrote the STRING `"null"`,
/// so the content attribute was PRESENT with an invalid value and the getter's invalid-value default
/// answered `"anonymous"` where the spec and Chrome answer `null`. Removing the attribute for
/// null/undefined is exactly what `cb_reflect_nullable_str_set` already does for the `DOMString?`
/// reflections, and a nullable enum's setter differs from it in nothing (the limiting is the
/// getter's job here as it is for `reflect_enum`). Shared by `HTMLScriptElement.crossOrigin`,
/// `HTMLLinkElement.crossOrigin` and `HTMLMediaElement.crossOrigin` -- every row this kit has.
pub fn reflect_nullable_enum(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_nullable_enum_get, cb_reflect_nullable_str_set, false);
}
/// `[Reflect] attribute Element?` (an attr-element reference such as `popoverTargetElement`).
pub fn reflect_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    reflect_pair(scope, proto, member, cb_reflect_element_get, cb_reflect_element_set, false);
}

/// `readonly attribute SVGAnimated<T>` -- one animated SVG content attribute (`rect.x`,
/// `linearGradient.gradientUnits`). `cfg` is `kind|attr|keywords`, the row the generator built from
/// the webref IDL type and the interface's own constants; the body is
/// `core/dom/svg_values.rs`, which mints a `[SameObject]` wrapper with NO own properties (Chrome has
/// none either) reading THROUGH to the attribute.
///
/// A kit rather than a per-interface body because 282 rows across 65 interfaces name it -- the second
/// table (`generated_svg::SVG_ANIMATED`) and the second installer (`op_install_svg_animated`) this
/// replaces existed only to carry them (goal invariants I1 and I2).
pub fn svg_animated(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::dom::svg_values::install_svg_animated(scope, proto, member);
}
/// One member of an SVG VALUE TYPE (`SVGLength.value`, `SVGAnimatedLength.baseVal`, `SVGRect.x`, a
/// list's `numberOfItems`/`getItem`). Which interface the member belongs to is decided by the
/// RECEIVER's own source slot, so one kit serves all twenty-one of them; see `core/dom/svg_values.rs`.
pub fn svg_value(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::dom::svg_values::install_svg_value(scope, proto, member);
}
/// `svg.createSVGLength()` and its five siblings: a DETACHED value object of the interface `cfg`
/// names, at its IDL initial value.
pub fn svg_create_value(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::dom::svg_values::install_svg_create_value(scope, proto, member);
}
/// `readonly attribute SVGStringList requiredExtensions` / `systemLanguage` -- a `[SameObject]` live
/// view of the content attribute of the same name (`core/dom/svg_string_list.rs`).
///
/// A kit rather than a per-interface body because four REGISTRY interfaces declare the pair
/// (SVGGraphicsElement, SVGPatternElement, SVGMaskElement, SVGAnimationElement) and none of the four
/// is an `interface!` table, so the rows can only reach a body through the generator's own claim --
/// which `tools/sync/gen_shells.py` derives by scanning the capture for the two member names.
pub fn svg_string_list_attr(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::dom::svg_string_list::install_svg_string_list_attr(scope, proto, member);
}
/// The SVG geometry and timeline members pages call -- `getBBox`, `getTotalLength`,
/// `getPointAtLength`, `ownerSVGElement`, `pauseAnimations`/`unpauseAnimations`/`animationsPaused`
/// (`core/dom/svg_geometry.rs`). A kit because their interfaces are REGISTRY rows the
/// generator claims by name (`tools/sync/gen_shells.py`), for the reason `svg_string_list_attr` is one.
pub fn svg_geometry(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::dom::svg_geometry::install_svg_geometry(scope, proto, member);
}

/// `OffscreenCanvas`'s three implemented rows -- `width`, `height` and `getContext`
/// (`core/html/html_canvas_element.rs`).
///
/// A kit for the same reason `svg_string_list_attr` is one, one clause further: the interface is a
/// REGISTRY row and not an `interface!` table, so its rows can only reach a body through the
/// generator's claim. It stays a registry row deliberately -- it is `[Exposed=(Window,Worker)]` and
/// sits in both realms' capture orders, and `globalThis` insertion order is fingerprint surface, so
/// moving it out means re-creating it in position in two realms. Before this claim
/// `wire_canvas_oracle` defined all three over the rows afterwards, which is the second-writer
/// arrangement invariant I2 ends: `installer::audit_surface` reported all three `clobber:engine`, the
/// last three of W7.4's twelve.
/// `OffscreenCanvasRenderingContext2D`'s rows over `CanvasRenderingContext2D`'s bodies (a mixin
/// claimed by `tools/sync/gen_shells.py`). The bodies key on the context object, so the same callback serves
/// either receiver; the brand is the row's own prototype.
pub fn canvas_2d(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::html::canvas_2d::install_canvas_2d(scope, proto, member);
}

pub fn offscreen_canvas(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::html::html_canvas_element::install_offscreen_canvas(scope, proto, member);
}

/// `ImageBitmap`'s three rows -- `width`, `height`, `close` -- over the bitmap store of
/// `core/html/canvas_image.rs`. A registry row for the same reason `OffscreenCanvas` is.
pub fn image_bitmap(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::html::canvas_image::install_image_bitmap(scope, proto, member);
}

/// `CSSAnimation.animationName` and `CSSTransition.transitionProperty`
/// (`core/animation/css.rs`).
///
/// A kit for the same reason `offscreen_canvas` is one: both interfaces are REGISTRY rows and stay
/// that way, because they sit in the capture's own-global order and `globalThis` insertion order is
/// fingerprint surface. The claim is also what puts `CSSAnimation.prototype` and
/// `CSSTransition.prototype` into the animation store's prototype registry, so a CSS animation can
/// be minted wearing the right interface without reading `globalThis.CSSAnimation`, which a page can
/// overwrite. Until it, both rows were stubs answering `""` -- which is what a page saw for every
/// animation a stylesheet declared, since there were none to ask.
pub fn css_animation(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::animation::css::install_css_animation(scope, proto, member);
}

/// The grouping and at-rule CSSOM interfaces' members (`CSSMediaRule.media`, `CSSGroupingRule.cssRules`,
/// `CSSImportRule.styleSheet`, …). A kit for the same reason `css_animation` is one: the
/// eleven interfaces sit in the capture's own-global order, so they stay REGISTRY rows and the bodies
/// arrive through this claim (`core/css/css_rule.rs::install_rule_kit`).
pub fn css_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::css::css_rule::install_rule_kit(scope, proto, member);
}

/// `CaretPosition`'s three members (`core/dom/geometry_queries.rs`). A registry row for
/// the OffscreenCanvas reason: its global sits in the capture's own-global order. `offsetNode` and
/// `offset` are the branded slot getters a minted position fills; `getClientRect` is a branded method.
pub fn caret_position(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::dom::geometry_queries::install_caret_position(scope, proto, member);
}

/// `WebSocket`'s eight rows with a body -- `url`, `readyState`, `bufferedAmount`, `extensions`,
/// `protocol`, `binaryType`, `close`, `send` (`modules/websocket/web_socket.rs`).
///
/// A kit for the reason `offscreen_canvas` is one: the interface is a REGISTRY row in both realms'
/// capture orders, and `globalThis` insertion order is fingerprint surface, so the generator claims
/// the members (`tools/sync/gen_shells.py`) instead of a hand table re-creating the interface out of
/// position.
pub fn websocket(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::modules::websocket::web_socket::install_websocket(scope, proto, member);
}

/// An `on<type>` event-handler accessor pair over the native EventTarget handler store. The event
/// type is the member name without its `on` prefix.
pub fn event_handler(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let ty = member.name.strip_prefix("on").unwrap_or(member.name);
    let g = make_et_on_getter(scope, ty);
    let s = make_et_on_setter(scope, ty);
    define_accessor(scope, proto, member, g, Some(s));
}

/// A slot-backed accessor: the getter reads the instance's private slot named after the member
/// (`null`, not `undefined`, when the row is `nullable`); the setter writes it unless the row is
/// readonly.
pub fn slot_get_set(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let gname = format!("get {}", member.name);
    let g = if member.nullable { make_evt_slot_get_null(scope, &gname, member.name) } else { make_evt_slot_get(scope, &gname, member.name) };
    let s = if is_readonly(member) { None } else { Some(make_evt_slot_setter(scope, &format!("set {}", member.name), member.name)) };
    define_accessor(scope, proto, member, g, s);
}
/// A readonly slot-backed getter (`undefined` when unset).
pub fn slot_get(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let g = make_evt_slot_get(scope, &format!("get {}", member.name), member.name);
    define_accessor(scope, proto, member, g, None);
}
/// A readonly slot-backed getter that reads `null` when unset (a nullable IDL attribute).
pub fn slot_get_null(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let g = make_evt_slot_get_null(scope, &format!("get {}", member.name), member.name);
    define_accessor(scope, proto, member, g, None);
}

// ── the BRANDED slot kits ───────────────────────────────────────────────────────────
//
// `slot_*` above answers `undefined`/`null` for a receiver that never ran the interface's
// constructor. Every browser throws `TypeError: Illegal invocation` there instead, and the event
// family's constructors are native, so they have no excuse to skip the brand check (the unit brief's
// failure mode 2). `evt_*` is the same slot read behind `core/events/ctor.rs`'s per-interface brand.
//
// The interface is the row's own `label` prefix, so a row cannot name a brand its table does not own.
// The SLOT is `cfg` when the row carries one and the member name otherwise -- which is how
// `MouseEvent.x` reads the `clientX` slot and `MouseEvent.y` the `clientY` one, the two aliases the
// capture records.

fn evt_iface(member: &MemberDef) -> &'static str {
    member.label.split("::").next().unwrap_or(member.name)
}
fn evt_slot(member: &MemberDef) -> &'static str {
    if member.cfg.is_empty() { member.name } else { member.cfg }
}

/// The origin private file system, `core/fileapi/opfs.rs`.
pub fn opfs(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::fileapi::opfs::install(scope, proto, member);
}

/// `core/navigator/answers.rs`.
pub fn promise_answers(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::navigator::answers::install(scope, proto, member);
}

/// `RemotePlayback.state`: `"disconnected"` -- this engine casts to nothing -- behind the
/// interface's brand, which only `media.remote` stamps.
/// `EditContext`'s members: `core/html/edit_context.rs`.
pub fn edit_context(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::html::edit_context::install(scope, proto, member);
}

pub fn remote_playback(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    fn cb_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
        if !crate::install::webidl::has_brand(scope, args.this().into(), "RemotePlayback") {
            crate::binding::protos::throw_type_error(scope, "Illegal invocation");
            return;
        }
        rv.set(crate::binding::protos::str_val(scope, "disconnected"));
    }
    let g = crate::binding::protos::build_fn(scope, cb_state, &format!("get {}", member.name), 0);
    define_accessor(scope, proto, member, g, None);
}

/// `tabIndex` on `SVGElement` and `MathMLElement`: the same accessor as `HTMLElement`'s,
/// whose default depends on the element (`core/html/html_element.rs` `default_tab_index`) -- the
/// generic long reflection answered 0 for every SVG element, where Chrome answers -1 but for `<a>`.
pub fn tab_index(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let g = crate::binding::protos::build_fn(scope, crate::core::html::html_element::cb_he_tab_index_get, &format!("get {}", member.name), 0);
    let s = crate::binding::protos::build_fn(scope, crate::core::html::html_element::cb_he_tab_index_set, &format!("set {}", member.name), 1);
    define_accessor(scope, proto, member, g, Some(s));
}

/// `StorageEvent`'s members: `core/events/event.rs`.
pub fn storage_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::events::event::install_storage_event(scope, proto, member);
}

/// Whether the row is a `FrozenArray<T>` attribute Chrome hands out FRESH on every read -- Blink's rule for it
/// (`install::frozen_array`), which decides between the slot getters below whichever of them the row names: the
/// generator picked by `[SameObject]` alone, and `FontFaceSetLoadEvent.fontfaces` is `[SameObject]` and still a new
/// array per read in Chrome 150, its getter returning the vector by value (C2).
fn fresh_frozen_row(member: &MemberDef) -> Option<bool> {
    use crate::install::frozen_array::{row, Cache};
    let r = row(evt_iface(member), member.name)?;
    Some(r.cache == Cache::Fresh)
}

/// A branded readonly slot getter (`undefined` when unset). A FrozenArray row Blink hands out fresh gets a new
/// frozen copy per read ([`fresh_frozen_row`]); a kept one is the stored array, which its constructor froze.
pub fn evt_get(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if fresh_frozen_row(member) == Some(true) {
        return evt_get_fresh_frozen(scope, proto, member);
    }
    let g = make_evt_get(scope, &format!("get {}", member.name), evt_iface(member), evt_slot(member));
    define_accessor(scope, proto, member, g, None);
}

/// A branded readonly getter for a `FrozenArray<T>` attribute Blink hands out fresh: a new frozen copy of the
/// stored array on every read (`core/events/event.rs`). A row Blink's table says is KEPT reads the stored array
/// itself ([`fresh_frozen_row`]).
pub fn evt_get_fresh_frozen(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let g = if fresh_frozen_row(member) == Some(false) {
        make_evt_get(scope, &format!("get {}", member.name), evt_iface(member), evt_slot(member))
    } else {
        crate::core::events::event::make_evt_get_fresh_frozen(scope, &format!("get {}", member.name), evt_iface(member), evt_slot(member))
    };
    define_accessor(scope, proto, member, g, None);
}

/// A branded readonly slot getter for a NULLABLE attribute (`null` when unset); a FrozenArray row Blink hands out
/// fresh copies its stored array per read, as [`evt_get`].
pub fn evt_get_null(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if fresh_frozen_row(member) == Some(true) {
        return evt_get_fresh_frozen(scope, proto, member);
    }
    let g = make_evt_get_null(scope, &format!("get {}", member.name), evt_iface(member), evt_slot(member));
    define_accessor(scope, proto, member, g, None);
}

/// A branded `on<type>` handler pair. The same store `event_handler` uses, behind the interface's
/// receiver check: Chrome throws `Illegal invocation` for `Notification.prototype.onclick.call({})`
/// (measured), where the plain kit answers `null`. Only an interface whose constructor stamps a brand
/// may name this — the generic `event_handler` stays the body for everything else.
pub fn evt_handler(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::events::ctor::brand_accessor;
    let iface = evt_iface(member);
    let ty = member.name.strip_prefix("on").unwrap_or(member.name);
    let g = make_et_on_getter(scope, ty);
    let s = make_et_on_setter(scope, ty);
    let gg = brand_accessor(scope, iface, &format!("get {}", member.name), 0, g);
    let ss = brand_accessor(scope, iface, &format!("set {}", member.name), 1, s);
    define_accessor(scope, proto, member, gg, Some(ss));
}

/// A branded read/write slot accessor (`BeforeUnloadEvent.returnValue`).
pub fn evt_get_set(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = evt_iface(member);
    let slot = evt_slot(member);
    let g = if member.nullable {
        make_evt_get_null(scope, &format!("get {}", member.name), iface, slot)
    } else {
        make_evt_get(scope, &format!("get {}", member.name), iface, slot)
    };
    let s = if is_readonly(member) { None } else { Some(make_evt_set(scope, &format!("set {}", member.name), iface, slot)) };
    define_accessor(scope, proto, member, g, s);
}

/// A readonly-attribute row with the prototype bound as the body's `data`, so the body can brand-check
/// (folded from identical copies; the data is
/// [`proto_data_fn`]'s, whose template form resolves per realm).
pub(crate) fn brand_getter(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let g = proto_data_fn(scope, proto, cb, &format!("get {}", member.name), 0);
    define_accessor(scope, proto, member, g, None);
}

/// A function whose body brand-checks against the prototype it is mounted on, carried as its `data` (the
/// F3 shape; [`branded`] reads it back). While a template capture runs it declares that form, so a realm
/// that instantiates the interface's template gets its OWN prototype, through the template
/// (`install::templates`).
pub(crate) fn proto_data_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    name: &str,
    length: i32,
) -> v8::Local<'s, v8::Function> {
    // Re-anchored through a `Global` so the function's data carries the SCOPE's lifetime: an install
    // receives `proto` with a lifetime of its own.
    let held = v8::Global::new(scope, proto);
    let data: v8::Local<v8::Value> = v8::Local::new(scope, &held).into();
    let f = build_fn_data(scope, cb, name, length, data);
    if crate::install::templates::capturing() {
        crate::install::templates::declare(scope, f, crate::install::templates::Form::ProtoData);
    }
    f
}

/// A read-write-attribute row whose getter and setter each take a string `data` (folded by
/// identical copies).
#[allow(clippy::too_many_arguments)]
pub(crate) fn str_data_accessor(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
    gdata: &str,
    scb: impl v8::MapFnTo<v8::FunctionCallback>,
    sdata: &str,
) {
    let d = str_val(scope, gdata);
    let g = build_fn_data(scope, gcb, &format!("get {}", m.name), 0, d);
    let d = str_val(scope, sdata);
    let s = build_fn_data(scope, scb, &format!("set {}", m.name), 1, d);
    define_accessor(scope, proto, m, g, Some(s));
}

/// A readonly-attribute row whose getter's `data` is a string -- the shape `webrtc` / `media` rows share
/// (folded from identical copies).
pub(crate) fn str_data_getter(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    m: &'static MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
    gdata: &str,
) {
    let d = str_val(scope, gdata);
    let g = build_fn_data(scope, gcb, &format!("get {}", m.name), 0, d);
    define_accessor(scope, proto, m, g, None);
}

/// Whether `this` has the prototype bound as the body's `data` on its chain -- the brand check of a row
/// mounted with the prototype as its data (folded from identical copies).
///
/// The data is the prototype itself where the row was mounted by the ordinary install, or -- in a realm
/// that instantiated the interface from its template -- a reference to the interface's
/// template, which resolves to THIS realm's prototype (`install::templates::realm_proto_of`).
pub(crate) fn branded(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    let data = crate::binding::protos::body_data(scope, args);
    // a template's form: its record's ID (a Smi, for the context snapshot) or, in an analysis build, an External --
    // both resolved by `realm_proto_of`; the ordinary install's data is the prototype itself
    let want = if data.is_int32() || data.is_external() {
        match crate::install::templates::realm_proto_of(scope, data) {
            Some(p) => p,
            None => return false,
        }
    } else {
        let Ok(o) = v8::Local::<v8::Object>::try_from(data) else { return false };
        o
    };
    let mut cur = args.this().get_prototype(scope);
    while let Some(p) = cur {
        let Ok(o) = v8::Local::<v8::Object>::try_from(p) else { return false };
        if o == want {
            return true;
        }
        cur = o.get_prototype(scope);
    }
    false
}
