//! `core/events/typing.rs` — typing: where a keystroke goes, and what an edit does to the field.
//!
//! # What this is, and what it replaces
//!
//! Once every synthetic keystroke this engine produced was dispatched **on
//! `document`** ([`shared::dispatch_trusted_key`]), and the only thing that could change a field's
//! `value` was `Engine::type_into`, which took a CSS SELECTOR. CDP's `Input.insertText` and
//! `Input.dispatchKeyEvent` carry no selector — they target whatever has focus — so nothing that
//! arrived over the protocol could reach it. The interactivity sweep measured the result: the
//! `input` capability scored 0 of its 169 floor rows, with `counts.keydown` non-zero on every one
//! of them. The keystrokes were landing; they were landing nowhere useful.
//!
//! This file is the browser half: resolve the focus, dispatch at it, and run the editing command.
//!
//! # Asked of the browser, not read off the spec
//!
//! Stock Chrome 150.0.7871.46, headed under Xvfb, launched with `--remote-debugging-port` (never
//! by Playwright's launcher, which adds `--enable-automation`), attached over CDP, with a
//! capturing listener on `window` for every keyboard / input / composition / focus event recording
//! `type`, `target`, `constructor.name`, `keyCode`, `charCode`, `data`, `inputType` and the
//! field's `value` at that instant. A FRESH NAVIGATION before each case, because the first run of
//! this probe answered "nothing focused" wrongly: a selection left inside the field by the
//! previous case made Chrome re-focus it, and the contaminated reading said insertText types at
//! the body. It does not.
//!
//! **`Input.insertText` into a focused `<input>`** — no key events at all, and the whole string in
//! ONE edit:
//!
//! ```text
//! beforeinput  on #f   bubbles cancelable   InputEvent  data="ab"  inputType="insertText"   value=""
//! textInput    on #f   bubbles cancelable   TextEvent   data="ab"                           value=""
//! input        on #f   bubbles              InputEvent  data="ab"  inputType="insertText"   value="ab"
//! ```
//!
//! `"hello"` produces exactly the same three events with `data="hello"`, not five of anything —
//! `insertText` is not a keystroke stream and must not be dispatched as one.
//!
//! **`Input.dispatchKeyEvent` `keyDown{text:"a"}` + `keyUp`** into the same field:
//!
//! ```text
//! keydown      on #f   KeyboardEvent  key="a"  keyCode=65  charCode=0
//! keypress     on #f   KeyboardEvent  key="a"  keyCode=97  charCode=97
//! beforeinput  on #f   InputEvent     data="a" inputType="insertText"
//! textInput    on #f   TextEvent      data="a"
//! input        on #f   InputEvent     data="a" inputType="insertText"   value="a"
//! keyup        on #f   KeyboardEvent  key="a"  keyCode=65  charCode=0
//! ```
//!
//! `keypress`'s `keyCode` is the CHARACTER's code point and `keydown`/`keyup`'s is the virtual-key
//! code — they differ for every key whose two differ (`'a'` 65/97, `'A'` 65/65, `'-'` 189/45,
//! `'1'` 49/49, `' '` 32/32), which is why [`vk_code`] and the character are computed separately.
//!
//! **With nothing focused** (`document.activeElement === document.body`, fresh page):
//!
//! ```text
//! Input.insertText          -> NOTHING. No event of any kind, no value change.
//! Input.dispatchKeyEvent    -> keydown / keypress / keyup on <body>. No beforeinput, no input.
//! ```
//!
//! and the same for a focused element that is not editable (a `<button>`, a `<div tabindex=0>`):
//! the key events fire on it, the editing command does not run. So "types at the document" was
//! wrong in both directions — Chrome types at the BODY, and only when it is a key event at all.
//!
//! **The cancellation points**, both measured rather than assumed:
//!
//! ```text
//! keydown     preventDefault  -> no keypress, no beforeinput, no input, value unchanged; keyup still fires
//! beforeinput preventDefault  -> no textInput, no input,                value unchanged
//! readonly field              -> beforeinput and textInput fire, input does NOT, value unchanged
//! ```
//!
//! **Backspace** (`keyDown{key:"Backspace"}`, no `text`, caret at 3 in `"abc"`):
//!
//! ```text
//! keydown      key="Backspace" keyCode=8   (no keypress -- it produces no character)
//! beforeinput  data=null  inputType="deleteContentBackward"
//! input        data=null  inputType="deleteContentBackward"   value="ab"
//! keyup        key="Backspace" keyCode=8
//! ```
//!
//! **Enter** into a single-line `<input>`: `keydown`, `keypress` (keyCode 13, charCode 13),
//! `beforeinput` with `inputType="insertLineBreak"`, no `input` and no value change -- and then,
//! IMPLICIT SUBMISSION, which this paragraph used to call "nothing": `change` if the user
//! edited the value, the form's default button's `click`, its `submit` (recorded with real X11 keys,
//! `tests/fixtures/key-events/`; `html_input_element.cc:1618-1638`, `html_form_element.cc:539-577`).
//! The "nothing" was a field in no form, where the submission has no form to submit.
//!
//! **The whole key event.** Every trusted key event carried no modifier state, CDP's
//! `modifiers` / `windowsVirtualKeyCode` / `location` / `autoRepeat` were dropped, Shift's `keyCode`
//! was 83, the editing command was chosen from the key NAME, and a key press was no user activation.
//! [`KeyInput`] is the whole `WebKeyboardEvent` now, [`key_at_focus`] runs Blink's
//! `KeyboardEventManager::KeyEvent` over it, and `core/events/user_activation.rs` holds the activation.
//!
//! # What is NOT here, named rather than overlooked
//!
//! **`textInput`.** Chrome fires it between `beforeinput` and `input`, and this engine does not.
//! It is a `TextEvent`, an interface Chrome itself declares `newable: false` (the capture records
//! it) and that this tree carries as a D2 stub row in the generated registry — `data` and
//! `initTextEvent` both `BodyKind::Stub`. Dispatching a `textInput` whose object was an
//! `InputEvent` or a bare `Event` would be a forgery a page reads off `e.constructor.name` in one
//! line, and the D2 stub is the honest, counted representation of an interface with no body. So
//! the one difference from the sequences above is declared here, asserted by the live gate as a
//! difference, and left for whoever implements `TextEvent`.
//!
//! **`change` is here, and it is the focus ring that fires it.** Chrome fires it on the field when
//! the user's own editing changed the value and the focus then leaves --- before `blur`, not after.
//! The condition is measured and is stricter than either half alone: see [`UserEditState`].
//!
//! **A caret from geometry.** The insertion point is `selectionStart`/`selectionEnd` — real API
//! state this engine already keeps and clamps — and a press does not move it, because Korvus has
//! no caret geometry. A field nobody has called `setSelectionRange` on inserts at offset 0 and the
//! selection advances from there, so consecutive typing appends exactly as it does in a browser.

use deno_core::v8;
use korvus_dom::NodeId;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{call_method, get_prop, int_val, set_bool, set_prop, str_val, wrap_cached};
use crate::core::events::focus;
use crate::core::events::shared::build_trusted_key_full;
use crate::core::html::html_dialog_element::mark_trusted;

/// The value a text control held immediately before the USER's first edit of the current focus
/// session, keyed by arena `NodeId`. Absent = no user edit has touched it since it was focused.
///
/// This is what `change` compares against when the focus leaves, and the comparison is Chrome's,
/// measured rather than reasoned (stock Chrome 150, fresh navigation per case, the frame focused by
/// a pointer move first so that the focus events dispatch at all):
///
/// ```text
/// type "XY" into "seed", then blur                      -> change, then blur, then focusout
/// type "XY" then Backspace twice (back to "seed"), blur -> NO change
/// focus and blur with no typing                         -> NO change
/// el.value = "PROG" from script, then blur              -> NO change
/// type "XY", then el.value = "seed" from script, blur   -> NO change
/// ```
///
/// So neither "a value that differs from the one at focus time" nor "an edit happened" is the rule
/// on its own: it is BOTH, against the value from just before the first USER edit. A script's write
/// never earns a `change`, and an edit the user undoes does not either.
///
/// Same shape as `FocusState` (and as `CheckedState` was until checkedness moved into the
/// arena): OpState-owned, keyed by node id, because the
/// reader (`core/events/focus.rs`, leaving the element) and the writer (the editing command below)
/// meet nowhere else.
#[derive(Default)]
pub struct UserEditState(pub std::cell::RefCell<std::collections::HashMap<u32, String>>);

/// What one editing command does to the field. `Insert` carries the text; `DeleteBackward` is
/// Backspace, `DeleteForward` Delete; `LineBreak` is Enter, which a `<textarea>` inserts and a
/// single-line control announces and turns into implicit submission.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Insert(String),
    DeleteBackward,
    DeleteForward,
    LineBreak,
    /// An IME composition's text replacing the composition so far (D88 step 2).
    Compose(String),
}

