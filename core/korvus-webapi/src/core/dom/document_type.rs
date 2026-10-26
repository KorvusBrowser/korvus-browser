//! `core/dom/document_type.rs` -- the `DocumentType` table.
//!
//! A doctype is an arena node (nodeType 10) behind the same cppgc wrapper as every node: `name` is its
//! node name (the `_p` polymorphic getter), `publicId`/`systemId` read `Dom::doctype_fields`, and the
//! ChildNode mixin rows are the element callbacks over the same arena.

use deno_core::v8;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{str_val, unwrap_id};
use crate::core::dom::node::{cb_after, cb_before, cb_node_name_p, cb_remove_self, cb_replace_with};
use crate::core::dom::shared::{row_getter, row_method};
use crate::install::MemberDef;

fn doctype_field(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, which: u8) -> String {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return String::new() };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let (_, public_id, system_id) = st.borrow::<HostDom>().0.doctype_fields(id);
    if which == 0 { public_id } else { system_id }
}
/// `DocumentType.publicId` -- empty for a `<!doctype html>`.
pub(crate) fn cb_doctype_public_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = doctype_field(scope, &args, 0);
    let v = str_val(scope, &s);
    rv.set(v);
}
/// `DocumentType.systemId`.
pub(crate) fn cb_doctype_system_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = doctype_field(scope, &args, 1);
    let v = str_val(scope, &s);
    rv.set(v);
}

/// The `DocumentType` rows.
pub fn install_document_type(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "name" => row_getter(scope, proto, member, cb_node_name_p),
        "publicId" => row_getter(scope, proto, member, cb_doctype_public_id),
        "systemId" => row_getter(scope, proto, member, cb_doctype_system_id),
        "after" => row_method(scope, proto, member, cb_after),
        "before" => row_method(scope, proto, member, cb_before),
        "remove" => row_method(scope, proto, member, cb_remove_self),
        "replaceWith" => row_method(scope, proto, member, cb_replace_with),
        other => debug_assert!(false, "install_document_type: unexpected member {other}"),
    }
}

webapi::interface! {
    "DocumentType" : "Node" [interface, window] {
        ctor "DocumentType" len 0 = stub,
        attr "name" : Object readonly = install_document_type,
        attr "publicId" : Object readonly = install_document_type,
        attr "systemId" : Object readonly = install_document_type,
        method "after" : Object len 0 = install_document_type,
        method "before" : Object len 0 = install_document_type,
        method "remove" : Object len 0 = install_document_type,
        method "replaceWith" : Object len 0 = install_document_type,
        method "@@unscopables" : Object len 0 hidden cfg "after,before,remove,replaceWith" = unscopables
    }
}
