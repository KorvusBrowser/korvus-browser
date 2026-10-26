//! `core/html/html_image_element.rs` — HTMLImageElement interface implementation

use deno_core::v8;
use crate::binding::protos::{build_fn, define_method, str_val, throw_type_error, global_get};
use crate::core::dom::element::{el_id_of};
use crate::install::MemberDef;

/// `HTMLImageElement.x` — the x coordinate of the element's left border edge.
///
/// Per CSSOM View, this is 0 when the element has no layout box, which is always true
/// for Korvus's DOM (no rendering/layout engine).
pub(crate) fn cb_image_x_get(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(v8::Integer::new(scope, 0).into());
}

/// `HTMLImageElement.y` — the y coordinate of the element's top border edge.
///
/// Per CSSOM View, this is 0 when the element has no layout box, which is always true
/// for Korvus's DOM (no rendering/layout engine).
pub(crate) fn cb_image_y_get(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(v8::Integer::new(scope, 0).into());
}

/// `HTMLImageElement.width` — the displayed width of the image in CSS pixels.
///
/// When there's no layout object, the spec says to return the width attribute's value
/// if it's a valid non-negative integer, otherwise return the image's natural width.
/// Korvus has no layout engine or image loader, so we return 0 (the fallback).
pub(crate) fn cb_image_width_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = el_id_of(scope, args.this()).map_or(0, |id| image_dimension(scope, id, 0));
    rv.set(v8::Integer::new_from_unsigned(scope, v).into());
}

/// `width` / `height` of an `<img>`: the RENDERED size when the image generates a box, and
/// otherwise the attribute when it parses, and otherwise the image's natural size (Blink's
/// `HTMLImageElement::width()` order; the spec names the rendered and the natural sizes). They read the
/// attribute or 0 -- this engine "had no layout engine or image loader" -- so a loaded 120x40 logo
/// with no attributes read 0 wide, and a carousel sized from it never grew.
fn image_dimension(scope: &mut v8::PinScope, id: u32, axis: usize) -> u32 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let rendered = crate::core::dom::geometry_queries::element_content_box(&mut op_state.borrow_mut(), id);
    if let Some((w, h)) = rendered {
        return (if axis == 0 { w } else { h }).max(0.0).round() as u32;
    }
    let attr = op_state
        .borrow()
        .borrow::<crate::binding::host_dom::HostDom>()
        .0
        .get_attribute(id, if axis == 0 { "width" } else { "height" })
        .and_then(html_non_negative_integer);
    if let Some(v) = attr {
        return v;
    }
    super::canvas_image::img_natural_size(scope, id).map_or(0, |(w, h)| if axis == 0 { w } else { h })
}

/// HTML's "rules for parsing non-negative integers", which Blink's `width()`/`height()` read the
/// attribute with (`ParseHTMLNonNegativeInteger`): leading white space and a `+` skipped, then the
/// digits up to the first non-digit -- `width="30px"` is 30 on a detached image. It parsed the whole
/// trimmed value, so anything with a unit read as no attribute at all.
fn html_non_negative_integer(value: &str) -> Option<u32> {
    let text = value.trim_start_matches([' ', '\t', '\n', '\u{c}', '\r']);
    let text = text.strip_prefix('+').unwrap_or(text);
    let end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    text[..end].parse::<u64>().ok().filter(|n| *n <= i32::MAX as u64).map(|n| n as u32)
}

/// `HTMLImageElement.width` setter — sets the width content attribute.
///
/// Note: Blink stores this as a reflected attribute, but WebIDL shows it as non-reflected.
/// We set the attribute content string (which Blink would parse later).
pub(crate) fn cb_image_width_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => return };
    let val = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_attribute(id, "width", &val);
}

/// `HTMLImageElement.height` -- see [`image_dimension`].
pub(crate) fn cb_image_height_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = el_id_of(scope, args.this()).map_or(0, |id| image_dimension(scope, id, 1));
    rv.set(v8::Integer::new_from_unsigned(scope, v).into());
}

