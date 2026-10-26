//! `core/events/event.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::binding::protos::{build_fn, build_fn_data, get_prop, global_get, str_val, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::MemberDef;

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// Event methods: state ops over `this` (the JS Event instance). Match the JS class bodies exactly.
// CustomEvent.initCustomEvent(type, bubbles, cancelable, detail): initEvent + set `detail`. The first
// argument is mandatory (WebIDL) -> TypeError when absent; `detail` defaults to null, not undefined.
// Legacy `Event.cancelBubble`: getter reflects the propagation-stopped flag; setting it TRUE stops
// propagation (setting false is a no-op, per the legacy alias of stopPropagation).
//
// WHERE THE BASE STATE LIVES. `type`, `target`, `currentTarget`, `eventPhase`, `bubbles`, `cancelable`,
// `defaultPrevented`, `composed` and `timeStamp` are v8::Private SLOTS on the event object, read back
// through getter-only native accessors on Event.prototype (the `EVENT` table at the end of this file,
// installed by `events::install_event_model`). They used
// to be own data properties written by the JS constructor, which is backwards from every browser: a real
// Event's prototype carries these nine as native accessors and the instance owns nothing but the
// unforgeable `isTrusted`. So NOTHING may `set_prop` them any more -- the writers below, the dispatch
// machinery in event_target.rs, the trusted-input builders in shared.rs and SecurityPolicyViolationEvent's
// native constructor all go through `evt_slot_write` / `evt_flag_set`.

/// DOM "set the canceled flag": only a cancelable event, and never from a PASSIVE listener (the dispatch sets
/// `_passive` around one) -- Chrome 150: `{passive: true}`, or the default-passive scrolling listeners, cannot cancel;
/// korvus let every listener cancel.
fn cb_evt_prevent_default(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if evt_flag_get(scope, this, "cancelable") && !evt_flag_get(scope, this, "_passive") { evt_flag_set(scope, this, "defaultPrevented", true); }
}
fn cb_evt_stop_propagation(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { evt_flag_set(scope, args.this(), "_stop", true); }
fn cb_evt_stop_immediate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this(); evt_flag_set(scope, this, "_stop", true); evt_flag_set(scope, this, "_stopImmediate", true);
}
/// `composedPath()` (FIXER C6, 2026-10-11): the dispatch's path as the CURRENT target's tree may see it --
/// Blink's `TreeScopeEventContext::EnsureEventPath`: every path node whose tree is visible from the current
/// target's (`event_target::tree_visible_from`: no node below a closed shadow root the listener is outside of),
/// then the window. Empty outside a dispatch, as in Chrome. It answered `[currentTarget]`, so a window listener
/// saw a path of one where Chrome 150 reports `target > ... > html > document > window` (a bot sensor's probe: length
/// 1 against 5-7 on every mouse event).
fn cb_evt_composed_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let current = evt_slot_read(scope, this, "currentTarget").filter(|c| !c.is_null_or_undefined());
    let nodes = evt_slot_read(scope, this, "_pathNodes").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    let trees = evt_slot_read(scope, this, "_pathTrees").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
    let (Some(current), Some(nodes), Some(trees)) = (current, nodes, trees) else {
        // A dispatch outside a node path (a flat target: an XHR, a port) still sees its current target.
        match evt_slot_read(scope, this, "currentTarget") {
            Some(c) if !c.is_null_or_undefined() => {
                let arr = v8::Array::new(scope, 1);
                arr.set_index(scope, 0, c);
                rv.set(arr.into());
            }
            _ => {
                let arr = v8::Array::new(scope, 0);
                rv.set(arr.into());
            }
        }
        return;
    };
    let n = nodes.length();
    let mut steps: Vec<(v8::Local<v8::Value>, i64)> = Vec::with_capacity(n as usize);
    for i in 0..n {
        let Some(node) = nodes.get_index(scope, i) else { continue };
        let tree = trees.get_index(scope, i).and_then(|t| t.integer_value(scope)).unwrap_or(-1);
        steps.push((node, tree));
    }
    let Some(listener_tree) = steps.iter().find(|(node, _)| node.strict_equals(current)).map(|(_, t)| *t) else {
        let arr = v8::Array::new(scope, 0);
        rv.set(arr.into());
        return;
    };
    let visible: Vec<bool> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        match st.try_borrow::<crate::binding::host_dom::HostDom>() {
            Some(h) => steps
                .iter()
                .map(|(_, t)| *t < 0 || listener_tree < 0 || crate::core::events::event_target::tree_visible_from(&h.0, *t as u32, listener_tree as u32))
                .collect(),
            None => vec![true; steps.len()],
        }
    };
    let out: Vec<v8::Local<v8::Value>> = steps.iter().zip(visible).filter(|(_, v)| *v).map(|((node, _), _)| *node).collect();
    let arr = v8::Array::new_with_elements(scope, &out);
    rv.set(arr.into());
}
/// DOM's "initialize" (`initEvent` step 1, and Blink's `Event::initEvent` prologue in
/// `core/dom/events/event.cc:125`): **if this's dispatch flag is set, return.** An event being
/// dispatched cannot be re-initialised -- a listener that calls `initEvent`/`initCustomEvent`/
/// `initUIEvent`/`initMouseEvent`/`initKeyboardEvent` on the event it was handed changes nothing.
///
/// Returns `false` when it did nothing, so a caller with more slots to write (`initCustomEvent`'s
/// `detail`, the three legacy positional initialisers) returns too. The flag is the same
/// `korvus_dispatching` private `event_target.rs:480`/`:497` set and clear around the dispatch.
///
/// ONE production, reached from both families: `init_common` below (the legacy three) delegates
/// here rather than writing `type`/`bubbles`/`cancelable` a second time, exactly as Blink's
/// `UIEvent::InitUIEventInternal` calls `initEvent`.
fn evt_init_common(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments) -> bool {
    if evt_flag_get(scope, this, "_dispatching") { return false; }
    let ty = args.get(0).to_rust_string_lossy(scope); let tyv = str_val(scope, &ty); evt_slot_write(scope, this, "type", tyv);
    let b = args.get(1).boolean_value(scope); evt_flag_set(scope, this, "bubbles", b);
    let c = args.get(2).boolean_value(scope); evt_flag_set(scope, this, "cancelable", c);
    // initEvent/initCustomEvent RE-initialize the event: clear the dispatch flags so a reused event
    // starts clean (WPT `initEvent should unset defaultPrevented`).
    evt_flag_set(scope, this, "defaultPrevented", false);
    evt_flag_set(scope, this, "_stop", false);
    evt_flag_set(scope, this, "_stopImmediate", false);
    // The event is now initialised -- `document.createEvent(...)` + `initEvent(...)` is dispatchable.
    evt_flag_set(scope, this, "_uninit", false);
    true
}
fn cb_evt_init_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // `DOMString type` is required (bubbles/cancelable are optional, hence length 1): a bare
    // `initEvent()` is a TypeError, recorded from the browser (behavior-cases/Event.json).
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'initEvent' on 'Event': 1 argument required, but only 0 present.");
        return;
    }
    let _ = evt_init_common(scope, args.this(), &args);
}

fn cb_evt_init_custom_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.length() < 1 {
        // V8's own TypeError, never the page's replacement.
        let m = v8::String::new(scope, "Failed to execute 'initCustomEvent': 1 argument required").unwrap();
        let exc = v8::Exception::type_error(scope, m);
        scope.throw_exception(exc);
        return;
    }
    let this = args.this();
    // Blink (custom_event.cc): the whole re-initialisation, `detail` included, is skipped while the
    // event is being dispatched -- and that test now lives once, in `evt_init_common`, instead of
    // here alone. `detail`'s value lives in the same private slot the prototype getter reads -- the
    // instance never owns a `detail` property (Chrome stores it in C++; an own data prop is a
    // fingerprint tell).
    if !evt_init_common(scope, this, &args) { return; }
    let detail: v8::Local<v8::Value> = if args.length() >= 4 { args.get(3) } else { v8::null(scope).into() };
    evt_set_detail(scope, this, detail);
}

fn cb_evt_cancel_bubble_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    rv.set_bool(evt_flag_get(scope, this, "_stop"));
}
fn cb_evt_cancel_bubble_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if args.get(0).boolean_value(scope) { evt_flag_set(scope, this, "_stop", true); }
}

