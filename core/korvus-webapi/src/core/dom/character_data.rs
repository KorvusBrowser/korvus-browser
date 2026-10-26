//! `core/dom/character_data.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, OpState};
use crate::binding::HostDom;
use crate::core::dom::shared::{notify_character_data};
use crate::modules::crypto::shared::{crypto_num};
use crate::binding::protos::{arg_node_id, throw_dom, unwrap_id};
// The live-range notifications are Rust functions in `core/dom/range.rs` now. They were
// `KB.__ranges.<method>(...)` calls through `bridge_get` -- a JS object looked up by name, its
// method fetched by name, and the node handed over as a JS wrapper so the JS side could compare
// identities. Nothing between a mutation and a boundary is JS any more, so the arena id goes
// straight across.
use crate::core::dom::range::{arg_offset, ranges_data_replaced, ranges_text_deleted, ranges_text_inserted, ranges_text_replaced};

fn node_str(_scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> String {
    op_state.borrow().borrow::<HostDom>().0.node_value(id).unwrap_or_default()
}

/// Step 2 of `substringData`, `insertData`, `deleteData` and `replaceData` — WHATWG DOM §4.10, and
/// the SAME step in all four: *"If offset is greater than node's length, then throw an
/// `IndexSizeError`."* `false` means an exception is pending and the caller must return.
///
/// ONE CHECK, FOUR CALLERS. Each of the four used to read its offset as
/// `args.get(n).integer_value(scope).unwrap_or(0).max(0) as usize`, which is neither of the two
/// things this one algorithm asks of them:
///
///   * `offset` is a WebIDL `unsigned long`, so the conversion is **ToUint32 — modulo 2³²**, not a
///     clamp at zero. `substringData(-4294967294, 1)` IS `substringData(2, 1)` and answers `"s"`;
///     `.max(0)` answered `"t"`. That conversion is [`arg_offset`], which `Range` has used for its
///     own `unsigned long` offsets since RANGE1 — the callers below are its second user, not a
///     second copy of it. The same applies to `count`, which is `unsigned long` too, so
///     `deleteData(2, -1)` is `deleteData(2, 4294967295)` and deletes to the end.
///   * an offset PAST the end is this error, not a silent clamp. `node.insertData(5, "x")` on a
///     four-unit node quietly appended.
///
/// `count` is deliberately NOT checked here: the spec clamps it (*"if offset plus count is greater
/// than length, set count to length minus offset"*), which is exactly what the arena's slicing in
/// `korvus_dom::Dom::{substring_data, delete_data, replace_data}` already does.
///
/// The message is Chrome's, measured on this machine (headless 150, `--dump-dom`): all four say
/// `on 'CharacterData'`, never the concrete interface, and both numbers are the converted ones.
fn cd_offset_in_range(scope: &mut v8::PinScope, id: u32, method: &str, offset: i64) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let len = node_str(scope, &op_state, id).encode_utf16().count() as i64;
    if offset <= len {
        return true;
    }
    throw_dom(
        scope,
        &format!(
            "Failed to execute '{method}' on 'CharacterData': The offset {offset} is greater than the node's length ({len})."
        ),
        "IndexSizeError",
    );
    false
}

pub(crate) fn cb_cd_data_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match unwrap_id(scope, args.this().into()) { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = node_str(scope, &op_state, id);
    let v = v8::String::new(scope, &s).unwrap();
    rv.set(v.into());
}

pub(crate) fn cb_cd_data_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    // `[LegacyNullToEmptyString] attribute DOMString data` — `text.data = null` stores the EMPTY
    // string, not the six characters `"null"` that ToString would give (WHATWG DOM's IDL; Chrome
    // measured on this machine). `undefined` is NOT covered by the extended attribute and really
    // does store `"undefined"`, which is why this tests `is_null` and not `is_null_or_undefined`.
    let a0 = args.get(0);
    let val = if a0.is_null() { String::new() } else { a0.to_rust_string_lossy(scope) };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let old = node_str(scope, &op_state, id);
    let old_len = old.encode_utf16().count();
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_node_value(id, &val);
    let new_len = val.encode_utf16().count();
    ranges_text_replaced(scope, id, old_len as i64, new_len as i64);
    notify_character_data(scope, this.into(), Some(old));
}