/// `HTMLImageElement.height` setter — sets the height content attribute.
pub(crate) fn cb_image_height_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => return };
    let val = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_attribute(id, "height", &val);
}

/// `HTMLImageElement.naturalWidth` — the intrinsic width of the image in CSS pixels.
///
/// Korvus has no image loader, so we return 0. This matches the behavior when an image
/// has no available image data.
pub(crate) fn cb_image_natural_width_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let w = el_id_of(scope, args.this()).and_then(|id| super::canvas_image::img_natural_size(scope, id)).map(|(w, _)| w).unwrap_or(0);
    rv.set_uint32(w);
}

/// `HTMLImageElement.naturalHeight` — the intrinsic height of the image in CSS pixels.
///
/// Korvus has no image loader, so we return 0.
pub(crate) fn cb_image_natural_height_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let h = el_id_of(scope, args.this()).and_then(|id| super::canvas_image::img_natural_size(scope, id)).map(|(_, h)| h).unwrap_or(0);
    rv.set_uint32(h);
}

/// `HTMLImageElement.complete` — whether the image has finished loading.
///
/// `true` when the element has no request (no `src`/`srcset`) or its request has settled (loaded or
/// broken); `false` while a request is in flight. The loading state is tracked by the element loader
/// (`subresource::image_is_complete`). Measured, Chrome 150 (`ACC-img-complete`): `new Image()` reads
/// `true`, assigning `src` makes it `false` synchronously, and it returns to `true` on the request's
/// `load`/`error`.
pub(crate) fn cb_image_complete_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let complete = el_id_of(scope, args.this())
        .map(|id| crate::core::html::subresource::image_is_complete(scope, id))
        .unwrap_or(true);
    rv.set(v8::Boolean::new(scope, complete).into());
}

/// `HTMLImageElement.currentSrc` — the absolute URL of the selected image source.
pub(crate) fn cb_image_current_src_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // The current request's URL -- its selected `srcset` candidate (or `<picture>` source), else `src` resolved -- once
    // the request has something available (Chrome answers "" before that).
    let url = el_id_of(scope, args.this()).and_then(|id| {
        super::canvas_image::img_natural_size(scope, id)?;
        super::canvas_image::img_url(scope, id)
    });
    let v = str_val(scope, url.as_deref().unwrap_or(""));
    rv.set(v);
}

/// `img.decode()` as Blink answers it (`ImageLoader::Decode`; 2026-10-08, measured on stock Chrome
/// 150): a promise settled once the image's request has -- resolved for an image that has a size, a header
/// that parsed (a PNG whose pixel data cannot decode included: it resolves), rejected with EncodingError "The
/// source image cannot be decoded." for a broken one and for an `<img>` with no source. Called while the request
/// is in flight, it waits for it ([`settle_decodes`], called right after the element's `load`/`error`).
///
/// It REJECTED every call, a loaded image's too: a page awaiting `img.decode()` before drawing saw a failure no
/// browser gives.
pub(crate) fn cb_image_decode(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let promise = resolver.get_promise(scope);
    rv.set(promise.into());
    let id = el_id_of(scope, args.this()).filter(|&id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<crate::binding::host_dom::HostDom>().0;
        dom.get_attribute(id, "src").is_some_and(|s| !s.trim().is_empty()) || dom.get_attribute(id, "srcset").is_some()
    });
    let Some(id) = id else {
        settle(scope, resolver, false);
        return;
    };
    if !crate::core::html::subresource::image_is_complete(scope, id) {
        let g = v8::Global::new(scope, resolver);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if !st.has::<PendingDecodes>() {
            st.put(PendingDecodes::default());
        }
        st.borrow_mut::<PendingDecodes>().0.entry(id).or_default().push(g);
        return;
    }
    let ok = super::canvas_image::img_natural_size(scope, id).is_some();
    settle(scope, resolver, ok);
}

