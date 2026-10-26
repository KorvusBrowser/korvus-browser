//! `core/html/html_slot_element.rs` — `HTMLSlotElement` as one `interface!` table.
//!
//! The Shadow DOM slot surface (Korvus GS 2-B2): `assignedNodes` and `assignedElements` live here, on
//! the `<slot>` element. `assignedSlot` — the inverse edge, a slotted light node to its `<slot>` — is
//! the `Slottable` mixin and is a row of the `Element` and `Text` tables, not of this one.
//!
//! Two of the four rows carry a Rust body, and both are `core/dom/element.rs`'s own callbacks, which
//! is what `dom/08b-reflected.js` used to mount, through the `op_assigned_nodes_getter` /
//! `op_assigned_elements_getter` doors that both went with it -- this table names
//! `cb_assigned_nodes` and `cb_assigned_elements` directly. `name` is the registry's `reflect_str` row (the `name` IDL
//! attribute of `<slot>` is an ordinary reflection) and `assign` stays an honest D2 stub, which is
//! what the registry already mounted for it.

use deno_core::v8;

use crate::core::dom::element::{cb_assigned_elements, cb_assigned_nodes};
use crate::install::live::method_row;
use crate::install::MemberDef;

pub fn install_html_slot_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "assignedElements" => method_row(scope, proto, member, cb_assigned_elements),
        "assignedNodes" => method_row(scope, proto, member, cb_assigned_nodes),
        other => debug_assert!(false, "install_html_slot_element: unexpected member {other}"),
    }
}

webapi::interface! {
    // Rows VERBATIM from the `HTMLSlotElement` block of `install/generated_registry.rs` -- the
    // capture's order, which puts `assignedElements` before `assignedNodes`.
    "HTMLSlotElement" : "HTMLElement" [element, window] {
        ctor   "HTMLSlotElement"                     len 0 = stub,
        attr   "name"             : DomString cfg "name"   = reflect_str,
        method "assign"           : Undefined len 0        = stub,
        method "assignedElements" : Object    len 0        = install_html_slot_element,
        method "assignedNodes"    : Object    len 0        = install_html_slot_element
    }
}