pub(crate) fn cb_cd_substring_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id = match unwrap_id(scope, args.this().into()) { Some(i) => i, None => return };
    // Every argument is converted before any step runs (WebIDL), then the shared offset check.
    let offset = arg_offset(scope, args.get(0));
    let count = arg_offset(scope, args.get(1));
    if !cd_offset_in_range(scope, id, "substringData", offset) { return; }
    let (offset, count) = (offset as usize, count as usize);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = op_state.borrow().borrow::<HostDom>().0.substring_data(id, offset, count);
    let v = v8::String::new(scope, &s).unwrap();
    rv.set(v.into());
}

pub(crate) fn cb_cd_append_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let s = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let old = node_str(scope, &op_state, id);
    let old_len = old.encode_utf16().count();
    op_state.borrow_mut().borrow_mut::<HostDom>().0.append_data(id, &s);
    ranges_text_inserted(scope, id, old_len as i64, s.encode_utf16().count() as i64);
    notify_character_data(scope, this.into(), Some(old));
}

pub(crate) fn cb_cd_insert_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let offset = arg_offset(scope, args.get(0));
    let s = args.get(1).to_rust_string_lossy(scope);
    if !cd_offset_in_range(scope, id, "insertData", offset) { return; }
    let offset = offset as usize;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let old = node_str(scope, &op_state, id);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.insert_data(id, offset, &s);
    ranges_text_inserted(scope, id, offset as i64, s.encode_utf16().count() as i64);
    notify_character_data(scope, this.into(), Some(old));
}

pub(crate) fn cb_cd_delete_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let offset = arg_offset(scope, args.get(0));
    let count = arg_offset(scope, args.get(1));
    if !cd_offset_in_range(scope, id, "deleteData", offset) { return; }
    let (offset, count) = (offset as usize, count as usize);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let old = node_str(scope, &op_state, id);
    // Step 3 clamps `count` ("if offset plus count is greater than length, set count to length minus
    // offset") BEFORE the live-range steps run, and the arena's slicing clamps identically -- so the
    // boundary fixup below must be told the CLAMPED count. `deleteData(2, -1)` is `count` 4294967295
    // and removes two code units; handing 4294967295 to the range registry moved every boundary in
    // the node to a position the data never had. `offset <= len` is guaranteed above, so this cannot
    // underflow.
    let count = count.min(old.encode_utf16().count() - offset);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.delete_data(id, offset, count);
    ranges_text_deleted(scope, id, offset as i64, count as i64);
    notify_character_data(scope, this.into(), Some(old));
}

pub(crate) fn cb_cd_replace_data(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let id = match unwrap_id(scope, this.into()) { Some(i) => i, None => return };
    let offset = arg_offset(scope, args.get(0));
    let count = arg_offset(scope, args.get(1));
    let s = args.get(2).to_rust_string_lossy(scope);
    if !cd_offset_in_range(scope, id, "replaceData", offset) { return; }
    let (offset, count) = (offset as usize, count as usize);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let old = node_str(scope, &op_state, id);
    // The clamped `count` -- see the note in `cb_cd_delete_data`; `replaceData(2, -1, "yo")` is the
    // same call with `count` 4294967295 and replaces the two units that are there.
    let count = count.min(old.encode_utf16().count() - offset);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.replace_data(id, offset, count, &s);
    ranges_data_replaced(scope, id, offset as i64, count as i64, s.encode_utf16().count() as i64);
    notify_character_data(scope, this.into(), Some(old));
}

/// CharacterData.length: the node's data length in UTF-16 code units (native getter over the arena).
pub(crate) fn cb_cd_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let len = match arg_node_id(scope, args.this().into()) {
        Some(id) => { let op = deno_core::JsRuntime::op_state_from(scope); let n = op.borrow().borrow::<HostDom>().0.node_value(id).unwrap_or_default().encode_utf16().count(); n }
        None => 0,
    };
    let n = crypto_num(scope, len as f64); rv.set(n);
}

// ── the CharacterData, Comment and ProcessingInstruction tables ─────────────────────

use crate::binding::protos::{cb_null_get, require_args, str_val};
use crate::core::dom::node::{cb_after, cb_before, cb_pi_target, cb_remove_self, cb_replace_with};
use crate::core::dom::shared::{cb_next_element_sibling, cb_previous_element_sibling, row_accessor, row_getter, row_method};
use crate::install::MemberDef;