/// The `img.decode()` promises waiting on an image's request, by node.
#[derive(Default)]
pub(crate) struct PendingDecodes(std::collections::HashMap<u32, Vec<v8::Global<v8::PromiseResolver>>>);

fn settle(scope: &mut v8::PinScope, resolver: v8::Local<v8::PromiseResolver>, ok: bool) {
    if ok {
        let u = v8::undefined(scope).into();
        resolver.resolve(scope, u);
    } else if let Some(e) = crate::binding::protos::make_dom_exc(scope, "The source image cannot be decoded.", "EncodingError") {
        resolver.reject(scope, e);
    }
}

/// Image `id`'s request settled -- `available` or broken: settle the `decode()` promises waiting on it.
pub(crate) fn settle_decodes(scope: &mut v8::PinScope, id: u32, available: bool) {
    let waiting = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<PendingDecodes>().and_then(|p| p.0.remove(&id)).unwrap_or_default()
    };
    for g in waiting {
        let r = v8::Local::new(scope, &g);
        settle(scope, r, available);
    }
}
// `crossOrigin` is the CORS-settings reflection every element shares (`kits::reflect_nullable_enum`):
// ABSENT reads null and an invalid value reads "anonymous" -- the hand body that was here
// answered "anonymous" for a fresh <img>, removed an invalid value instead of setting it, and wrote the
// arena directly, so no mutation record or style invalidation saw the change.

/// `HTMLImageElement` constructor — the `Image()` legacy factory function.
///
/// `new Image(width?, height?)` creates an HTMLImageElement with the optional width/height
/// attributes set. The WebIDL shows this as `LegacyFactoryFunction=Image`.
///
/// Note: The HTMLConstructor on HTMLImageElement itself throws "Illegal constructor"
/// (you can't do `new HTMLImageElement()`), only `new Image()` works.
pub(crate) fn cb_image_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // THE NAME IS THE CONSTRUCTOR'S, NOT THE INTERFACE'S. This one callback is `Image`'s
    // body, and `Image` is a legacy FACTORY: Chrome answers a bare `Image()` with
    // `Failed to construct 'Image': Please use the 'new' operator, ...` -- naming the factory, not
    // `HTMLImageElement`, which is only what it produces (measured over loopback, 2026-09-12). The
    // name was spelled here and so was always the wrong one; it is bound as the callback's `data` by
    // `build_interface_ctor` now, like every other interface object's.
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg(&crate::binding::protos::ctor_name(scope, &args));
        throw_type_error(scope, &msg);
        return;
    }

    // document.createElement("img") -- the legacy factory builds a real arena element (so the IDL
    // accessors resolve its node id) and then applies the optional width/height -- through the REALM'S own
    // `createElement` and setters: a factory's steps call nothing a page can wrap
    // (`new Image()` called the page's `document.createElement` and `width` setter; `Audio` and `Option` were
    // fixed the day before).
    let Some(document) = global_get(scope, "document").filter(|v| v.is_object()) else { return };
    let tag: v8::Local<v8::Value> = str_val(scope, "img");
    let Some(img) = crate::binding::protos::pristine_call(scope, document, "Document.createElement", &[tag]) else { return };
    if !img.is_object() {
        return;
    }

    // A PRESENT argument (including `null`) is ToUint32'd and written through the `width` / `height` IDL
    // setters; only a MISSING/`undefined` argument is skipped. `null >>> 0` is 0, and a u32 above 2^31 must
    // round-trip as an unsigned decimal, so the value is a Number.
    let n = args.length();
    for (i, member) in [(0, "HTMLImageElement.width"), (1, "HTMLImageElement.height")] {
        if n > i {
            let v = args.get(i);
            if !v.is_undefined() {
                if let Some(u) = v.uint32_value(scope) {
                    let num: v8::Local<v8::Value> = v8::Number::new(scope, u as f64).into();
                    crate::binding::protos::pristine_set(scope, img, member, num);
                }
            }
        }
    }

    rv.set(img);
}

