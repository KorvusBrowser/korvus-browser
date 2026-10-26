//! `core/dom/document_fragment.rs` -- the `DocumentFragment` and `ShadowRoot` tables.
//!
//! A fragment is an arena node (`NodeKind::Fragment`, nodeType 11) reached through the same cppgc
//! `JsElement` wrapper every node uses, so its ParentNode rows are the ELEMENT callbacks -- the
//! same `cb_append`/`cb_query_selector`/... over the same arena walk -- and `getElementById` is the
//! subtree-scoped form. A shadow root is a fragment the host owns: its own rows are the `cb_shadow_*`
//! callbacks over `Dom::shadow_*`, plus the members it shares with `Document` (`styleSheets`,
//! `getSelection`, the null-valued element slots) through the same bodies.
//!
//! `new DocumentFragment()` is real (`cb_fragment_ctor`); `new ShadowRoot()` throws "Illegal
//! constructor" -- both as the capture records (`newable` true / false).

use deno_core::v8;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{cb_noop, cb_null_get, require_args, wrap_cached};
use crate::core::dom::collections::{builder_data, cb_el_collection, Kind};
use crate::core::dom::document::{cb_adopted_style_sheets_get, cb_adopted_style_sheets_set, cb_doc_get_selection, cb_style_sheets};
use crate::core::dom::element::{
    cb_el_child_element_count, cb_el_custom_element_registry, cb_el_inner_html_get, cb_el_inner_html_set, cb_get_html,
    cb_set_html_unsafe, cb_shadow_clonable_get, cb_shadow_delegates_focus_get, cb_shadow_host_get, cb_shadow_mode_get,
    cb_shadow_reference_target_get, cb_shadow_reference_target_set, cb_shadow_serializable_get, cb_shadow_slot_assignment_get,
    el_id_of,
};
use crate::core::dom::node::{cb_append, cb_frag_get_element_by_id, cb_move_before, cb_prepend, cb_replace_children};
use crate::core::dom::shared::{
    cb_first_element_child, cb_last_element_child, cb_query_selector, cb_query_selector_all, row_accessor, row_getter,
    row_getter_data, row_method,
};
use crate::core::events::shared::FocusState;
use crate::install::MemberDef;

/// The `DocumentFragment` rows -- the ParentNode mixin over the fragment's own subtree.
pub fn install_document_fragment(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "children" => {
            let d = builder_data(scope, "HTMLCollection", Kind::Children);
            row_getter_data(scope, proto, member, cb_el_collection, d);
        }
        "firstElementChild" => row_getter(scope, proto, member, cb_first_element_child),
        "lastElementChild" => row_getter(scope, proto, member, cb_last_element_child),
        "childElementCount" => row_getter(scope, proto, member, cb_el_child_element_count),
        "append" => row_method(scope, proto, member, cb_append),
        "getElementById" => row_method(scope, proto, member, cb_frag_get_element_by_id),
        "moveBefore" => row_method(scope, proto, member, cb_move_before),
        "prepend" => row_method(scope, proto, member, cb_prepend),
        "querySelector" => row_method(scope, proto, member, cb_query_selector),
        "querySelectorAll" => row_method(scope, proto, member, cb_query_selector_all),
        "replaceChildren" => row_method(scope, proto, member, cb_replace_children),
        other => debug_assert!(false, "install_document_fragment: unexpected member {other}"),
    }
}

/// `shadowRoot.activeElement` -- the focused element when it is inside this root, else null.
fn cb_sr_active_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(root) = el_id_of(scope, args.this()) else { rv.set_null(); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let hit = {
        let st = op_state.borrow();
        let focused = st.borrow::<FocusState>().0.get();
        let dom = &st.borrow::<HostDom>().0;
        focused.filter(|&f| dom.contains(root, f))
    };
    match hit {
        Some(f) => { let o = wrap_cached(scope, &op_state, f); rv.set(o.into()); }
        None => rv.set_null(),
    }
}