impl Edit {
    /// The `InputEvent.inputType` Chrome reports for this command.
    fn input_type(&self) -> &'static str {
        match self {
            Edit::Insert(_) => "insertText",
            Edit::Compose(_) => "insertCompositionText",
            Edit::DeleteBackward => "deleteContentBackward",
            Edit::DeleteForward => "deleteContentForward",
            Edit::LineBreak => "insertLineBreak",
        }
    }
    /// The `InputEvent.data` Chrome reports: the inserted text, or `null` for a deletion or a
    /// line break.
    fn data(&self) -> Option<&str> {
        match self {
            Edit::Insert(s) | Edit::Compose(s) => Some(s.as_str()),
            _ => None,
        }
    }
    /// A composition's edit cannot be cancelled -- its `beforeinput` is not cancelable (UI Events; Chrome 150) -- and
    /// reads `isComposing`.
    fn composing(&self) -> bool {
        matches!(self, Edit::Compose(_))
    }
}

// `Edit::for_key` IS GONE. It chose the command from the key NAME, falling back to the
// name when there was no text -- so a CDP `keyDown{key:"a"}` with no `text` typed an "a" Chrome never
// types, and Backspace deleted whatever its `windowsVirtualKeyCode` said. Blink picks the command from
// the virtual key and the modifiers on `keydown` and from the TEXT on `keypress`
// (`editing_behavior.cc:242-288`); `keydown_command` / `keypress_is_line_break` /
// `should_insert_character` below are those three tables.

/// The virtual-key code for `keydown`/`keyup` — the legacy `keyCode`, which is a US-layout
/// **scan-code-ish** number and NOT the character. Measured off Chrome 150 on Linux, which reports
/// the Windows VK numbering (`'-'` is 189, not 45).
///
/// `code` (the physical key, `"KeyA"`/`"Digit1"`/`"Minus"`) is preferred where the behaviour model
/// supplies one, because it is layout-independent; `key` is the fallback for a source that only
/// names the character.
fn vk_code(key: &str, code: &str) -> f64 {
    if let Some(rest) = code.strip_prefix("Key") {
        if let Some(c) = rest.chars().next() {
            return c.to_ascii_uppercase() as u32 as f64;
        }
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        if let Some(c) = rest.chars().next() {
            return c as u32 as f64;
        }
    }
    if let Some(n) = code.strip_prefix('F').and_then(|r| r.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
        return (0x6F + n) as f64; // VKEY_F1 = 0x70
    }
    if let Some(d) = code.strip_prefix("Numpad").and_then(|r| r.parse::<u32>().ok()).filter(|d| *d <= 9) {
        return (0x60 + d) as f64; // VKEY_NUMPAD0 = 0x60
    }
    let named = match code {
        "Space" => Some(32),
        "Enter" | "NumpadEnter" => Some(13),
        "Backspace" => Some(8),
        "Tab" => Some(9),
        "Escape" => Some(27),
        "ArrowLeft" => Some(37),
        "ArrowUp" => Some(38),
        "ArrowRight" => Some(39),
        "ArrowDown" => Some(40),
        // The modifier and editing keys. Shift answered 83 -- the `'S'` its NAME starts
        // with -- because no row here named it and the key-name fallback below took the first letter.
        // Recorded with real X11 keys on Chrome 150: ShiftLeft 16, ControlLeft 17. The rest are
        // `ui/events/keycodes/keyboard_codes_posix.h`'s VKEY values for the same DOM codes
        // (`dom_code_data.inc`).
        "ShiftLeft" | "ShiftRight" => Some(16),
        "ControlLeft" | "ControlRight" => Some(17),
        "AltLeft" | "AltRight" => Some(18),
        "MetaLeft" => Some(91),
        "MetaRight" => Some(92),
        "ContextMenu" => Some(93),
        "CapsLock" => Some(20),
        "NumLock" => Some(144),
        "ScrollLock" => Some(145),
        "Pause" => Some(19),
        "PrintScreen" => Some(44),
        "Insert" => Some(45),
        "Delete" => Some(46),
        "Home" => Some(36),
        "End" => Some(35),
        "PageUp" => Some(33),
        "PageDown" => Some(34),
        "NumpadMultiply" => Some(106),
        "NumpadAdd" => Some(107),
        "NumpadSubtract" => Some(109),
        "NumpadDecimal" => Some(110),
        "NumpadDivide" => Some(111),
        "IntlBackslash" => Some(226),
        "Semicolon" => Some(186),
        "Equal" => Some(187),
        "Comma" => Some(188),
        "Minus" => Some(189),
        "Period" => Some(190),
        "Slash" => Some(191),
        "Backquote" => Some(192),
        "BracketLeft" => Some(219),
        "Backslash" => Some(220),
        "BracketRight" => Some(221),
        "Quote" => Some(222),
        _ => None,
    };
    if let Some(n) = named {
        return n as f64;
    }
    match key {
        "Enter" => 13.0,
        "Backspace" => 8.0,
        "Tab" => 9.0,
        "Escape" => 27.0,
        "ArrowLeft" => 37.0,
        "ArrowUp" => 38.0,
        "ArrowRight" => 39.0,
        "ArrowDown" => 40.0,
        "Shift" => 16.0,
        "Control" => 17.0,
        "Alt" => 18.0,
        "Meta" => 91.0,
        "CapsLock" => 20.0,
        "Delete" => 46.0,
        "Insert" => 45.0,
        "Home" => 36.0,
        "End" => 35.0,
        "PageUp" => 33.0,
        "PageDown" => 34.0,
        " " => 32.0,
        // A CHARACTER only: a key name of more than one character is never read by its first letter.
        k if k.chars().count() != 1 => 0.0,
        k => match k.chars().next() {
            Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase() as u32 as f64,
            Some(c) if c.is_ascii_digit() => c as u32 as f64,
            // The shifted digits sit on their digit's key (`'@'` is Digit2, keyCode 50: recorded).
            Some(c) if "!@#$%^&*()".contains(c) => b"1234567890"["!@#$%^&*()".find(c).unwrap_or(0)] as f64,
            Some(c) if (c as u32) < 128 => match c {
                ';' | ':' => 186.0,
                '=' | '+' => 187.0,
                ',' | '<' => 188.0,
                '-' | '_' => 189.0,
                '.' | '>' => 190.0,
                '/' | '?' => 191.0,
                '`' | '~' => 192.0,
                '[' | '{' => 219.0,
                '\\' | '|' => 220.0,
                ']' | '}' => 221.0,
                '\'' | '"' => 222.0,
                _ => 0.0,
            },
            _ => 0.0,
        },
    }
}

/// How editable the focused element is. `Text` is a control with a `value` a user may edit;
/// `TextReadOnly` is one whose value is frozen but which still ANNOUNCES the edit (Chrome fires
/// `beforeinput` on a readonly field and then refuses); `No` runs no editing command at all.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Editable {
    Text,
    TextReadOnly,
    No,
}

/// The input `type` values whose value is free text a keystroke edits. `date`/`color`/`range` and
/// the button-ish types are excluded: they have pickers, and typing into one does not append to
/// its value.
pub(crate) fn text_entry_type(ty: &str) -> bool {
    matches!(ty, "text" | "search" | "url" | "tel" | "password" | "email" | "number" | "")
}

/// Read the editability of `id` off the DOM: tag, `type`, `readonly`, `disabled`.
fn editable(dom: &korvus_dom::Dom, id: NodeId) -> Editable {
    let Some(tag) = dom.tag_name(id) else { return Editable::No };
    let tag = tag.to_ascii_lowercase();
    let frozen = dom.get_attribute(id, "readonly").is_some() || dom.get_attribute(id, "disabled").is_some();
    let is_text = match tag.as_str() {
        "input" => {
            let ty = dom
                .get_attribute(id, "type")
                .map(|t| t.trim().to_ascii_lowercase())
                .unwrap_or_default();
            text_entry_type(&ty)
        }
        "textarea" => true,
        _ => false,
    };
    if !is_text {
        // A contenteditable subtree IS editable in Chrome and this engine has no text-node editing
        // model for one, so it is reported as not editable rather than edited wrongly. The key
        // events still fire on it; only the command does not run. Counted, not hidden.
        return Editable::No;
    }
    if frozen {
        Editable::TextReadOnly
    } else {
        Editable::Text
    }
}

