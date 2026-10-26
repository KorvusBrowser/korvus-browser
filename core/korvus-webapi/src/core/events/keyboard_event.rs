//! `core/events/keyboard_event.rs` — `KeyboardEvent`, `InputEvent` and `FocusEvent`.
//!
//! The three `UIEvent` subclasses of the text-entry plane. They are one file because they are one
//! interaction: a key press produces a `KeyboardEvent`, the editing host turns it into an
//! `InputEvent`, and moving between fields produces a `FocusEvent` — and all three were the same
//! three-line `class … extends UIEvent` in `host.js`, with the prototype described a second time in
//! `generated_event_protos.rs` (KeyboardEvent) or `generated_registry.rs` (the other two).
//!
//! Measured against `google-chrome --headless=new`: `{location: 2.9}` reads back `2` (WebIDL
//! `unsigned long` truncates), `new InputEvent("i").data` is `null`, `new InputEvent("i").inputType`
//! is `""`, `new FocusEvent("f").relatedTarget` is `null`, and `getTargetRanges()` answers an empty
//! array rather than the `Illegal invocation` a D2 stub row gave it.

use deno_core::v8;

use crate::core::events::ctor::{
    arg_string, brand, dict_bool, dict_int, dict_interface, dict_string, init_dict, init_event, require_args, require_new,
};
use crate::core::events::event::{
    evt_slot_write, evt_slot_write_null, evt_slot_write_num, install_init_keyboard_event, install_modifier_state,
};
use crate::core::events::ui_event::init_ui_event;
use crate::install::MemberDef;

fn write_bool(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, b: bool) {
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
    evt_slot_write(scope, this, name, v);
}
fn write_str(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, s: &str) {
    let v = crate::binding::protos::str_val(scope, s);
    evt_slot_write(scope, this, name, v);
}

// ── KeyboardEvent ───────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_keyboard_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "KeyboardEvent") || !require_args(scope, &args, "KeyboardEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "KeyboardEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "KeyboardEvent", "KeyboardEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() {
        return;
    }
    for name in ["key", "code"] {
        let s = match dict_string(scope, dict, name) { Ok(v) => v.unwrap_or_default(), Err(_) => return };
        write_str(scope, this, name, &s);
    }
    let location = match dict_int(scope, dict, "location", 32, false) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    evt_slot_write_num(scope, this, "location", location);
    for name in ["repeat", "isComposing", "ctrlKey", "shiftKey", "altKey", "metaKey"] {
        let b = dict_bool(scope, dict, name);
        write_bool(scope, this, name, b);
    }
    crate::core::events::event::init_modifier_states(scope, this, dict);
    // The two deprecated numeric members. `KeyboardEventInit` still declares both.
    let char_code = match dict_int(scope, dict, "charCode", 32, false) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    let key_code = match dict_int(scope, dict, "keyCode", 32, false) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    evt_slot_write_num(scope, this, "charCode", char_code);
    evt_slot_write_num(scope, this, "keyCode", key_code);
    // `UIEvent.which` on a key event is the keyCode, not a button number.
    evt_slot_write_num(scope, this, "which", key_code);
    brand(scope, this, "KeyboardEvent");
    rv.set(this.into());
}

// ── InputEvent ──────────────────────────────────────────────────────────────────────────────────

/// The Input Events Level 2 `inputType` values Blink's InputType enum names (third_party/blink/.../input_type_names).
const INPUT_TYPES: &[&str] = &[
    "insertText", "insertReplacementText", "insertLineBreak", "insertParagraph", "insertOrderedList",
    "insertUnorderedList", "insertHorizontalRule", "insertFromYank", "insertFromDrop", "insertFromPaste",
    "insertFromPasteAsQuotation", "insertTranspose", "insertCompositionText", "insertLink", "deleteWordBackward",
    "deleteWordForward", "deleteSoftLineBackward", "deleteSoftLineForward", "deleteEntireSoftLine",
    "deleteHardLineBackward", "deleteHardLineForward", "deleteByDrag", "deleteByCut", "deleteContent",
    "deleteContentBackward", "deleteContentForward", "historyUndo", "historyRedo", "formatBold", "formatItalic",
    "formatUnderline", "formatStrikeThrough", "formatSuperscript", "formatSubscript", "formatJustifyFull",
    "formatJustifyCenter", "formatJustifyRight", "formatJustifyLeft", "formatIndent", "formatOutdent",
    "formatRemove", "formatSetBlockTextDirection", "formatSetInlineTextDirection", "formatBackColor",
    "formatFontColor", "formatFontName",
];