/// `shadowRoot.elementsFromPoint(x, y)` / `elementFromPoint(x, y)` (DocumentOrShadowRoot): the node
/// document's hit list, each element RETARGETED against this shadow root (an element inside a deeper
/// shadow tree answers as its host here), duplicates dropped. A shadow root shares its document's
/// hit test -- Chrome 150 answers `[html]` at (0, 0) even for a detached host's shadow root, where
/// this answered `[]` (measured).
fn sr_hits(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<Vec<u32>> {
    let this = args.this();
    let sr = crate::binding::protos::unwrap_id(scope, this.into())?;
    let doc = crate::binding::protos::get_prop(scope, this, "ownerDocument")?;
    let (x, y) = (args.get(0), args.get(1));
    let list = crate::binding::protos::call_method(scope, doc, "elementsFromPoint", &[x, y])?;
    let arr = v8::Local::<v8::Array>::try_from(list).ok()?;
    let mut ids: Vec<u32> = Vec::new();
    for i in 0..arr.length() {
        if let Some(id) = arr.get_index(scope, i).and_then(|v| crate::binding::protos::unwrap_id(scope, v)) {
            ids.push(id);
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    // DOM "retarget A against B": climb out of shadow trees that are not B's shadow-including ancestors.
    let tree_root = |mut n: u32| {
        while let Some(p) = dom.parent(n) {
            n = p;
        }
        n
    };
    let incl_ancestor = |anc: u32, mut n: u32| loop {
        if n == anc {
            return true;
        }
        n = match dom.parent(n) {
            Some(p) => p,
            None => match dom.is_shadow_root(n).then(|| dom.shadow_host_of(n)).flatten() {
                Some(h) => h,
                None => return false,
            },
        };
    };
    let mut out: Vec<u32> = Vec::new();
    for mut a in ids {
        for _ in 0..1024 {
            let r = tree_root(a);
            if !dom.is_shadow_root(r) || incl_ancestor(r, sr) {
                break;
            }
            match dom.shadow_host_of(r) {
                Some(h) => a = h,
                None => break,
            }
        }
        if !out.contains(&a) {
            out.push(a);
        }
    }
    Some(out)
}
fn cb_sr_element_from_point(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "ShadowRoot", "elementFromPoint") { return; }
    match sr_hits(scope, &args).and_then(|h| h.first().copied()) {
        Some(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let o = wrap_cached(scope, &op_state, id);
            rv.set(o.into());
        }
        None => rv.set_null(),
    }
}
fn cb_sr_elements_from_point(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "ShadowRoot", "elementsFromPoint") { return; }
    let hits = sr_hits(scope, &args).unwrap_or_default();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let arr = v8::Array::new(scope, hits.len() as i32);
    for (i, id) in hits.into_iter().enumerate() {
        let o = wrap_cached(scope, &op_state, id);
        arr.set_index(scope, i as u32, o.into());
    }
    rv.set(arr.into());
}
/// `shadowRoot.getAnimations()`: no animation targets a shadow root itself, and the elements inside it
/// are reached through `Element.getAnimations({ subtree: true })` on the host.
fn cb_sr_get_animations(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let arr = v8::Array::new(scope, 0);
    rv.set(arr.into());
}

/// The `ShadowRoot` rows.
pub fn install_shadow_root(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "mode" => row_getter(scope, proto, member, cb_shadow_mode_get),
        "host" => row_getter(scope, proto, member, cb_shadow_host_get),
        "innerHTML" => row_accessor(scope, proto, member, cb_el_inner_html_get, cb_el_inner_html_set),
        "delegatesFocus" => row_getter(scope, proto, member, cb_shadow_delegates_focus_get),
        "slotAssignment" => row_getter(scope, proto, member, cb_shadow_slot_assignment_get),
        "serializable" => row_getter(scope, proto, member, cb_shadow_serializable_get),
        "clonable" => row_getter(scope, proto, member, cb_shadow_clonable_get),
        "activeElement" => row_getter(scope, proto, member, cb_sr_active_element),
        "styleSheets" => row_getter(scope, proto, member, cb_style_sheets),
        "pointerLockElement" | "pictureInPictureElement" => row_getter(scope, proto, member, cb_null_get),
        // Blink's lenient setter on the deprecated fullscreen attribute: the capture records `set: true`.
        "fullscreenElement" => row_accessor(scope, proto, member, cb_null_get, cb_noop),
        "adoptedStyleSheets" => row_accessor(scope, proto, member, cb_adopted_style_sheets_get, cb_adopted_style_sheets_set),
        "elementFromPoint" => row_method(scope, proto, member, cb_sr_element_from_point),
        "elementsFromPoint" => row_method(scope, proto, member, cb_sr_elements_from_point),
        "getAnimations" => row_method(scope, proto, member, cb_sr_get_animations),
        "getHTML" => row_method(scope, proto, member, cb_get_html),
        "getSelection" => row_method(scope, proto, member, cb_doc_get_selection),
        "setHTMLUnsafe" => row_method(scope, proto, member, cb_set_html_unsafe),
        "customElementRegistry" => row_getter(scope, proto, member, cb_el_custom_element_registry),
        "referenceTarget" => row_accessor(scope, proto, member, cb_shadow_reference_target_get, cb_shadow_reference_target_set),
        other => debug_assert!(false, "install_shadow_root: unexpected member {other}"),
    }
}