/// Build a trusted `InputEvent` of `kind` for `edit` and dispatch it on `el`. Returns the
/// event's "not cancelled" answer — `false` when a listener called `preventDefault()`, which is
/// what stops `beforeinput` from becoming an `input`.
fn fire_input_event(
    scope: &mut v8::PinScope,
    el: v8::Local<v8::Object>,
    kind: &str,
    edit: &Edit,
    cancelable: bool,
    timestamp_ms: f64,
) -> bool {
    let Some(ctor) =
        crate::binding::protos::iface_object(scope, "InputEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return false;
    };
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", true);
    set_bool(scope, init, "cancelable", cancelable);
    // Composed, like every UA-fired input event (recorded with real keys: `beforeinput` and `input`
    // both `composed: true`, and both with a NULL `view`, which the absent `view` already is).
    set_bool(scope, init, "composed", true);
    if edit.composing() {
        set_bool(scope, init, "isComposing", true);
    }
    // `data` is ABSENT rather than explicitly null for a deletion, and the difference is not
    // cosmetic: `InputEventInit`'s `DOMString? data` goes through `dict_string`, whose `present()`
    // filters `undefined` and not `null`, so `{data: null}` would reach the slot as the STRING
    // "null". Absent takes the `Ok(None)` arm and writes a real null, which is what Chrome reports
    // for `deleteContentBackward`. (The ctor's own handling of an explicit null is a separate,
    // pre-existing divergence; it is not on this path and is not silently worked around here --
    // nothing on this path ever passes one.)
    if let Some(d) = edit.data() {
        let dv = str_val(scope, d);
        set_prop(scope, init, "data", dv);
    }
    let it = str_val(scope, edit.input_type());
    set_prop(scope, init, "inputType", it);
    let ty = str_val(scope, kind);
    let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) else { return false };
    mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", timestamp_ms);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, el.into(), "dispatchEvent", &[evv])
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false)
}

/// Splice `edit` into the control's value at its selection, and leave the selection collapsed
/// after the change. `None` when the edit changed nothing (Backspace at offset 0, an empty
/// insertion, a line break in a single-line control).
///
/// Offsets are UTF-16 code units, because that is what `selectionStart` indexes.
fn apply_edit(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, edit: &Edit) -> Option<String> {
    let cur = get_prop(scope, el, "value")
        .filter(|v| v.is_string())
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let units: Vec<u16> = cur.encode_utf16().collect();
    let len = units.len();
    let read_off = |scope: &mut v8::PinScope, name: &str| -> Option<usize> {
        get_prop(scope, el, name)
            .filter(|v| v.is_number())
            .and_then(|v| v.number_value(scope))
            .map(|n| (n.max(0.0) as usize).min(len))
    };
    // A control with no selection API (`<input type=email>`, `<input type=number>`) answers null:
    // its caret is the end of the value, which is where a person who has just focused it types.
    let start = read_off(scope, "selectionStart").unwrap_or(len);
    let end = read_off(scope, "selectionEnd").unwrap_or(len).max(start);
    let (new_units, caret) = match edit {
        Edit::Insert(s) | Edit::Compose(s) => {
            let ins: Vec<u16> = s.encode_utf16().collect();
            if ins.is_empty() && start == end {
                return None;
            }
            let mut out = units[..start].to_vec();
            out.extend_from_slice(&ins);
            out.extend_from_slice(&units[end..]);
            (out, start + ins.len())
        }
        Edit::DeleteBackward => {
            if end > start {
                let mut out = units[..start].to_vec();
                out.extend_from_slice(&units[end..]);
                (out, start)
            } else if start == 0 {
                return None;
            } else {
                // One code POINT, not one code unit: a surrogate pair is deleted whole.
                let back = if start >= 2 && (0xDC00..=0xDFFF).contains(&units[start - 1]) && (0xD800..=0xDBFF).contains(&units[start - 2]) { 2 } else { 1 };
                let mut out = units[..start - back].to_vec();
                out.extend_from_slice(&units[start..]);
                (out, start - back)
            }
        }
        Edit::DeleteForward => {
            if end > start {
                let mut out = units[..start].to_vec();
                out.extend_from_slice(&units[end..]);
                (out, start)
            } else if start == len {
                return None;
            } else {
                let fwd = if start + 1 < len && (0xD800..=0xDBFF).contains(&units[start]) && (0xDC00..=0xDFFF).contains(&units[start + 1]) { 2 } else { 1 };
                let mut out = units[..start].to_vec();
                out.extend_from_slice(&units[start + fwd..]);
                (out, start)
            }
        }
        // Only a `<textarea>` gets here: a single-line field turns its line break into implicit
        // submission before any edit (`keypress_default`).
        Edit::LineBreak => {
            let mut out = units[..start].to_vec();
            out.push(u16::from(b'\n'));
            out.extend_from_slice(&units[end..]);
            (out, start + 1)
        }
    };
    let newv = String::from_utf16_lossy(&new_units);
    let nv = str_val(scope, &newv);
    set_prop(scope, el, "value", nv);
    // Collapse the selection where the caret now is, so consecutive keystrokes append rather than
    // overwriting the same offset. `setSelectionRange` throws on a control that has no selection
    // API, which is why the call is guarded by the getter answering a number.
    if get_prop(scope, el, "selectionStart").map(|v| v.is_number()).unwrap_or(false) {
        let a = int_val(scope, caret as i64);
        let b = int_val(scope, caret as i64);
        call_method(scope, el.into(), "setSelectionRange", &[a, b]);
    }
    Some(newv)
}

/// Run one editing command at the focused element: `beforeinput`, the value change, then `input`.
/// Returns how many events were dispatched (0, 1 or 2).
///
/// The three ways it stops short are all Chrome's, all measured: a `beforeinput` a listener
/// cancels fires no `input`; a readonly control fires `beforeinput` and no `input`; and an edit
/// that changes nothing (Backspace at offset 0, Enter in a single-line field) fires no `input`
/// either. In every one of them the value is unchanged, so the pair "value changed" / "input
/// fired" never comes apart.
fn run_edit(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, ed: Editable, edit: &Edit, timestamp_ms: f64) -> usize {
    if ed == Editable::No {
        return 0;
    }
    if !fire_input_event(scope, el, "beforeinput", edit, !edit.composing(), timestamp_ms) && !edit.composing() {
        return 1; // cancelled: beforeinput fired, nothing else does
    }
    if ed == Editable::TextReadOnly {
        return 1;
    }
    1 + commit_edit(scope, el, edit, timestamp_ms)
}

/// The half of an editing command after its `beforeinput` was let through: the value change, the
/// user-edit record `change` compares against, and `input`. Returns 1 if `input` fired, 0 when the
/// edit changed nothing.
fn commit_edit(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, edit: &Edit, timestamp_ms: f64) -> usize {
    // The value as it stands BEFORE this edit. Recorded only if this is the first user edit of the
    // focus session, and only once the edit is going to happen -- a cancelled `beforeinput` or a
    // readonly refusal has changed nothing and must not arm a `change`.
    let before = get_prop(scope, el, "value")
        .filter(|v| v.is_string())
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    if apply_edit(scope, el, edit).is_none() {
        return 0;
    }
    if let Some(id) = crate::core::dom::element::el_id_of(scope, el) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if let Some(u) = st.try_borrow::<UserEditState>() {
            u.0.borrow_mut().entry(id).or_insert(before);
        }
    }
    fire_input_event(scope, el, "input", edit, false, timestamp_ms);
    1
}

/// Fire `change` on `node` if the user edited it since it was focused AND the value is not back
/// where it started, then forget the session either way. Returns 1 if it fired.
///
/// Called by `core/events/focus.rs` as the focus LEAVES, before `blur` -- which is where Chrome
/// puts it. A plain `Event`, bubbling and not cancelable, dispatched at the element (measured:
/// phase 2 at the control, phase 3 at `document`).
pub fn change_on_leaving(scope: &mut v8::PinScope, node: NodeId, timestamp_ms: f64) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let before = {
        let st = op_state.borrow();
        match st.try_borrow::<UserEditState>() {
            Some(u) => u.0.borrow_mut().remove(&node),
            None => None,
        }
    };
    let Some(before) = before else { return 0 };
    let el = wrap_cached(scope, &op_state, node);
    let now = get_prop(scope, el, "value")
        .filter(|v| v.is_string())
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    if now == before {
        return 0;
    }
    let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return 0;
    };
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", true);
    set_bool(scope, init, "cancelable", false);
    let ty = str_val(scope, "change");
    let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) else { return 0 };
    mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", timestamp_ms);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, el.into(), "dispatchEvent", &[evv]);
    1
}

/// Forget any half-finished edit session on `node`. The focus ARRIVING calls this, so a control
/// focused a second time starts its comparison from the value it has now rather than from a value
/// a previous session left behind.
pub fn forget_edit_session(scope: &mut v8::PinScope, node: NodeId) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    if let Some(u) = st.try_borrow::<UserEditState>() {
        u.0.borrow_mut().remove(&node);
    }
}

