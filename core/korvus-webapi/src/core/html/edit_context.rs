//! `EditContext`: the text, selection and bounds state an editor keeps in it.
//!
//! Monaco (microsoft.github.io/monaco-editor, godbolt.org) drives its input through an `EditContext`
//! when the browser has one, and every member was a D2 stub: `updateText(0, 0, "abc")` left `text` at
//! "" and `updateSelection(1, 2)` left both ends at 0. Chrome 150, measured (`tools/realweb/seed/cs2`):
//! `["abc", 1, 2]`. The state lives in private slots on the instance; nothing here fires input events
//! (there is no user typing into a headless page).

use deno_core::v8;

use crate::binding::protos::{build_fn, get_prop, str_val, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;

fn key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus.editContext.{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn this_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if crate::install::webidl::has_brand(scope, this.into(), "EditContext") {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

fn text_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Vec<u16> {
    let k = key(scope, "text");
    match this.get_private(scope, k).filter(|v| v.is_string()) {
        Some(v) => v.to_rust_string_lossy(scope).encode_utf16().collect(),
        None => Vec::new(),
    }
}

fn num_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> u32 {
    let k = key(scope, name);
    this.get_private(scope, k).and_then(|v| v.uint32_value(scope)).unwrap_or(0)
}

fn set_text(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, text: &[u16]) {
    let k = key(scope, "text");
    let s = String::from_utf16_lossy(text);
    let v = str_val(scope, &s);
    this.set_private(scope, k, v);
}

fn set_num(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, n: u32) {
    let k = key(scope, name);
    let v = v8::Integer::new_from_unsigned(scope, n);
    this.set_private(scope, k, v.into());
}

/// An `unsigned long` argument (WebIDL: ToNumber, then modulo 2^32).
fn ulong(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u32 {
    v.uint32_value(scope).unwrap_or(0)
}

/// `new EditContext({ text, selectionStart, selectionEnd })`.
pub(crate) fn edit_context_init(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments) {
    let mut text: Vec<u16> = Vec::new();
    let (mut start, mut end) = (0u32, 0u32);
    if let Ok(init) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        if let Some(t) = get_prop(scope, init, "text").filter(|v| !v.is_undefined()) {
            text = t.to_rust_string_lossy(scope).encode_utf16().collect();
        }
        if let Some(s) = get_prop(scope, init, "selectionStart").filter(|v| !v.is_undefined()) {
            start = ulong(scope, s);
        }
        if let Some(e) = get_prop(scope, init, "selectionEnd").filter(|v| !v.is_undefined()) {
            end = ulong(scope, e);
        }
    }
    let len = text.len() as u32;
    set_text(scope, this, &text);
    set_num(scope, this, "selectionStart", start.min(len));
    set_num(scope, this, "selectionEnd", end.min(len));
}

fn cb_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_ok(scope, args.this()) { return; }
    let t = text_of(scope, args.this());
    let v = str_val(scope, &String::from_utf16_lossy(&t));
    rv.set(v);
}

fn cb_selection_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_ok(scope, args.this()) { return; }
    rv.set_uint32(num_of(scope, args.this(), "selectionStart"));
}

fn cb_selection_end(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_ok(scope, args.this()) { return; }
    rv.set_uint32(num_of(scope, args.this(), "selectionEnd"));
}

fn cb_range_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_ok(scope, args.this()) { return; }
    rv.set_uint32(num_of(scope, args.this(), "characterBoundsRangeStart"));
}

/// `updateText(rangeStart, rangeEnd, text)`: the range (in either order, clamped to the text) is
/// replaced by `text`.
fn cb_update_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !this_ok(scope, this) { return; }
    let mut text = text_of(scope, this);
    let len = text.len() as u32;
    let a = ulong(scope, args.get(0)).min(len);
    let b = ulong(scope, args.get(1)).min(len);
    let (lo, hi) = (a.min(b) as usize, a.max(b) as usize);
    let insert: Vec<u16> = args.get(2).to_rust_string_lossy(scope).encode_utf16().collect();
    text.splice(lo..hi, insert);
    set_text(scope, this, &text);
}

/// `updateSelection(start, end)`, each clamped to the text.
fn cb_update_selection(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !this_ok(scope, this) { return; }
    let len = text_of(scope, this).len() as u32;
    let s = ulong(scope, args.get(0)).min(len);
    let e = ulong(scope, args.get(1)).min(len);
    set_num(scope, this, "selectionStart", s);
    set_num(scope, this, "selectionEnd", e);
}

/// `updateControlBounds` / `updateSelectionBounds`: the rectangles are the IME's, which a headless
/// page has none of; they are accepted and kept nowhere, which is what a page can observe of them.
fn cb_update_bounds(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _ = this_ok(scope, args.this());
}

/// `updateCharacterBounds(rangeStart, characterBounds)` and the two readers of what it stored.
fn cb_update_character_bounds(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !this_ok(scope, this) { return; }
    let start = ulong(scope, args.get(0));
    set_num(scope, this, "characterBoundsRangeStart", start);
    let k = key(scope, "characterBounds");
    let bounds = args.get(1);
    this.set_private(scope, k, bounds);
}

fn cb_character_bounds(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !this_ok(scope, this) { return; }
    let k = key(scope, "characterBounds");
    let stored = this.get_private(scope, k).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    let len = stored.map(|a| a.length()).unwrap_or(0);
    let out = v8::Array::new(scope, len as i32);
    if let Some(a) = stored {
        for i in 0..len {
            if let Some(v) = a.get_index(scope, i) {
                out.set_index(scope, i, v);
            }
        }
    }
    rv.set(out.into());
}

fn cb_attached_elements(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !this_ok(scope, args.this()) { return; }
    rv.set(v8::Array::new(scope, 0).into());
}

/// The kit (`tools/sync/gen_shells.py` claims EditContext's rows as `edit_context`).
pub(crate) fn install(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "text" => { let g = build_fn(scope, cb_text, "get text", 0); define_accessor(scope, proto, member, g, None); }
        "selectionStart" => { let g = build_fn(scope, cb_selection_start, "get selectionStart", 0); define_accessor(scope, proto, member, g, None); }
        "selectionEnd" => { let g = build_fn(scope, cb_selection_end, "get selectionEnd", 0); define_accessor(scope, proto, member, g, None); }
        "characterBoundsRangeStart" => { let g = build_fn(scope, cb_range_start, "get characterBoundsRangeStart", 0); define_accessor(scope, proto, member, g, None); }
        "updateText" => method_row(scope, proto, member, cb_update_text),
        "updateSelection" => method_row(scope, proto, member, cb_update_selection),
        "updateControlBounds" | "updateSelectionBounds" => method_row(scope, proto, member, cb_update_bounds),
        "updateCharacterBounds" => method_row(scope, proto, member, cb_update_character_bounds),
        "characterBounds" => method_row(scope, proto, member, cb_character_bounds),
        "attachedElements" => method_row(scope, proto, member, cb_attached_elements),
        other => debug_assert!(false, "edit_context::install: unexpected member {other}"),
    }
}
