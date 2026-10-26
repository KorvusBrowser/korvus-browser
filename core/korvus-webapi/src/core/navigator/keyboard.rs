//! `core/navigator/keyboard.rs` -- `navigator.keyboard`: the Keyboard Map and Keyboard Lock APIs,
//! `Keyboard` and `KeyboardLayoutMap` as `interface!` tables.
//!
//! WHY. The row was a recording constant, `keyboard = null`, which is BRAVE 151's answer and nobody
//! else's: Chrome 150, Edge 151 and Opera 135 hand back a `Keyboard` whose `getLayoutMap()` resolves a
//! 48-entry `KeyboardLayoutMap` on this US-layout desktop (measured 2026-10-08, stock browsers over a
//! loopback page). a bot-challenge frame reads `navigator.keyboard` and walks the map with
//! `forEach`; under the Chrome persona this engine answered Brave's `null`.
//!
//! THE MAP IS THE PERSONA'S. The entries describe the device's keyboard layout and their ORDER is the
//! one Blink's `HashMap` iterates (`KK, KG, Digit2, ...`), which only a measurement can supply -- so
//! both come from the bundle ([`spec::NavigatorProfile::keyboard_layout`]), and a persona without one
//! (`None`) answers `navigator.keyboard === null`, as Brave does.
//!
//! THE ALGORITHMS are Blink's `keyboard_layout.cc` / `keyboard_lock.cc` and the browser half in
//! `content/browser/keyboard_lock/keyboard_lock_service_impl.cc`:
//!
//! * `getLayoutMap()` settles on a later task (the browser's reply); calls made before it settles share
//!   ONE promise (`ScriptPromiseProperty`), and every settlement mints a new map. A subframe without the
//!   `keyboard-map` permission policy (default `'self'`) is refused: `SecurityError`, "getLayoutMap()
//!   must be called from a top-level browsing context or allowed by the permission policy."
//! * `lock(keyCodes = [])` from a subframe rejects `InvalidStateError` ("Failed to execute 'lock' on
//!   'Keyboard': lock() must be called from a primary top-level browsing context.", the renderer's own
//!   throw); a code `ui::KeycodeConverter` does not know rejects
//!   `InvalidAccessError` ("No valid key codes passed into lock()."); otherwise it resolves on a later
//!   task. A newer `lock()` before an older one settles rejects the older with `AbortError`.
//! * `unlock()` does nothing observable.
//!
//! Every exception of a promise-returning operation is a REJECTION, per WebIDL -- including the
//! receiver check's `TypeError: Illegal invocation`.

use deno_core::v8;