/// The focused element as a wrapper, with its editability. `None` when there is no document.
fn focus_pair<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Option<(v8::Local<'s, v8::Object>, Editable)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (id, ed) = {
        let st = op_state.borrow();
        let id = focus::key_target(&st)?;
        let dom = &st.try_borrow::<HostDom>()?.0;
        (id, editable(dom, id))
    };
    Some((wrap_cached(scope, &op_state, id), ed))
}

/// CDP `Input.insertText`: insert `text` at the focused element's caret as ONE edit, with no key
/// events. Returns how many events were dispatched — **0 when nothing editable is focused**, which
/// is Chrome's answer and not a failure (it fires nothing at all in that case).
///
/// During an IME composition it COMMITS the composition with `text` instead (D88 step 2, Chrome 150): a last
/// `compositionupdate`, the composition's edit, then `compositionend`.
pub fn insert_text_at_focus(scope: &mut v8::PinScope, text: &str, timestamp_ms: f64) -> usize {
    let Some((el, ed)) = focus_pair(scope) else { return 0 };
    if ed == Editable::No {
        return 0;
    }
    if let Some((start, len)) = composition_in(scope, el) {
        set_composition(scope, None);
        let mut n = fire_composition(scope, el, "compositionupdate", text, timestamp_ms);
        n += compose(scope, el, ed, start, len, text, timestamp_ms);
        return n + fire_composition(scope, el, "compositionend", text, timestamp_ms);
    }
    run_edit(scope, el, ed, &Edit::Insert(text.to_string()), timestamp_ms)
}

/// An IME composition in progress (D88 step 2): the control it is in, and the UTF-16 range of its value the
/// composition's text holds.
#[derive(Default)]
struct Composition(Option<(NodeId, usize, usize)>);

fn set_composition(scope: &mut v8::PinScope, c: Option<(NodeId, usize, usize)>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(Composition(c));
}

/// The composition in `el`, when there is one there.
fn composition_in(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> Option<(usize, usize)> {
    let id = crate::core::dom::element::el_id_of(scope, el)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<Composition>().and_then(|c| c.0).filter(|c| c.0 == id).map(|(_, s, l)| (s, l))
}

/// A trusted `CompositionEvent` of `kind` carrying `data` at `el`; 1 when it was dispatched.
fn fire_composition(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, kind: &str, data: &str, timestamp_ms: f64) -> usize {
    let Some(ctor) = crate::binding::protos::iface_object(scope, "CompositionEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return 0;
    };
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", true);
    set_bool(scope, init, "cancelable", kind == "compositionstart");
    set_bool(scope, init, "composed", true);
    let dv = str_val(scope, data);
    set_prop(scope, init, "data", dv);
    let ty = str_val(scope, kind);
    let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) else { return 0 };
    mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", timestamp_ms);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, el.into(), "dispatchEvent", &[evv]);
    1
}

/// The composition's edit: `text` in place of the `len` units at `start`.
fn compose(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, ed: Editable, start: usize, len: usize, text: &str, timestamp_ms: f64) -> usize {
    let a: v8::Local<v8::Value> = v8::Number::new(scope, start as f64).into();
    let b: v8::Local<v8::Value> = v8::Number::new(scope, (start + len) as f64).into();
    if get_prop(scope, el, "selectionStart").is_some_and(|v| v.is_number()) {
        crate::binding::protos::engine_call(scope, el.into(), "setSelectionRange", &[a, b]);
    }
    run_edit(scope, el, ed, &Edit::Compose(text.to_string()), timestamp_ms)
}

/// CDP `Input.imeSetComposition`: the IME's composition text at the focus, as Chrome 150 fires it -- a
/// `compositionstart` when one begins (at the selection, which it replaces), then `compositionupdate` and the
/// composition's `beforeinput` / `input` (`insertCompositionText`, `isComposing`) each time it changes. `insertText`
/// commits it ([`insert_text_at_focus`]). 0 when nothing editable is focused.
pub fn ime_set_composition_at_focus(scope: &mut v8::PinScope, text: &str, timestamp_ms: f64) -> usize {
    let Some((el, ed)) = focus_pair(scope) else { return 0 };
    if ed == Editable::No {
        return 0;
    }
    let Some(id) = crate::core::dom::element::el_id_of(scope, el) else { return 0 };
    let mut n = 0;
    let (start, len) = match composition_in(scope, el) {
        Some(c) => c,
        None => {
            n += fire_composition(scope, el, "compositionstart", "", timestamp_ms);
            let read = |scope: &mut v8::PinScope, name: &str| get_prop(scope, el, name).filter(|v| v.is_number()).and_then(|v| v.number_value(scope));
            let len_now = get_prop(scope, el, "value").filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope).encode_utf16().count()).unwrap_or(0);
            let s = read(scope, "selectionStart").map(|n| n as usize).unwrap_or(len_now);
            let e = read(scope, "selectionEnd").map(|n| n as usize).unwrap_or(s).max(s);
            (s, e - s)
        }
    };
    n += fire_composition(scope, el, "compositionupdate", text, timestamp_ms);
    n += compose(scope, el, ed, start, len, text, timestamp_ms);
    set_composition(scope, Some((id, start, text.encode_utf16().count())));
    n
}

/// Which of CDP's four `Input.dispatchKeyEvent` types this is — and, for a source that is not CDP
/// at all (the behaviour model's humanised stream), which of them it behaves like.
///
/// `Down` is the whole keystroke: `keydown`, then the `keypress` and the editing command its
/// default action runs. `RawDown` is the bare `keydown` with no `keypress`, which is what a
/// client sends when it means to supply the character separately as a `Char`. `Up` is the bare
/// `keyup`. This mapping is CDP's own contract (`keyboard_event_manager.cc:329-411`: a `kKeyDown`
/// dispatches its `keydown` as a `kRawKeyDown` and falls through to the `kChar` case only when it
/// carries text), and `rawKeyDown` + `char` + `keyUp` was measured to produce exactly the same six
/// events as `keyDown{text}` + `keyUp`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stroke {
    Down,
    RawDown,
    Char,
    Up,
}

/// A key event's modifier state -- the part of Blink's `WebInputEvent::Modifiers` a page can read,
/// through `shiftKey` / `ctrlKey` / `altKey` / `metaKey` and `getModifierState()`.
///
/// Every trusted key event this engine produced carried NONE of it. A `keydown` of `'@'`
/// typed with Shift held answered `shiftKey === false`, which stock Chrome never does -- recorded with
/// real X11 keys (`tests/fixtures/key-events/`): Shift's own `keydown` already reads `shiftKey: true`,
/// every event while it is held reads it, and Shift's `keyup` reads `false`. deviceandbrowserinfo
/// scored the difference `suspiciousClientSideBehavior`.
///
/// ONE carrier for the keyboard AND the pointer: [`KeyMods::write_init`] writes the state into any
/// `EventModifierInit` dictionary, so a `MouseEvent` / `PointerEvent` built while a key is held reads
/// the same bits ([`held_modifiers`] is the platform keyboard's state).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMods(pub u32);

impl KeyMods {
    pub const SHIFT: u32 = 1 << 0;
    pub const CTRL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    pub const META: u32 = 1 << 3;
    pub const ALT_GRAPH: u32 = 1 << 4;
    pub const CAPS_LOCK: u32 = 1 << 5;
    pub const NUM_LOCK: u32 = 1 << 6;
    pub const SCROLL_LOCK: u32 = 1 << 7;
    pub const FN: u32 = 1 << 8;
    pub const SYMBOL: u32 = 1 << 9;

    pub fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    /// CDP's `modifiers` bitmask -- Alt=1, Ctrl=2, Meta/Command=4, Shift=8 -- as
    /// `input_handler.cc:91-111` (`GetEventModifiers`) maps it. Nothing else is reachable over CDP:
    /// no lock state, no AltGraph.
    pub fn from_cdp(m: i64) -> Self {
        let mut out = 0;
        if m & 1 != 0 {
            out |= Self::ALT;
        }
        if m & 2 != 0 {
            out |= Self::CTRL;
        }
        if m & 4 != 0 {
            out |= Self::META;
        }
        if m & 8 != 0 {
            out |= Self::SHIFT;
        }
        KeyMods(out)
    }

    /// The four keys Blink's editing key bindings look at (`editing_behavior.cc:267`: `kShiftKey |
    /// kAltKey | kCtrlKey | kMetaKey`).
    fn editing(self) -> u32 {
        self.0 & (Self::SHIFT | Self::CTRL | Self::ALT | Self::META)
    }