// The two tables: the capture's rows, verbatim (`generated_registry.rs` at the cutover).
webapi::interface! {
    "DocumentFragment" : "Node" [value, window] {
        ctor "DocumentFragment" len 0 = stub,
        attr "children" : Object readonly = install_document_fragment,
        attr "firstElementChild" : Object readonly nullable = install_document_fragment,
        attr "lastElementChild" : Object readonly nullable = install_document_fragment,
        attr "childElementCount" : Object readonly = install_document_fragment,
        method "append" : Object len 0 = install_document_fragment,
        method "getElementById" : Object len 1 required 1 nullable = install_document_fragment,
        method "moveBefore" : Object len 2 required 2 = install_document_fragment,
        method "prepend" : Object len 0 = install_document_fragment,
        method "querySelector" : Object len 1 required 1 nullable = install_document_fragment,
        method "querySelectorAll" : Object len 1 required 1 = install_document_fragment,
        method "replaceChildren" : Object len 0 = install_document_fragment,
        method "@@unscopables" : Object len 0 hidden cfg "append,prepend,replaceChildren" = unscopables
    }
}

webapi::interface! {
    "ShadowRoot" : "DocumentFragment" [interface, window] {
        ctor "ShadowRoot" len 0 = stub,
        attr "mode" : Object readonly = install_shadow_root,
        attr "host" : Object readonly = install_shadow_root,
        handler "onslotchange" = event_handler,
        attr "innerHTML" : Object = install_shadow_root,
        attr "delegatesFocus" : Object readonly = install_shadow_root,
        attr "slotAssignment" : Object readonly = install_shadow_root,
        attr "serializable" : Object readonly = install_shadow_root,
        attr "clonable" : Object readonly = install_shadow_root,
        attr "activeElement" : Object readonly nullable = install_shadow_root,
        attr "styleSheets" : Object readonly = install_shadow_root,
        attr "pointerLockElement" : Object readonly nullable = install_shadow_root,
        attr "fullscreenElement" : Object nullable = install_shadow_root,
        attr "adoptedStyleSheets" : Object = install_shadow_root,
        attr "pictureInPictureElement" : Object readonly nullable = install_shadow_root,
        method "elementFromPoint" : Object len 2 = install_shadow_root,
        method "elementsFromPoint" : Object len 2 = install_shadow_root,
        method "getAnimations" : Object len 0 = install_shadow_root,
        method "getHTML" : Object len 0 = install_shadow_root,
        method "getSelection" : Object len 0 nullable = install_shadow_root,
        method "setHTMLUnsafe" : Object len 1 required 1 = install_shadow_root,
        attr "customElementRegistry" : Object readonly nullable = install_shadow_root,
        // `undefined`-returning (the Sanitizer API); the stub is typed so the D2 stub answers undefined.
        method "setHTML" : Undefined len 1 required 1 = stub,
        // Chrome 152 (`shadow_root.idl`, `[RuntimeEnabled=ShadowRootReferenceTarget] attribute DOMString?`).
        attr "referenceTarget" : Object nullable = install_shadow_root
    }
}