/// Blink's own identifier table, verbatim from `core/events/ui_event_with_key_state.cc`, and
/// case-SENSITIVE like it: `getModifierState("control")` is false, `("Control")` is not -- each identifier a
/// bit of the event's own modifier state, read from its slot (never through the page-replaceable `ctrlKey`
/// getter). "Accel" is Control everywhere but macOS. The six lock and modifier states beyond the four keys
/// come from `EventModifierInit` (Chrome 150: `new KeyboardEvent('k', {modifierAltGraph: true})
/// .getModifierState('AltGraph')` is true; korvus answered false for all six).
const MODIFIER_IDENTIFIERS: [(&str, &str); 11] = [
    ("Shift", "shiftKey"),
    ("Control", "ctrlKey"),
    ("Alt", "altKey"),
    ("Meta", "metaKey"),
    ("AltGraph", "modifierAltGraph"),
    ("Accel", "ctrlKey"),
    ("Fn", "modifierFn"),
    ("CapsLock", "modifierCapsLock"),
    ("ScrollLock", "modifierScrollLock"),
    ("NumLock", "modifierNumLock"),
    ("Symbol", "modifierSymbol"),
];

/// `EventModifierInit`'s six lock and modifier states beyond the four keys, by the slots they are kept in.
pub(crate) const MODIFIER_STATES: [&str; 6] = ["modifierAltGraph", "modifierFn", "modifierCapsLock", "modifierScrollLock", "modifierNumLock", "modifierSymbol"];

/// A `KeyboardEvent`'s, `MouseEvent`'s or `TouchEvent`'s six lock and modifier states, from its converted init
/// dictionary (`None`: all clear, as the legacy `init*Event` calls leave them -- Blink's `InitModifiers`).
pub(crate) fn init_modifier_states(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, dict: Option<v8::Local<v8::Object>>) {
    for member in MODIFIER_STATES {
        let b = crate::install::webidl::dict_bool(scope, dict, member);
        evt_flag_set(scope, this, member, b);
    }
}

fn cb_evt_get_modifier_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // `getModifierState()` with no argument is a TypeError, not a `false` -- `keyArg` is required.
    // Recorded from a real browser for both MouseEvent and KeyboardEvent, which is also why the
    // message names the interface the call landed on rather than a fixed one.
    if args.length() < 1 {
        let iface = get_prop(scope, args.this(), "constructor")
            .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
            .and_then(|c| get_prop(scope, c, "name"))
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_else(|| "MouseEvent".to_string());
        crate::binding::protos::throw_type_error(scope, &format!("Failed to execute 'getModifierState' on '{iface}': 1 argument required, but only 0 present."));
        return;
    }
    let this = args.this();
    let k = args.get(0).to_rust_string_lossy(scope);
    let Some((_, slot)) = MODIFIER_IDENTIFIERS.iter().find(|(id, _)| *id == k) else {
        rv.set_bool(false);
        return;
    };
    let val = evt_flag_get(scope, this, slot);
    rv.set_bool(val);
}

// Two WHATWG dispatch flags, bits of the event's private flags slot so a page can never see them: they are not
// own properties, carry no key `Object.getOwnPropertyNames`/`getOwnPropertySymbols` can reach, and do
// not survive structured cloning. [`evt_flag_get`] / [`evt_flag_set`] name them `_dispatching` and `_uninit`.
//
// `_dispatching` is the DISPATCH flag ([`FLAG_DISPATCH`]): set for the duration of `dispatchEvent`, checked on entry.
// Without it, re-dispatching an in-flight event recurses native -> JS listener -> native until V8's
// `Check failed: IsOnCentralStack()` aborts the PROCESS -- three lines of page script kill the engine.
// `_uninit` is the inverse of the INITIALIZED flag ([`FLAG_UNINIT`]): `document.createEvent()` sets it (that legacy
// factory hands back an uninitialized event), `initEvent`/`initCustomEvent` clear it, and dispatching
// while it is set is an InvalidStateError. Constructed events never carry it, so the default is right.

const TRUSTED_PRIV: &str = "korvus_trusted";
pub(crate) fn trusted_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    crate::binding::protos::private_key(scope, TRUSTED_PRIV)
}
/// `data` is the interface whose instance template carries this getter: like each of Blink's, it answers only for an
/// instance of that interface (Chrome 152: `MouseEvent`'s getter called on an `Event`, a `PointerEvent`'s on a
/// `MouseEvent`, any on `{}` or a prototype throws `Illegal invocation`; `Event`'s on a `MouseEvent`, or another
/// realm's event, answers).
fn cb_evt_is_trusted(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let data = crate::binding::protos::body_data(scope, &args);
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let iface: std::borrow::Cow<str> = match v8::Local::<v8::String>::try_from(data) {
        Ok(s) => s.to_rust_cow_lossy(scope, &mut buf),
        Err(_) => "Event".into(),
    };
    if !crate::binding::protos::implements(scope, this.into(), &iface) {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let key = trusted_key(scope);
    let trusted = this.get_private(scope, key).map(|v| v.is_true()).unwrap_or(false);
    rv.set_bool(trusted);
}

/// `[LegacyUnforgeable] readonly attribute boolean isTrusted` — the ONE own property a real event
/// instance carries, and the reason it is not on Event.prototype here either.
///
/// WebIDL's LegacyUnforgeable puts the attribute on the INSTANCE, not the prototype: Blink installs it
/// on the instance template, so every event object owns an enumerable, NON-configurable accessor named
/// `isTrusted` and no prototype anywhere carries one. The recorded Brave proves it -- its Event.prototype
/// has 21 members and `isTrusted` is not among them, and a scan of all 1,000-odd captured prototypes
/// finds it on none of them. Korvus used to have it exactly backwards (prototype accessor, no own
/// property), so `Object.getOwnPropertyNames(ev)` and `'isTrusted' in Object.getOwnPropertyDescriptors(
/// Event.prototype)` both answered the opposite of a browser.
///
/// The getter is ONE function per interface and realm, as Blink's: each interface's instance template carries the
/// `[LegacyUnforgeable]` attribute itself, so two `MouseEvent`s share a getter, a `MouseEvent` and an `Event` do not,
/// and a page's subclass of `Event` has `Event`'s (Chrome 152, measured; korvus gave every event one getter). It is
/// the realm's function of one isolate template per interface ([`is_trusted_template`]), which that interface's
/// instance template carries as the accessor itself ([`declare_is_trusted`]).
fn is_trusted_template<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::FunctionTemplate> {
    use v8::MapFnTo;
    crate::install::templates::standalone_template(scope, cb_evt_is_trusted.map_fn_to(), "get isTrusted", 0, Some(iface), true, false, None)
}
fn is_trusted_getter<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Function> {
    use v8::MapFnTo;
    crate::install::templates::standalone_fn(scope, cb_evt_is_trusted.map_fn_to(), "get isTrusted", 0, Some("Event"), true, false)
}
/// Event interface `iface`'s instance template, with the own accessor: every event its template makes -- `new X()`, a
/// page's subclass, a mint -- is born with it, where defining it on each (`install_own_is_trusted`) was 12% of a
/// `new Event` loop.
pub(crate) fn declare_is_trusted(scope: &mut v8::PinScope, iface: &str, it: v8::Local<v8::ObjectTemplate>) {
    let t = is_trusted_template(scope, iface);
    let Some(name) = crate::binding::protos::key_str(scope, "isTrusted") else { return };
    it.set_accessor_property(name.into(), Some(t), None, v8::PropertyAttribute::DONT_DELETE);
}
/// Install that own accessor on one event object -- unless its template made it with one (an object no event template
/// made gets `Event`'s getter). Called by every constructor
/// that produces an event: the native `Event` constructor's steps (`ctor::init_event`), SecurityPolicyViolationEvent's
/// native constructor, and the generated shell constructor for anything that inherits Event.prototype.
pub(crate) fn install_own_is_trusted(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let Some(name) = crate::binding::protos::key_str(scope, "isTrusted") else { return };
    if ev.has_own_property(scope, name.into()).unwrap_or(false) {
        return;
    }
    let getter = is_trusted_getter(scope);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut desc = v8::PropertyDescriptor::new_from_get_set(getter.into(), undef);
    desc.set_enumerable(true);
    desc.set_configurable(false);
    ev.define_property(scope, name.into(), &desc);
}
// `cb_evt_own_is_trusted` / `KB.__evtOwnIsTrusted` were the bridge the JS `Event` constructor called
// to install the property above. Every constructor of the family is Rust now, so they call
// `install_own_is_trusted` directly and the bridge is deleted.

// ── CustomEvent.detail ──────────────────────────────────────────────────────────────────────────
//
// `readonly attribute any detail` (custom_event.idl). Chrome holds it in C++ behind a prototype
// getter; Korvus mirrors that with the same private-slot mechanism `isTrusted` uses, so the instance
// carries NO own `detail` (`Object.getOwnPropertyNames` stays clean) and page JS can neither
// enumerate nor forge the slot.
//
// IT USED TO HAVE A SLOT NAMESPACE OF ITS OWN -- `korvus_detail`, with a `detail_key`, an
// `evt_set_detail` and a `cb_evt_detail_get` beside the generic ones below, because the JS
// `CustomEvent` class reached it through the `KB.__evtSetDetail` bridge. Now `detail` is a
// table row over the SHARED `korvus_evt_*` namespace, which left that private slot written by
// `initCustomEvent` and read by nobody. One namespace now, and the writer below is the same
// `evt_slot_write` every other member uses.

/// Blink (custom_event.cc): a missing OR null/undefined `CustomEventInit.detail` leaves the event's
/// detail NULL -- never `undefined` -- and so does `initCustomEvent` with no fourth argument.
fn evt_set_detail(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, detail: v8::Local<v8::Value>) {
    if detail.is_null_or_undefined() {
        evt_slot_write_null(scope, ev, "detail");
    } else {
        evt_slot_write(scope, ev, "detail", detail);
    }
}

// ── The generic init-dictionary slot, for every Event subclass ──────────────────────────────────
//
// `detail` above is one member of one interface done this way. MouseEvent has twenty-five, and
// KeyboardEvent eleven, and every one of them has the same problem: a value that arrives in the
// constructor's init dictionary and must be readable afterwards from a NATIVE accessor on the
// PROTOTYPE. Storing them as own properties on the instance -- which is what the JS classes in
// host.js used to do -- is a double tell: the prototype is bare where a browser has twenty-five
// accessors, and every instance carries own enumerable data properties where a browser has none.
//
// So: one private slot per member name, one privileged setter for the constructor to fill them, and
// one accessor factory for the prototype. Nothing here is interface-specific.

/// Boolean members and flags, in private slots rather than in properties.
///
/// Two of the names are internal -- `_stop` and `_stopImmediate`, the propagation flags, which used to
/// be own properties by those exact names, so `Object.getOwnPropertyNames(event)` printed the engine's
/// internals to anyone who asked -- and the rest are the WebIDL booleans `bubbles`, `cancelable`,
/// `composed`, `defaultPrevented` and the passive-listener flag. All share one slot namespace with
/// `evt_slot_write`, so the `bubbles` slot getter on the prototype reads exactly what
/// `evt_flag_set(.., "bubbles", ..)` wrote. A slot is invisible to enumeration, to `hasOwnProperty`,
/// and to a Proxy trap.
pub(crate) fn evt_flag_set(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, name: &str, v: bool) {
    if let Some(bit) = event_flag_bit(name) {
        let key = flags_key(scope);
        let f = evt_flags_at(scope, ev, key);
        let n = if v { f | bit } else { f & !bit };
        if n != f {
            evt_flags_put(scope, ev, key, n);
        }
        return;
    }
    let key = slot_key(scope, name);
    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, v).into();
    ev.set_private(scope, key, b);
}
pub(crate) fn evt_flag_get(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, name: &str) -> bool {
    if let Some(bit) = event_flag_bit(name) {
        let key = flags_key(scope);
        return evt_flags_at(scope, ev, key) & bit != 0;
    }
    let key = slot_key(scope, name);
    ev.get_private(scope, key).map(|v| v.boolean_value(scope)).unwrap_or(false)
}