    /// The modifier bit a key NAME holds down, for a key that is one. The three lock keys toggle
    /// rather than hold; see [`KeyMods::lock_for`].
    fn held_by(key: &str) -> u32 {
        match key {
            "Shift" => Self::SHIFT,
            "Control" => Self::CTRL,
            "Alt" => Self::ALT,
            "Meta" => Self::META,
            "AltGraph" => Self::ALT_GRAPH,
            "Fn" => Self::FN,
            "Symbol" => Self::SYMBOL,
            _ => 0,
        }
    }

    fn lock_for(key: &str) -> u32 {
        match key {
            "CapsLock" => Self::CAPS_LOCK,
            "NumLock" => Self::NUM_LOCK,
            "ScrollLock" => Self::SCROLL_LOCK,
            _ => 0,
        }
    }

    /// Write the state into an `EventModifierInit` dictionary: the four key booleans and the six
    /// `modifier*` states `getModifierState()` answers from (`event.rs::MODIFIER_STATES`). The event's
    /// own constructor converts them, so a trusted event and a script-built one with the same dict are
    /// the same object.
    pub fn write_init(self, scope: &mut v8::PinScope, init: v8::Local<v8::Object>) {
        for (name, bit) in [
            ("ctrlKey", Self::CTRL),
            ("shiftKey", Self::SHIFT),
            ("altKey", Self::ALT),
            ("metaKey", Self::META),
            ("modifierAltGraph", Self::ALT_GRAPH),
            ("modifierFn", Self::FN),
            ("modifierCapsLock", Self::CAPS_LOCK),
            ("modifierScrollLock", Self::SCROLL_LOCK),
            ("modifierNumLock", Self::NUM_LOCK),
            ("modifierSymbol", Self::SYMBOL),
        ] {
            if self.has(bit) {
                set_bool(scope, init, name, true);
            }
        }
    }
}

/// `IsDomKeyForModifier` (`ui/events/keycodes/keyboard_code_conversion.cc`): the keys whose `keydown`
/// does NOT count as a user activation.
fn is_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "Accel" | "Alt" | "AltGraph" | "CapsLock" | "Control" | "Fn" | "FnLock" | "Hyper" | "Meta" | "NumLock"
            | "ScrollLock" | "Shift" | "Super" | "Symbol" | "SymbolLock"
    )
}

/// ONE key event as Blink's `WebKeyboardEvent` carries it into the renderer: everything a page can
/// read off the `KeyboardEvent` it becomes, and the `text` its default action types.
///
/// CDP's `Input.dispatchKeyEvent` reached the engine as `(type, key, code, text)` -- its
/// `modifiers`, `windowsVirtualKeyCode`, `autoRepeat`, `isKeypad` and `location` were dropped, and the
/// engine re-derived a key code from the key NAME, which made Shift's `keyCode` 83 (the `'S'` of
/// "Shift") where Chrome reports 16. This is the whole record now, filled verbatim by the CDP arm
/// ([`KeyInput::from_cdp`]) or by the platform keyboard ([`platform_key_input`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyInput {
    /// `KeyboardEvent.key`.
    pub key: String,
    /// `KeyboardEvent.code`.
    pub code: String,
    /// What the key types -- `WebKeyboardEvent::text`, at most 3 UTF-16 units. Empty for a key that
    /// types nothing, and then a `keyDown` has no `keypress` (`keyboard_event_manager.cc:395`).
    pub text: String,
    /// `windows_key_code`: the `keyCode` / `which` of `keydown` and `keyup`.
    pub key_code: u32,
    pub mods: KeyMods,
    /// `KeyboardEvent.location`: 0 standard, 1 left, 2 right, 3 numpad.
    pub location: u32,
    /// `KeyboardEvent.repeat` (`kIsAutoRepeat`).
    pub repeat: bool,
}

impl KeyInput {
    /// CDP `Input.dispatchKeyEvent`'s parameters, as `InputHandler::DispatchKeyEvent`
    /// (`input_handler.cc:1181-1270`) turns them into a `NativeWebKeyboardEvent`. Every absent field is
    /// Chrome's default, NOT a value derived from another field: no `windowsVirtualKeyCode` is a
    /// `keyCode` of 0, and no `text` is a key that types nothing -- which is why a client that wants a
    /// keystroke to type sends both (Puppeteer and Playwright do).
    ///
    /// `location` maps through the modifier bits it sets (`kIsLeft` for bit 1, `kIsRight` for bit 2,
    /// `kIsKeyPad` for `isKeypad`) and back out through `GetKeyLocationCode` (`keyboard_event.cc:61-69`),
    /// whose order -- keypad, left, right -- is this one.
    #[allow(clippy::too_many_arguments)]
    pub fn from_cdp(
        key: &str,
        code: &str,
        text: &str,
        windows_virtual_key_code: Option<i64>,
        modifiers: Option<i64>,
        auto_repeat: bool,
        is_keypad: bool,
        location: Option<i64>,
    ) -> KeyInput {
        let loc = location.unwrap_or(0);
        let location = if is_keypad {
            3
        } else if loc & 1 != 0 {
            1
        } else if loc & 2 != 0 {
            2
        } else {
            0
        };
        KeyInput {
            key: key.to_string(),
            code: code.to_string(),
            text: text.to_string(),
            key_code: windows_virtual_key_code.unwrap_or(0).clamp(0, u32::MAX as i64) as u32,
            mods: KeyMods::from_cdp(modifiers.unwrap_or(0)),
            location,
            repeat: auto_repeat,
        }
    }

    /// The first UTF-16 unit of `text`: the `keypress`'s `charCode` AND `keyCode`
    /// (`keyboard_event.cc:124-133`: `char_code_ = key.text[0]`, and a `keypress`'s key code is its
    /// char code). 0 for no text.
    fn char_code(&self) -> u32 {
        self.text.encode_utf16().next().unwrap_or(0) as u32
    }
}

/// The keyboard's own state: which modifiers the PLATFORM keyboard holds and which locks it has on,
/// and the control a held Space has made active. OpState-owned, created on first use.
///
/// Only the platform path ([`platform_key_input`]: the behaviour model's stream and
/// `Engine::dispatch_key`) moves `held`. A CDP key event does not: Chrome's DevTools input is "from
/// debugger" (`kFromDebugger`), carries its own `modifiers`, and changes no OS keyboard state -- a CDP
/// `keyDown` of Shift leaves the next CDP event's `shiftKey` to that event's own `modifiers`.
#[derive(Default)]
pub struct KeyboardState {
    held: std::cell::Cell<u32>,
    space_active: std::cell::Cell<Option<NodeId>>,
}

fn keyboard<R>(scope: &mut v8::PinScope, f: impl FnOnce(&KeyboardState) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if op_state.borrow().try_borrow::<KeyboardState>().is_none() {
        op_state.borrow_mut().put(KeyboardState::default());
    }
    let st = op_state.borrow();
    f(st.borrow::<KeyboardState>())
}

/// The modifiers the platform keyboard holds RIGHT NOW -- what an input event the platform makes (a
/// mouse press while Shift is down) carries. The pointer half reads this.
pub fn held_modifiers(scope: &mut v8::PinScope) -> KeyMods {
    keyboard(scope, |k| KeyMods(k.held.get()))
}

/// One key event from the PLATFORM keyboard -- a person's key, which is what the behaviour model's
/// stream and `Engine::dispatch_key` stand for -- as the `WebKeyboardEvent` Chrome on Linux builds from
/// it. Measured with real X11 keys on stock Chrome 150 (`tests/fixtures/key-events/`):
///
/// * the modifier state is the keyboard's, and a modifier key's own event already includes its
///   change: Shift's `keydown` reads `shiftKey: true`, its `keyup` `false`;
/// * `keyCode` is the Windows virtual key ([`vk_code`]: Shift 16, Control 17, `'@'` on Digit2 50), and
///   `location` is 1 / 2 for the left / right modifier keys, 3 on the numpad;
/// * with Control held a key types nothing -- Ctrl+A fires `keydown` and `keyup` and no `keypress`;
/// * Enter types `"\r"`, which is what makes its `keypress` (charCode 13).
pub fn platform_key_input(scope: &mut v8::PinScope, stroke: Stroke, key: &str, code: &str, text: &str) -> KeyInput {
    let held = KeyMods::held_by(key);
    let lock = KeyMods::lock_for(key);
    let mods = keyboard(scope, |k| {
        let mut m = k.held.get();
        match stroke {
            Stroke::Down | Stroke::RawDown => {
                m |= held;
                // A lock key flips its lock on the press. UNMEASURED (the recording holds no lock key).
                m ^= lock;
            }
            Stroke::Up => m &= !held,
            Stroke::Char => {}
        }
        k.held.set(m);
        KeyMods(m)
    });
    let text = if stroke == Stroke::Up || mods.has(KeyMods::CTRL) {
        String::new()
    } else if !text.is_empty() {
        text.to_string()
    } else if key == "Enter" {
        "\r".to_string()
    } else if key.chars().count() == 1 {
        key.to_string()
    } else {
        String::new()
    };
    KeyInput {
        key: key.to_string(),
        code: code.to_string(),
        text,
        key_code: vk_code(key, code) as u32,
        mods,
        location: key_location(code),
        repeat: false,
    }
}