// ---------------------------------------------------------------------------------------------------
// One-table BODIES. Each of the 27 prototype members installs the SAME
// accessor the pre-cutover glue did, so the live surface stays byte- AND value-identical to the
// capture:
//   * 10 NATIVE members -- the CSSOM/loader reads in this file (`width`, `naturalWidth`, `complete`,
//     `x`, `crossOrigin`, `decode`, ...), mounted from the `cb_image_*` callbacks below (Element
//     receiver = arena node id via `el_id_of`). Each used to carry an `#[op2]` wrapper beside it that
//     handed the same callback to JS; those are gone and this file declares no ops.
//   * 17 REFLECTED members -- the reflect DSL below mounts the SHARED reflect callbacks
//     (`cb_reflect_{str,url,bool,ulong,enum}_*`) over the arena content attributes, with the exact
//     attribute name / enum config `08b-reflected.js` + `08k-image-element.js` + `generated_reflect.js`
//     used. The content attribute is the ASCII-lowercase IDL name for all 17 (`useMap`->`usemap`,
//     `longDesc`->`longdesc`, ...). Every member is enumerable+configurable and a readonly attribute
//     has no setter -- the same descriptor the installer's stub path builds, so the SHAPE gate holds.

/// Define an attribute accessor with the WebIDL interface-attribute descriptor
/// (`{ get, set?, enumerable: true, configurable: true }`); a readonly member passes `None`.
fn define_img_accessor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    name: &str,
    getter: v8::Local<'s, v8::Function>,
    setter: Option<v8::Local<'s, v8::Function>>,
) {
    let setter_v: v8::Local<v8::Value> = match setter {
        Some(s) => s.into(),
        None => v8::undefined(scope).into(),
    };
    let mut desc = v8::PropertyDescriptor::new_from_get_set(getter.into(), setter_v);
    desc.set_enumerable(true);
    desc.set_configurable(true);
    if let Some(k) = v8::String::new(scope, name) {
        proto.define_property(scope, k.into(), &desc);
    }
}

// The 17 REFLECTED members use the shared kits (`install::kits::reflect_*`, W1.3): a row names the
// kit and, for the enumerated ones, carries its `attr|keywords|missing|invalid` config in `cfg`.
// The private reflect DSL that lived here was the first cut of those kits.

/// The native (non-reflected) members: CSSOM/loader reads with an Element receiver. Dispatches on the
/// member name to the `cb_image_*` callback in this file (`decode` is a method; the rest are
/// accessors, three of them writable).
fn install_img_native(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "decode" => {
            let f = build_fn(scope, cb_image_decode, "decode", 0);
            define_method(scope, proto, "decode", f);
        }
        "width" => {
            let g = build_fn(scope, cb_image_width_get, "get width", 0);
            let s = build_fn(scope, cb_image_width_set, "set width", 1);
            define_img_accessor(scope, proto, "width", g, Some(s));
        }
        "height" => {
            let g = build_fn(scope, cb_image_height_get, "get height", 0);
            let s = build_fn(scope, cb_image_height_set, "set height", 1);
            define_img_accessor(scope, proto, "height", g, Some(s));
        }
        "naturalWidth" => {
            let g = build_fn(scope, cb_image_natural_width_get, "get naturalWidth", 0);
            define_img_accessor(scope, proto, "naturalWidth", g, None);
        }
        "naturalHeight" => {
            let g = build_fn(scope, cb_image_natural_height_get, "get naturalHeight", 0);
            define_img_accessor(scope, proto, "naturalHeight", g, None);
        }
        "complete" => {
            let g = build_fn(scope, cb_image_complete_get, "get complete", 0);
            define_img_accessor(scope, proto, "complete", g, None);
        }
        "currentSrc" => {
            let g = build_fn(scope, cb_image_current_src_get, "get currentSrc", 0);
            define_img_accessor(scope, proto, "currentSrc", g, None);
        }
        "x" => {
            let g = build_fn(scope, cb_image_x_get, "get x", 0);
            define_img_accessor(scope, proto, "x", g, None);
        }
        "y" => {
            let g = build_fn(scope, cb_image_y_get, "get y", 0);
            define_img_accessor(scope, proto, "y", g, None);
        }
        other => debug_assert!(false, "install_img_native: unexpected member {other}"),
    }
}