/// The base Event's own booleans -- its three init flags, the canceled flag, the two propagation flags, the
/// passive-listener flag and HTML's legacy target override -- are BITS of one slot (`_flags`, a Smi), not a slot
/// each: `new Event` wrote eleven private slots before it returned (1,810 ns, Chrome 152 160 ns), and a dispatch
/// rewrote the propagation and passive flags at every listener. A bit never set is false, so an event whose slot was
/// never written has every flag clear -- which is what the constructor leaves for an empty `EventInit`.
pub(crate) const FLAG_BUBBLES: i32 = 1;
pub(crate) const FLAG_CANCELABLE: i32 = 1 << 1;
pub(crate) const FLAG_COMPOSED: i32 = 1 << 2;
pub(crate) const FLAG_CANCELED: i32 = 1 << 3;
pub(crate) const FLAG_STOP: i32 = 1 << 4;
pub(crate) const FLAG_STOP_IMMEDIATE: i32 = 1 << 5;
pub(crate) const FLAG_PASSIVE: i32 = 1 << 6;
pub(crate) const FLAG_TARGET_OVERRIDE: i32 = 1 << 7;
/// DOM's dispatch flag, set for the duration of `dispatchEvent` ([`dispatch_key`]'s note).
pub(crate) const FLAG_DISPATCH: i32 = 1 << 8;
/// The inverse of DOM's initialized flag: set by `document.createEvent`, cleared by `initEvent`.
pub(crate) const FLAG_UNINIT: i32 = 1 << 9;

/// The bit of an [`evt_flag_get`] / [`evt_flag_set`] name that is one of the base Event's flags; any other name
/// (a subclass's boolean member, a modifier state) keeps a slot of its own.
fn event_flag_bit(name: &str) -> Option<i32> {
    Some(match name {
        "bubbles" => FLAG_BUBBLES,
        "cancelable" => FLAG_CANCELABLE,
        "composed" => FLAG_COMPOSED,
        "defaultPrevented" => FLAG_CANCELED,
        "_stop" => FLAG_STOP,
        "_stopImmediate" => FLAG_STOP_IMMEDIATE,
        "_passive" => FLAG_PASSIVE,
        "_targetOverride" => FLAG_TARGET_OVERRIDE,
        "_dispatching" => FLAG_DISPATCH,
        "_uninit" => FLAG_UNINIT,
        _ => return None,
    })
}
/// The base slots, as private names ([`slot_key`]'s), every event's instance template declares
/// (`install::instance_shapes::declare_engine_slots`): the constructor and a dispatch write them into the event's own
/// slots instead of each adding a property -- a map transition per write, 6.3% of a `dispatchEvent` loop. Read as
/// `undefined`, which every reader takes for unwritten; none is asked `has_private` or deleted.
pub(crate) const ENGINE_SLOTS: [&str; 8] = [
    "korvus_evt_type",
    "korvus_evt_timeStamp",
    "korvus_evt__flags",
    "korvus_evt_target",
    "korvus_evt_currentTarget",
    "korvus_evt_eventPhase",
    "korvus_evt__pathNodes",
    "korvus_evt__pathTrees",
];

pub(crate) fn flags_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    slot_key(scope, "_flags")
}
/// The flag bits, with their slot's key in hand (0: never written).
pub(crate) fn evt_flags_at(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, key: v8::Local<v8::Private>) -> i32 {
    ev.get_private(scope, key).and_then(|v| v8::Local::<v8::Int32>::try_from(v).ok()).map_or(0, |i| i.value())
}
pub(crate) fn evt_flags_put(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, key: v8::Local<v8::Private>, bits: i32) {
    let v: v8::Local<v8::Value> = v8::Integer::new(scope, bits).into();
    ev.set_private(scope, key, v);
}

/// The event slots' and flags' private keys by name, made once per isolate. Made per call,
/// each cost a `format!` and a lookup in V8's private registry, and a dispatch reads or writes a slot
/// or flag several times at every node of its path.
#[derive(Default, Clone)]
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct SlotKeys(std::rc::Rc<std::cell::RefCell<SlotKeyMaps>>);

#[derive(Default)]
struct SlotKeyMaps {
    by_name: std::collections::HashMap<Box<str>, v8::Global<v8::Private>>,
    /// In front of `by_name`: `by_name` SipHashes every name (a page chooses the names
    /// `structured_clone` writes); this memo is how `preventDefault`'s two flags skip that. Filled from
    /// the engine's literals only ([`crate::binding::keys::fillable`]).
    recent: crate::binding::keys::AddrMemo<v8::Global<v8::Private>, 64>,
}