pub(crate) fn cb_input_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "InputEvent") || !require_args(scope, &args, "InputEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "InputEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "InputEvent", "InputEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() {
        return;
    }
    // `DOMString? data` -- null when absent OR explicitly null; the dictionary distinguishes neither.
    match dict_string(scope, dict, "data") {
        Ok(Some(s)) => write_str(scope, this, "data", &s),
        Ok(None) => evt_slot_write_null(scope, this, "data"),
        Err(_) => return,
    }
    // Blink holds `inputType` as its InputType enum: a string it does not know reads back "" (the
    // probe: `new InputEvent("i", {inputType: "nonsense"}).inputType` is "" in Chrome 150).
    let input_type = match dict_string(scope, dict, "inputType") { Ok(v) => v.unwrap_or_default(), Err(_) => return };
    let input_type = if INPUT_TYPES.contains(&input_type.as_str()) { input_type } else { String::new() };
    write_str(scope, this, "inputType", &input_type);
    let composing = dict_bool(scope, dict, "isComposing");
    write_bool(scope, this, "isComposing", composing);
    match dict_interface(scope, dict, "dataTransfer", "InputEvent", "InputEventInit", &["DataTransfer"]) {
        Ok(Some(v)) if !v.is_null() => evt_slot_write(scope, this, "dataTransfer", v),
        Ok(_) => evt_slot_write_null(scope, this, "dataTransfer"),
        Err(_) => return,
    }
    brand(scope, this, "InputEvent");
    rv.set(this.into());
}

/// `getTargetRanges()` — the static ranges an input event would replace. This engine composes no
/// text and has no editing host that stages one, so the list is empty, which is also what Chrome
/// answers for a script-constructed `InputEvent`. A real body, not a stub: the row used to be a D2
/// stub that threw `Illegal invocation` on a genuine `InputEvent`.
fn cb_input_target_ranges(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let a = v8::Array::new(scope, 0);
    rv.set(a.into());
}
pub fn install_input_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    crate::core::events::event::evt_branded_method(scope, proto, member, cb_input_target_ranges);
}

// ── FocusEvent ──────────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_focus_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "FocusEvent") || !require_args(scope, &args, "FocusEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "FocusEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "FocusEvent", "FocusEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() {
        return;
    }
    match dict_interface(scope, dict, "relatedTarget", "FocusEvent", "FocusEventInit", &["EventTarget"]) {
        Ok(Some(v)) if !v.is_null() => evt_slot_write(scope, this, "relatedTarget", v),
        Ok(_) => evt_slot_write_null(scope, this, "relatedTarget"),
        Err(_) => return,
    }
    brand(scope, this, "FocusEvent");
    rv.set(this.into());
}

// ── the three tables ────────────────────────────────────────────────────────────────────────────

webapi::interface! {
    "KeyboardEvent" : "UIEvent" [value, window] {
        ctor            "KeyboardEvent"                              len 1 = stub,
        attr            "key"                       : Any readonly         = evt_get,
        attr            "code"                      : Any readonly         = evt_get,
        attr            "location"                  : Any readonly         = evt_get,
        attr            "ctrlKey"                   : Any readonly         = evt_get,
        attr            "shiftKey"                  : Any readonly         = evt_get,
        attr            "altKey"                    : Any readonly         = evt_get,
        attr            "metaKey"                   : Any readonly         = evt_get,
        attr            "repeat"                    : Any readonly         = evt_get,
        attr            "isComposing"               : Any readonly         = evt_get,
        attr            "charCode"                  : Any readonly         = evt_get,
        attr            "keyCode"                   : Any readonly         = evt_get,
        constant        "DOM_KEY_LOCATION_STANDARD" : Any = 0,
        constant        "DOM_KEY_LOCATION_LEFT"     : Any = 1,
        constant        "DOM_KEY_LOCATION_RIGHT"    : Any = 2,
        constant        "DOM_KEY_LOCATION_NUMPAD"   : Any = 3,
        method          "getModifierState"          : Any len 1 required 1 = install_modifier_state,
        method          "initKeyboardEvent"         : Any len 1 required 1 = install_init_keyboard_event,
        static_constant "DOM_KEY_LOCATION_STANDARD" : Any = 0,
        static_constant "DOM_KEY_LOCATION_LEFT"     : Any = 1,
        static_constant "DOM_KEY_LOCATION_RIGHT"    : Any = 2,
        static_constant "DOM_KEY_LOCATION_NUMPAD"   : Any = 3
    }
}

webapi::interface! {
    "InputEvent" : "UIEvent" [value, window] {
        ctor   "InputEvent"                            len 1 = stub,
        attr   "data"            : Any readonly nullable     = evt_get_null,
        attr   "isComposing"     : Any readonly              = evt_get,
        attr   "inputType"       : Any readonly              = evt_get,
        attr   "dataTransfer"    : Any readonly nullable     = evt_get_null,
        method "getTargetRanges" : Any len 0                 = install_input_event
    }
}

webapi::interface! {
    "FocusEvent" : "UIEvent" [value, window] {
        ctor "FocusEvent"                             len 1 = stub,
        attr "relatedTarget" : Any readonly nullable         = evt_get_null
    }
}
