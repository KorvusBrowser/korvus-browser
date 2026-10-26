//! `core/dom/abstract_range.rs` -- the `AbstractRange` table.
//!
//! `AbstractRange` is the abstract base `Range` and `StaticRange` inherit: the four boundary
//! attributes live in the instance's private SLOTS (both subclasses write them), and `collapsed` is
//! computed from them. The CLASS stays in `core/korvus-runtime/src/js/host.js` until W4.b; what
//! changes hands is the PROTOTYPE, whose five rows are `installer::overwrite_prototype`'s -- replacing
//! the registry's member backfill for the four slot rows and the one `install(...)` site in
//! `js/dom/08h-abort-attrs.js` for `collapsed`.
//!
//! `[interface, window]`: `worker.surface.json` has no `AbstractRange`.

use deno_core::v8;
use crate::binding::protos::{arg_node_id, get_prop};
use crate::install::MemberDef;

/// `AbstractRange.prototype.collapsed`: start and end boundary points are equal. Node wrappers are
/// canonical (one cached wrapper per arena node), so identity IS the comparison.
pub(crate) fn cb_abstract_range_collapsed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let a = get_prop(scope, this, "startContainer").and_then(|v| arg_node_id(scope, v));
    let b = get_prop(scope, this, "endContainer").and_then(|v| arg_node_id(scope, v));
    let so = get_prop(scope, this, "startOffset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let eo = get_prop(scope, this, "endOffset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    rv.set_bool(a == b && so == eo);
}

/// The bodies the implemented rows of [`ABSTRACTRANGE`] name.
pub fn install_abstract_range(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "collapsed" => crate::install::live::getter_row(scope, proto, member, cb_abstract_range_collapsed),
        "startOffset" | "endOffset" => install_offset(scope, proto, member),
        other => debug_assert!(false, "install_abstract_range: unexpected member {other}"),
    }
}

// ── Chrome 152: NodeRange and OpaqueRange ─────────────────────────────────────────────────────────
//
// Chrome 152 split `AbstractRange` (`abstract_range.idl`, `node_range.idl`, `opaque_range.idl`): the two
// container attributes moved to the new `NodeRange`, which `Range` and `StaticRange` now inherit, and
// `OpaqueRange` -- a range over a text control's VALUE, made by `createValueRange` -- inherits
// `AbstractRange` directly, with offsets and no containers. Its offsets are live: the control's value
// edits move them (`korvus-dom`'s `OpaqueRange`), so `startOffset`/`endOffset` read the DOM for one and the
// boundary slots for the other two.

const OPAQUE_SLOT: &str = "korvus_opaque_range";

fn opaque_id(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    let key = crate::binding::protos::private_key(scope, OPAQUE_SLOT);
    this.get_private(scope, key).filter(|v| v.is_uint32()).and_then(|v| v.uint32_value(scope))
}

fn opaque_state(scope: &mut v8::PinScope, rid: u32) -> Option<korvus_dom::forms::OpaqueRange> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.borrow::<crate::binding::host_dom::HostDom>().0.opaque_range(rid)
}

/// A new `OpaqueRange` instance for range `rid`, on the current realm's own prototype.
pub(crate) fn new_opaque_range<'s>(scope: &mut v8::PinScope<'s, '_>, rid: u32) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "OpaqueRange")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let key = crate::binding::protos::private_key(scope, OPAQUE_SLOT);
    let id = v8::Integer::new_from_unsigned(scope, rid).into();
    obj.set_private(scope, key, id);
    Some(obj)
}

/// `AbstractRange.prototype.startOffset` / `endOffset`: an `OpaqueRange`'s live offset, else the boundary slot.
fn cb_abstract_range_offset(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    if let Some(rid) = opaque_id(scope, this) {
        let r = opaque_state(scope, rid).unwrap_or(korvus_dom::forms::OpaqueRange { element: None, start: 0, end: 0 });
        rv.set_uint32(if name == "startOffset" { r.start } else { r.end });
        return;
    }
    if let Some(v) = crate::core::events::event::evt_slot_read(scope, this, &name) {
        rv.set(v);
    }
}

fn install_offset(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let d = crate::binding::protos::str_val(scope, member.name);
    let g = crate::binding::protos::build_fn_data(scope, cb_abstract_range_offset, &format!("get {}", member.name), 0, d);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}

