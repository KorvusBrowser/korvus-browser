//! `install/instance_shapes.rs` -- what an interface's INSTANCES carry beyond its members.
//!
//! An interface with indexed or named properties answers them from an interceptor, and V8 fixes an object's
//! interceptors when the object is made, from the template it is made from. Blink's bindings put them on the
//! interface's instance template, so every instance has them from birth, in every realm; this is the same.
//! The builder (`install::templates::build`) applies [`configure`] to the one instance template each
//! interface has per isolate, before anything instantiates it, and every mint of the interface --
//! `binding::protos::mint_on`, `mint_wrapper_on` -- makes an object with that shape.
//!
//! ONE table, keyed by interface name. The callbacks stay with their families. Each family's interceptors
//! decline (`Intercepted::kNo`) on a holder that lacks the family's own state, so an instance the family did
//! not make -- a subclass instance a page constructs, or one minted by another path -- behaves as an
//! ordinary object. `Object.create(I.prototype)` is never an instance and never reaches them.

use deno_core::v8;

/// What an interface's instances carry beyond their members: THE table (one row per shaped interface).
#[derive(Clone, Copy)]
enum Shape {
    /// a platform list: the indexed interceptor, and the named one too when `named`
    List { named: bool },
    /// `localStorage` / `sessionStorage`: a named interceptor over the holder's area
    Storage,
    /// a CSS declaration: named and indexed interceptors
    Declaration,
    /// a document's named properties, on every document interface
    Document,
    /// `sel[0]`, `sel[3] = new Option()`
    Select,
    /// a form's controls by name and by index
    Form,
    TextTrackList,
    /// `sheet.media[0]`
    MediaList,
    /// `el.dataset`
    DataSet,
    /// `eval` and `new Function` take a TrustedScript as source text
    CodeLike,
    /// `document.all`: a list, undetectable, callable
    All,
    /// `Object.setPrototypeOf(location, x)` throws on every browser of the family
    ImmutableProto,
    /// `points[1]`, `x.baseVal[0]`: an SVG list's items by index
    SvgList,
}

fn shape_of(iface: &str) -> Option<Shape> {
    if let Some(named) = crate::core::dom::collections::list_shape(iface) {
        return Some(Shape::List { named });
    }
    Some(match iface {
        "Storage" => Shape::Storage,
        "CSSStyleDeclaration" => Shape::Declaration,
        "Document" | "HTMLDocument" | "XMLDocument" => Shape::Document,
        "HTMLSelectElement" => Shape::Select,
        "HTMLFormElement" => Shape::Form,
        "TextTrackList" => Shape::TextTrackList,
        "MediaList" => Shape::MediaList,
        "DOMStringMap" => Shape::DataSet,
        "TrustedScript" => Shape::CodeLike,
        "HTMLAllCollection" => Shape::All,
        "Location" => Shape::ImmutableProto,
        "SVGPointList" | "SVGLengthList" | "SVGNumberList" | "SVGTransformList" => Shape::SvgList,
        _ => return None,
    })
}

/// Whether instances of `iface` carry anything beyond their members (an interceptor, a V8 mark).
pub(crate) fn has_shape(iface: &str) -> bool {
    shape_of(iface).is_some()
}

/// Give interface `iface`'s instance template `it` the shape its instances need, if any.
pub(crate) fn configure(scope: &mut v8::PinScope, iface: &str, it: v8::Local<v8::ObjectTemplate>) {
    let Some(shape) = shape_of(iface) else { return };
    match shape {
        Shape::List { named } => crate::core::dom::collections::configure_list(it, named),
        Shape::Storage => crate::modules::storage::storage::configure(it),
        Shape::Declaration => crate::core::css::css_style_declaration::configure(it),
        Shape::Document => it.set_named_property_handler(crate::core::dom::document::doc_named_config()),
        Shape::Select => it.set_indexed_property_handler(crate::core::html::html_select_element::select_indexed_config()),
        Shape::Form => {
            it.set_named_property_handler(crate::core::html::html_form_element::form_named_config());
            it.set_indexed_property_handler(crate::core::html::html_form_element::form_indexed_config());
        }
        Shape::TextTrackList => crate::core::html::text_tracks::configure(it),
        Shape::MediaList => crate::core::css::style_sheet::configure_media_list(it),
        Shape::DataSet => crate::binding::protos::configure_dataset(it),
        Shape::CodeLike => crate::core::trusted_types::tables::configure_code_like(it),
        Shape::All => crate::core::dom::collections::configure_all(scope, it),
        Shape::ImmutableProto => it.set_immutable_proto(),
        Shape::SvgList => crate::core::dom::svg_values::configure_list(it),
    }
}

/// Declare on interface `iface`'s instance template, as `undefined`, the private slots the engine writes on every
/// instance it mints (`collections::engine_slot_names`, a MutationRecord's value slot): an instance is born with
/// them, so each write is a store into its own slot rather than a property added, and a map transition, per mint.
/// A private is invisible to every reflection path and to freezing, and `get_private` reads an absent one as
/// `undefined`, so a slot declared and not yet written is what an absent one was to every reader. Never a key read
/// with `has_private` or removed with `delete_private` (the brands, `korvus_iface`). Not part of [`has_shape`]:
/// that answers whether an instance carries interceptors or V8 marks (what `structured_clone` asks).
pub(crate) fn declare_engine_slots(scope: &mut v8::PinScope, iface: &str, it: v8::Local<v8::ObjectTemplate>) {
    let names: &[&str] = match iface {
        "MutationRecord" => &[crate::core::dom::mutation_observer::RECORD_SLOT_PRIVATE],
        // an event's base slots (`core/events/event.rs`): every WebIDL event interface's name ends so, and a slot
        // declared on one that is not an event is only a slot nobody writes
        _ if iface.ends_with("Event") => &crate::core::events::event::ENGINE_SLOTS,
        _ => crate::core::dom::collections::engine_slot_names(iface),
    };
    if iface.ends_with("Event") {
        crate::core::events::event::declare_is_trusted(scope, iface, it);
    }
    if names.is_empty() {
        return;
    }
    let undefined: v8::Local<v8::Value> = v8::undefined(scope).into();
    for n in names {
        let k = crate::binding::protos::private_key(scope, n);
        crate::install::templates::set_private_on(&it, k, undefined.into());
    }
}