use crate::binding::protos::{brand_shell_instance, build_fn_data, call_intrinsic, make_dom_exc, mint_on, rejected_promise, shell_proto, str_val, table_receiver_ok, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::core::dom::shared::{row_getter, row_method};
use crate::core::dom::webidl_iter::{make_iter, pair, IterKind};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::kits::define_value;
use crate::install::MemberDef;

/// A `KeyboardLayoutMap`'s codes and keys, index-aligned in iteration order. Its presence IS the brand.
const KLM_KEYS: &str = "korvus_klm_keys";
const KLM_VALS: &str = "korvus_klm_vals";
/// The realm's pending `getLayoutMap()` promise (`[resolver]`), shared by every call until it settles.
const PENDING_MAP: &str = "kbPendingMap";
/// The id of the realm's newest `lock()` request; an older one settling sees it superseded.
const LOCK_SEQ: &str = "kbLockSeq";

/// Every code string `ui::KeycodeConverter::CodeStringToDomCode` recognises: the non-NULL `code`
/// column of Chromium 150's `ui/events/keycodes/dom/dom_code_data.inc`, in its order.
const DOM_CODES: &[&str] = &[
    "Hyper", "Super", "FnLock", "Suspend", "Resume", "Turbo", "Sleep", "WakeUp", "Fn", "DisplayToggleIntExt",
    "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM",
    "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ",
    "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9", "Digit0",
    "Enter", "Escape", "Backspace", "Tab", "Space", "Minus", "Equal", "BracketLeft", "BracketRight",
    "Backslash", "Semicolon", "Quote", "Backquote", "Comma", "Period", "Slash", "CapsLock", "F1", "F2", "F3",
    "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "PrintScreen", "ScrollLock", "Pause", "Insert",
    "Home", "PageUp", "Delete", "End", "PageDown", "ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp",
    "NumLock", "NumpadDivide", "NumpadMultiply", "NumpadSubtract", "NumpadAdd", "NumpadEnter", "Numpad1",
    "Numpad2", "Numpad3", "Numpad4", "Numpad5", "Numpad6", "Numpad7", "Numpad8", "Numpad9", "Numpad0",
    "NumpadDecimal", "IntlBackslash", "ContextMenu", "Power", "NumpadEqual", "F13", "F14", "F15", "F16",
    "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "Open", "Help", "Select", "Again", "Undo", "Cut",
    "Copy", "Paste", "Find", "AudioVolumeMute", "AudioVolumeUp", "AudioVolumeDown", "NumpadComma", "IntlRo",
    "KanaMode", "IntlYen", "Convert", "NonConvert", "Lang1", "Lang2", "Lang3", "Lang4", "Lang5", "Abort",
    "Props", "NumpadParenLeft", "NumpadParenRight", "NumpadBackspace", "NumpadMemoryStore",
    "NumpadMemoryRecall", "NumpadMemoryClear", "NumpadMemoryAdd", "NumpadMemorySubtract", "NumpadClear",
    "NumpadClearEntry", "ControlLeft", "ShiftLeft", "AltLeft", "MetaLeft", "ControlRight", "ShiftRight",
    "AltRight", "MetaRight", "BrightnessUp", "BrightnessDown", "MediaPlay", "MediaPause", "MediaRecord",
    "MediaFastForward", "MediaRewind", "MediaTrackNext", "MediaTrackPrevious", "MediaStop", "Eject",
    "MediaPlayPause", "MediaSelect", "LaunchMail", "LaunchApp2", "LaunchApp1", "LaunchControlPanel",
    "SelectTask", "LaunchScreenSaver", "LaunchAssistant", "BrowserSearch", "BrowserHome", "BrowserBack",
    "BrowserForward", "BrowserStop", "BrowserRefresh", "BrowserFavorites", "ZoomToggle", "MailReply",
    "MailForward", "MailSend", "KeyboardLayoutSelect", "ShowAllWindows",
];

/// `navigator.keyboard`: this realm's `Keyboard`, or `null` when the persona's browser answers `null`.
pub(super) fn build_keyboard<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let has_map = super::navigator::persona(scope).is_some_and(|p| p.navigator().keyboard_layout.is_some());
    let proto = if has_map { shell_proto(scope, "Keyboard") } else { None };
    let Some(proto) = proto else { return v8::null(scope).into() };
    let o = mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    o.into()
}

/// Is the running realm's document in a subframe -- an in-process frame or a remote one?
fn in_subframe(scope: &mut v8::PinScope) -> bool {
    if let Some(id) = crate::core::html::browsing_context::current_realm_ctx(scope) {
        return crate::core::html::browsing_context::frame_parent(scope, id).is_some();
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::core::html::remote_frame::embedding(&st).is_some()
}

fn receiver_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str) -> bool {
    shell_proto(scope, iface).is_some_and(|p| table_receiver_ok(scope, this.into(), p.into()))
}

/// Blink's context applied after the error is made (`deno_core::error::native_error`).
fn type_error<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str) -> v8::Local<'s, v8::Value> {
    deno_core::error::native_error(scope, "TypeError", msg)
}

fn dom_exc<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str, name: &str) -> v8::Local<'s, v8::Value> {
    make_dom_exc(scope, msg, name).unwrap_or_else(|| v8::undefined(scope).into())
}

/// A resolver this file stored, read back. V8 has no `PromiseResolver::Cast` (a resolver IS its promise
/// object), so the read is unchecked -- sound only because every slot it reads holds nothing else.
fn as_resolver<'s>(v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::PromiseResolver>> {
    // SAFETY: the caller's slot was written with a `v8::PromiseResolver` by this file and by nothing else.
    v.is_promise().then(|| unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(v) })
}

// ── Keyboard ─────────────────────────────────────────────────────────────────────────────────────