pub(crate) fn slot_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let i = crate::binding::keys::AddrMemo::<v8::Global<v8::Private>, 64>::entry_of(name);
    if let Some(k) = scope.get_slot::<SlotKeys>() {
        if let Some(key) = k.0.borrow().recent.get(i, name) {
            return v8::Local::new(scope, key);
        }
    }
    let keys = match scope.get_slot::<SlotKeys>() {
        Some(k) => k.clone(),
        None => {
            let k = SlotKeys::default();
            scope.set_slot(k.clone());
            k
        }
    };
    let found = keys.0.borrow().by_name.get(name).map(|k| v8::Local::new(scope, k));
    let key = match found {
        Some(k) => k,
        None => {
            let key = crate::binding::protos::private_key(scope, &format!("korvus_evt_{name}"));
            keys.0.borrow_mut().by_name.insert(name.into(), v8::Global::new(scope, key));
            key
        }
    };
    if crate::binding::keys::fillable(name) {
        keys.0.borrow_mut().recent.fill(i, name, || v8::Global::new(scope, key));
    }
    key
}

/// Write / read one member slot from Rust. The whole engine writes event state through these two:
/// the members are getter-only accessors on the prototype, so an assignment would be silently dropped
/// (sloppy mode) or throw (strict), and re-adding an own data property would put back the exact tell
/// this indirection exists to remove.
pub(crate) fn evt_slot_write(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let key = slot_key(scope, name);
    ev.set_private(scope, key, v);
}
pub(crate) fn evt_slot_read<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let key = slot_key(scope, name);
    ev.get_private(scope, key).filter(|v| !v.is_undefined())
}
/// `evt_slot_write` for the two shapes the writers actually need.
pub(crate) fn evt_slot_write_num(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, name: &str, n: f64) {
    let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
    evt_slot_write(scope, ev, name, v);
}
pub(crate) fn evt_slot_write_null(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, name: &str) {
    let v: v8::Local<v8::Value> = v8::null(scope).into();
    evt_slot_write(scope, ev, name, v);
}

// THE THREE JS-FACING SLOT/FLAG BRIDGES ARE DELETED: `cb_evt_slot_set` /
// `op_evt_slot_set`, `cb_evt_flag_set` / `op_evt_flag_set` and `cb_evt_flag_get` /
// `op_evt_flag_get`, which `host.js` installed as `KB.__evtSlotSet` / `__evtFlagSet` / `__evtFlagGet`.
//
// Each was a thin `args.get(0..2)` wrapper around the Rust function immediately above it
// (`evt_slot_write`, `evt_flag_set`, `evt_flag_get`), and each outlived its last JS caller by a lane:
// `__evtSlotSet` served `host.js`'s `SLOT`, whose last caller was the `StaticRange` constructor, now
// `core/dom/static_range.rs`; `__evtFlagSet` had had no caller since W4.b took the `Event` class;
// `__evtFlagGet` was read by the `dispatchEvent` in `events.js`, which W4.j deleted. An op whose only
// purpose is to be reachable from JS does not outlive the JS.

/// The prototype accessor. `args.data()` carries the member name, bound at install time, so one
/// callback serves every member of every event interface.
fn cb_evt_slot_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let key = slot_key(scope, &name);
    match args.this().get_private(scope, key).filter(|v| !v.is_undefined()) {
        Some(v) => rv.set(v),
        // A member never filled in reads as undefined rather than as a wrong zero: the constructor
        // fills every member it declares, so this only happens for an object that is not one of ours.
        None => rv.set_undefined(),
    }
}
/// Build the slot getter. `installer::member` and `install::kits` are the callers -- the one way a
/// `slot_get` row is mounted, since the `#[op2]` wrapper `op_evt_slot_get` that
/// let JS ask for the same accessor by string was deleted.
pub(crate) fn make_evt_slot_get<'s>(scope: &mut v8::PinScope<'s, '_>, fname: &str, member: &str) -> v8::Local<'s, v8::Function> { let d = str_val(scope, member); build_fn_data(scope, cb_evt_slot_get, fname, 0, d) }

/// The same slot read, for a member whose WebIDL type is NULLABLE.
///
/// `cb_evt_slot_get` answers `undefined` for a slot nobody wrote, which is right for a member whose
/// type has no empty value. A nullable attribute HAS one: `attribute Element? source` that was never
/// set is null in every browser, never undefined. The two callbacks differ only in that default, and
/// which one an attribute gets is decided by the IDL in `scripts/gen_shells.py` -- not per member.
fn cb_evt_slot_get_null(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let key = slot_key(scope, &name);
    match args.this().get_private(scope, key).filter(|v| !v.is_undefined()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}
/// Nullable slot getter. It was `op_evt_slot_get_null`'s core until that op was deleted with
/// the sixteen other event ops no JS called any more.
pub(crate) fn make_evt_slot_get_null<'s>(scope: &mut v8::PinScope<'s, '_>, fname: &str, member: &str) -> v8::Local<'s, v8::Function> { let d = str_val(scope, member); build_fn_data(scope, cb_evt_slot_get_null, fname, 0, d) }

/// The SETTER half of the pair above: a one-argument native accessor bound to a member name, writing
/// the same private slot `cb_evt_slot_get` reads.
///
/// It exists because of what the alternative does. A WRITABLE attribute the shells fill was getting a
/// pair of `op_noop_method` stubs, so `bc.onmessage = fn` type-checked, matched the recorded shape, and
/// silently discarded the handler -- an assignment that vanishes is worse than a member that is not
/// there, because the page has no way to notice. With this, a filled writable attribute round-trips:
/// what a page writes is what it reads back, which is the least a member can honestly do before someone
/// implements it.
fn cb_evt_slot_setter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let key = slot_key(scope, &name);
    let v = args.get(0);
    args.this().set_private(scope, key, v);
}
/// The writable-slot setter half, built the one way (its `#[op2]` wrapper `op_evt_slot_setter` went
/// with `op_evt_slot_get`'s).
pub(crate) fn make_evt_slot_setter<'s>(scope: &mut v8::PinScope<'s, '_>, fname: &str, member: &str) -> v8::Local<'s, v8::Function> { let d = str_val(scope, member); build_fn_data(scope, cb_evt_slot_setter, fname, 1, d) }

// ── the BRANDED slot accessors ──────────────────────────────────────────────────────
//
// The three above answer `undefined`/`null` for a receiver that is not one of ours, which is how
// `Event.prototype.type.call({})` used to return `undefined` where every browser throws
// `TypeError: Illegal invocation` (measured on `google-chrome --headless=new`, all seventeen rows).
// These do the same slot read behind `ctor::receiver`, the per-interface brand check. The bound data
// is `"<Interface>\u{1f}<slot>"`, so ONE callback still serves every member of every event interface
// and the slot can be an ALIAS of the member name (`MouseEvent.x` reads `clientX`).

/// Split the bound `"<Interface>\u{1f}<slot>"` datum.
fn branded_data(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> (String, String) {
    let d = crate::binding::protos::body_data(scope, args).to_rust_string_lossy(scope);
    match d.split_once('\u{1f}') {
        Some((i, s)) => (i.to_string(), s.to_string()),
        None => (String::new(), d),
    }
}

/// A slot getter's function data -- `<iface>\x1f<slot>`, see [`branded_data`] -- resolved once per isolate
/// into the two privates a read needs: the interface's brand (`webidl::has_brand`'s key) and the slot's.
/// Resolving them per call cost three `String`s for the data, a `format!` per key and a
/// SipHash lookup per key, on every `event.type` / `event.target` read.
#[derive(Default, Clone)]
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct EvtKeys(std::rc::Rc<std::cell::RefCell<EvtKeyMaps>>);

type KeyPair = (v8::Global<v8::Private>, v8::Global<v8::Private>);

#[derive(Default)]
struct EvtKeyMaps {
    /// by the data's text
    by_text: std::collections::HashMap<Box<str>, KeyPair>,
    /// by the fused member's record (`templates::fused_rec_addr`), for a getter that is one: the
    /// text path copied the data string out of V8 and SipHashed it on every read
    by_rec: std::collections::HashMap<usize, KeyPair, std::hash::BuildHasherDefault<AddrHasher>>,
}

/// A record address is already a unique number: one multiply spreads it over the table.
#[derive(Default)]
struct AddrHasher(u64);

impl std::hash::Hasher for AddrHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

fn evt_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> (v8::Local<'s, v8::Private>, v8::Local<'s, v8::Private>) {
    let keys = match scope.get_slot::<EvtKeys>() {
        Some(k) => k.clone(),
        None => {
            let k = EvtKeys::default();
            scope.set_slot(k.clone());
            k
        }
    };
    let rec = crate::install::templates::fused_rec_addr(args.data());
    if let Some((brand, slot)) = rec.and_then(|r| keys.0.borrow().by_rec.get(&r).map(|(b, s)| (v8::Local::new(scope, b), v8::Local::new(scope, s)))) {
        return (brand, slot);
    }
    let data = crate::binding::protos::body_data(scope, args);
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 96];
    let text: std::borrow::Cow<str> = match v8::Local::<v8::String>::try_from(data) {
        Ok(s) => s.to_rust_cow_lossy(scope, &mut buf),
        Err(_) => data.to_rust_string_lossy(scope).into(),
    };
    let found = keys.0.borrow().by_text.get(text.as_ref()).map(|(b, s)| (v8::Local::new(scope, b), v8::Local::new(scope, s)));
    let (brand, slot_k) = match found {
        Some(pair) => pair,
        None => {
            let (iface, slot) = text.split_once('\u{1f}').unwrap_or(("", text.as_ref()));
            let brand = crate::binding::protos::private_key(scope, &format!("korvus_brand_{iface}"));
            let slot_k = slot_key(scope, slot);
            keys.0.borrow_mut().by_text.insert(text.as_ref().into(), (v8::Global::new(scope, brand), v8::Global::new(scope, slot_k)));
            (brand, slot_k)
        }
    };
    if let Some(r) = rec {
        keys.0.borrow_mut().by_rec.insert(r, (v8::Global::new(scope, brand), v8::Global::new(scope, slot_k)));
    }
    (brand, slot_k)
}