/// `KeyboardEvent.location` from the physical key: the left and right halves of the paired modifier
/// keys, and the numeric keypad.
fn key_location(code: &str) -> u32 {
    match code {
        "ShiftLeft" | "ControlLeft" | "AltLeft" | "MetaLeft" => 1,
        "ShiftRight" | "ControlRight" | "AltRight" | "MetaRight" => 2,
        c if c.starts_with("Numpad") => 3,
        _ => 0,
    }
}

/// The focused element's role in the keyboard's default actions -- the element kinds Blink gives a key
/// default action of their own.
#[derive(Clone, Copy, PartialEq, Debug)]
enum KeyTarget {
    /// `<input>` whose type is a text field (`IsTextField`): editing, then implicit submission.
    TextField,
    /// `<textarea>`.
    TextArea,
    /// `<input>` of a button type (`KeyboardClickableInputTypeView`): Enter / Space click it.
    InputButton,
    /// Any other `<input>`: Enter submits its form implicitly (`InputTypeView::ShouldSubmitImplicitly`).
    OtherInput,
    /// `<button>` (`HTMLElement::HandleKeyboardActivation`).
    Button,
    /// `<a href>` / `<area href>`: Enter's `keydown` follows it.
    Link,
    Other,
}

fn key_target_kind(dom: &korvus_dom::Dom, id: NodeId) -> KeyTarget {
    let Some(tag) = dom.tag_name(id) else { return KeyTarget::Other };
    match tag.to_ascii_lowercase().as_str() {
        "input" => {
            let ty = dom.get_attribute(id, "type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
            if text_entry_type(&ty) {
                KeyTarget::TextField
            } else if matches!(ty.as_str(), "submit" | "reset" | "button" | "image" | "file" | "color") {
                KeyTarget::InputButton
            } else {
                KeyTarget::OtherInput
            }
        }
        "textarea" => KeyTarget::TextArea,
        "button" => KeyTarget::Button,
        "a" | "area" if dom.get_attribute(id, "href").is_some() => KeyTarget::Link,
        _ => KeyTarget::Other,
    }
}

/// Build a trusted `KeyboardEvent` of `kind` for `input`, with the legacy numbers `kind` carries, and
/// dispatch it at `el`. Answers "not cancelled"; `None` when the event could not be built.
fn fire_key(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, kind: &str, input: &KeyInput, timestamp_ms: f64) -> Option<bool> {
    // `keydown` / `keyup` carry the virtual key with charCode 0; `keypress` carries the character in
    // both (`keyboard_event.cc:124-133`).
    let (key_code, char_code) = if kind == "keypress" {
        let c = input.char_code();
        (c, c)
    } else {
        (input.key_code, 0)
    };
    let ev = build_trusted_key_full(scope, kind, input, key_code as f64, char_code as f64, timestamp_ms)?;
    let evv: v8::Local<v8::Value> = ev.into();
    Some(
        crate::binding::protos::engine_call(scope, el.into(), "dispatchEvent", &[evv])
            .map(|v| v.boolean_value(scope))
            .unwrap_or(false),
    )
}

/// The editing command a `keydown` runs, by `editing_behavior.cc:94-191`'s
/// `kKeyboardCodeKeyDownEntries` (virtual key + the four editing modifiers). Only the rows this
/// engine's text-control model can run; a key with no row here runs nothing, which for the movement
/// rows (arrows, Home, End) changes no value and fires no event in Chrome either -- they move a caret
/// this engine does not draw. Ctrl+Backspace / Ctrl+Delete (`deleteWord*`) are NOT here: they need
/// Blink's word boundaries.
#[derive(Clone, Copy, PartialEq, Debug)]
enum KeyDownCommand {
    DeleteBackward,
    DeleteForward,
    SelectAll,
    /// `MoveLeft` / `MoveRight` / `MoveToBeginningOfLine` / `MoveToEndOfLine`, and with Shift their
    /// `...AndModifySelection` twins: the caret (or the selection's focus) moves, no value changes and no event fires.
    Move(Caret, bool),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Caret {
    Left,
    Right,
    Home,
    End,
}

fn keydown_command(key_code: u32, mods: KeyMods) -> Option<KeyDownCommand> {
    const VKEY_BACK: u32 = 0x08;
    const VKEY_DELETE: u32 = 0x2E;
    const VKEY_A: u32 = 0x41;
    const VKEY_END: u32 = 0x23;
    const VKEY_HOME: u32 = 0x24;
    const VKEY_LEFT: u32 = 0x25;
    const VKEY_RIGHT: u32 = 0x27;
    let caret = match key_code {
        VKEY_LEFT => Some(Caret::Left),
        VKEY_RIGHT => Some(Caret::Right),
        VKEY_HOME => Some(Caret::Home),
        VKEY_END => Some(Caret::End),
        _ => None,
    };
    match (key_code, mods.editing()) {
        (VKEY_BACK, 0) | (VKEY_BACK, KeyMods::SHIFT) => Some(KeyDownCommand::DeleteBackward),
        (VKEY_DELETE, 0) => Some(KeyDownCommand::DeleteForward),
        (VKEY_A, KeyMods::CTRL) => Some(KeyDownCommand::SelectAll),
        (_, 0) => caret.map(|c| KeyDownCommand::Move(c, false)),
        (_, KeyMods::SHIFT) => caret.map(|c| KeyDownCommand::Move(c, true)),
        _ => None,
    }
}

/// Move a text control's caret, or with `extend` its selection's focus (the anchor stays), as Blink's editor commands
/// do in a single line of text: by one code point left or right -- a collapsing move from a selection lands on its
/// start or end -- or to the value's start or end. Offsets are UTF-16 code units, as `selectionStart` indexes them.
fn move_caret(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, to: Caret, extend: bool) {
    if !get_prop(scope, el, "selectionStart").map(|v| v.is_number()).unwrap_or(false) {
        return;
    }
    let units: Vec<u16> = get_prop(scope, el, "value").filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope).encode_utf16().collect()).unwrap_or_default();
    let len = units.len();
    let read = |scope: &mut v8::PinScope, k: &str| get_prop(scope, el, k).and_then(|v| v.number_value(scope)).map(|n| (n.max(0.0) as usize).min(len)).unwrap_or(len);
    let (start, end) = (read(scope, "selectionStart"), read(scope, "selectionEnd"));
    let backward = get_prop(scope, el, "selectionDirection").map(|v| v.to_rust_string_lossy(scope)).as_deref() == Some("backward");
    let (anchor, focus) = if backward { (end, start) } else { (start, end) };
    let left = |i: usize| if i >= 2 && (0xDC00..=0xDFFF).contains(&units[i - 1]) && (0xD800..=0xDBFF).contains(&units[i - 2]) { i - 2 } else { i.saturating_sub(1) };
    let right = |i: usize| if i + 1 < len && (0xD800..=0xDBFF).contains(&units[i]) && (0xDC00..=0xDFFF).contains(&units[i + 1]) { i + 2 } else { (i + 1).min(len) };
    let (a, b, dir) = if extend {
        let f = match to {
            Caret::Left => left(focus),
            Caret::Right => right(focus),
            Caret::Home => 0,
            Caret::End => len,
        };
        (anchor.min(f), anchor.max(f), if f < anchor { "backward" } else { "forward" })
    } else {
        let c = match to {
            Caret::Left if start != end => start,
            Caret::Right if start != end => end,
            Caret::Left => left(start),
            Caret::Right => right(end),
            Caret::Home => 0,
            Caret::End => len,
        };
        (c, c, "forward")
    };
    let (av, bv) = (int_val(scope, a as i64), int_val(scope, b as i64));
    let dv = str_val(scope, dir);
    call_method(scope, el.into(), "setSelectionRange", &[av, bv, dv]);
}

/// The editing command a `keypress` runs, by `kKeyboardCodeKeyPressEntries` (`editing_behavior.cc:193-198`):
/// `'\r'` alone is `InsertNewline`, with Shift `InsertLineBreak` -- in a plain-text control both are a
/// line break (`InputTypeFromCommandType`: `CanEditRichly()` is false, so `insertLineBreak`).
fn keypress_is_line_break(char_code: u32, mods: KeyMods) -> bool {
    char_code == u32::from(b'\r') && matches!(mods.editing(), 0 | KeyMods::SHIFT)
}