/// A new `KeyboardLayoutMap` over the persona's entries.
fn make_layout_map<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let entries = super::navigator::persona(scope)?.navigator().keyboard_layout?;
    let proto = shell_proto(scope, "KeyboardLayoutMap")?;
    let o = mint_on(scope, proto.into());
    brand_shell_instance(scope, o);
    let keys = v8::Array::new(scope, entries.len() as i32);
    let vals = v8::Array::new(scope, entries.len() as i32);
    for (i, (k, v)) in entries.iter().enumerate() {
        let k = str_val(scope, k);
        keys.set_index(scope, i as u32, k);
        let v = str_val(scope, v);
        vals.set_index(scope, i as u32, v);
    }
    priv_set(scope, o, KLM_KEYS, keys.into());
    priv_set(scope, o, KLM_VALS, vals.into());
    Some(o)
}

fn cb_get_layout_map<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !receiver_ok(scope, args.this(), "Keyboard") {
        let e = type_error(scope, "Illegal invocation");
        let p = rejected_promise(scope, e);
        rv.set(p);
        return;
    }
    // Calls before the browser's reply share its promise (`layout_map_property_`).
    let global = scope.get_current_context().global(scope);
    if let Some(r) = evt_slot_read(scope, global, PENDING_MAP).and_then(as_resolver) {
        let p = r.get_promise(scope);
        rv.set(p.into());
        return;
    }
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let r: v8::Local<v8::Object> = resolver.into();
    evt_slot_write(scope, global, PENDING_MAP, r.into());
    let f = build_fn_data(scope, cb_layout_map_reply, "", 0, r.into());
    // after a message posted right after the call, and before a 0 ms timer, as Chrome 150's reply lands
    crate::core::intersection_observer::rendering::queue_reply(scope, f, 1.0);
    let p = resolver.get_promise(scope);
    rv.set(p.into());
}

/// The browser's reply (`KeyboardLayout::GotKeyboardLayoutMap`): the map, or the policy refusal.
fn cb_layout_map_reply(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(resolver) = as_resolver(crate::binding::protos::body_data(scope, &args)) else { return };
    let global = scope.get_current_context().global(scope);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    evt_slot_write(scope, global, PENDING_MAP, undef);
    if in_subframe(scope) && !crate::core::feature_policy::enabled_in_current_realm(scope, "keyboard-map") {
        let e = dom_exc(scope, "getLayoutMap() must be called from a top-level browsing context or allowed by the permission policy.", "SecurityError");
        resolver.reject(scope, e);
        return;
    }
    match make_layout_map(scope) {
        Some(m) => resolver.resolve(scope, m.into()),
        None => {
            let e = dom_exc(scope, "getLayoutMap() request could not be completed.", "InvalidStateError");
            resolver.reject(scope, e)
        }
    };
}

fn cb_lock<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let reject_now = |scope: &mut v8::PinScope<'s, '_>, e: v8::Local<'s, v8::Value>, rv: &mut v8::ReturnValue| {
        let p = rejected_promise(scope, e);
        rv.set(p);
    };
    if !receiver_ok(scope, args.this(), "Keyboard") {
        let e = type_error(scope, "Illegal invocation");
        return reject_now(scope, e, &mut rv);
    }
    // `optional sequence<DOMString> keyCodes = []`.
    let arg = args.get(0);
    let mut codes = Vec::new();
    if !arg.is_undefined() {
        let iterable = v8::Local::<v8::Object>::try_from(arg).ok().is_some_and(|o| {
            let k = v8::Symbol::get_iterator(scope);
            o.get(scope, k.into()).is_some_and(|f| f.is_function())
        });
        if !iterable {
            let e = type_error(scope, "Failed to execute 'lock' on 'Keyboard': The provided value cannot be converted to a sequence.");
            return reject_now(scope, e, &mut rv);
        }
        let list = {
            v8::tc_scope!(let tc, scope);
            match call_intrinsic(tc, "Array.from", &[arg]).and_then(|x| v8::Local::<v8::Array>::try_from(x).ok()) {
                Some(l) => Ok(v8::Global::new(tc, l)),
                None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
            }
        };
        let list = match list {
            Ok(l) => v8::Local::new(scope, &l),
            Err(e) => {
                let e = match e {
                    Some(e) => v8::Local::new(scope, &e),
                    None => v8::undefined(scope).into(),
                };
                return reject_now(scope, e, &mut rv);
            }
        };
        for i in 0..list.length() {
            let Some(el) = list.get_index(scope, i) else { continue };
            let s = {
                v8::tc_scope!(let tc, scope);
                match el.to_string(tc) {
                    Some(s) => Ok(s.to_rust_string_lossy(tc)),
                    None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
                }
            };
            match s {
                Ok(s) => codes.push(s),
                Err(e) => {
                    let e = match e {
                        Some(e) => v8::Local::new(scope, &e),
                        None => v8::undefined(scope).into(),
                    };
                    return reject_now(scope, e, &mut rv);
                }
            }
        }
    }
    if in_subframe(scope) {
        // Thrown in the renderer through its `ExceptionState`, so it carries the operation's prefix --
        // unlike the browser's replies below.
        let e = dom_exc(scope, "Failed to execute 'lock' on 'Keyboard': lock() must be called from a primary top-level browsing context.", "InvalidStateError");
        return reject_now(scope, e, &mut rv);
    }
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let global = scope.get_current_context().global(scope);
    let seq = evt_slot_read(scope, global, LOCK_SEQ).and_then(|v| v.number_value(scope)).unwrap_or(0.0) + 1.0;
    let sv: v8::Local<v8::Value> = v8::Number::new(scope, seq).into();
    evt_slot_write(scope, global, LOCK_SEQ, sv);
    let valid = codes.iter().all(|c| DOM_CODES.contains(&c.as_str()));
    let d = v8::Array::new(scope, 3);
    let r: v8::Local<v8::Object> = resolver.into();
    d.set_index(scope, 0, r.into());
    d.set_index(scope, 1, sv);
    let vv: v8::Local<v8::Value> = v8::Boolean::new(scope, valid).into();
    d.set_index(scope, 2, vv);
    let f = build_fn_data(scope, cb_lock_reply, "", 0, d.into());
    crate::core::intersection_observer::rendering::queue_task(scope, f, 0.0);
    let p = resolver.get_promise(scope);
    rv.set(p.into());
}