/// The slot read behind a getter: `webidl::receiver`'s brand check, then the slot, or `None` for a
/// refused receiver (the TypeError already thrown).
fn evt_get_slot<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Option<Option<v8::Local<'s, v8::Value>>> {
    let (brand, slot) = evt_keys(scope, args);
    let this = args.this();
    if !this.has_private(scope, brand).unwrap_or(false) {
        crate::binding::protos::illegal_invocation(scope);
        return None;
    }
    Some(this.get_private(scope, slot).filter(|v| !v.is_undefined()))
}

fn cb_evt_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match evt_get_slot(scope, &args) {
        Some(Some(v)) => rv.set(v),
        Some(None) => rv.set_undefined(),
        None => {}
    }
}

fn cb_evt_get_null(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match evt_get_slot(scope, &args) {
        Some(Some(v)) => rv.set(v),
        Some(None) => rv.set_null(),
        None => {}
    }
}

/// `eventPhase`: NONE until a dispatch writes it -- the constructor leaves the slot unwritten.
fn cb_evt_get_zero(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match evt_get_slot(scope, &args) {
        Some(Some(v)) => rv.set(v),
        Some(None) => rv.set_int32(0),
        None => {}
    }
}

/// One of the base Event's boolean attributes: its bit of the flags slot (the row's slot is `_flags`).
fn cb_evt_get_flag<const BIT: i32>(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(v) = evt_get_slot(scope, &args) {
        let bits = v.and_then(|v| v8::Local::<v8::Int32>::try_from(v).ok()).map_or(0, |i| i.value());
        rv.set_bool(bits & BIT != 0);
    }
}

/// The getters of the base Event's members whose value before any write is a default rather than a slot the
/// constructor fills: `target` and `currentTarget` (null), `eventPhase` (NONE) and the four booleans (their bits).
fn install_event_base_getter(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let fname = format!("get {}", member.name);
    let g = match member.name {
        "target" | "currentTarget" => make_evt_get_null(scope, &fname, "Event", member.name),
        "eventPhase" => {
            let d = branded_datum(scope, "Event", "eventPhase");
            build_fn_data(scope, cb_evt_get_zero, &fname, 0, d)
        }
        flag => {
            let d = branded_datum(scope, "Event", "_flags");
            match flag {
                "bubbles" => build_fn_data(scope, cb_evt_get_flag::<FLAG_BUBBLES>, &fname, 0, d),
                "cancelable" => build_fn_data(scope, cb_evt_get_flag::<FLAG_CANCELABLE>, &fname, 0, d),
                "composed" => build_fn_data(scope, cb_evt_get_flag::<FLAG_COMPOSED>, &fname, 0, d),
                _ => build_fn_data(scope, cb_evt_get_flag::<FLAG_CANCELED>, &fname, 0, d),
            }
        }
    };
    define_accessor(scope, proto, member, g, None);
}

/// A `FrozenArray<T>` attribute WITHOUT `[SameObject]`: Blink's bindings build the frozen array from the
/// stored sequence on every read, so `r.packets !== r.packets` (measured, Chrome 150,
/// `USBIsochronousInTransferResult`) while the elements are the same objects.
fn cb_evt_get_fresh_frozen(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match evt_get_slot(scope, &args) {
        Some(Some(v)) => match v8::Local::<v8::Array>::try_from(v) {
            Ok(a) => {
                let items: Vec<v8::Local<v8::Value>> = (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect();
                let copy = v8::Array::new_with_elements(scope, &items);
                let _ = copy.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
                rv.set(copy.into());
            }
            Err(_) => rv.set(v),
        },
        Some(None) => rv.set_undefined(),
        None => {}
    }
}
pub(crate) fn make_evt_get_fresh_frozen<'s>(scope: &mut v8::PinScope<'s, '_>, fname: &str, iface: &str, slot: &str) -> v8::Local<'s, v8::Function> {
    let d = branded_datum(scope, iface, slot);
    build_fn_data(scope, cb_evt_get_fresh_frozen, fname, 0, d)
}

fn cb_evt_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (iface, slot) = branded_data(scope, &args);
    let Some(this) = crate::core::events::ctor::receiver(scope, args.this().into(), &iface) else { return };
    let v = args.get(0);
    evt_slot_write(scope, this, &slot, v);
}

// ── StorageEvent ───────────────────────────────────────────────────────────────────
//
// A registry shell whose five members were D2 stubs -- `initStorageEvent(...)` did nothing and every
// getter answered its shape default, so `e.key` read `""` after `initStorageEvent("storage", false,
// false, "k", ...)`. Measured in Chrome 150 and Brave 151 (`tools/realweb/seed/sx`):
// `storage,k,o,n,http://u/` after that call; on `document.createEvent("StorageEvent")` the three
// nullable strings and `storageArea` are null and `url` is `""`.

/// `(member, nullable)`, in `initStorageEvent`'s argument order after `cancelable`.
const STORAGE_MEMBERS: [(&str, bool); 5] = [("key", true), ("oldValue", true), ("newValue", true), ("url", false), ("storageArea", true)];

/// Write the five members from `values` (a missing or `undefined` one takes its default: null, or
/// `""` for `url`). The strings are converted as WebIDL `DOMString?` / `USVString` convert.
fn storage_event_fill(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, values: [Option<v8::Local<v8::Value>>; 5]) {
    for ((name, nullable), v) in STORAGE_MEMBERS.iter().zip(values) {
        let v = v.filter(|v| !v.is_undefined());
        let out: v8::Local<v8::Value> = match (v, *name) {
            (None, "url") => str_val(scope, ""),
            (None, _) => v8::null(scope).into(),
            (Some(v), "storageArea") => v,
            (Some(v), _) if *nullable && v.is_null() => v,
            (Some(v), _) => {
                let s = v.to_rust_string_lossy(scope);
                str_val(scope, &s)
            }
        };
        evt_slot_write(scope, this, name, out);
    }
}

/// `new StorageEvent(type, init)`: the dictionary's five members, after the `Event` steps.
pub(crate) fn storage_event_from_dict(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, dict: Option<v8::Local<v8::Object>>) {
    let mut values: [Option<v8::Local<v8::Value>>; 5] = [None; 5];
    if let Some(d) = dict {
        for (i, (name, _)) in STORAGE_MEMBERS.iter().enumerate() {
            let k = str_val(scope, name);
            values[i] = d.get(scope, k);
        }
    }
    storage_event_fill(scope, this, values);
}

fn cb_storage_event_init(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !evt_init_common(scope, this, &args) {
        return;
    }
    let mut values: [Option<v8::Local<v8::Value>>; 5] = [None; 5];
    for (i, v) in values.iter_mut().enumerate() {
        if args.length() > 3 + i as i32 {
            *v = Some(args.get(3 + i as i32));
        }
    }
    storage_event_fill(scope, this, values);
}