/// `EditingBehavior::ShouldInsertCharacter` (`editing_behavior.cc:290-336`) on Linux: more than one unit
/// always inserts; one unit inserts unless it is a control character or Control is held.
fn should_insert_character(input: &KeyInput) -> bool {
    let mut units = input.text.encode_utf16();
    let Some(first) = units.next() else { return false };
    if units.next().is_some() {
        return true;
    }
    first >= 0x20 && !input.mods.has(KeyMods::CTRL)
}

/// `DispatchSimulatedClick(underlying_event)` from a key (`event_dispatcher.cc:95-157`,
/// `simulated_event_util.cc:79-190`): ONE `click`, a trusted `PointerEvent` with `pointerId` -1
/// (`kReservedNonPointerId`), an empty `pointerType`, no coordinates, `detail` 0, composed, `view` the
/// window, and the KEY's modifiers. Measured with real keys: Enter in a form's text field fires exactly
/// that on the default button. The click's activation behaviour (a submit button submits) runs in the
/// dispatch itself (`core/html/activation.rs`). A disabled control gets nothing.
pub(crate) fn dispatch_simulated_click(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, mods: KeyMods, timestamp_ms: f64) {
    let sel = str_val(scope, ":disabled");
    if crate::binding::protos::pristine_call(scope, el.into(), "Element.matches", &[sel]).is_some_and(|v| v.boolean_value(scope)) {
        return;
    }
    let Some(ctor) =
        crate::binding::protos::iface_object(scope, "PointerEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return;
    };
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", true);
    set_bool(scope, init, "cancelable", true);
    set_bool(scope, init, "composed", true);
    let win: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    set_prop(scope, init, "view", win);
    let pid = int_val(scope, -1);
    set_prop(scope, init, "pointerId", pid);
    mods.write_init(scope, init);
    let ty = str_val(scope, "click");
    let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) else { return };
    mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", timestamp_ms);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, el.into(), "dispatchEvent", &[evv]);
}

/// `HTMLInputElement::DefaultEventHandler`'s implicit-submission step (`html_input_element.cc:1618-1638`):
/// "form submission finishes editing, just as loss of focus does" -- `change` first if the user's edit
/// changed the value, then `HTMLFormElement::SubmitImplicitly` with the field's
/// `CanTriggerImplicitSubmission()` (a text field's). Measured with real keys (Enter in a form's text
/// field): `change`, the default button's `click`, then `submit` with that button as the submitter.
fn submit_implicitly(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, id: NodeId, text_field: bool, mods: KeyMods, timestamp_ms: f64) -> usize {
    let n = change_on_leaving(scope, id, timestamp_ms);
    n + crate::core::html::html_form_element::submit_implicitly(scope, el, text_field, mods, timestamp_ms)
}

/// One keystroke at the focused element, the way `KeyboardEventManager::KeyEvent` and the default event
/// handlers behind it run one. Returns the number of events dispatched -- `0` only when the document has
/// no body to target.
///
/// ```text
/// Down / RawDown   user activation (not for a modifier key or Escape), then `keydown`;
///                  not cancelled -> its default action: a keydown editing command (Backspace,
///                  Delete, Ctrl+A), Enter on a link, Space arming a button
///   Down only      not cancelled, not handled, and the key types something -> `keypress`
/// Char             `keypress`
///   keypress       not cancelled -> its default action: a line break / the typed text into a text
///                  control, Enter's implicit submission, Enter on a button
/// Up               `keyup`; Space releasing the armed button clicks it
/// ```
///
/// A `keydown` whose default action RAN suppresses the `keypress` exactly as a cancelled one does
/// (`keyboard_event_manager.cc:359-362`: `DispatchEvent` answers `kCanceledByDefaultEventHandler`) --
/// which is why Backspace and Ctrl+A fire no `keypress`.
pub fn key_at_focus(scope: &mut v8::PinScope, stroke: Stroke, input: &KeyInput, timestamp_ms: f64) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (id, ed, kt) = {
        let st = op_state.borrow();
        let Some(id) = focus::key_target(&st) else { return 0 };
        let Some(h) = st.try_borrow::<HostDom>() else { return 0 };
        (id, editable(&h.0, id), key_target_kind(&h.0, id))
    };
    let el = wrap_cached(scope, &op_state, id);
    let mut n = 0;
    if stroke == Stroke::Up {
        if fire_key(scope, el, "keyup", input, timestamp_ms).is_none() {
            return 0;
        }
        n += 1;
        // `HandleKeyboardActivation` / `KeyboardClickableInputTypeView::HandleKeyupEvent`: Space's
        // release clicks the control its press made active.
        if input.key == " " {
            let armed = keyboard(scope, |k| k.space_active.take());
            if armed == Some(id) && matches!(kt, KeyTarget::Button | KeyTarget::InputButton) {
                dispatch_simulated_click(scope, el, input.mods, timestamp_ms);
                n += 1;
            }
        }
        return n;
    }
    if stroke != Stroke::Char {
        // `keyboard_event_manager.cc:249-259`: the activation is the INPUT's, set before the page sees
        // the event, so a `keydown` listener already reads `navigator.userActivation.isActive`.
        if !is_modifier_key(&input.key) && input.key != "Escape" {
            crate::core::events::user_activation::notify_user_activation(scope);
        }
        let Some(alive) = fire_key(scope, el, "keydown", input, timestamp_ms) else { return 0 };
        n += 1;
        // A cancelled keydown suppresses keypress and the editing command; the keyup that follows is a
        // separate dispatch and still fires. Measured.
        if !alive {
            return n;
        }
        let (handled, k) = keydown_default(scope, el, id, ed, kt, input, timestamp_ms);
        n += k;
        if handled || stroke == Stroke::RawDown || input.text.is_empty() {
            return n;
        }
    }
    // keypress -- `keyCode` and `charCode` are BOTH the character here, where keydown's `keyCode` was
    // the virtual key.
    let Some(live) = fire_key(scope, el, "keypress", input, timestamp_ms) else { return n };
    n += 1;
    if !live {
        return n;
    }
    n + keypress_default(scope, el, id, ed, kt, input, timestamp_ms)
}

/// The `keydown`'s default action. Answers (handled, events dispatched).
#[allow(clippy::too_many_arguments)]
fn keydown_default(
    scope: &mut v8::PinScope,
    el: v8::Local<v8::Object>,
    id: NodeId,
    ed: Editable,
    kt: KeyTarget,
    input: &KeyInput,
    timestamp_ms: f64,
) -> (bool, usize) {
    // `Editor::HandleEditingKeyboardEvent` for a `kRawKeyDown`: a non-insertion command runs here.
    if matches!(kt, KeyTarget::TextField | KeyTarget::TextArea) {
        match keydown_command(input.key_code, input.mods) {
            Some(KeyDownCommand::SelectAll) => {
                select_all(scope, el);
                return (true, 0);
            }
            Some(KeyDownCommand::DeleteBackward) if ed != Editable::No => {
                return (true, run_edit(scope, el, ed, &Edit::DeleteBackward, timestamp_ms));
            }
            Some(KeyDownCommand::DeleteForward) if ed != Editable::No => {
                return (true, run_edit(scope, el, ed, &Edit::DeleteForward, timestamp_ms));
            }
            Some(KeyDownCommand::Move(to, extend)) => {
                move_caret(scope, el, to, extend);
                return (true, 0);
            }
            _ => {}
        }
    }
    match kt {
        // `IsEnterKeyKeydownEvent` (`html_anchor_element.cc:193-197, 597-602`): Enter's keydown on a
        // focused link clicks it, not on a repeat.
        KeyTarget::Link if input.key == "Enter" && !input.repeat => {
            dispatch_simulated_click(scope, el, input.mods, timestamp_ms);
            (true, 1)
        }
        // Space's press makes the control active; the click is its release's (not default-handled:
        // "IE dispatches a keypress in this case").
        KeyTarget::Button | KeyTarget::InputButton if input.key == " " => {
            keyboard(scope, |k| k.space_active.set(Some(id)));
            (false, 0)
        }
        _ => (false, 0),
    }
}

