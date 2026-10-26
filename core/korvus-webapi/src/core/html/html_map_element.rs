//! `core/html/html_map_element.rs` -- `HTMLMapElement` as one `interface!` table.
//!
//! Two rows: `name`, a plain reflection, and `areas` -- the member that made `<map>` part of this
//! lane at all. `areas` LOOKS AT OTHER ELEMENTS (the map's descendant `<area>`s), which is why plain
//! attribute reflection never reached it and why `dom/09c-small-elements.js` carried a
//! `KB.__mapAreas` bridge slot behind an `op_helper_get` native mask. Both are gone; the row is
//! mounted by THE installer (`install::live::install_window_element_refs`) with the name, order, kind and
//! descriptor flags VERBATIM from the `HTMLMapElement` block of `install/generated_registry.rs`.
//!
//! `areas` IS AN `HTMLCollection`, NOT AN ARRAY. The deleted JS built a plain `Array` from
//! `querySelectorAll("area")` and patched `item` and `namedItem` onto it as own properties -- so
//! `Object.prototype.toString.call(map.areas)` read `[object Array]`, `map.areas` was a DEAD snapshot
//! that never saw a later `appendChild`, and the two patched functions were own JS properties a page
//! could read the source of. The bundle's own oracle records
//! `Object.prototype.toString.call(map.areas) === "[object HTMLCollection]"`, and that case was the
//! one unverified member of this interface before this lane. It is the live-collection mint that
//! already exists (`core/dom/collections.rs`, `Kind::ElByTag` over `"area"`), which is the same
//! machinery `document.images` and `fieldset.elements` are built from -- so `item`, `namedItem`,
//! indexed access and liveness all come from the one implementation instead of a second one.
//!
//! Measured in Chrome 150 over loopback, and reproduced by `looking_elements_live.rs`:
//!
//! ```text
//! Object.prototype.toString.call(map.areas)  -> "[object HTMLCollection]"
//! map.areas === map.areas                    -> true          ([SameObject])
//! map.areas.length after appendChild(area)   -> grows          (LIVE)
//! map.innerHTML = "<div><area></div>"        -> areas.length 1 (DESCENDANTS, not children)
//! map.areas.namedItem("x")                   -> matches id OR the `name` attribute
//! HTMLMapElement.prototype.areas getter on {} -> TypeError: Illegal invocation
//! ```
//!
//! `[SameObject]` is kept the way `element.classList` keeps it (`core/dom/element.rs`): the minted
//! collection is cached on the element wrapper under a V8 private, so every read hands back the same
//! object. The collection is live, so a cached one is never stale and there is nothing to reseed.

use deno_core::v8;

use crate::binding::protos::{throw_type_error};
use crate::core::dom::collections::{make_live, Kind};
use crate::core::dom::element::el_id_of;
use crate::install::live::getter_row;
use crate::install::MemberDef;

/// The `[SameObject]` cache for `map.areas`, on the element wrapper (stable via the NodeCache).
const MAP_AREAS_PRIV: &str = "korvus_map_areas";

/// `true` when the receiver is a `<map>` element; otherwise Chrome's `Illegal invocation` TypeError.
fn this_map(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    let this = args.this();
    let ok = el_id_of(scope, this).is_some()
        && crate::page::tag_name_of(scope, (this).into()).is_some_and(|t| t.eq_ignore_ascii_case("map"));
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// `map.areas` -- the live `HTMLCollection` of the map's descendant `<area>` elements.
fn cb_map_areas(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_map(scope, &args) {
        return;
    }
    let this = args.this();
    let Some(id) = el_id_of(scope, this) else { return };
    let key = v8::Private::for_api(scope, v8::String::new(scope, MAP_AREAS_PRIV));
    if let Some(cached) = this.get_private(scope, key).filter(|v| v.is_object()) {
        rv.set(cached);
        return;
    }
    let list = make_live(scope, "HTMLCollection", id, Kind::ElByTag, "area", None);
    this.set_private(scope, key, list);
    rv.set(list);
}

pub fn install_html_map_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "areas" => getter_row(scope, proto, member, cb_map_areas),
        other => debug_assert!(false, "install_html_map_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLMapElement" : "HTMLElement" [element, window] {
        ctor   "HTMLMapElement"          len 0      = stub,
        attr   "name"  : Object cfg "name"          = reflect_str,
        attr   "areas" : Object readonly            = install_html_map_element
    }
}