/// `KeyboardLock::LockRequestFinished`.
fn cb_lock_reply(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let (Some(r), Some(seq), Some(valid)) = (d.get_index(scope, 0), d.get_index(scope, 1), d.get_index(scope, 2)) else { return };
    let Some(resolver) = as_resolver(r) else { return };
    let global = scope.get_current_context().global(scope);
    let newest = evt_slot_read(scope, global, LOCK_SEQ).and_then(|v| v.number_value(scope));
    if newest != seq.number_value(scope) {
        let e = dom_exc(scope, "This request has been superseded by a subsequent lock() method call.", "AbortError");
        resolver.reject(scope, e);
    } else if !valid.is_true() {
        let e = dom_exc(scope, "No valid key codes passed into lock().", "InvalidAccessError");
        resolver.reject(scope, e);
    } else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        resolver.resolve(scope, u);
    }
}

fn cb_unlock<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !receiver_ok(scope, args.this(), "Keyboard") {
        throw_type_error(scope, "Illegal invocation");
    }
}

pub fn install_keyboard(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getLayoutMap" => row_method(scope, proto, member, cb_get_layout_map),
        "lock" => row_method(scope, proto, member, cb_lock),
        "unlock" => row_method(scope, proto, member, cb_unlock),
        other => debug_assert!(false, "install_keyboard: unexpected member {other}"),
    }
}

// ── KeyboardLayoutMap (readonly maplike<DOMString, DOMString>) ───────────────────────────────────

/// The receiver's keys and values, or a pending `TypeError: Illegal invocation`.
fn klm<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Object>) -> Option<(v8::Local<'s, v8::Array>, v8::Local<'s, v8::Array>)> {
    let k = priv_get(scope, this, KLM_KEYS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    let v = priv_get(scope, this, KLM_VALS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    match (k, v) {
        (Some(k), Some(v)) => Some((k, v)),
        _ => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

fn items<'s>(scope: &mut v8::PinScope<'s, '_>, a: v8::Local<'s, v8::Array>) -> Vec<v8::Local<'s, v8::Value>> {
    (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect()
}

fn index_of<'s>(scope: &mut v8::PinScope<'s, '_>, keys: v8::Local<'s, v8::Array>, key: v8::Local<'s, v8::Value>) -> Option<u32> {
    // The map's keys are strings: SameValueZero against a non-string never matches.
    if !key.is_string() {
        return None;
    }
    (0..keys.length()).find(|&i| keys.get_index(scope, i).is_some_and(|k| k.strict_equals(key)))
}

fn require_one(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, op: &str) -> bool {
    if args.length() < 1 {
        throw_type_error(scope, &format!("Failed to execute '{op}' on 'KeyboardLayoutMap': 1 argument required, but only 0 present."));
        return false;
    }
    true
}

fn cb_klm_size<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((keys, _)) = klm(scope, args.this()) else { return };
    rv.set_uint32(keys.length());
}

fn cb_klm_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((keys, vals)) = klm(scope, args.this()) else { return };
    if !require_one(scope, &args, "get") {
        return;
    }
    if let Some(v) = index_of(scope, keys, args.get(0)).and_then(|i| vals.get_index(scope, i)) {
        rv.set(v);
    }
}