// ---------------------------------------------------------------------------------------------------
// One-table SHAPE (the third proof interface). This is HTMLImageElement's
// union descriptor: the 27 own members of its prototype, in the captured order, with the WebIDL
// descriptor each carries. Bodies are all `stub` for now -- the SHAPE gate below proves the table +
// installer reproduce the captured prototype byte-for-byte; wiring the native ops (this file) and the
// reflect DSL (generated_reflect.js) as the real bodies, plus the deep chain splice onto
// HTMLElement.prototype and the `img` tag re-registration, is the live cutover step.
//
// Exposed=Window only (no worker). Model=element: `new HTMLImageElement()` throws "Illegal
// constructor" (the [HTMLConstructor] path); instances arrive via createElement("img") / new Image().
webapi::interface! {
    "HTMLImageElement" [element, window] {
        ctor "HTMLImageElement" len 0 = stub,
        attr "alt" : DomString = reflect_str,
        attr "src" : UsvString = reflect_url,
        attr "srcset" : UsvString = reflect_str,
        attr "sizes" : DomString = reflect_str,
        attr "crossOrigin" : DomString nullable cfg "crossorigin|anonymous,use-credentials|anonymous" = reflect_nullable_enum,
        attr "useMap" : DomString = reflect_str,
        attr "isMap" : Boolean = reflect_bool,
        attr "width" : Integer = install_img_native,
        attr "height" : Integer = install_img_native,
        attr "naturalWidth" : Integer readonly = install_img_native,
        attr "naturalHeight" : Integer readonly = install_img_native,
        attr "complete" : Boolean readonly = install_img_native,
        attr "currentSrc" : UsvString readonly = install_img_native,
        attr "referrerPolicy" : DomString cfg "referrerpolicy|no-referrer,no-referrer-when-downgrade,same-origin,origin,strict-origin,origin-when-cross-origin,strict-origin-when-cross-origin,unsafe-url||" = reflect_enum,
        attr "decoding" : DomString cfg "decoding|sync,async,auto|auto|auto" = reflect_enum,
        attr "fetchPriority" : DomString cfg "fetchpriority|high,low,auto|auto|auto" = reflect_enum,
        attr "loading" : DomString cfg "loading|lazy,eager|auto|auto" = reflect_enum,
        attr "name" : DomString = reflect_str,
        attr "lowsrc" : UsvString = reflect_url,
        attr "align" : DomString = reflect_str,
        attr "hspace" : Integer cfg "hspace|0" = reflect_ulong,
        attr "vspace" : Integer cfg "vspace|0" = reflect_ulong,
        attr "longDesc" : UsvString = reflect_url,
        attr "border" : DomString = reflect_str,
        attr "x" : Integer readonly = install_img_native,
        attr "y" : Integer readonly = install_img_native,
        method "decode" : Promise len 0 = install_img_native,
        // THE OTHER DEVICE'S ROW(S): what `chrome-152-linux-20261016T1022`
        // records here and `brave-151-linux-20260907T1006` does not. Installed only when the
        // ACTIVE capture lists them (`install::installer::join`), so neither device grows the
        // other's surface; appended rather than interleaved because this table's order is the
        // fallback a realm with no bundle selector walks, not the order a page reads. D2 stubs.
        attr "browsingTopics" : Any = stub,
        attr "attributionSrc" : Any = stub,
        attr "sharedStorageWritable" : Object cfg "sharedstoragewritable" = reflect_bool
    }
}