/// `OpaqueRange.prototype.disconnect()`: the range stops following its control and reads `[0, 0]`.
fn cb_opaque_disconnect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(rid) = opaque_id(scope, args.this()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.disconnect_opaque_range(rid);
}

/// The control an `OpaqueRange` measures against, when it has geometry at all: connected, still followed, and
/// not collapsed for `getClientRects` (Chrome: a collapsed or contextless range has an empty list).
fn opaque_geometry_element<'s>(scope: &mut v8::PinScope<'s, '_>, rid: u32, need_extent: bool) -> Option<v8::Local<'s, v8::Object>> {
    let r = opaque_state(scope, rid)?;
    let el = r.element?;
    if need_extent && r.start == r.end {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.is_connected(el) {
        return None;
    }
    let w = crate::binding::protos::wrap_cached(scope, &op_state, el);
    Some(w)
}

/// `OpaqueRange.prototype.getBoundingClientRect()`. korvus measures ranges at the box level (as its `Range`
/// does, `range.rs`): a followed range in a connected control answers the control's box, anything else a
/// zero `DOMRect` (Chrome's `DOMRect::Create()` for a range with no geometry context).
fn cb_opaque_bounding_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(rid) = opaque_id(scope, args.this()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    if let Some(el) = opaque_geometry_element(scope, rid, true) {
        if let Some(r) = crate::binding::protos::pristine_call(scope, el.into(), "Element.getBoundingClientRect", &[]) {
            rv.set(r);
            return;
        }
    }
    let Some(ctor) = crate::binding::protos::iface_object(scope, "DOMRect").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let z: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    if let Some(r) = ctor.new_instance(scope, &[z, z, z, z]) {
        rv.set(r.into());
    }
}

/// `OpaqueRange.prototype.getClientRects()`: the control's rects for a followed, non-collapsed range in a
/// connected control, else an empty `DOMRectList`.
fn cb_opaque_client_rects(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(rid) = opaque_id(scope, args.this()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    if let Some(el) = opaque_geometry_element(scope, rid, true) {
        if let Some(r) = crate::binding::protos::pristine_call(scope, el.into(), "Element.getClientRects", &[]) {
            rv.set(r);
            return;
        }
    }
    let arr = v8::Array::new(scope, 0);
    let list = crate::core::dom::collections::static_list_reuse(scope, "DOMRectList", arr, None);
    rv.set(list);
}

/// The `OpaqueRange` rows' bodies.
pub fn install_opaque_range(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::method_row;
    match member.name {
        "disconnect" => method_row(scope, proto, member, cb_opaque_disconnect),
        "getBoundingClientRect" => method_row(scope, proto, member, cb_opaque_bounding_rect),
        "getClientRects" => method_row(scope, proto, member, cb_opaque_client_rects),
        other => debug_assert!(false, "install_opaque_range: unexpected member {other}"),
    }
}

// The capture's `AbstractRange` block (Chrome 152): the two offsets, then `collapsed`.
webapi::interface! {
    "AbstractRange" [interface, window] {
        ctor "AbstractRange"                 len 0 = stub,
        attr "startOffset"    : Object readonly    = install_abstract_range,
        attr "endOffset"      : Object readonly    = install_abstract_range,
        attr "collapsed"      : Object readonly    = install_abstract_range
    }
}

// Chrome 152's `NodeRange`: the two slot-backed containers `Range` and `StaticRange` write.
webapi::interface! {
    "NodeRange" : "AbstractRange" [interface, window] {
        ctor "NodeRange"                     len 0 = stub,
        attr "startContainer" : Object readonly    = slot_get,
        attr "endContainer"   : Object readonly    = slot_get
    }
}

// Chrome 152's `OpaqueRange`: three methods over the inherited offsets.
webapi::interface! {
    "OpaqueRange" : "AbstractRange" [interface, window] {
        ctor   "OpaqueRange"                   len 0 = stub,
        method "disconnect"            : Object len 0 = install_opaque_range,
        method "getBoundingClientRect" : Object len 0 = install_opaque_range,
        method "getClientRects"        : Object len 0 = install_opaque_range
    }
}