/// `StorageEvent`'s rows (a `_KITS` claim in `tools/sync/gen_shells.py`).
pub(crate) fn install_storage_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "initStorageEvent" => evt_branded_method(scope, proto, member, cb_storage_event_init),
        name => {
            let g = make_evt_get_null(scope, &format!("get {name}"), "StorageEvent", name);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
    }
}

fn branded_datum<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, slot: &str) -> v8::Local<'s, v8::Value> {
    str_val(scope, &format!("{iface}\u{1f}{slot}"))
}

/// A branded readonly slot getter (`undefined` when the slot was never written).
pub(crate) fn make_evt_get<'s>(scope: &mut v8::PinScope<'s, '_>, fname: &str, iface: &str, slot: &str) -> v8::Local<'s, v8::Function> {
    let d = branded_datum(scope, iface, slot);
    build_fn_data(scope, cb_evt_get, fname, 0, d)
}
/// A branded readonly slot getter for a NULLABLE attribute (`null` when the slot was never written).
pub(crate) fn make_evt_get_null<'s>(scope: &mut v8::PinScope<'s, '_>, fname: &str, iface: &str, slot: &str) -> v8::Local<'s, v8::Function> {
    let d = branded_datum(scope, iface, slot);
    build_fn_data(scope, cb_evt_get_null, fname, 0, d)
}
/// The branded setter half.
pub(crate) fn make_evt_set<'s>(scope: &mut v8::PinScope<'s, '_>, fname: &str, iface: &str, slot: &str) -> v8::Local<'s, v8::Function> {
    let d = branded_datum(scope, iface, slot);
    build_fn_data(scope, cb_evt_set, fname, 1, d)
}

/// A branded METHOD row: the receiver check FIRST, then the WebIDL arity check, then the body -- the
/// order Chrome uses (`Event.prototype.initEvent.call({})` is `Illegal invocation`, not the arity
/// message). `crate::install::live::method_row`'s generic guard checks arity first, so the family
/// mounts its methods through this instead.
pub(crate) fn evt_branded_method<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let f = build_fn(scope, cb, member.name, member.length as i32);
    let guard = crate::core::events::ctor::brand_guard(scope, iface, member, f);
    crate::install::kits::define_value(scope, target, member, guard, true);
}

// ── The three legacy initialisers ───────────────────────────────────────────────────────────────
//
// `initUIEvent`/`initMouseEvent`/`initKeyboardEvent` predate constructible events and are still on
// the prototypes. They re-initialise an event in place from POSITIONAL arguments, and they are real
// here rather than no-ops: they write the same slots the constructors write, so a page that builds
// an event with `document.createEvent("MouseEvent")` and fills it in this way reads back what it
// set. All three declare `length` 1 -- only `type` is required -- and all three throw when called
// with nothing, which is what the recordings show.

use evt_slot_write as slot_write;
fn arg_bool<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, i: i32) -> v8::Local<'s, v8::Value> {
    let b = args.get(i).boolean_value(scope);
    v8::Boolean::new(scope, b).into()
}
fn arg_int<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, i: i32) -> v8::Local<'s, v8::Value> {
    let n = args.get(i).number_value(scope).unwrap_or(0.0);
    let n = if n.is_finite() { n.trunc() } else { 0.0 };
    v8::Number::new(scope, n).into()
}
fn arg_str<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, i: i32) -> v8::Local<'s, v8::Value> {
    let s = args.get(i).to_rust_string_lossy(scope);
    str_val(scope, &s)
}

/// The shared prologue: arity, then DOM's "initialize" via [`evt_init_common`]. Returns false when
/// it has already thrown OR when the event is being dispatched, so the caller just returns and its
/// own positional slots are left alone -- which is what Blink's `UIEvent::InitUIEventInternal`
/// (`ui_event.cc:76`) does with `if (IsBeingDispatched()) return;` before it calls `initEvent`.
///
/// ARITY IS CHECKED FIRST and the dispatch flag second: the WebIDL conversion happens before the
/// algorithm runs, so `ev.initMouseEvent()` inside a listener is still the TypeError.
fn init_common(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str, name: &str) -> bool {
    if args.length() < 1 {
        crate::binding::protos::throw_type_error(scope, &format!("Failed to execute '{name}' on '{iface}': 1 argument required, but only 0 present."));
        return false;
    }
    evt_init_common(scope, args.this(), args)
}

/// `initUIEvent(type, bubbles, cancelable, view, detail)`
fn cb_evt_init_ui_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !init_common(scope, &args, "UIEvent", "initUIEvent") { return; }
    let this = args.this();
    let view = args.get(3);
    slot_write(scope, this, "view", view);
    let detail = arg_int(scope, &args, 4);
    slot_write(scope, this, "detail", detail);
}

/// `initMouseEvent(type, bubbles, cancelable, view, detail, screenX, screenY, clientX, clientY,
///                 ctrlKey, altKey, shiftKey, metaKey, button, relatedTarget)`
/// Note the argument order: ctrl, ALT, SHIFT, meta -- alt comes before shift, unlike everywhere else.
fn cb_evt_init_mouse_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !init_common(scope, &args, "MouseEvent", "initMouseEvent") { return; }
    let this = args.this();
    let view = args.get(3);
    slot_write(scope, this, "view", view);
    for (i, name) in [(4, "detail"), (5, "screenX"), (6, "screenY"), (7, "clientX"), (8, "clientY")] {
        let v = arg_int(scope, &args, i);
        slot_write(scope, this, name, v);
    }
    // page* track client*, and x/y are read through the clientX/clientY slots, so both follow.
    for (i, name) in [(7, "pageX"), (8, "pageY")] {
        let v = arg_int(scope, &args, i);
        slot_write(scope, this, name, v);
    }
    for (i, name) in [(9, "ctrlKey"), (10, "altKey"), (11, "shiftKey"), (12, "metaKey")] {
        let v = arg_bool(scope, &args, i);
        slot_write(scope, this, name, v);
    }
    init_modifier_states(scope, this, None);
    let button = arg_int(scope, &args, 13);
    slot_write(scope, this, "button", button);
    let which = args.get(13).number_value(scope).unwrap_or(0.0);
    let which: v8::Local<v8::Value> = v8::Number::new(scope, if which.is_finite() { which.trunc() + 1.0 } else { 1.0 }).into();
    slot_write(scope, this, "which", which);
    let related = args.get(14);
    let related = if related.is_undefined() { v8::null(scope).into() } else { related };
    slot_write(scope, this, "relatedTarget", related);
}

/// `initKeyboardEvent(type, bubbles, cancelable, view, key, location, ctrlKey, altKey, shiftKey,
///                    metaKey)` — Chrome's shape, which takes `key` where the older draft took
/// `keyIdentifier`.
fn cb_evt_init_keyboard_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !init_common(scope, &args, "KeyboardEvent", "initKeyboardEvent") { return; }
    let this = args.this();
    let view = args.get(3);
    slot_write(scope, this, "view", view);
    let key = arg_str(scope, &args, 4);
    slot_write(scope, this, "key", key);
    let loc = arg_int(scope, &args, 5);
    slot_write(scope, this, "location", loc);
    for (i, name) in [(6, "ctrlKey"), (7, "altKey"), (8, "shiftKey"), (9, "metaKey")] {
        let v = arg_bool(scope, &args, i);
        slot_write(scope, this, name, v);
    }
    init_modifier_states(scope, this, None);
}

// The native ops this file owns — declared here, beside their definitions, so that adding one
// touches no shared file (binding/exposure.rs). build.rs collects every such declaration into the
// extension's op table on each build.
// ---------------------------------------------------------------------------------------------
// The `EVENT_PROTOS` walker that WAS HERE is gone.
//
// `EvtMember`, `build_event_method`, `install_evt_member`, `install_event_protos` and
// `op_install_event_protos` existed to re-describe UIEvent / MouseEvent / KeyboardEvent's prototypes
// after `host.js`'s classes had built them in the wrong order -- a generated table
// (`core/events/generated_event_protos.rs`), a generator (`tools/sync/gen_event_protos.py`) and a
// one-line ESM caller (`js/generated_event_protos.js`), all three of which are deleted. The three
// interfaces are `interface!` tables now (`ui_event.rs`, `mouse_event.rs`, `keyboard_event.rs`), so
// THE installer mounts their rows once, in the capture's order, and there is nothing to repair.
// The four bespoke method bodies it reached through ops -- `cb_evt_init_ui_event`,
// `cb_evt_get_modifier_state`, `cb_evt_init_mouse_event`, `cb_evt_init_keyboard_event` -- are the
// same functions those tables' rows name, through the `install_*_row` helpers above.
// ── Event legacy accessors ──────────────────────────────────────────────────────────────────────
//
// `srcElement` and `returnValue` read the event's own slots, as Blink's `srcElement()` / `legacyReturnValue()` are
// `target()` / `!defaultPrevented()` in C++: a page that replaced `target`, `defaultPrevented` or `preventDefault`
// on `Event.prototype` sees none of them called (Chrome 150, measured). They used to go through `this`, which ran the
// page's replacements. `BeforeUnloadEvent` shadows `returnValue` with an OWN data property (a define, not an
// assignment -- it does not run this setter), so its DOMString `returnValue` is unaffected.

