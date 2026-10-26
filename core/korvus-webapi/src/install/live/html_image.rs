//! `HTMLImageElement` and the legacy `Image` factory.
//!
//! The prototype is built from the table by the same [`installer::create_interface`] the registry
//! uses, spliced onto the real `HTMLElement` chain; what is here is the wiring the table cannot
//! express -- the legacy `Image` factory that shares the prototype. (The `img` tag re-registration
//! that used to be here is gone: `install::dom_chain::fill_dom_protos` maps every tag
//! `korvus_dom::tags` names, `img` included, and it runs after this.)

use deno_core::v8;

use crate::core::html::html_image_element::{cb_image_ctor, HTMLIMAGEELEMENT};
use crate::install::installer::{self, Flavor};

/// Build `HTMLImageElement` from its `interface!` table and make it the live window authority (Phase 3
/// Track C). Runs at context creation AFTER the bootstrap ESM layer, so it OVERWRITES the generated
/// shell + `08b`/`08k`/`09i` glue: it
///   1. mounts the 27-member prototype (native CSSOM/loader ops + the reflect kits, in table order)
///      spliced onto the REAL `HTMLElement.prototype` (-> Element -> Node -> EventTarget), and sets the
///      non-newable `HTMLImageElement` interface object (`new HTMLImageElement()` throws "Illegal
///      constructor") on the global;
///   2. installs the legacy `Image` factory -- a native constructable whose non-writable `.prototype`
///      IS `HTMLImageElement.prototype`, so `new Image() instanceof HTMLImageElement` holds.
/// `tests/html_image_live.rs` proves the table reproduces the old surface byte- AND value-identically.
pub fn install_window_html_image_element<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    

    // Parent: the real HTMLElement ctor + prototype the spine built. The splice gives the recorded
    // chain HTMLImageElement.prototype -> HTMLElement -> Element -> Node -> EventTarget.
    let he = crate::binding::protos::iface_object(scope, "HTMLElement").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    let parent = he.and_then(|he| {
        let pk = v8::String::new(scope, "prototype")?;
        let hep = he.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        Some((hep, he))
    });

    let mut ledger = installer::take_ledger(scope);
    crate::core::events::install_event_model(scope, &mut ledger, false);
    let built = installer::create_interface(scope, global, &HTMLIMAGEELEMENT, "HTMLImageElement", parent, None, Flavor::Table, &mut ledger);
    installer::put_ledger(scope, ledger);
    let proto = built.proto;
    for g in &built.gaps {
        eprintln!("[korvus] html_image install gap: {}.{}", g.interface, g.member);
    }

    // NOTHING RE-REGISTERS THE `img` TAG HERE. It used to: this function wrote
    // `TagProtos["img"] = proto` by hand, the one survivor of the per-tag hand writes the `<table>`
    // and `<dialog>` lanes had already retired. It ran BEFORE
    // [`crate::install::dom_chain::fill_dom_protos`], which reads `globalThis[iface].prototype` for
    // every tag `korvus_dom::tags` names and writes the SAME map -- so the hand write was overwritten
    // by the table pass with the identical prototype, and `img` reaches this one through the ONE
    // mechanism like the other 139 HTML tags.

    // globalThis.Image: the legacy factory. A native constructable whose non-writable `.prototype` IS
    // HTMLImageElement.prototype (so `new Image() instanceof HTMLImageElement` and `Image.prototype`
    // reads the 27 members). Overwrites the shell's `Image`, exactly as 09i-singletons.js did.
    // `length` FROM THE ROW, not from this call site. It was the literal `2` here, and
    // `Image.length` answered 2 against a capture -- and a headless Chrome 150 -- that both say 0.
    install_image_factory(scope, global, proto);
}

/// `globalThis.Image` over `proto` (`HTMLImageElement.prototype` of the realm) -- also what a realm built
/// from templates makes for itself (`core/html/browsing_context.rs`).
pub fn install_image_factory<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, proto: v8::Local<'s, v8::Object>) {
    // From the isolate's template, its data `build_interface_ctor`'s "<name>|<required>".
    let len = installer::row_ctor_length("Image");
    let image = crate::binding::protos::template_ctor(scope, cb_image_ctor, "Image", len, Some(&format!("Image|{len}")));
    if let Some(pk) = v8::String::new(scope, "prototype") {
        image.define_own_property(
            scope,
            pk.into(),
            proto.into(),
            v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::DONT_DELETE,
        );
    }
    {
        crate::install::installer::publish_on_global(scope, global, "Image", image.into());
    }
}