/// The `keypress`'s default action. Answers the events dispatched.
#[allow(clippy::too_many_arguments)]
fn keypress_default(
    scope: &mut v8::PinScope,
    el: v8::Local<v8::Object>,
    id: NodeId,
    ed: Editable,
    kt: KeyTarget,
    input: &KeyInput,
    timestamp_ms: f64,
) -> usize {
    let cc = input.char_code();
    let enter = cc == u32::from(b'\r');
    match kt {
        KeyTarget::TextField | KeyTarget::TextArea => {
            // The editor first (`call_base_class_early` for a text field's keypress). Its line-break
            // command is enabled only in editable text: a readonly field skips it.
            if keypress_is_line_break(cc, input.mods) && ed == Editable::Text {
                // `EditorCommand::Execute` announces the command with `beforeinput` before running it;
                // a cancelled announcement runs nothing.
                if !fire_input_event(scope, el, "beforeinput", &Edit::LineBreak, true, timestamp_ms) {
                    return 1;
                }
                if kt == KeyTarget::TextArea {
                    return 1 + commit_edit(scope, el, &Edit::LineBreak, timestamp_ms);
                }
                // `ExecuteInsertNewline` / `ExecuteInsertLineBreak` hand the line break to the field as a
                // `textInput` of "\n", which a single-line field turns into implicit submission
                // (`TextFieldInputType::ShouldSubmitImplicitly`) and stops before any listener sees it
                // (`LegacyPreActivationBehavior`) -- the recording shows no `textInput` and no `input`.
                return 1 + submit_implicitly(scope, el, id, true, input.mods, timestamp_ms);
            }
            if !enter && should_insert_character(input) {
                return run_edit(scope, el, ed, &Edit::Insert(input.text.clone()), timestamp_ms);
            }
            // The editor declined (a readonly field, Ctrl+Enter): the keypress reaches the input's own
            // handler, and a `'\r'` submits (`InputTypeView::ShouldSubmitImplicitly`).
            if kt == KeyTarget::TextField && enter {
                return submit_implicitly(scope, el, id, true, input.mods, timestamp_ms);
            }
            0
        }
        // `KeyboardClickableInputTypeView::HandleKeypressEvent`: Enter clicks it.
        KeyTarget::InputButton if input.key == "Enter" => {
            dispatch_simulated_click(scope, el, input.mods, timestamp_ms);
            1
        }
        KeyTarget::OtherInput if enter => submit_implicitly(scope, el, id, false, input.mods, timestamp_ms),
        // `HTMLElement::HandleKeyboardActivation`: a `'\r'` keypress clicks the button.
        KeyTarget::Button if enter => {
            dispatch_simulated_click(scope, el, input.mods, timestamp_ms);
            1
        }
        _ => 0,
    }
}

/// `SelectAll` in a text control: the whole value, as a forward selection. No event of its own
/// (measured: Ctrl+A fires `keydown` and `keyup` and the selection is 0..length at the `keyup`).
fn select_all(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) {
    if !get_prop(scope, el, "selectionStart").map(|v| v.is_number()).unwrap_or(false) {
        return;
    }
    let len = get_prop(scope, el, "value")
        .filter(|v| v.is_string())
        .map(|v| v.to_rust_string_lossy(scope).encode_utf16().count())
        .unwrap_or(0);
    let a = int_val(scope, 0);
    let b = int_val(scope, len as i64);
    call_method(scope, el.into(), "setSelectionRange", &[a, b]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cdp(key: &str, code: &str, text: &str, vk: i64, mods: i64) -> KeyInput {
        KeyInput::from_cdp(key, code, text, Some(vk), Some(mods), false, false, None)
    }

    #[test]
    fn vk_codes_are_the_virtual_key() {
        // Measured off Chrome 150: keydown keyCode per key (CDP P3b and real X11 keys).
        assert_eq!(vk_code("a", "KeyA"), 65.0);
        assert_eq!(vk_code("A", "KeyA"), 65.0);
        assert_eq!(vk_code("1", "Digit1"), 49.0);
        assert_eq!(vk_code("@", "Digit2"), 50.0);
        assert_eq!(vk_code(" ", "Space"), 32.0);
        assert_eq!(vk_code("-", "Minus"), 189.0);
        assert_eq!(vk_code("Enter", "Enter"), 13.0);
        assert_eq!(vk_code("Backspace", "Backspace"), 8.0);
        assert_eq!(vk_code("ArrowLeft", "ArrowLeft"), 37.0);
        assert_eq!(vk_code("Shift", "ShiftLeft"), 16.0);
        assert_eq!(vk_code("Control", "ControlLeft"), 17.0);
        // A key NAME is never read by its first letter (Shift was 83, the 'S').
        assert_eq!(vk_code("Shift", ""), 16.0);
        assert_eq!(vk_code("Unidentified", ""), 0.0);
        // The code column is preferred, but a source that supplies only `key` still answers.
        assert_eq!(vk_code("-", ""), 189.0);
        assert_eq!(vk_code("z", ""), 90.0);
        assert_eq!(vk_code("@", ""), 50.0);
        assert_eq!(vk_code("F5", "F5"), 116.0);
        assert_eq!(vk_code("3", "Numpad3"), 99.0);
    }

    #[test]
    fn a_keypress_carries_the_first_unit_of_the_text() {
        assert_eq!(cdp("a", "KeyA", "a", 65, 0).char_code(), 97);
        assert_eq!(cdp("-", "Minus", "-", 189, 0).char_code(), 45);
        assert_eq!(cdp("Enter", "Enter", "\r", 13, 0).char_code(), 13);
        assert_eq!(cdp("Backspace", "Backspace", "", 8, 0).char_code(), 0);
    }

    #[test]
    fn cdp_parameters_are_taken_verbatim() {
        // input_handler.cc GetEventModifiers: Alt=1, Ctrl=2, Meta=4, Shift=8.
        let k = cdp("@", "Digit2", "@", 50, 8);
        assert!(k.mods.has(KeyMods::SHIFT) && !k.mods.has(KeyMods::CTRL));
        assert_eq!(KeyMods::from_cdp(1 | 2 | 4 | 8).0, KeyMods::SHIFT | KeyMods::CTRL | KeyMods::ALT | KeyMods::META);
        // An absent windowsVirtualKeyCode is 0, not a code derived from the key.
        assert_eq!(KeyInput::from_cdp("a", "KeyA", "a", None, None, false, false, None).key_code, 0);
        // location: keypad wins, then left, then right (GetKeyLocationCode's order).
        assert_eq!(KeyInput::from_cdp("Shift", "ShiftLeft", "", Some(16), Some(8), false, false, Some(1)).location, 1);
        assert_eq!(KeyInput::from_cdp("Shift", "ShiftRight", "", Some(16), Some(8), false, false, Some(2)).location, 2);
        assert_eq!(KeyInput::from_cdp("1", "Numpad1", "1", Some(97), None, false, true, Some(1)).location, 3);
        assert!(KeyInput::from_cdp("a", "KeyA", "a", Some(65), None, true, false, None).repeat);
    }

    #[test]
    fn the_editing_commands_are_blinks_key_bindings() {
        let none = KeyMods::default();
        assert_eq!(keydown_command(8, none), Some(KeyDownCommand::DeleteBackward));
        assert_eq!(keydown_command(8, KeyMods(KeyMods::SHIFT)), Some(KeyDownCommand::DeleteBackward));
        assert_eq!(keydown_command(8, KeyMods(KeyMods::CTRL)), None); // DeleteWordBackward: not modelled
        assert_eq!(keydown_command(46, none), Some(KeyDownCommand::DeleteForward));
        assert_eq!(keydown_command(65, KeyMods(KeyMods::CTRL)), Some(KeyDownCommand::SelectAll));
        assert_eq!(keydown_command(65, none), None);
        // A lock state is not an editing modifier.
        assert_eq!(keydown_command(65, KeyMods(KeyMods::CTRL | KeyMods::NUM_LOCK)), Some(KeyDownCommand::SelectAll));
        assert!(keypress_is_line_break(13, none));
        assert!(keypress_is_line_break(13, KeyMods(KeyMods::SHIFT)));
        assert!(!keypress_is_line_break(13, KeyMods(KeyMods::CTRL)));
        assert!(should_insert_character(&cdp("a", "KeyA", "a", 65, 0)));
        assert!(!should_insert_character(&cdp("a", "KeyA", "a", 65, 2)));
        assert!(!should_insert_character(&cdp("Enter", "Enter", "\r", 13, 0)));
        assert!(should_insert_character(&cdp("", "", "ab", 0, 2)));
    }

    #[test]
    fn modifier_keys_are_not_activation() {
        for k in ["Shift", "Control", "Alt", "Meta", "CapsLock", "AltGraph"] {
            assert!(is_modifier_key(k), "{k}");
        }
        for k in ["a", "Enter", "Escape", " "] {
            assert!(!is_modifier_key(k), "{k}");
        }
    }

    #[test]
    fn the_input_event_fields_are_the_ones_chrome_reports() {
        assert_eq!(Edit::Insert("ab".into()).input_type(), "insertText");
        assert_eq!(Edit::Insert("ab".into()).data(), Some("ab"));
        assert_eq!(Edit::DeleteBackward.input_type(), "deleteContentBackward");
        assert_eq!(Edit::DeleteBackward.data(), None);
        assert_eq!(Edit::DeleteForward.input_type(), "deleteContentForward");
        assert_eq!(Edit::LineBreak.input_type(), "insertLineBreak");
        assert_eq!(Edit::LineBreak.data(), None);
    }
}