/// `srcElement`: the legacy alias for `target`, read off the event's own `target` slot.
fn cb_evt_src_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match evt_slot_read(scope, args.this(), "target") {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}
/// `returnValue`'s getter: the legacy inverse of the canceled flag (an un-dispatched event reads `true`), read off
/// the flag itself -- Blink's `Event::legacyReturnValue` is `!defaultPrevented()` in C++, so a page's own
/// `defaultPrevented` getter on `Event.prototype` is never asked (Chrome 150, measured: it is not called).
fn cb_evt_return_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let prevented = evt_flag_get(scope, args.this(), "defaultPrevented");
    rv.set_bool(!prevented);
}
/// `returnValue`'s setter: a FALSY value sets the canceled flag, by DOM's own rule ([`cb_evt_prevent_default`]'s:
/// cancelable, not from a passive listener); a truthy one cannot un-cancel. Blink's `setLegacyReturnValue` sets the
/// flag in C++ -- it does NOT call `this.preventDefault`, so a page that replaced `Event.prototype.preventDefault`
/// sees no call (Chrome 150, measured). This one used to call the page's.
fn cb_evt_return_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    if !args.get(0).boolean_value(scope) {
        cb_evt_prevent_default(scope, args, rv);
    }
}

/// Mint a bare instance on `globalThis[iface].prototype` — the Rust way in for an interface whose
/// constructor a page is not allowed to call (`BeforeUnloadEvent`, whose only producer is
/// `document.createEvent`). The caller then runs the interface's init steps, which stamp the brand.
pub(crate) fn mint_on_interface_prototype<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = global_get(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let proto = get_prop(scope, ctor, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    Some(obj)
}

/// The legacy factory's mint for an interface object that is NOT constructible — the whole of
/// `document.createEvent("<iface>")` for such a name: a bare object on the interface's own
/// prototype, the Event init steps with the EMPTY type (DOM 2.9: the factory hands back an
/// uninitialised event), and the interface's own brand on top of the `Event` brand `init_event`
/// stamps.
///
/// Two names reach this and they are the two Chrome answers "Illegal constructor" for:
/// `BeforeUnloadEvent` and `TextEvent`. It is one function rather than one per interface because
/// the steps are identical — `BeforeUnloadEvent` differs only by a `returnValue` slot, which its
/// own mint site writes after calling this.
pub(crate) fn mint_legacy_event<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let obj = mint_on_interface_prototype(scope, iface)?;
    crate::core::events::ctor::init_event(scope, obj, "", None);
    crate::install::webidl::brand(scope, obj, iface);
    Some(obj)
}

// ── the Event constructor ──────────────────────────────────────────────────────────────────────

/// `new Event(type, eventInitDict)` — native, so `Event.toString()` is `[native code]` by
/// construction, `Event.length` is the capture's 1 (a JS `class Event(type, init)` reported 2), and
/// `Event("x")` without `new` throws Chrome's "Please use the 'new' operator" rather than V8's
/// "Class constructor Event cannot be invoked without 'new'". Replaces `host.js`'s class and
/// `worker-host.js`'s copy of it.
pub(crate) fn cb_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    use crate::core::events::ctor::{arg_string, event_init_bits, init_event_flags, require_args, require_new};
    if !require_new(scope, &args, "Event") || !require_args(scope, &args, "Event", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "Event") else { return };
    // `EventInit` read into its values, not into a dictionary object nobody else reads -- and `undefined`/`null`
    // is the empty dictionary, every member false, with nothing to read (the object it made was 23% of a
    // `new Event('x')` loop)
    let init = args.get(1);
    let bits = if init.is_null_or_undefined() {
        0
    } else {
        match crate::install::ctor_dicts::convert_dict_values(scope, init, "EventInit", "Failed to construct 'Event': ") {
            Ok(values) => event_init_bits(&values),
            Err(()) => return,
        }
    };
    let this = args.this();
    init_event_flags(scope, this, &ty, bits);
    rv.set(this.into());
}

// ── Event.prototype, the table ─────────────────────────────────────────────────────────────────

/// A method row of this family: the receiver check FIRST, then the WebIDL arity check, then the body
/// (the order Chrome uses; see `ctor::cb_brand_guard`).
fn evt_method_row<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    evt_branded_method(scope, proto, member, cb);
}
/// An accessor row of this family whose body is bespoke rather than a plain slot read. The getter and
/// the setter are wrapped in the same receiver check the slot kits carry.
fn evt_accessor_row<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static MemberDef, getter: impl v8::MapFnTo<v8::FunctionCallback>, setter: Option<v8::Local<'s, v8::Function>>) {
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let g = build_fn(scope, getter, &format!("get {}", member.name), 0);
    let gg = crate::core::events::ctor::brand_accessor(scope, iface, &format!("get {}", member.name), 0, g);
    let ss = setter.map(|s| crate::core::events::ctor::brand_accessor(scope, iface, &format!("set {}", member.name), 1, s));
    define_accessor(scope, proto, member, gg, ss);
}

/// `initCustomEvent`, for the `CustomEvent` table's one method row (the body is this file's; the
/// table is `custom_event.rs`).
pub(crate) fn install_custom_event_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    evt_branded_method(scope, proto, member, cb_evt_init_custom_event);
}

// The four legacy/bespoke method bodies the UI-event tables name. Their callbacks are this file's
// (they predate the tables and are shared between `initMouseEvent` and `getModifierState`); the
// tables reach them through these one-row installers, so no table imports a `cb_*` directly.

/// `UIEvent.prototype.initUIEvent`.
pub(crate) fn install_ui_event_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    evt_branded_method(scope, proto, member, cb_evt_init_ui_event);
}
/// `MouseEvent.prototype.getModifierState` and `KeyboardEvent.prototype.getModifierState` — one body,
/// two tables, which is the whole point of the row naming it by word.
pub fn install_modifier_state(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    evt_branded_method(scope, proto, member, cb_evt_get_modifier_state);
}
/// `MouseEvent.prototype.initMouseEvent`.
pub fn install_init_mouse_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    evt_branded_method(scope, proto, member, cb_evt_init_mouse_event);
}
/// `KeyboardEvent.prototype.initKeyboardEvent`.
pub fn install_init_keyboard_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    evt_branded_method(scope, proto, member, cb_evt_init_keyboard_event);
}
/// The table body of every hand-written `Event` row (`type` and `timeStamp` are the `evt_get` kit; the other base
/// attributes read a default before any write, [`install_event_base_getter`]).
pub fn install_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "srcElement" => evt_accessor_row(scope, proto, member, cb_evt_src_element, None),
        "returnValue" => {
            let s = build_fn(scope, cb_evt_return_value_set, "set returnValue", 1);
            evt_accessor_row(scope, proto, member, cb_evt_return_value_get, Some(s));
        }
        "cancelBubble" => {
            let s = build_fn(scope, cb_evt_cancel_bubble_set, "set cancelBubble", 1);
            evt_accessor_row(scope, proto, member, cb_evt_cancel_bubble_get, Some(s));
        }
        "composedPath" => evt_method_row(scope, proto, member, cb_evt_composed_path),
        "initEvent" => evt_method_row(scope, proto, member, cb_evt_init_event),
        "preventDefault" => evt_method_row(scope, proto, member, cb_evt_prevent_default),
        "stopImmediatePropagation" => evt_method_row(scope, proto, member, cb_evt_stop_immediate),
        "stopPropagation" => evt_method_row(scope, proto, member, cb_evt_stop_propagation),
        "target" | "currentTarget" | "eventPhase" | "bubbles" | "cancelable" | "defaultPrevented" | "composed" => {
            install_event_base_getter(scope, proto, member)
        }
        other => debug_assert!(false, "install_event: unexpected member {other}"),
    }
}

