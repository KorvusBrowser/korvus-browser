//! `core/dom/text.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::binding::HostDom;
use crate::core::dom::shared::{notify_character_data, notify_childlist_sib};
use crate::binding::protos::{require_args, throw_dom, unwrap_id, wrap_cached};
use crate::core::dom::range::{ranges_text_replaced, ranges_text_split};

/// Notify the MutationObserver registry of a childList mutation on `parent` (added/removed node lists;
/// each usually a single node, or several for a DocumentFragment insert). Mirrors `notify_insert`:
/// builds the addedNodes/removedNodes JS arrays and calls `KB.__mutobs.childList`. Cheap no-op (an empty
/// JS Map walk) when nothing is observing.
pub(crate) fn notify_childlist(scope: &mut v8::PinScope, parent: v8::Local<v8::Value>, added: &[v8::Local<v8::Value>], removed: &[v8::Local<v8::Value>]) {
    notify_childlist_sib(scope, parent, added, removed, None, None);
}

pub(crate) fn cb_text_split(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // `Text splitText(unsigned long offset)` -- the argument is required.
    if !require_args(scope, &args, 1, "Text", "splitText") { return; }
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let offset = args.get(0).integer_value(scope).unwrap_or(0).max(0) as usize;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The spec's FIRST step after the argument conversion: "if offset is greater than node's length,
    // throw an IndexSizeError". Clamping instead meant `new Text().splitText(1)` quietly returned an
    // empty node where Chrome throws. The length is in UTF-16 code units -- the unit `data`, `length`
    // and `substringData` already count in, not bytes.
    let len = op_state.borrow().borrow::<HostDom>().0.node_value(id).unwrap_or_default().encode_utf16().count();
    if offset > len {
        throw_dom(
            scope,
            &format!("Failed to execute 'splitText' on 'Text': The offset {offset} is larger than the Text node's length."),
            "IndexSizeError",
        );
        return;
    }
    let new_id = op_state.borrow_mut().borrow_mut::<HostDom>().0.split_text(id, offset);
    let nw = wrap_cached(scope, &op_state, new_id);
    ranges_text_split(scope, id, offset as i64, new_id);
    rv.set(nw.into());
}

pub(crate) fn cb_text_content_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A Document receiver's textContent is null (Chrome), not an unset return value; an Attr's is its value.
    let id = match unwrap_id(scope, args.this().into()) {
        Some(i) => i,
        None => {
            let this = args.this();
            if deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, this.into()).is_some() { rv.set_null(); }
            else if let Some(v) = crate::core::dom::attr::attr_value_of(scope, this) { let s = v8::String::new(scope, &v).unwrap(); rv.set(s.into()); }
            return;
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // THE WHOLE algorithm, from the arena: `Dom::text_content_value`. This used to call
    // `Dom::text_content`, which is only its Element/DocumentFragment arm -- so a Comment and a
    // CDATASection read `""` instead of their data, and a DocumentType read `""` instead of null.
    // The SETTER below has always carried the same per-nodeType switch.
    let s = op_state.borrow().borrow::<HostDom>().0.text_content_value(id);
    match s {
        Some(s) => { let v = v8::String::new(scope, &s).unwrap(); rv.set(v.into()); }
        None => rv.set_null(),
    }
}

pub(crate) fn cb_text_content_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let a0 = args.get(0);
    // (DOMString or TrustedScript)?: a TrustedScript read as itself, nothing on it called; a `<script>`'s is a
    // Trusted Types sink ("HTMLScriptElement textContent"), any other node's is not
    let s = match crate::core::trusted_types::tables::html_script_id(scope, this) {
        Some(id) => {
            let Some(t) = crate::core::trusted_types::tables::script_text_sink(scope, id, (!a0.is_null_or_undefined()).then_some(a0), "textContent") else { return };
            t
        }
        None if a0.is_null_or_undefined() => String::new(),
        None => {
            let Some(t) = crate::core::trusted_types::tables::sink_text(scope, a0, "TrustedScript") else { return };
            t
        }
    };
    set_text_content_of(scope, this, &s);
}