#[cfg(test)]
mod shape_tests {
    //! The HTMLImageElement one-table SHAPE matches the captured Brave surface (Phase 3 §0.2): the 27
    //! own prototype members, in the captured order, with the captured descriptors (readonly vs
    //! writable accessor, the `decode` method), the non-enumerable `constructor`, the [object
    //! HTMLImageElement] tag, and a non-newable interface object whose `new` throws "Illegal
    //! constructor". The deep prototype chain (HTMLElement/Element/Node/EventTarget) is a live-install
    //! concern -- it needs the real ancestor prototypes -- and the splice mechanism is already proven
    //! by the DOMRect gate; this gate is HTMLImageElement's OWN surface.
    use super::HTMLIMAGEELEMENT;
    use webapi::install::installer::install_constructable;
    use webapi::install::verify::describe_own;
    use deno_core::{v8, RuntimeOptions};

    fn fixture() -> String {
        // The full surface capture: the shipped bundle's `surface` artifact.
        std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()
    }

    #[test]
    fn html_image_element_surface_is_byte_identical_to_the_capture() {
        let fx: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
        let members = fx["interfaces"]["HTMLImageElement"]["members"].as_array().unwrap();
        // Chrome 150's 30 (Brave 151, the persona until 2026-10-16, hid browsingTopics, attributionSrc and
        // sharedStorageWritable: 27).
        assert_eq!(members.len(), 30, "the HTMLImageElement baseline is 30 members");

        // Build the expected describe_own lines and the selector (names in captured order) together.
        let mut expected: Vec<String> = Vec::new();
        let mut selector: Vec<String> = Vec::new();
        for m in members {
            let name = m["name"].as_str().unwrap();
            selector.push(name.to_string());
            match m["kind"].as_str().unwrap() {
                "accessor" => {
                    let s = m["set"].as_bool().unwrap() as u8;
                    expected.push(format!("{name}:accessor:e=1:c=1:s={s}"));
                }
                "method" => expected.push(format!("{name}:data:e=1:c=1:w=1")),
                other => panic!("unexpected kind {other}"),
            }
        }
        expected.push("constructor:data:e=0:c=1:w=1".to_string());
        let selector: Vec<&str> = selector.iter().map(|s| s.as_str()).collect();

        let mut rt = engine::new_runtime(RuntimeOptions::default());
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let global = scope.get_current_context().global(scope);

        // No statics (the fixture's `statics` is empty); no parent for the OWN-surface gate.
        let (proto, _ctor, gaps) =
            install_constructable(scope, global, &HTMLIMAGEELEMENT, "HTMLImageElement", &selector, &[], None);
        assert!(gaps.is_empty(), "table missing selected members: {:?}", gaps.iter().map(|g| &g.member).collect::<Vec<_>>());

        assert_eq!(describe_own(scope, proto), expected, "HTMLImageElement.prototype shape != capture");

        // Non-newable + toStringTag, both part of the surface.
        let src = v8::String::new(
            scope,
            r#"(() => {
                let threw = "";
                try { new HTMLImageElement(); } catch (e) { threw = e.constructor.name + ":" + e.message; }
                return [threw,
                        HTMLImageElement.length,
                        Object.prototype.toString.call(Object.create(HTMLImageElement.prototype))].join("|");
            })()"#,
        ).unwrap();
        let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
        // `new X()` carries Chrome's construction context. `HTMLImageElement` is an
        // `[HTMLConstructor]` interface, so a BARE `HTMLImageElement()` is the prefixed "Please use
        // the 'new' operator" sentence instead -- gated in `korvus-runtime/tests/dom_spine_live.rs`
        // and `tests/installer_parity.rs`, which see the whole surface; this one owns the shape.
        assert_eq!(out, "TypeError:Failed to construct 'HTMLImageElement': Illegal constructor|0|[object HTMLImageElement]");
    }
}