fn cb_klm_has<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((keys, _)) = klm(scope, args.this()) else { return };
    if !require_one(scope, &args, "has") {
        return;
    }
    rv.set_bool(index_of(scope, keys, args.get(0)).is_some());
}

fn klm_iter<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue, kind: u8) {
    let Some((keys, vals)) = klm(scope, args.this()) else { return };
    let keys = items(scope, keys);
    let vals = items(scope, vals);
    let out: Vec<v8::Local<v8::Value>> = match kind {
        0 => keys,
        1 => vals,
        _ => keys.iter().zip(vals.iter()).map(|(k, v)| pair(scope, *k, *v)).collect(),
    };
    if let Some(it) = make_iter(scope, "KeyboardLayoutMap", IterKind::Map, &out) {
        rv.set(it.into());
    }
}

fn cb_klm_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    klm_iter(scope, &args, &mut rv, 0);
}
fn cb_klm_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    klm_iter(scope, &args, &mut rv, 1);
}
fn cb_klm_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    klm_iter(scope, &args, &mut rv, 2);
}

fn cb_klm_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some((keys, vals)) = klm(scope, args.this()) else { return };
    let keys = items(scope, keys);
    let vals = items(scope, vals);
    crate::core::dom::webidl_iter::for_each(scope, &args, "KeyboardLayoutMap", |_, i| Some((*vals.get(i)?, *keys.get(i)?)));
}

pub fn install_keyboard_layout_map(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => row_getter(scope, proto, member, cb_klm_size),
        "entries" => row_method(scope, proto, member, cb_klm_entries),
        "forEach" => row_method(scope, proto, member, cb_klm_for_each),
        "get" => row_method(scope, proto, member, cb_klm_get),
        "has" => row_method(scope, proto, member, cb_klm_has),
        "keys" => row_method(scope, proto, member, cb_klm_keys),
        "values" => row_method(scope, proto, member, cb_klm_values),
        // A maplike's `@@iterator` IS its `entries` (Chrome 150: `m[Symbol.iterator] === m.entries`).
        "@@iterator" => {
            let Some(entries) = crate::binding::protos::get_prop(scope, proto, "entries") else { return };
            define_value(scope, proto, member, entries, true);
        }
        other => debug_assert!(false, "install_keyboard_layout_map: unexpected member {other}"),
    }
}

// Rows from the generated registry's `Keyboard` / `KeyboardLayoutMap` blocks, in the capture's order,
// with their lengths; `[SecureContext]` as both IDLs declare and `secure_context.json` measured.
webapi::interface! {
    "Keyboard" [interface, window, secure] {
        ctor   "Keyboard"                 len 0 = stub,
        method "getLayoutMap" : Promise   len 0 = install_keyboard,
        method "lock"         : Promise   len 0 = install_keyboard,
        method "unlock"       : Undefined len 0 = install_keyboard
    }
}

webapi::interface! {
    "KeyboardLayoutMap" [interface, window, secure] {
        ctor   "KeyboardLayoutMap"       len 0 = stub,
        attr   "size"       : Any readonly     = install_keyboard_layout_map,
        method "entries"    : Any len 0        = install_keyboard_layout_map,
        method "forEach"    : Any len 1        = install_keyboard_layout_map,
        method "get"        : Any len 1        = install_keyboard_layout_map,
        method "has"        : Any len 1        = install_keyboard_layout_map,
        method "keys"       : Any len 0        = install_keyboard_layout_map,
        method "values"     : Any len 0        = install_keyboard_layout_map,
        method "@@iterator" : Any len 0 hidden = install_keyboard_layout_map
    }
}