/// The `CharacterData` rows: `data`/`length` and the five data methods over the arena, the
/// NonDocumentTypeChildNode siblings and the ChildNode mixin -- the element callbacks, since a text
/// node is the same cppgc wrapper over the same arena.
pub fn install_character_data(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "data" => row_accessor(scope, proto, member, cb_cd_data_get, cb_cd_data_set),
        "length" => row_getter(scope, proto, member, cb_cd_length),
        "previousElementSibling" => row_getter(scope, proto, member, cb_previous_element_sibling),
        "nextElementSibling" => row_getter(scope, proto, member, cb_next_element_sibling),
        "after" => row_method(scope, proto, member, cb_after),
        "appendData" => row_method(scope, proto, member, cb_cd_append_data),
        "before" => row_method(scope, proto, member, cb_before),
        "deleteData" => row_method(scope, proto, member, cb_cd_delete_data),
        "insertData" => row_method(scope, proto, member, cb_cd_insert_data),
        "remove" => row_method(scope, proto, member, cb_remove_self),
        "replaceData" => row_method(scope, proto, member, cb_cd_replace_data),
        "replaceWith" => row_method(scope, proto, member, cb_replace_with),
        "substringData" => row_method(scope, proto, member, cb_cd_substring_data),
        other => debug_assert!(false, "install_character_data: unexpected member {other}"),
    }
}

// A processing instruction carries no attributes, and the recorded browser's PI prototype still owns
// the seven attribute members (Brave 151): they answer for an attribute-less node -- null, false, an
// empty list, undefined -- with the WebIDL arity the rows carry.
fn cb_pi_get_attribute(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { let _ = scope; rv.set_null(); }
fn cb_pi_get_attribute_names(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { let a = v8::Array::new(scope, 0); rv.set(a.into()); }
fn cb_pi_has_attribute(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { let _ = scope; rv.set_bool(false); }
fn cb_pi_attribute_noop(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// The `ProcessingInstruction` rows: `target` over the arena PI fields, `sheet` (no PI ever loads a
/// stylesheet here), and the seven attribute members above.
pub fn install_processing_instruction(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "target" => row_getter(scope, proto, member, cb_pi_target),
        "sheet" => row_getter(scope, proto, member, cb_null_get),
        "getAttribute" => row_method(scope, proto, member, cb_pi_get_attribute),
        "getAttributeNames" => row_method(scope, proto, member, cb_pi_get_attribute_names),
        "hasAttribute" | "hasAttributes" => row_method(scope, proto, member, cb_pi_has_attribute),
        "removeAttribute" | "setAttribute" | "toggleAttribute" => row_method(scope, proto, member, cb_pi_attribute_noop),
        other => debug_assert!(false, "install_processing_instruction: unexpected member {other}"),
    }
}

webapi::interface! {
    "CharacterData" : "Node" [interface, window] {
        ctor "CharacterData" len 0 = stub,
        attr "data" : Object = install_character_data,
        attr "length" : Object readonly = install_character_data,
        attr "previousElementSibling" : Object readonly nullable = install_character_data,
        attr "nextElementSibling" : Object readonly nullable = install_character_data,
        method "after" : Object len 0 = install_character_data,
        method "appendData" : Object len 1 required 1 = install_character_data,
        method "before" : Object len 0 = install_character_data,
        method "deleteData" : Object len 2 required 2 = install_character_data,
        method "insertData" : Object len 2 required 2 = install_character_data,
        method "remove" : Object len 0 = install_character_data,
        method "replaceData" : Object len 3 required 3 = install_character_data,
        method "replaceWith" : Object len 0 = install_character_data,
        method "substringData" : Object len 2 required 2 = install_character_data,
        method "@@unscopables" : Object len 0 hidden cfg "after,before,remove,replaceWith" = unscopables
    }
}

// `new Comment(data)` is real (`text.rs::cb_comment_ctor`); the interface adds no member of its own.
webapi::interface! {
    "Comment" : "CharacterData" [value, window] {
        ctor "Comment" len 0 = stub
    }
}

webapi::interface! {
    "ProcessingInstruction" : "CharacterData" [interface, window] {
        ctor "ProcessingInstruction" len 0 = stub,
        attr "target" : Object readonly = install_processing_instruction,
        attr "sheet" : Object readonly nullable = install_processing_instruction,
        method "getAttribute" : Object len 1 required 1 nullable = install_processing_instruction,
        method "getAttributeNames" : Object len 0 = install_processing_instruction,
        method "hasAttribute" : Object len 1 required 1 = install_processing_instruction,
        method "hasAttributes" : Object len 0 = install_processing_instruction,
        method "removeAttribute" : Object len 1 required 1 = install_processing_instruction,
        method "setAttribute" : Object len 2 required 2 = install_processing_instruction,
        method "toggleAttribute" : Object len 1 required 1 = install_processing_instruction
    }
}

#[allow(dead_code)]
fn _pi_uses(_: v8::Local<v8::Value>) { let _ = require_args; let _ = str_val; }