// The one place the Event interface is defined (goal invariant I1): the twenty-one members in the
// capture's order -- the nine slot-backed base attributes, the three legacy accessors, the four phase
// constants (on the prototype AND, as `static_constant`, on the interface object), the five methods
// -- and `Event.length` 1. The prototype is the JS `Event` class's (host.js / worker-host.js, until
// W4.b), made the table's by `installer::overwrite_prototype` from `events::install_event_model`.
// `isTrusted` is NOT here: `[LegacyUnforgeable]` puts it on the instance (`install_own_is_trusted`).
webapi::interface! {
    "Event" [value, both] {
        ctor            "Event"                                    len 1 = stub,
        attr            "type"             : DomString readonly          = evt_get,
        attr            "target"           : Object    readonly          = install_event,
        attr            "currentTarget"    : Object    readonly          = install_event,
        attr            "eventPhase"       : Integer   readonly          = install_event,
        attr            "bubbles"          : Boolean   readonly          = install_event,
        attr            "cancelable"       : Boolean   readonly          = install_event,
        attr            "defaultPrevented" : Boolean   readonly          = install_event,
        attr            "composed"         : Boolean   readonly          = install_event,
        attr            "timeStamp"        : Double    readonly          = evt_get,
        attr            "srcElement"       : Object    readonly          = install_event,
        attr            "returnValue"      : Boolean                     = install_event,
        attr            "cancelBubble"     : Boolean                     = install_event,
        constant        "NONE"             : Integer                     = 0,
        constant        "CAPTURING_PHASE"  : Integer                     = 1,
        constant        "AT_TARGET"        : Integer                     = 2,
        constant        "BUBBLING_PHASE"   : Integer                     = 3,
        method          "composedPath"     : Sequence  len 0             = install_event,
        method          "initEvent"        : Undefined len 1 required 1  = install_event,
        method          "preventDefault"   : Undefined len 0             = install_event,
        method          "stopImmediatePropagation" : Undefined len 0     = install_event,
        method          "stopPropagation"  : Undefined len 0             = install_event,
        static_constant "NONE"             : Integer                     = 0,
        static_constant "CAPTURING_PHASE"  : Integer                     = 1,
        static_constant "AT_TARGET"        : Integer                     = 2,
        static_constant "BUBBLING_PHASE"   : Integer                     = 3
    }
}

// SEVENTEEN OPS WERE HERE, and they are gone.
//
// An `op_evt_*` was a JS-facing builder: `OPS.op_evt_composed_path("composedPath", 0)` handed a JS
// file a native function to `Object.defineProperty` onto a prototype. Every caller was a `host.js`
// class body, `worker-host.js`'s `Event.prototype` rebuild loop or `dom/08g-observers.js`'s event
// block, and all three are deleted -- the table rows name the SAME `cb_*` callbacks directly, as Rust
// calls, so the op wrapper had nothing left to serve. Deleted: prevent_default, stop_propagation,
// stop_immediate, composed_path, init_event, init_custom_event, cancel_bubble_get/set,
// get_modifier_state, own_is_trusted, detail_getter, detail_set, slot_get_null, slot_setter,
// init_ui_event, init_mouse_event, init_keyboard_event.
//
// THE THREE JS-FACING ONES ARE GONE TOO -- `op_evt_slot_set`, `op_evt_flag_set` and
// `op_evt_flag_get`, the bridges behind `KB.__evtSlotSet` / `__evtFlagSet` / `__evtFlagGet`. See the
// note where their bodies were.
//
// AND SO ARE THE LAST TWO, `op_evt_slot_get` and `op_evt_slot_setter`. They were not
// bridges -- they BUILT accessors, which is a different job from writing one object's slot -- but
// they had no caller: the note that used to stand here said "both have Rust callers", and what
// `installer::member` and `install::kits` actually call is `make_evt_slot_get` /
// `make_evt_slot_setter`, the plain Rust functions the ops wrapped. Those are untouched and are now
// the single way a `slot_get` row is mounted. This file declares no ops at all any more.


/// A registry-built Event subclass's OWN members from its init dictionary: every readonly
/// attribute of the interface -- and of each registry-built ancestor down to the first hand-built one -- takes
/// the dictionary's member converted to its type, or the type's default, into the slot its getter reads
/// (`slot_backed`). 22 such interfaces answered their shape default whatever the dictionary said (the probe:
/// CloseEvent code/reason/wasClean, ToggleEvent newState/oldState, PageTransitionEvent persisted, ...).
const STRING_ENUM_MEMBERS: &[(&str, &str)] = &[("SpeechRecognitionErrorEvent", "error")];
/// A numeric member Blink's constructor gives a value no IDL default says when the dictionary omits it:
/// `BlobEvent`'s `timecode_(initializer->hasTimecode() ? initializer->timecode() : NAN)`
/// (modules/mediarecorder/blob_event.cc, Chromium 150).
const ABSENT_NAN_MEMBERS: &[(&str, &str)] = &[("BlobEvent", "timecode")];

pub(crate) fn shell_members_from_dict(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str, dict: Option<v8::Local<v8::Object>>) {
    use crate::install::{MemberKind, TypeTag};
    let mut name: Option<&str> = Some(iface);
    while let Some(n) = name {
        let Some(def) = crate::install::generated_registry::REGISTRY.iter().find(|d| d.name == n) else { break };
        // Slot-backed by data/slot_backed.json, or an Event subclass whose constructor converts an init
        // dictionary (its rows read these slots behind its brand -- a gen_shells rule).
        if !def.slot_backed && crate::install::ctor_dicts::dict_arg(n).is_none() {
            break;
        }
        for m in def.members.iter() {
            if m.static_member || !matches!(m.kind, MemberKind::Attr { .. }) {
                continue;
            }
            let given = dict.and_then(|d| crate::binding::protos::get_prop(scope, d, m.name)).filter(|v| !v.is_undefined());
            // `DeviceMotionEvent`'s three sub-objects are objects made from their dictionaries, not the dictionaries.
            if let Some(sub) = given.and_then(|g| crate::core::html::device_sensors::motion_init_member(scope, n, m.name, g)) {
                evt_slot_write(scope, this, m.name, sub);
                continue;
            }
            // An ENUM member the registry types `Any` (no enum tag): a string, "" when absent (Chrome 150:
            // `new SpeechRecognitionErrorEvent("x").error` is "", and an unknown value is kept as given).
            let ty = if STRING_ENUM_MEMBERS.contains(&(n, m.name)) { TypeTag::DomString } else { m.ty };
            let v: v8::Local<v8::Value> = match (ty, given) {
                (_, Some(g)) if m.nullable && g.is_null() => g,
                (TypeTag::Boolean, g) => v8::Boolean::new(scope, g.map(|g| g.boolean_value(scope)).unwrap_or(false)).into(),
                (TypeTag::Integer, Some(g)) => v8::Number::new(scope, g.number_value(scope).unwrap_or(0.0).trunc()).into(),
                (TypeTag::Double, Some(g)) => v8::Number::new(scope, g.number_value(scope).unwrap_or(0.0)).into(),
                (TypeTag::Integer | TypeTag::Double, None) if ABSENT_NAN_MEMBERS.contains(&(n, m.name)) => v8::Number::new(scope, f64::NAN).into(),
                (TypeTag::Integer | TypeTag::Double, None) if m.nullable => v8::null(scope).into(),
                (TypeTag::Integer | TypeTag::Double, None) => v8::Number::new(scope, 0.0).into(),
                (TypeTag::DomString | TypeTag::UsvString, Some(g)) => match g.to_string(scope) {
                    Some(s) => s.into(),
                    None => return, // ToString threw: the constructor throws
                },
                (TypeTag::DomString | TypeTag::UsvString, None) if m.nullable => v8::null(scope).into(),
                (TypeTag::DomString | TypeTag::UsvString, None) => crate::binding::protos::str_val(scope, ""),
                (_, Some(g)) => g,
                (_, None) => v8::null(scope).into(),
            };
            // A `FrozenArray` attribute holds the converted sequence frozen (`fontfaces`, `types`, `streams`).
            if v.is_array() && crate::install::ctor_dicts::is_frozen_attr(n, m.name) {
                if let Ok(a) = v8::Local::<v8::Object>::try_from(v) {
                    let _ = a.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
                }
            }
            evt_slot_write(scope, this, m.name, v);
        }
        name = def.parent;
    }
}