/// The `textContent` setter's steps for `this`, with the string already converted -- for engine members
/// defined as "set textContent" (`a.text = v`), which must not go through the page-visible setter.
pub(crate) fn set_text_content_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, s: &str) {
    let s = s.to_string();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (nt, old) = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; (dom.node_type(id), dom.node_value(id)) };
    // "Any other node: DO NOTHING" -- the switch's third arm, which was not here. A Document(9) and
    // a DocumentType(10) have no textContent to set; both fell into the container arm below, so
    // `document.doctype.textContent = "b"` hung a Text node off the DOCTYPE (WPT
    // dom/nodes/Node-textContent, "setting textContent should do nothing"). A shadow root reuses
    // nodeType 9's arena kind but IS a DocumentFragment, and keeps the container arm.
    if nt == 10 || (nt == 9 && !op_state.borrow().borrow::<HostDom>().0.is_shadow_root(id)) {
        return;
    }
    // CharacterData: Text(3), CDATASection(4), ProcessingInstruction(7), Comment(8).
    if matches!(nt, 3 | 4 | 7 | 8) {
        // Text/Comment: textContent= is a data mutation (characterData).
        {
            let mut st = op_state.borrow_mut();
            let dom = &mut st.borrow_mut::<HostDom>().0;
            dom.set_node_value(id, &s);
        }
        // ...and, being the spec's "replace data" with offset 0 and count = the old length, it
        // collapses every live-range boundary inside this node to 0. The `data` setter notified;
        // this one did not, so `node.textContent = "x"` left a range holding a stale offset.
        // -- WPT Range-mutations-dataChange.
        ranges_text_replaced(scope, id, old.as_deref().unwrap_or("").encode_utf16().count() as i64, s.encode_utf16().count() as i64);
        notify_character_data(scope, this.into(), old);
    } else {
        // Element (and other container nodes): textContent= replaces ALL children with (at most) one
        // new Text node -- a childList mutation. Capture the outgoing children before the arena mutation
        // and the incoming ones after, then notify through the SAME notify_childlist path
        // appendChild/removeChild use, so a childList MutationObserver sees the replace (WPT
        // MutationObserver-takeRecords.html: `n00.textContent = "old data"` must queue a childList
        // record with the removed old children + the added new text node).
        let before: Vec<u32> = op_state.borrow().borrow::<HostDom>().0.children(id).to_vec();
        {
            let mut st = op_state.borrow_mut();
            let dom = &mut st.borrow_mut::<HostDom>().0;
            dom.set_text_content(id, &s);
        }
        let after: Vec<u32> = op_state.borrow().borrow::<HostDom>().0.children(id).to_vec();
        if before != after {
            let removed: Vec<v8::Local<v8::Value>> = before.iter().map(|&c| wrap_cached(scope, &op_state, c).into()).collect();
            let added: Vec<v8::Local<v8::Value>> = after.iter().map(|&c| wrap_cached(scope, &op_state, c).into()).collect();
            notify_childlist(scope, this.into(), &added, &removed);
        }
    }
}

// ── the Text and CDATASection tables ───────────────────────────────────────────────

use crate::core::dom::character_data::cb_cd_data_get;
use crate::core::dom::shared::{cb_assigned_slot, row_getter, row_method};
use crate::install::MemberDef;

pub(crate) use crate::core::dom::document::{cb_comment_ctor, cb_text_ctor};

/// The `Text` rows: `wholeText` (the data -- adjacent text nodes are not merged here), the Slottable
/// mixin's `assignedSlot`, and `splitText`.
pub fn install_text(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "wholeText" => row_getter(scope, proto, member, cb_cd_data_get),
        "assignedSlot" => row_getter(scope, proto, member, cb_assigned_slot),
        "splitText" => row_method(scope, proto, member, cb_text_split),
        other => debug_assert!(false, "install_text: unexpected member {other}"),
    }
}

// `new Text(data)` is real (`cb_text_ctor`); CDATASection has no constructor and adds no member.
webapi::interface! {
    "Text" : "CharacterData" [value, window] {
        ctor "Text" len 0 = stub,
        attr "wholeText" : Object readonly = install_text,
        attr "assignedSlot" : Object readonly nullable = install_text,
        method "splitText" : Object len 1 required 1 = install_text
    }
}

webapi::interface! {
    "CDATASection" : "Text" [interface, window] {
        ctor "CDATASection" len 0 = stub
    }
}
