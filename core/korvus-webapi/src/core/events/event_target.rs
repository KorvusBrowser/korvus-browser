//! `core/events/event_target.rs` -- the DOM event model's one store and one algorithm.
//!
//! `EventListeners` (in `OpState`) holds, keyed exactly alike by [`EtKey`]:
//!   * the LISTENER LISTS -- one `Vec<EtListener>` per (target, type), in registration order. An entry
//!     is a page callback (`addEventListener`), the EVENT HANDLER LISTENER (HTML's "event handler
//!     processing algorithm": one per (target, type), registered when the handler first becomes
//!     non-null, reading the CURRENT `on<type>` value at fire time), a `when()` subscription, or the
//!     abort hook a `subscribe(_, {signal})` registers on its signal. None of the last three is a
//!     page-visible function: they are Rust entries, so a handler keeps its position among listeners
//!     and a listener list never carries an engine-made closure.
//!   * the HANDLER VALUES -- what `on<type>` reads back: the object the IDL attribute was set to
//!     (`[LegacyTreatNonObjectAsNull]`: a non-object reads null), or the function compiled once from
//!     the `on<type>` content attribute (`new Function("event", code)` semantics), remembered with
//!     the source it came from.
//!   * the CURRENT-EVENT STACK -- pushed before and popped after every listener call; `window.event`
//!     is its top (`undefined` outside a dispatch).
//!
//! `et_fire_at` calls every listener itself: a function with `this` = currentTarget, an object's
//! `handleEvent` (looked up at call time) with `this` = the listener; a throw is caught and handed to
//! `report::report_exception`, and the next listener still runs. The window's `onerror` is the one
//! handler with a different shape -- called with `(message, filename, lineno, colno, error)` for an
//! `ErrorEvent`, and `true` cancels; every other handler cancels by returning `false`.
//!
//! Dispatch proper (capture/target/bubble with the window at the end of the path, once/passive/capture
//! options, dedup, the dispatch-flag and uninitialised-flag InvalidStateErrors, stop flags reset at the
//! end) is unchanged from the spec-shaped form it had before this file owned the listener call.

use deno_core::{op2, v8, OpState};
use crate::binding::protos::{arg_node_id, build_fn, build_fn_data, get_prop, require_args, int_val, str_val, throw_dom, throw_type_error};
use crate::core::events::event::{
    evt_flag_get, evt_flag_set, evt_flags_at, evt_flags_put, evt_slot_read, evt_slot_write, flags_key, FLAG_BUBBLES, FLAG_CANCELED, FLAG_COMPOSED,
    FLAG_DISPATCH, FLAG_PASSIVE, FLAG_STOP, FLAG_STOP_IMMEDIATE, FLAG_TARGET_OVERRIDE, FLAG_UNINIT,
};

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub(crate) enum EtKey { Window, Document, Node(u32), Object(u32) }

/// What one entry of a listener list is.
enum EtKind {
    /// `addEventListener(type, callback)`: a function, or an object whose `handleEvent` is read at call time.
    Callback(v8::Global<v8::Value>),
    /// The event handler listener: fires the CURRENT `on<type>` value (HTML 8.1.8.1).
    Handler,
    /// A `when()` subscription: forwards the event to the subscriber's `next`.
    Observer(v8::Global<v8::Object>),
    /// The abort hook `subscribe(_, {signal})` registers on the signal: closes the subscriber.
    SubscriberAbort(v8::Global<v8::Object>),
}
impl EtKind {
    fn same(&self, other: &EtKind) -> bool {
        match (self, other) {
            (EtKind::Callback(a), EtKind::Callback(b)) => a == b,
            (EtKind::Handler, EtKind::Handler) => true,
            (EtKind::Observer(a), EtKind::Observer(b)) => a == b,
            (EtKind::SubscriberAbort(a), EtKind::SubscriberAbort(b)) => a == b,
            _ => false,
        }
    }
    fn clone_kind(&self) -> EtKind {
        match self {
            EtKind::Callback(g) => EtKind::Callback(g.clone()),
            EtKind::Handler => EtKind::Handler,
            EtKind::Observer(s) => EtKind::Observer(s.clone()),
            EtKind::SubscriberAbort(s) => EtKind::SubscriberAbort(s.clone()),
        }
    }
}
/// One registration. A dispatch runs a SNAPSHOT of the list (`Rc`s, no handle made per listener) and skips an
/// entry whose `removed` flag is set: DOM's "removed" flag, Blink's `RegisteredEventListener::Removed()`. Asking the
/// live list instead -- "is an equal listener still registered?" -- ran a listener a listener before it had removed
/// and added again: Chrome 152 runs `a` alone for `[a, b]` where `a` does `remove(b); add(b)` (the new `b` is not in
/// the snapshot, the old one is removed); korvus ran `a b`.
struct EtListener { kind: EtKind, capture: bool, once: bool, passive: bool, removed: std::cell::Cell<bool> }
type EtEntry = std::rc::Rc<EtListener>;

/// The current value of an `on<type>` handler, as the store keeps it.
enum HandlerValue {
    /// Set through the IDL attribute (`el.onclick = f`): the object as given.
    Idl(v8::Global<v8::Value>),
    /// Compiled from the `on<type>` content attribute, with the source it was compiled from -- a
    /// later attribute write with different text recompiles; a removed attribute deactivates.
    Attr { compiled: v8::Global<v8::Value>, source: String },
}

#[derive(Default)]
pub(crate) struct EventListeners {
    map: korvus_dom::FxHashMap<EtKey, korvus_dom::FxHashMap<String, Vec<EtEntry>>>,
    handlers: korvus_dom::FxHashMap<EtKey, korvus_dom::FxHashMap<String, HandlerValue>>,
    next_id: u32,
}
const ET_ID_PRIV: &str = "korvus_et_id";
/// The per-object store id's private, through the engine's key cache (`protos::private_key`): `Private::for_api`
/// made a V8 string and probed V8's registry on every listener call on a non-node target.
fn et_object_id_read(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Option<u32> {
    let key = crate::binding::protos::private_key(scope, ET_ID_PRIV);
    obj.get_private(scope, key).and_then(|v| v.uint32_value(scope)).filter(|&id| id != 0)
}
fn et_object_id(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) -> u32 {
    if let Some(id) = et_object_id_read(scope, obj) { return id; }
    let id = { let mut st = op_state.borrow_mut(); let s = st.borrow_mut::<EventListeners>(); s.next_id += 1; s.next_id };
    let key = crate::binding::protos::private_key(scope, ET_ID_PRIV);
    let idv = int_val(scope, id as i64);
    obj.set_private(scope, key, idv);
    id
}
/// A node's store key, from its wrapper alone: the page's own document object is
/// `EtKey::Document`, any other node `EtKey::Node(id)` -- what the checks below come to for a node, whose
/// `nodeType` is a number. Asking `nodeType` as a JS property ran its getter through V8 at every node of
/// every dispatch path (29 % of a dispatch). `None` for anything that is not a node wrapper.
fn node_key(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<EtKey> {
    let id = arg_node_id(scope, this)?;
    if id == crate::core::events::shared::ON_DOCUMENT_ID && page_document(scope).is_some_and(|doc| this.strict_equals(doc)) {
        return Some(EtKey::Document);
    }
    Some(EtKey::Node(id))
}
/// Store key for `this` (no id allocation) -- for remove/dispatch/snapshot/fire/the handler getter.
pub(crate) fn et_key(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<EtKey> {
    if let Some(key) = node_key(scope, this) {
        return Some(key);
    }
    let global: v8::Local<v8::Value> = crate::core::html::browsing_context::main_global(scope).into();
    if let Some(w) = frame_window_for(scope, this, global) { return et_key(scope, w.into()); }
    if this.strict_equals(global) { return Some(EtKey::Window); }
    // an isolated world's window stands for its frame's, or the page's
    match crate::core::html::isolated_world::world_window(scope, this) {
        Some(None) => return Some(EtKey::Window),
        Some(Some(f)) => {
            if let Some(w) = crate::core::html::browsing_context::frame_window(scope, f) {
                return et_key(scope, w.into());
            }
        }
        None => {}
    }
    if let Some(doc) = page_document(scope) { if this.strict_equals(doc) { return Some(EtKey::Document); } }
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    // A frame's window is never a node, and asking it for `nodeType` would install its realm.
    let is_window = crate::core::html::browsing_context::ctx_of_window(scope, obj).is_some();
    // A node is known by its wrapper, not by asking the page-visible `nodeType` getter (a page that
    // wraps `Node.prototype.nodeType` saw every listener registration read it).
    if !is_window {
        if let Some(id) = arg_node_id(scope, this) { return Some(EtKey::Node(id)); }
    }
    et_object_id_read(scope, obj).map(EtKey::Object)
}
/// The PAGE's document -- `EtKey::Document` is its key alone. Read through the memo, not the current
/// global's `document`: a frame's own `addEventListener` runs in the frame's realm,
/// whose `document` is the frame's, and its listeners were filed under the page's key.
fn page_document<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    crate::binding::protos::document_val(scope, &op_state)
}
/// Like et_key but allocates a per-object id for a non-node target (addEventListener, the handler setter).
pub(crate) fn et_key_add(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) -> Option<EtKey> {
    if let Some(key) = node_key(scope, this) {
        return Some(key);
    }
    let global: v8::Local<v8::Value> = crate::core::html::browsing_context::main_global(scope).into();
    if let Some(w) = frame_window_for(scope, this, global) { return et_key_add(scope, w.into(), op_state); }
    if this.strict_equals(global) { return Some(EtKey::Window); }
    // an isolated world's window stands for its frame's, or the page's
    match crate::core::html::isolated_world::world_window(scope, this) {
        Some(None) => return Some(EtKey::Window),
        Some(Some(f)) => {
            if let Some(w) = crate::core::html::browsing_context::frame_window(scope, f) {
                return et_key_add(scope, w.into(), op_state);
            }
        }
        None => {}
    }
    if let Some(doc) = page_document(scope) { if this.strict_equals(doc) { return Some(EtKey::Document); } }
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    // A frame's window is never a node, and asking it for `nodeType` would install its realm.
    let is_window = crate::core::html::browsing_context::ctx_of_window(scope, obj).is_some();
    if !is_window {
        if let Some(id) = arg_node_id(scope, this) { return Some(EtKey::Node(id)); }
    }
    Some(EtKey::Object(et_object_id(scope, obj, op_state)))
}
/// A frame's BARE `addEventListener(...)` means the frame's window. A frame runs in its own
/// V8 context, but every Web API function it calls is the page realm's, and V8 hands a native
/// function called with no receiver ITS OWN realm's global -- the page's. So `this` arrives as the
/// page window while a frame's script is running, and it is the frame's window that was meant. The
/// one call this misreads is a same-origin frame's explicit `parent.addEventListener(...)`, which
/// binds to the frame; a frame's own `window.addEventListener(...)` names its receiver and is exact.
fn frame_window_for<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, global: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    if !this.strict_equals(global) {
        return None;
    }
    crate::core::html::browsing_context::running_frame_window(scope).filter(|w| !this.strict_equals((*w).into()))
}
/// A listener's options, as WebIDL converts `(AddEventListenerOptions or boolean)` -- `(EventListenerOptions or
/// boolean)` for `removeEventListener` -- in Chrome 150: `undefined`, `null` and every object
/// are the dictionary, read in WebIDL's order (`capture`, then `once`, `passive`, `signal`) and converted as read --
/// a `signal` that is no `AbortSignal` (`null` included) is a TypeError -- and any other value is `capture`, as a
/// boolean (`addEventListener(t, f, 1)` captures). `passive` is `None` when the page gave none.
///
/// korvus read the three booleans alone, took a number or a string for "no capture", and never read `signal`.
struct ListenerOptions<'s> {
    capture: bool,
    once: bool,
    passive: Option<bool>,
    signal: Option<v8::Local<'s, v8::Value>>,
}

fn listener_options<'s>(scope: &mut v8::PinScope<'s, '_>, opts: v8::Local<'_, v8::Value>, op: &str) -> Result<ListenerOptions<'s>, ()> {
    if !(opts.is_null_or_undefined() || opts.is_object()) {
        return Ok(ListenerOptions { capture: opts.boolean_value(scope), once: false, passive: None, signal: None });
    }
    let dict = if op == "addEventListener" { "AddEventListenerOptions" } else { "EventListenerOptions" };
    // The two operations' error prefixes are constants; only another `op` (there is none today) formats one.
    let prefix: std::borrow::Cow<str> = match op {
        "addEventListener" => "Failed to execute 'addEventListener' on 'EventTarget': ".into(),
        "removeEventListener" => "Failed to execute 'removeEventListener' on 'EventTarget': ".into(),
        _ => format!("Failed to execute '{op}' on 'EventTarget': ").into(),
    };
    // no options (most calls): the dictionary's defaults, which read nothing and cannot throw (no member is required)
    if opts.is_null_or_undefined() {
        return Ok(ListenerOptions { capture: false, once: false, passive: None, signal: None });
    }
    // the converted members, read in place: no null-prototype object made to be read back
    let d = crate::install::ctor_dicts::convert_dict_values(scope, opts, dict, &prefix)?;
    let get = |k: &str| d.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    let flag = |k: &str| get(k).filter(|v| v.is_boolean()).map(|v| v.is_true());
    Ok(ListenerOptions { capture: flag("capture").unwrap_or(false), once: flag("once").unwrap_or(false), passive: flag("passive"), signal: get("signal").filter(|v| v.is_object()) })
}

/// DOM's "default passive value": `true` for a `touchstart`, `touchmove`, `wheel` or `mousewheel` listener on a
/// Window, a Document, or a document's document element or body element -- Chrome's scrolling intervention, which
/// makes such a listener's `preventDefault()` do nothing unless the page asked `{passive: false}` (Chrome 150,
/// measured: korvus let it cancel the event).
fn default_passive(scope: &mut v8::PinScope, key: &EtKey, target: v8::Local<v8::Value>, ty: &str) -> bool {
    if !matches!(ty, "touchstart" | "touchmove" | "wheel" | "mousewheel") {
        return false;
    }
    match key {
        EtKey::Window | EtKey::Document => true,
        EtKey::Node(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let dom = &st.borrow::<crate::binding::host_dom::HostDom>().0;
            if dom.node_type(*id) == 9 {
                return true;
            }
            let root = dom.root_of(*id);
            dom.node_type(root) == 9
                && (crate::core::dom::document::doc_element_child(dom, root) == Some(*id) || crate::core::dom::document::body_element_of(&st, root) == Some(*id))
        }
        EtKey::Object(_) => v8::Local::<v8::Object>::try_from(target).ok().is_some_and(|o| crate::core::html::browsing_context::ctx_of_window(scope, o).is_some()),
    }
}

/// `type` and `callback`, converted as WebIDL converts them before any step runs: a DOMString (a Symbol is
/// Chrome's TypeError, its context in front) and an `EventListener?` -- an object or `null`/`undefined`, anything
/// else "parameter 2 is not of type 'Object'." (Chrome 150; korvus took a number or a string for a listener).
///
/// The type is read into the caller's `buf` -- an event type is short -- rather than a heap `String` per call.
fn listener_args<'b>(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, op: &str, buf: &'b mut [std::mem::MaybeUninit<u8>; 64]) -> Option<std::borrow::Cow<'b, str>> {
    let t = args.get(0);
    if t.is_symbol() {
        throw_type_error(scope, &format!("Failed to execute '{op}' on 'EventTarget': Cannot convert a Symbol value to a string"));
        return None;
    }
    let ty = t.to_string(scope)?.to_rust_cow_lossy(scope, buf);
    let cb = args.get(1);
    if !cb.is_null_or_undefined() && !cb.is_object() {
        throw_type_error(scope, &format!("Failed to execute '{op}' on 'EventTarget': parameter 2 is not of type 'Object'."));
        return None;
    }
    Some(ty)
}

// ── the list, from Rust ────────────────────────────────────────────────────────────────────────

/// Append `kind` to the (key, type) list unless an equal entry with the same `capture` is there
/// (the spec's dedup for callbacks; the same rule keeps one handler / one subscription per list).
fn et_push(scope: &mut v8::PinScope, key: EtKey, ty: &str, kind: EtKind, capture: bool, once: bool, passive: bool) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let s = st.borrow_mut::<EventListeners>();
    let types = s.map.entry(key).or_default();
    // the type's own String is made once, with its list; every later add finds the list by `&str`
    let list = match types.get_mut(ty) {
        Some(list) => list,
        None => types.entry(ty.to_string()).or_default(),
    };
    if list.iter().any(|e| e.kind.same(&kind) && e.capture == capture) { return false; }
    list.push(std::rc::Rc::new(EtListener { kind, capture, once, passive, removed: std::cell::Cell::new(false) }));
    true
}
fn et_retain(scope: &mut v8::PinScope, key: EtKey, ty: &str, keep: impl Fn(&EtListener) -> bool) {
    let emptied = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let s = st.borrow_mut::<EventListeners>();
        match s.map.get_mut(&key).and_then(|t| t.get_mut(ty)) {
            Some(list) if !list.is_empty() => {
                list.retain(|e| {
                    let k = keep(e);
                    if !k {
                        e.removed.set(true);
                    }
                    k
                });
                list.is_empty()
            }
            _ => false,
        }
    };
    // Blink's `LocalDOMWindow::RemovedEventListener` observers: a device sensor controller stops with its type's
    // last listener (`core/html/device_sensors.rs`).
    if emptied {
        crate::core::html::device_sensors::did_remove_last_listener(scope, key, ty);
    }
}
/// Has arena node `id` a listener for `ty` -- one registered (only a CAPTURING one when `capture_only`), or an
/// `on<type>` content attribute not compiled yet? Blink's `HasEventListeners` / `HasCapturingEventListeners`, which
/// decide whether a `mouseenter` / `mouseleave` / `pointerenter` / `pointerleave` is dispatched at all (see
/// `core/events/pointer_input.rs`). The page document's arena root answers for the document.
pub(crate) fn node_has_listener(scope: &mut v8::PinScope, id: u32, ty: &str, capture_only: bool) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = st.try_borrow::<crate::binding::host_dom::HostDom>().map(|h| &h.0);
    let key = if dom.is_some_and(|d| d.root() == id) { EtKey::Document } else { EtKey::Node(id) };
    let registered = st
        .try_borrow::<EventListeners>()
        .and_then(|l| l.map.get(&key))
        .and_then(|t| t.get(ty))
        .is_some_and(|l| l.iter().any(|e| !capture_only || e.capture));
    if registered || capture_only {
        return registered;
    }
    dom.is_some_and(|d| d.node_type(id) == 1 && d.get_attribute(id, &format!("on{ty}")).is_some())
}

/// The nodes with a listener of their own for any of `types` -- registered or set through an `on<type>` IDL
/// attribute, on any node; or an `on<type>` content attribute not compiled yet, on an element of `among`: what
/// [`node_has_listener`] answers for one node, for a whole document in one pass (the accessibility tree's clickable
/// elements).
pub(crate) fn nodes_listening(scope: &mut v8::PinScope, types: &[&str], among: &[u32]) -> std::collections::HashSet<u32> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let mut out = std::collections::HashSet::new();
    if let Some(l) = st.try_borrow::<EventListeners>() {
        for (key, by_type) in &l.map {
            if let EtKey::Node(id) = key {
                if types.iter().any(|t| by_type.get(*t).is_some_and(|l| !l.is_empty())) {
                    out.insert(*id);
                }
            }
        }
    }
    if let Some(dom) = st.try_borrow::<crate::binding::host_dom::HostDom>().map(|h| &h.0) {
        let attrs: Vec<String> = types.iter().map(|t| format!("on{t}")).collect();
        out.extend(among.iter().copied().filter(|&id| dom.node_type(id) == 1 && attrs.iter().any(|a| dom.get_attribute(id, a).is_some())));
    }
    out
}

/// Is a listener for `ty` registered on any target at all -- Blink's `EventHandlerRegistry` question for
/// `pointerrawupdate`, which is dispatched only when some listener wants it.
pub(crate) fn any_listener(scope: &mut v8::PinScope, ty: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<EventListeners>().is_some_and(|l| l.map.values().any(|t| t.get(ty).is_some_and(|v| !v.is_empty())))
}

/// Is a NON-passive listener for any of `types` registered anywhere? That is what makes Chrome's compositor hand
/// a wheel to the page as a blocking -- cancelable -- event (`WebMouseWheelEvent::dispatch_type`); with only
/// passive listeners (the default on the window, the document and the body) `cancelable` is false (measured,
/// `tests/fixtures/mouse-input`).
pub(crate) fn any_blocking_listener(scope: &mut v8::PinScope, types: &[&str]) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<EventListeners>().is_some_and(|l| {
        l.map.values().any(|t| types.iter().any(|ty| t.get(*ty).is_some_and(|v| v.iter().any(|e| !e.passive))))
    })
}


/// Register a `when()` subscription on `target` (an `Observer` entry, at the end of the list).
pub(crate) fn et_add_observer(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, ty: &str, subscriber: v8::Local<v8::Object>, capture: bool, passive: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(key) = et_key_add(scope, target, &op_state) else { return };
    let sub = v8::Global::new(scope, subscriber);
    et_push(scope, key, ty, EtKind::Observer(sub), capture, false, passive);
}
/// Remove a `when()` subscription (the subscription's teardown).
pub(crate) fn et_remove_observer(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, ty: &str, subscriber: v8::Local<v8::Object>, capture: bool) {
    let Some(key) = et_key(scope, target) else { return };
    let sub = v8::Global::new(scope, subscriber);
    et_retain(scope, key, ty, |e| !(matches!(&e.kind, EtKind::Observer(s) if *s == sub) && e.capture == capture));
}
/// Register the abort hook of `subscribe(_, {signal})` on `signal` (a `SubscriberAbort` entry).
pub(crate) fn et_add_subscriber_abort(scope: &mut v8::PinScope, signal: v8::Local<v8::Value>, subscriber: v8::Local<v8::Object>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(key) = et_key_add(scope, signal, &op_state) else { return };
    let sub = v8::Global::new(scope, subscriber);
    et_push(scope, key, "abort", EtKind::SubscriberAbort(sub), false, true, false);
}

fn cb_et_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // `EventListener? callback` is nullable, not optional: two arguments are REQUIRED, and a
    // one-argument call is a TypeError before the type string is even read.
    if !require_args(scope, &args, 2, "EventTarget", "addEventListener") { return; }
    // The three arguments are converted before any step runs: a `null` listener's options are read too.
    let mut tbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let Some(ty) = listener_args(scope, &args, "addEventListener", &mut tbuf) else { return };
    let Ok(o) = listener_options(scope, args.get(2), "addEventListener") else { return };
    // DOM "add an event listener": an aborted signal adds nothing; neither does a null callback
    if o.signal.is_some_and(|s| crate::core::dom::abort_controller::aborted_reason(scope, s).is_some()) { return; }
    let cb = args.get(1);
    if cb.is_null_or_undefined() { return; }
    let this: v8::Local<v8::Value> = args.this().into();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let key = match et_key_add(scope, this, &op_state) { Some(k) => k, None => return };
    let passive = match o.passive {
        Some(p) => p,
        None => default_passive(scope, &key, this, &ty),
    };
    let cbg = v8::Global::new(scope, cb);
    // A signal removes the listener when it aborts -- as Blink, only a listener this call added (an equal one
    // already listed keeps its own life).
    if et_push(scope, key, &ty, EtKind::Callback(cbg), o.capture, o.once, passive) {
        // `LocalDOMWindow::AddedEventListener`'s observers -- only for a listener this call added.
        crate::core::html::device_sensors::did_add_listener(scope, key, this, &ty);
        if let Some(sig) = o.signal {
            let data = v8::Array::new(scope, 4);
            let tyv = str_val(scope, &ty);
            let cap: v8::Local<v8::Value> = v8::Boolean::new(scope, o.capture).into();
            for (i, v) in [this, tyv, cb, cap].into_iter().enumerate() {
                data.set_index(scope, i as u32, v);
            }
            let remove = build_fn_data(scope, cb_et_signal_remove, "", 0, data.into());
            crate::core::dom::abort_controller::add_abort_algorithm(scope, sig, remove);
        }
    }
}

/// The abort algorithm a `{signal}` listener leaves on its signal (DOM "remove an event listener"): data =
/// `[target, type, callback, capture]`. The engine's own removal, never the page-visible `removeEventListener`.
fn cb_et_signal_remove(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let (Some(target), Some(ty), Some(cb), Some(cap)) = (d.get_index(scope, 0), d.get_index(scope, 1), d.get_index(scope, 2), d.get_index(scope, 3)) else { return };
    let ty = ty.to_rust_string_lossy(scope);
    let capture = cap.is_true();
    let Some(key) = et_key(scope, target) else { return };
    let cbg = v8::Global::new(scope, cb);
    et_retain(scope, key, &ty, |e| !(matches!(&e.kind, EtKind::Callback(c) if *c == cbg) && e.capture == capture));
}
#[op2] pub(crate) fn op_et_add<'s>(scope: &mut v8::PinScope<'s, '_>, #[string] fname: &str, #[smi] length: i32) -> v8::Local<'s, v8::Function> { build_fn(scope, cb_et_add, fname, length) }
fn cb_et_remove(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // `EventListener? callback` is nullable, not optional: two arguments are REQUIRED, and a
    // one-argument call is a TypeError before the type string is even read.
    if !require_args(scope, &args, 2, "EventTarget", "removeEventListener") { return; }
    let mut tbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let Some(ty) = listener_args(scope, &args, "removeEventListener", &mut tbuf) else { return };
    let Ok(o) = listener_options(scope, args.get(2), "removeEventListener") else { return };
    let capture = o.capture;
    let this: v8::Local<v8::Value> = args.this().into();
    let cb = args.get(1);
    let key = match et_key(scope, this) { Some(k) => k, None => return };
    et_retain(scope, key, &ty, |e| !(matches!(&e.kind, EtKind::Callback(c) if *c == cb) && e.capture == capture));
}
#[op2] pub(crate) fn op_et_remove<'s>(scope: &mut v8::PinScope<'s, '_>, #[string] fname: &str, #[smi] length: i32) -> v8::Local<'s, v8::Function> { build_fn(scope, cb_et_remove, fname, length) }

// ── the event handler store ────────────────────────────────────────────────────────────────────

/// The `on<type>` content attribute of a node target, if it has one (only an element can).
/// The (key, type) list in one borrow: whether it holds a markup handler's entry, and a
/// snapshot of every entry (its `Rc`s). A dispatch used to ask `et_has`, deleted with the entries' removed flag,
/// whether each snapshotted listener was still registered.
fn et_listeners(scope: &mut v8::PinScope, key: EtKey, ty: &str) -> (bool, Vec<EtEntry>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    match st.borrow::<EventListeners>().map.get(&key).and_then(|t| t.get(ty)) {
        Some(l) => (l.iter().any(|e| matches!(e.kind, EtKind::Handler)), l.clone()),
        None => (false, Vec::new()),
    }
}
/// Is `name` (`on<type>`) an EVENT HANDLER of the element `node` -- a member its interfaces define, on
/// its prototype chain? Only those content attributes are handlers: Chrome 150 runs a
/// `<div onclick>` and an `onbeforecopy` / `onanimationend`, and never an `ont7`, an `onfoo` or a
/// `<div onmessage>` (a WINDOW handler, which only `<body>` forwards). Every `on<anything>` attribute
/// used to run.
fn has_handler_member(scope: &mut v8::PinScope, node: v8::Local<v8::Value>, name: &str) -> bool {
    let Ok(o) = v8::Local::<v8::Object>::try_from(node) else { return false };
    let Some(key) = v8::String::new(scope, name) else { return false };
    let mut proto = o.get_prototype(scope);
    while let Some(p) = proto.and_then(|p| v8::Local::<v8::Object>::try_from(p).ok()) {
        if p.has_own_property(scope, key.into()).unwrap_or(false) {
            return true;
        }
        proto = p.get_prototype(scope);
    }
    false
}

/// [`node_attr`] with the attribute's name (`on<type>`) already made.
fn node_attr_named(scope: &mut v8::PinScope, key: EtKey, name: &str) -> Option<String> {
    let EtKey::Node(id) = key else { return None };
    if id == crate::core::events::shared::ON_DOCUMENT_ID { return None; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = st.try_borrow::<crate::binding::host_dom::HostDom>()?;
    dom.0.get_attribute(id, name).map(str::to_string)
}
fn node_attr(scope: &mut v8::PinScope, key: EtKey, ty: &str) -> Option<String> {
    let EtKey::Node(id) = key else { return None };
    if id == crate::core::events::shared::ON_DOCUMENT_ID { return None; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = st.try_borrow::<crate::binding::host_dom::HostDom>()?;
    dom.0.get_attribute(id, &format!("on{ty}")).map(str::to_string)
}
/// The compiled content-attribute handler (HTML "getting the current value of the event handler", step 3), named
/// after its attribute (`el.onclick.name === "onclick"`), or None on a syntax error (a content attribute that does
/// not parse is no handler at all).
///
/// The attribute's text is the BODY of a function with `params`, compiled by V8's `CompileFunction` with the scope
/// HTML gives it as context extensions -- the element's node document, then its form owner, then the element itself,
/// innermost last -- which is what Blink's `JSEventHandlerForContentAttribute::GetCompiledHandler` does. Measured on
/// Chrome 150 (`<button onclick>` in a form): `q.value` reads the form's field, `typeof submit` is the form's method,
/// `typeof URL` is `document.URL`'s `"string"`; `toString()` is `function onclick(event) {\n…\n}`; and a body that
/// closes the function early (`}); x(); (function(){`) is a SyntaxError -- null handler, `x()` never runs.
///
/// It used to be pasted into `(function anonymous(event\n) {\n…\n})` and RUN as a script: the body saw only the
/// global scope, `toString()` answered `function anonymous(event\n) {…}` -- a one-line tell no Chrome gives -- and
/// the early-closing body above ran `x()` at compile time.
pub(crate) fn compile_content_handler<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    realm: v8::Local<'s, v8::Context>,
    element: Option<u32>,
    ty: &str,
    params: &[&str],
    code: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let compiled = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let mut scopes: Vec<v8::Local<v8::Object>> = Vec::with_capacity(3);
        if let Some(id) = element {
            let el = v8::Local::<v8::Object>::try_from(crate::page::node_object(scope, id)).ok()?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let document = crate::core::dom::node::node_document(scope, &op_state, id).or_else(|| crate::binding::protos::global_get(scope, "document"));
            if let Some(d) = document.and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) {
                scopes.push(d);
            }
            // Only a form-associated element HAS a form owner (Blink: `HTMLElement::formOwner()` is null for the
            // rest), so a `<div onclick>` inside a form does not see the form's members.
            let (html, custom) = {
                let st = op_state.borrow();
                let dom = &st.borrow::<crate::binding::host_dom::HostDom>().0;
                (dom.is_html_ns(id), dom.is_form_associated_custom(id))
            };
            let listed = html
                && crate::page::tag_name_of(scope, el.into())
                    .is_some_and(|t| matches!(t.as_str(), "BUTTON" | "FIELDSET" | "INPUT" | "OBJECT" | "OUTPUT" | "SELECT" | "TEXTAREA" | "IMG"));
            if html && (custom || listed) {
                if let Some(f) = crate::core::html::html_input_element::form_owner_of(scope, el).and_then(|f| v8::Local::<v8::Object>::try_from(f).ok()) {
                    scopes.push(f);
                }
            }
            scopes.push(el);
        }
        let params: Vec<v8::Local<v8::String>> = params.iter().map(|p| v8::String::new(scope, p)).collect::<Option<_>>()?;
        let mut source = v8::script_compiler::Source::new(v8::String::new(scope, code)?, None);
        v8::tc_scope!(let tc, scope);
        let r = v8::script_compiler::compile_function(
            tc,
            &mut source,
            &params,
            &scopes,
            v8::script_compiler::CompileOptions::NoCompileOptions,
            v8::script_compiler::NoCacheReason::NoReason,
        )
        .map(|f| v8::Global::new(tc, f));
        tc.reset();
        r
    };
    let f = v8::Local::new(scope, &compiled?);
    if let Some(n) = v8::String::new(scope, &format!("on{ty}")) {
        f.set_name(n);
    }
    Some(f.into())
}

fn compile_handler<'s>(scope: &mut v8::PinScope<'s, '_>, key: EtKey, ty: &str, code: &str) -> Option<v8::Local<'s, v8::Value>> {
    // A sandboxed frame document's content attributes compile no handler: scripting is disabled there (HTML "getting
    // the current value of the event handler"). Chrome 150: `<img onerror>` and `<body onload>` in a `sandbox=""`
    // frame run nothing.
    if let EtKey::Node(id) = key {
        if let crate::core::html::subresource::Home::Frame(ctx) = crate::core::html::subresource::home_of(scope, id) {
            if crate::core::html::browsing_context::scripts_sandboxed(scope, ctx) {
                return None;
            }
        }
    }
    // An element of a FRAME's document compiles its handler in the frame's realm, where the names
    // its code uses are.
    let (realm, element) = match key {
        EtKey::Node(id) => (crate::core::html::browsing_context::realm_of_element(scope, id), Some(id)),
        _ => (None, None),
    };
    let realm = match realm {
        Some(r) => r,
        None => scope.get_current_context(),
    };
    compile_content_handler(scope, realm, element, ty, &["event"], code)
}
/// Put the event handler listener in the (key, type) list unless it is there ("activate an event
/// handler": the listener is registered when the handler first becomes non-null, at the end of the
/// list, which is what keeps its position among `addEventListener` listeners).
fn handler_activate(scope: &mut v8::PinScope, key: EtKey, ty: &str) -> bool {
    et_push(scope, key, ty, EtKind::Handler, false, false, false)
}
/// "deactivate an event handler": the value is gone, and so is its listener.
fn handler_deactivate(scope: &mut v8::PinScope, key: EtKey, ty: &str) {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(m) = st.borrow_mut::<EventListeners>().handlers.get_mut(&key) { m.remove(ty); }
    }
    et_retain(scope, key, ty, |e| !matches!(e.kind, EtKind::Handler));
}
fn handler_store(scope: &mut v8::PinScope, key: EtKey, ty: &str, v: HandlerValue) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    st.borrow_mut::<EventListeners>().handlers.entry(key).or_default().insert(ty.to_string(), v);
}
/// "Get the current value of the event handler": the IDL-set object, or the content attribute
/// compiled once (recompiled if its text changed; deactivated if it is gone). Activates the handler
/// listener when a content attribute is compiled for the first time.
pub(crate) fn handler_current<'s>(scope: &mut v8::PinScope<'s, '_>, key: EtKey, ty: &str) -> Option<v8::Local<'s, v8::Value>> {
    let cur: Option<(v8::Global<v8::Value>, Option<String>)> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<EventListeners>().handlers.get(&key).and_then(|m| m.get(ty)).map(|v| match v {
            HandlerValue::Idl(g) => (g.clone(), None),
            HandlerValue::Attr { compiled, source } => (compiled.clone(), Some(source.clone())),
        })
    };
    match cur {
        Some((g, None)) => Some(v8::Local::new(scope, &g)),
        Some((g, Some(source))) => match node_attr(scope, key, ty) {
            Some(code) if code == source => Some(v8::Local::new(scope, &g)),
            Some(code) => compile_and_store(scope, key, ty, &code),
            None => { handler_deactivate(scope, key, ty); None }
        },
        None => {
            let code = node_attr(scope, key, ty)?;
            handler_activate(scope, key, ty);
            compile_and_store(scope, key, ty, &code)
        }
    }
}
fn compile_and_store<'s>(scope: &mut v8::PinScope<'s, '_>, key: EtKey, ty: &str, code: &str) -> Option<v8::Local<'s, v8::Value>> {
    match compile_handler(scope, key, ty, code) {
        Some(f) => {
            let g = v8::Global::new(scope, f);
            handler_store(scope, key, ty, HandlerValue::Attr { compiled: g, source: code.to_string() });
            Some(f)
        }
        None => { handler_deactivate(scope, key, ty); None }
    }
}

// ── the listener call ──────────────────────────────────────────────────────────────────────────

#[cfg(feature = "analysis")]
thread_local! {
    /// set by [`fire_handler`] for the [`call_listener`] it makes: the recorder labels that call a handler's
    static HANDLER_CALL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Call `f` with `this` and `args` as a listener: the event is the current event for the duration,
/// and a throw is reported (`report_exception`), not propagated -- the next listener still runs.
fn call_listener<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<v8::Function>, this: v8::Local<v8::Value>, ev: v8::Local<v8::Value>, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    // A listener an ISOLATED WORLD added sees the event as that world's object of its interface, as Blink hands each
    // listener its own world's wrapper: `event instanceof TouchEvent` in Playwright's utility world -- its tap
    // check -- read false for the page's event, and the check aimed at an undefined point. The event's state is in
    // its private slots, which every realm's getters read, so it is the prototype that moves, for the call.
    let reworlded = world_prototype(scope, f, ev);
    let ret = call_listener_as(scope, f, this, ev, args);
    if let Some((o, page_proto)) = reworlded {
        let o = v8::Local::new(scope, &o);
        let p = v8::Local::new(scope, &page_proto);
        o.set_prototype(scope, p.into());
    }
    ret
}

/// When `f` is an isolated world's function and `ev` an event of the page's: `ev` put on the world's prototype of its
/// interface, and what to put back.
fn world_prototype(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, ev: v8::Local<v8::Value>) -> Option<(v8::Global<v8::Object>, v8::Global<v8::Value>)> {
    let o = v8::Local::<v8::Object>::try_from(ev).ok()?;
    let ctx = f.get_creation_context(scope)?;
    crate::core::html::isolated_world::isolated_world_of(scope, ctx)?;
    let proto = o.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())?;
    let there = crate::binding::protos::proto_in_realm(scope, ctx, proto);
    if there == proto {
        return None;
    }
    o.set_prototype(scope, there.into());
    Some((v8::Global::new(scope, o), v8::Global::new(scope, v8::Local::<v8::Value>::from(proto))))
}

fn call_listener_as<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<v8::Function>, this: v8::Local<v8::Value>, ev: v8::Local<v8::Value>, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    let saved = current_event_enter(scope, f, this, ev);
    // The listener runs as its own realm: a frame's listener that calls a bare
    // `addEventListener`, or throws, belongs to the frame.
    let ret = crate::core::html::browsing_context::as_callback_realm(scope, f, |scope| {
        let (ret, exc) = {
            v8::tc_scope!(let tc, scope);
            // The engine calling into the page, labelled as Chromium's
            // CallbackInvokeHelper labels it -- a handler attribute's `EventHandlerNonNull`, else `EventListener`
            #[cfg(feature = "analysis")]
            let kseq = {
                let label = if HANDLER_CALL.with(|h| h.replace(false)) { "EventHandlerNonNull" } else { "EventListener" };
                crate::analysis_hook::callback_begin(tc, label, f, this, args)
            };
            let r = {
                let _below = crate::core::html::task_queue::ScriptBelow::enter();
                f.call(tc, this, args).map(|v| v8::Global::new(tc, v))
            };
            #[cfg(feature = "analysis")]
            {
                let rl = if tc.has_caught() { None } else { r.as_ref().map(|g| v8::Local::new(tc, g)) };
                crate::analysis_hook::callback_end(tc, kseq, rl);
            }
            let exc = if tc.has_caught() {
                tc.exception().map(|e| {
                    let m = tc.message();
                    (v8::Global::new(tc, e), crate::core::events::report::caught_site(tc, e, m))
                })
            } else {
                None
            };
            tc.reset();
            (r, exc)
        };
        // Blink's order (`JSBasedEventListener::Invoke`): a throw is reported as it leaves the listener (a
        // verbose TryCatch), the listener's microtasks run when its call returns to an empty stack
        // (`V8ScriptRunner::CallFunction`'s MicrotasksScope -- HTML "clean up after running a callback"), and
        // only then is `window.event` put back. With a script on the stack (`dispatchEvent`, `click()` from
        // script) there is no checkpoint, and inside a promise job V8 makes it a no-op.
        if let Some((e, site)) = exc {
            let e = v8::Local::new(scope, &e);
            crate::core::events::report::report_exception_at(scope, e, site);
        }
        crate::core::html::task_queue::clean_up_after_running_callback(scope);
        current_event_leave(scope, &saved);
        ret
    });
    ret.map(|g| v8::Local::new(scope, &g))
}
/// Is `ev` an `ErrorEvent` -- of any realm (its brand): a frame's error event is made by the frame's
/// constructor, which is not `instanceof` the page's.
fn is_error_event(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) -> bool {
    crate::binding::protos::implements(scope, ev.into(), "ErrorEvent")
}
/// Is the target a `Window`: the page's, or a frame's (which is keyed as an object).
fn is_window_target(scope: &mut v8::PinScope, key: EtKey, node: v8::Local<v8::Value>) -> bool {
    matches!(key, EtKey::Window)
        || v8::Local::<v8::Object>::try_from(node).is_ok_and(|o| crate::core::html::browsing_context::ctx_of_window(scope, o).is_some())
}
/// The event handler processing algorithm (HTML 8.1.8.1) for the handler registered at (key, type).
fn fire_handler<'s>(scope: &mut v8::PinScope<'s, '_>, key: EtKey, node: v8::Local<'s, v8::Value>, ev: v8::Local<'s, v8::Object>, ty: &str) {
    let Some(h) = handler_current(scope, key, ty) else { return };
    let Ok(f) = v8::Local::<v8::Function>::try_from(h) else { return }; // a non-callable object is a legal value that never runs
    let evv: v8::Local<v8::Value> = ev.into();
    // "special error event handling": a WINDOW's `onerror` for an ErrorEvent is called with five
    // arguments and cancels by returning TRUE; every other handler gets the event and cancels with false.
    // Any window's: a frame's `onerror` (and its `<body onerror>`) was called with the event alone
    // (stock Chrome 150 passes the five in a cross-site frame as on the page).
    let special = ty == "error" && is_window_target(scope, key, node) && is_error_event(scope, ev);
    #[cfg(feature = "analysis")]
    HANDLER_CALL.with(|h| h.set(true));
    let ret = if special {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        let mut a: Vec<v8::Local<v8::Value>> = Vec::with_capacity(5);
        for k in ["message", "filename", "lineno", "colno", "error"] { a.push(get_prop(scope, ev, k).unwrap_or(undef)); }
        call_listener(scope, f, node, evv, &a)
    } else {
        call_listener(scope, f, node, evv, &[evv])
    };
    let Some(r) = ret else { return };
    let cancels = if special { r.is_true() } else { r.is_false() };
    if cancels && evt_flag_get(scope, ev, "cancelable") { evt_flag_set(scope, ev, "defaultPrevented", true); }
}
fn invoke<'s>(scope: &mut v8::PinScope<'s, '_>, kind: &EtKind, key: EtKey, node: v8::Local<'s, v8::Value>, ev: v8::Local<'s, v8::Object>, ty: &str) {
    let evv: v8::Local<v8::Value> = ev.into();
    match kind {
        EtKind::Callback(g) => {
            let cb = v8::Local::new(scope, g);
            if let Ok(f) = v8::Local::<v8::Function>::try_from(cb) {
                call_listener(scope, f, node, evv, &[evv]);
            } else if let Ok(o) = v8::Local::<v8::Object>::try_from(cb) {
                // The object listener: `handleEvent` is looked up at call time, `this` is the listener.
                if let Some(f) = get_prop(scope, o, "handleEvent").and_then(|h| v8::Local::<v8::Function>::try_from(h).ok()) {
                    call_listener(scope, f, cb, evv, &[evv]);
                }
            }
        }
        EtKind::Handler => fire_handler(scope, key, node, ev, ty),
        EtKind::Observer(sub) => {
            let sub = v8::Local::new(scope, sub);
            crate::core::events::observable::subscriber_next(scope, sub, evv);
        }
        EtKind::SubscriberAbort(sub) => {
            let sub = v8::Local::new(scope, sub);
            crate::core::events::observable::subscriber_close(scope, sub);
        }
    }
}

/// The propagation path: DOM's "get the parent" applied from the target until it answers null --
/// [target, ...ancestors, the tree's document, that document's window] (window-only when the target
/// is the window).
///
/// THE TAIL IS THE TREE'S OWN, NOT THE PAGE'S. The path used to end in the PAGE's
/// `document` and `window` whatever tree the target was in. DOM says a document's parent is its
/// relevant global only when it HAS a browsing context, and a node whose root is not a document has
/// no parent at all. Measured on dev.to (replay): once frames fired their own `DOMContentLoaded`,
/// a frame document's DCL bubbled into the page's `document` listeners, and dev.to's
/// `document.addEventListener('DOMContentLoaded', initAuth)` -- which appends a hidden iframe -- ran
/// again after every frame load: 1,715 frame navigations in 30 s, and the page never completed.
///
///   root of the target's tree        the path continues with
///   the page's document              the page's window
///   a frame's current document       THAT frame's window
///   any other document               nothing (DOMParser, createHTMLDocument, template contents)
///   a detached subtree's root        nothing
///   a shadow root                    its HOST -- for a composed event, or one from another tree (below)
///
/// THE SHADOW ROWS ARE DOM'S NOW, as Blink 150 walks them (`EventPath::CalculatePath`, `GetShadowRootParent`
/// in `core/dom/events/event_path.cc`), FIXER C6 2026-10-11. A node a slot is assigned to goes on to that slot;
/// a shadow root's parent is its host, except that a NOT-composed event stops at the shadow root of its own
/// target's tree. Every step carries the target and the related target as its node sees them (DOM "retarget",
/// Blink's `TreeScopeEventContext`), and a step where the two meet outside their own trees ends the path
/// (`EventPath::ShrinkForRelatedTarget`). The ruling this replaces kept a shadow root's tail at its host's
/// document and window and skipped the host, because no engine-fired event was composed; trusted mouse,
/// pointer, wheel, focus and key events are now built `composed` as Blink builds them. Measured on stock Chrome
/// 150 with real X input (`tests/fixtures/mouse-input`): a click in a CLOSED shadow root reaches a window
/// listener with `target === host` and `composedPath()` = `host > body > html > document > window`, and a
/// listener on the shadow root with the inner element as target and the whole path.
pub(crate) struct PathStep<'s> {
    /// The node (or window) whose listeners run at this step.
    node: v8::Local<'s, v8::Value>,
    /// The event's target as this step sees it.
    target: v8::Local<'s, v8::Value>,
    /// The event's related target as this step sees it (`None`: the event has none).
    related: Option<v8::Local<'s, v8::Value>>,
    /// The arena root of the tree the step's node is in (a document or a shadow root); a window shares its
    /// document's. `None` for a target that is no node. `composedPath()` decides what a listener sees from it.
    tree: Option<u32>,
}

/// The slot `id` is assigned to -- only asked of a child of a shadow host, as Blink's `IsChildOfShadowHost`.
fn assigned_slot_of(dom: &korvus_dom::Dom, id: u32) -> Option<u32> {
    let parent = dom.parent(id)?;
    dom.shadow_root_of(parent)?;
    dom.assigned_slot(id)
}

/// The node's parent across a shadow root: its host. `None` above a document or a detached root.
fn shadow_including_parent(dom: &korvus_dom::Dom, id: u32) -> Option<u32> {
    match dom.parent(id) {
        Some(p) => Some(p),
        None if dom.is_shadow_root(id) => dom.shadow_host_of(id),
        None => None,
    }
}

/// Is `ancestor` a shadow-including inclusive ancestor of `node`?
fn shadow_including_inclusive_ancestor(dom: &korvus_dom::Dom, ancestor: u32, mut node: u32) -> bool {
    for _ in 0..dom.len().max(1) {
        if node == ancestor {
            return true;
        }
        match shadow_including_parent(dom, node) {
            Some(p) => node = p,
            None => return false,
        }
    }
    false
}

/// DOM "retarget A against B": while A's root is a shadow root that is not a shadow-including inclusive
/// ancestor of B, A becomes that root's host.
pub(crate) fn retarget(dom: &korvus_dom::Dom, mut a: u32, b: u32) -> u32 {
    for _ in 0..256 {
        let root = dom.root_of(a);
        if !dom.is_shadow_root(root) || shadow_including_inclusive_ancestor(dom, root, b) {
            return a;
        }
        match dom.shadow_host_of(root) {
            Some(host) => a = host,
            None => return a,
        }
    }
    a
}

/// The tree a TREE (an arena root) hangs from: a shadow root's host's tree, `None` for a document's.
fn parent_tree(dom: &korvus_dom::Dom, tree: u32) -> Option<u32> {
    if !dom.is_shadow_root(tree) {
        return None;
    }
    dom.shadow_host_of(tree).map(|h| dom.root_of(h))
}

/// Is tree `a` an inclusive ancestor of tree `b` in the tree of trees?
fn tree_inclusive_ancestor(dom: &korvus_dom::Dom, a: u32, mut b: u32) -> bool {
    for _ in 0..256 {
        if a == b {
            return true;
        }
        match parent_tree(dom, b) {
            Some(p) => b = p,
            None => return false,
        }
    }
    false
}

/// The nearest CLOSED shadow root among `tree` and the trees it hangs from (Blink's
/// `ContainingClosedShadowTree`).
fn containing_closed_tree(dom: &korvus_dom::Dom, mut tree: u32) -> Option<u32> {
    for _ in 0..256 {
        if dom.shadow_mode(tree) == Some(korvus_dom::ShadowRootMode::Closed) {
            return Some(tree);
        }
        tree = parent_tree(dom, tree)?;
    }
    None
}

/// Blink's `TreeScopeEventContext::IsUnclosedTreeOf`: may a listener whose current target is in tree
/// `listener` see a path node in tree `node`? Its own and every enclosing tree, yes; a tree with no closed
/// shadow root above it, yes; a tree below the listener's, unless a closed shadow root lies in between.
pub(crate) fn tree_visible_from(dom: &korvus_dom::Dom, node: u32, listener: u32) -> bool {
    if tree_inclusive_ancestor(dom, node, listener) {
        return true;
    }
    let Some(closed) = containing_closed_tree(dom, node) else { return true };
    if tree_inclusive_ancestor(dom, listener, node) {
        // `node` is strictly below `listener`: hidden when the closed tree is strictly below it too.
        return !(closed != listener && tree_inclusive_ancestor(dom, listener, closed));
    }
    false
}

/// The wrapper of arena node `id`, made once per path (the target's own value is the first entry).
fn wrap_id<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, cache: &mut Vec<(u32, v8::Local<'s, v8::Value>)>) -> v8::Local<'s, v8::Value> {
    if let Some((_, v)) = cache.iter().find(|(i, _)| *i == id) {
        return *v;
    }
    let v = crate::page::node_object(scope, id);
    cache.push((id, v));
    v
}

fn et_path<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Value>,
    ev_type: &str,
    composed: bool,
    related: Option<v8::Local<'s, v8::Value>>,
) -> Vec<PathStep<'s>> {
    let alone = |node: v8::Local<'s, v8::Value>| vec![PathStep { node, target, related, tree: None }];
    // The PAGE's window, whichever realm dispatches (a frame's own `dispatchEvent` runs in the
    // frame's realm); a frame document's window is found below.
    let global: v8::Local<v8::Value> = crate::core::html::browsing_context::main_global(scope).into();
    // A frame's window is the end of its own path: it has no parent node to walk to.
    if v8::Local::<v8::Object>::try_from(target).ok().is_some_and(|o| crate::core::html::browsing_context::ctx_of_window(scope, o).is_some()) {
        return alone(target);
    }
    if target.strict_equals(global) {
        return alone(target);
    }
    let Some(raw) = arg_node_id(scope, target) else { return alone(target) };
    let related_raw = related.and_then(|r| arg_node_id(scope, r));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The walk, over the ARENA: every event the engine dispatched read the
    // page-visible `nodeType` / `host` / `ownerDocument` getters of its path when this went through V8.
    struct Walk {
        ids: Vec<u32>,
        targets: Vec<u32>,
        relateds: Vec<Option<u32>>,
        trees: Vec<u32>,
        /// the document at the top of the path, when the path reached one
        document: Option<u32>,
        page_root: u32,
    }
    let walk = {
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<crate::binding::host_dom::HostDom>() else { return alone(target) };
        let dom = &h.0;
        let resolve = |id: u32| if id == crate::core::events::shared::ON_DOCUMENT_ID { dom.root() } else { id };
        let start = resolve(raw);
        let own_tree = dom.root_of(start);
        let mut ids = vec![start];
        let mut cur = start;
        for _ in 0..4096 {
            let next = if let Some(slot) = assigned_slot_of(dom, cur) {
                Some(slot)
            } else if dom.is_shadow_root(cur) {
                if !composed && cur == own_tree { None } else { dom.shadow_host_of(cur) }
            } else {
                dom.parent(cur)
            };
            match next {
                Some(n) => {
                    ids.push(n);
                    cur = n;
                }
                None => break,
            }
        }
        let shadows = dom.has_shadow_roots();
        let targets: Vec<u32> = ids.iter().map(|&n| if shadows { retarget(dom, start, n) } else { start }).collect();
        // Blink adjusts the related target only inside the target's own document (`AdjustForRelatedTarget`):
        // one from another document -- the frame a pointer left -- is handed over as it is.
        let related_id = related_raw.map(resolve);
        let same_doc = related_id.is_some_and(|r| dom.shadow_including_root(r) == dom.shadow_including_root(start));
        let mut relateds: Vec<Option<u32>> =
            ids.iter().map(|&n| related_id.filter(|_| same_doc).map(|r| if shadows { retarget(dom, r, n) } else { r })).collect();
        let trees: Vec<u32> = ids.iter().map(|&n| dom.root_of(n)).collect();
        let mut ids = ids;
        let mut targets = targets;
        let mut trees = trees;
        if let (Some(r), true) = (related_id, same_doc && shadows) {
            // ShrinkForRelatedTarget: a step where target and related target are one node ends the path --
            // unless both are still the event's own, in their own trees.
            let r_tree = dom.root_of(r);
            if let Some(cut) = (0..ids.len()).find(|&i| {
                relateds[i] == Some(targets[i]) && !(dom.root_of(targets[i]) == own_tree && dom.root_of(targets[i]) == r_tree)
            }) {
                ids.truncate(cut);
                targets.truncate(cut);
                relateds.truncate(cut);
                trees.truncate(cut);
            }
        }
        let document = ids.last().copied().filter(|&top| dom.node_type(top) == 9 && !dom.is_shadow_root(top));
        Walk { ids, targets, relateds, trees, document, page_root: dom.root() }
    };
    // Shrunk to nothing: the target and the related target are one node from the target's own tree on, and
    // no listener sees the event (Blink `EventPath::Shrink(0)`).
    if walk.ids.is_empty() {
        return Vec::new();
    }
    let mut path: Vec<PathStep<'s>> = Vec::with_capacity(walk.ids.len() + 1);
    let mut cache: Vec<(u32, v8::Local<'s, v8::Value>)> = vec![(walk.ids[0], target)];
    for i in 0..walk.ids.len() {
        let node = wrap_id(scope, walk.ids[i], &mut cache);
        let t = wrap_id(scope, walk.targets[i], &mut cache);
        let rel = match (related, walk.relateds[i]) {
            (Some(_), Some(r)) => Some(wrap_id(scope, r, &mut cache)),
            (r, _) => r,
        };
        path.push(PathStep { node, target: t, related: rel, tree: Some(walk.trees[i]) });
    }
    // A DOCUMENT'S EVENT-PARENT IS NULL FOR `load`, so the path STOPS at the document and the
    // Window is never in it (DOM §2.9, "get the parent": «A document's get the parent algorithm,
    // given an event, returns null if event's type attribute value is "load"»). A `load` fired at
    // an element therefore never reaches a Window listener, not even a CAPTURING one.
    //
    // MEASURED (Chrome 150.0.7871.46 headless, loopback): a page with three `<img>`
    // and two `<link rel=stylesheet>` and `addEventListener("load", f, true)` on the WINDOW calls
    // `f` exactly ONCE, with `event.target === window` and `readyState === "complete"`. Korvus
    // called it SEVEN times -- five element loads plus two window loads.
    //
    // WHY THIS IS NOT COSMETIC. `web-vitals` -- which ships inside Next.js, Vercel Analytics and
    // GA4, i.e. a large fraction of the web -- is literally this:
    //
    //     o4 = e => document.readyState !== "complete"
    //             ? addEventListener("load", () => o4(e), true)   // capturing, on the window
    //             : setTimeout(e)
    //
    // Every element `load` that reaches the window re-registers a NEW closure, and the next one
    // runs all of them, each re-registering again: the listener list DOUBLES per element load.
    // Five subresources took it from 1 listener to 32; `grok.com`'s 244 took it past 100,000 and
    // the page never loaded again (measured: `ok` -> 60 s timeout on grok.com and genspark.ai).
    // The defect is old and was unreachable only because the parser's subresources
    // were never fetched, so no element `load` event ever existed to propagate.
    if ev_type == "load" {
        return path;
    }
    let Some(doc_id) = walk.document else { return path };
    let Some(last) = path.last() else { return path };
    let (doc_target, doc_related, doc_tree) = (last.target, last.related, last.tree);
    let doc = last.node;
    let window = if doc_id == walk.page_root {
        Some(global)
    } else {
        crate::core::html::browsing_context::frame_document_window(scope, doc).map(Into::into)
    };
    if let Some(w) = window {
        path.push(PathStep { node: w, target: doc_target, related: doc_related, tree: doc_tree });
    }
    path
}
/// `bubbles` / `defaultPrevented` off the event. These are prototype accessors over private slots now
/// (see core/events/event.rs), so dispatch reads the SLOT: a page cannot shadow the answer with an own
/// property, which is also how Blink behaves -- it reads the C++ member, not the JS property.
fn et_bool(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, key: &str) -> bool {
    evt_flag_get(scope, ev, key)
}
/// What a dispatch reads and writes at every node of its path, made once per dispatch: the
/// private keys of `currentTarget`, `eventPhase` and the event's flags, and the name of the content attribute a
/// handler for this type is written in (`on<type>`).
///
/// It also knows when the stop flags can have changed. Nothing but a listener can set them during a
/// dispatch (`stopPropagation`, `stopImmediatePropagation` and `cancelBubble` are the page's calls), so
/// `_stop` is read again, and `_stopImmediate` cleared again, only after a listener has run -- a path of
/// nodes with no listener reads and writes neither. Both start stale: the page can set them before
/// dispatching, and a getter the dispatch reads (a shadowed `type`) is page code too.
///
/// And it writes `currentTarget` and `eventPhase` only where something can read them: at a step that runs a
/// listener (or asks a handler's prototype chain, where a page's Proxy runs), before the first of them. No script
/// runs at a step without one, so what such a step wrote was overwritten unread by the next; and a dispatch that ran
/// no listener leaves both as they were, null and NONE, without putting them back.
struct DispatchKeys<'s> {
    current_target: v8::Local<'s, v8::Private>,
    phase: v8::Local<'s, v8::Private>,
    flags: v8::Local<'s, v8::Private>,
    on_attr: String,
    stop_stale: std::cell::Cell<bool>,
    stop_seen: std::cell::Cell<bool>,
    immediate_dirty: std::cell::Cell<bool>,
    /// the phase of the step being run, written with its node by [`DispatchKeys::at_node`]
    phase_now: std::cell::Cell<i32>,
    /// the phase last written in this dispatch, if any
    phase_written: std::cell::Cell<Option<i32>>,
    /// whether `currentTarget` was written in this dispatch
    current_written: std::cell::Cell<bool>,
}

impl<'s> DispatchKeys<'s> {
    fn of(scope: &mut v8::PinScope<'s, '_>, ev_type: &str) -> Self {
        use crate::core::events::event::slot_key;
        DispatchKeys {
            current_target: slot_key(scope, "currentTarget"),
            phase: slot_key(scope, "eventPhase"),
            flags: flags_key(scope),
            on_attr: format!("on{ev_type}"),
            stop_stale: std::cell::Cell::new(true),
            stop_seen: std::cell::Cell::new(false),
            immediate_dirty: std::cell::Cell::new(true),
            phase_now: std::cell::Cell::new(0),
            phase_written: std::cell::Cell::new(None),
            current_written: std::cell::Cell::new(false),
        }
    }

    /// The `_stop` flag, read again only if a listener has run since the last read.
    fn stopped(&self, scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) -> bool {
        if self.stop_stale.replace(false) {
            self.stop_seen.set(evt_flags_at(scope, ev, self.flags) & FLAG_STOP != 0);
        }
        self.stop_seen.get()
    }

    /// Clear `_stopImmediate`, unless no listener has run since it was last cleared.
    fn clear_immediate(&self, scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
        if self.immediate_dirty.replace(false) {
            let f = evt_flags_at(scope, ev, self.flags);
            if f & FLAG_STOP_IMMEDIATE != 0 {
                evt_flags_put(scope, ev, self.flags, f & !FLAG_STOP_IMMEDIATE);
            }
        }
    }

    /// A listener is about to run: it may set either flag.
    fn listener_runs(&self) {
        self.stop_stale.set(true);
        self.immediate_dirty.set(true);
    }

    /// The step at `node` is about to run script: its current target and phase.
    fn at_node(&self, scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, node: v8::Local<v8::Value>) {
        ev.set_private(scope, self.current_target, node);
        self.current_written.set(true);
        let p = self.phase_now.get();
        if self.phase_written.replace(Some(p)) != Some(p) {
            let v: v8::Local<v8::Value> = v8::Integer::new(scope, p).into();
            ev.set_private(scope, self.phase, v);
        }
    }

    /// The end of the dispatch: no current target, phase NONE.
    fn finish(&self, scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
        if self.phase_written.get().is_some_and(|p| p != 0) {
            let v: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
            ev.set_private(scope, self.phase, v);
        }
        if self.current_written.get() {
            let v: v8::Local<v8::Value> = v8::null(scope).into();
            ev.set_private(scope, self.current_target, v);
        }
    }
}

/// Fire the listeners registered at `node` for `ev_type`: only the capturing ones (`Some(true)`), only the others
/// (`Some(false)`), or every one in registration order (`None`, a target that is not a node -- see
/// [`et_dispatch_inner`]). Blink's `RegisteredEventListener::ShouldFire`.
fn et_fire_at<'s>(scope: &mut v8::PinScope<'s, '_>, node: v8::Local<'s, v8::Value>, ev: v8::Local<'s, v8::Object>, ev_type: &str, only: Option<bool>, keys: &DispatchKeys<'s>) {
    let key = match et_key(scope, node) { Some(k) => k, None => return };
    let (has_handler, mut snap) = et_listeners(scope, key, ev_type);
    let mut at = false;
    // A handler written in markup (`onclick="..."`) reaches the store through no setter, so its
    // listener is registered here, at the first dispatch that reaches the element -- the end of the
    // list, exactly where an IDL assignment made at that moment would have put it.
    if only != Some(true) && !has_handler && node_attr_named(scope, key, &keys.on_attr).is_some() {
        keys.at_node(scope, ev, node);
        at = true;
        if has_handler_member(scope, node, &keys.on_attr) {
            handler_activate(scope, key, ev_type);
            snap = et_listeners(scope, key, ev_type).1;
        }
    }
    for entry in snap {
        if only.is_some_and(|c| c != entry.capture) { continue; }
        // removed during this dispatch
        if entry.removed.get() { continue; }
        if entry.once {
            et_retain(scope, key, ev_type, |e| !std::ptr::eq(e, std::rc::Rc::as_ptr(&entry)));
        }
        let passive = entry.passive;
        if !at {
            keys.at_node(scope, ev, node);
            at = true;
        }
        // the passive flag is set around a passive listener alone, so it is clear for every other
        if passive {
            let f = evt_flags_at(scope, ev, keys.flags);
            evt_flags_put(scope, ev, keys.flags, f | FLAG_PASSIVE);
        }
        keys.listener_runs();
        invoke(scope, &entry.kind, key, node, ev, ev_type);
        let f = evt_flags_at(scope, ev, keys.flags);
        if passive {
            evt_flags_put(scope, ev, keys.flags, f & !FLAG_PASSIVE);
        }
        if f & FLAG_STOP_IMMEDIATE != 0 { return; }
    }
}
/// `EventTarget.dispatchEvent(event)` -- WHATWG DOM 2.9.
///
/// Step 1 of the spec is the reason this guard exists: "If event's dispatch flag is set, or if its
/// initialized flag is not set, then throw an InvalidStateError." Korvus used to skip it and simply
/// recurse, so a listener that re-dispatched its own event drove native -> JS -> native until V8's
/// `Check failed: IsOnCentralStack()` killed the process. That is an engine abort a page can trigger in
/// three lines, and it took the upstream `dom/` corpus down at file 72 of 526.
///
/// Spec step 2 ("set isTrusted to false") is deliberately NOT implemented: our trusted-input path in
/// `core::events::shared` dispatches through this very function, and honouring step 2 would zero out
/// the trust bit on every synthetic click and keypress. Re-dispatching a trusted event from a listener
/// is the only observable difference, and it costs a fingerprintable input pipeline to fix here.
fn cb_et_dispatch<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    // WebIDL: the argument is `Event event`, not optional and not nullable -- `dispatchEvent(null)` and
    // `dispatchEvent()` are TypeErrors, never a silent `true`.
    let ev = match v8::Local::<v8::Object>::try_from(args.get(0)) {
        Ok(o) => o,
        Err(_) => {
            throw_type_error(scope, "Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.");
            return;
        }
    };
    let fk = flags_key(scope);
    let f = evt_flags_at(scope, ev, fk);
    if f & FLAG_DISPATCH != 0 {
        throw_dom(scope, "The event is already being dispatched.", "InvalidStateError");
        return;
    }
    if f & FLAG_UNINIT != 0 {
        throw_dom(scope, "The event provided is uninitialized.", "InvalidStateError");
        return;
    }
    // script, or a callback the engine invoked, made this call: its listeners' microtasks wait for it
    let _below = crate::core::html::task_queue::ScriptBelow::enter();
    rv.set_bool(et_dispatch(scope, this, ev));
}

/// Dispatch `ev` at `target` from Rust (the dispatch flag set around the algorithm and always cleared
/// -- a listener that throws is reported inside the loop, so this is reached even then, and an event
/// left flagged would be permanently undispatchable). Returns what `dispatchEvent` returns.
pub(crate) fn et_dispatch<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Value>, ev: v8::Local<'s, v8::Object>) -> bool {
    // An isolated world's dispatch runs as the page's, so the page's listeners get the page's objects
    // (`core/html/isolated_world.rs`).
    if let Some((main, target)) = crate::core::html::isolated_world::dispatch_realm(scope, target) {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, main);
        return et_dispatch(scope, target, ev);
    }
    // A `click` that is a MouseEvent has an ACTIVATION TARGET (DOM "dispatch"): its
    // legacy-pre-activation behaviour runs before the listeners, and its activation behaviour -- or, when the
    // click was cancelled, its legacy-canceled-activation behaviour -- after them.
    let activation = if is_activation_event(scope, ev) {
        let bubbles = et_bool(scope, ev, "bubbles");
        crate::core::html::activation::find(scope, target, bubbles)
    } else {
        None
    };
    let before = activation.as_ref().map(|a| crate::core::html::activation::pre(scope, a));
    // the dispatch flag on, and HTML's legacy target override taken (see `et_dispatch_inner`), in one write
    let fk = flags_key(scope);
    let f = evt_flags_at(scope, ev, fk);
    evt_flags_put(scope, ev, fk, (f | FLAG_DISPATCH) & !FLAG_TARGET_OVERRIDE);
    et_dispatch_inner(scope, target, ev, f);
    // DOM's dispatch clears the stop-propagation flags on completion, with the dispatch flag
    let f = evt_flags_at(scope, ev, fk);
    let done = f & !(FLAG_DISPATCH | FLAG_STOP | FLAG_STOP_IMMEDIATE);
    evt_flags_put(scope, ev, fk, done);
    let not_cancelled = done & FLAG_CANCELED == 0;
    if let (Some(a), Some(b)) = (activation, before) {
        if not_cancelled {
            // a click the USER made (a trusted one) makes its link's navigation user-initiated
            let tk = crate::core::events::event::trusted_key(scope);
            let trusted = ev.get_private(scope, tk).is_some_and(|v| v.is_true());
            crate::core::html::activation::run(scope, &a, b, target, trusted, ev);
        } else {
            crate::core::html::activation::cancel(scope, &a, b);
        }
    }
    not_cancelled
}

/// A `click` event that is a `MouseEvent` (a `PointerEvent` included): the one kind with an activation target.
/// Every dispatch asks, so the event's own `type` slot is read -- not its `type` property, which a page can
/// shadow on the instance -- and anything not five characters long is out before a string is made.
fn is_activation_event(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) -> bool {
    let Some(ty) = crate::core::events::event::evt_slot_read(scope, ev, "type").and_then(|t| v8::Local::<v8::String>::try_from(t).ok()) else {
        return false;
    };
    if ty.length() != 5 || ty.to_rust_string_lossy(scope) != "click" {
        return false;
    }
    crate::binding::protos::iface_object(scope, "MouseEvent")
        .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
        .and_then(|c| ev.instance_of(scope, c))
        .unwrap_or(false)
}

/// The target and related target one path step sees, written before its listeners run.
fn at_step(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, step: &PathStep) {
    evt_slot_write(scope, ev, "target", step.target);
    if let Some(r) = step.related {
        evt_slot_write(scope, ev, "relatedTarget", r);
    }
}

/// The dispatch algorithm proper (steps 3+), the dispatch flag set around it by [`et_dispatch`], which also clears
/// the stop-propagation flags after it and answers `!canceled`. `flags` is the event's flags as the dispatch found
/// them: `bubbles` and `composed` cannot change while the dispatch flag is set (`initEvent` is a no-op then).
fn et_dispatch_inner<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'s, v8::Value>, ev: v8::Local<'s, v8::Object>, flags: i32) {
    // THE TARGET IS THIS DISPATCH'S: it was written only while unset, so an event
    // dispatched again kept its first target (Chrome 150: `a.dispatchEvent(e); b.dispatchEvent(e)`
    // gives `b`'s listener `e.target === b`). Only HTML's legacy target override -- a lifecycle event
    // fired at the Window whose target is the Document -- arrives with a target to keep (the flag is cleared
    // with the dispatch flag's write).
    if flags & FLAG_TARGET_OVERRIDE == 0 {
        evt_slot_write(scope, ev, "target", this);
    }
    // NB: the stop-propagation flags are reset at the END of dispatch (`et_dispatch`), not here -- the DOM
    // dispatch algorithm clears them on completion, so a stopPropagation() called BEFORE dispatch must
    // still suppress this dispatch (WPT Event-propagation `After stopPropagation()`), and a re-dispatch
    // of the same event then starts clean.
    // The event's own type: the `type` PROPERTY ran a getter a page put on the instance --
    // one that called stopPropagation() stopped every listener; Chrome reads the internal type and never
    // calls it.
    let ev_type = evt_slot_read(scope, ev, "type").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let global: v8::Local<v8::Value> = { let ctx = scope.get_current_context(); ctx.global(scope).into() };
    let is_window = this.strict_equals(global);
    // A frame's window is not a node, and asking it for `nodeType` would install its realm (F19).
    let frame_window = v8::Local::<v8::Object>::try_from(this).ok().is_some_and(|o| crate::core::html::browsing_context::ctx_of_window(scope, o).is_some());
    // A node wrapper is a node: its `nodeType` getter, run through V8 here, answered a number
    // for every one.
    let is_node = !is_window && !frame_window && arg_node_id(scope, this).is_some();
    let keys = DispatchKeys::of(scope, &ev_type);
    if !is_node {
        // A target that is not a node -- a window (the page's or a frame's), an `EventTarget`, an `AbortSignal`, an
        // XHR -- has no event path: Blink's `EventTarget::DispatchEventInternal` / `LocalDOMWindow::DispatchEvent`
        // set AT_TARGET and run `FireEventListeners` ONCE, with neither "only capture" nor "only non-capture" set,
        // so every listener fires in registration order, capturing or not. The capture-first split at the target
        // (`EventDispatcher::DispatchEventAtCapturing` / `AtBubbling`, M89) is the NODE path's. Chrome 150, measured
        // (`tests/fixtures/device-sensors/at-target-chrome-150.json`): `a`, `b` capturing, `a` capturing, `c` on a
        // window fire a, b, a, c; on a document or an element b, a, a, c.
        keys.phase_now.set(2);
        et_fire_at(scope, this, ev, &ev_type, None, &keys);
        keys.finish(scope, ev);
        return;
    }
    let composed = flags & FLAG_COMPOSED != 0;
    let related = evt_slot_read(scope, ev, "relatedTarget").filter(|r| r.is_object());
    let path = et_path(scope, this, &ev_type, composed, related);
    // What `composedPath()` reads while the event is being dispatched (see `event.rs`): the path's nodes and the
    // tree each is in. Cleared again below -- Chrome answers `[]` once the dispatch is over.
    {
        let nodes: Vec<v8::Local<v8::Value>> = path.iter().map(|step| step.node).collect();
        let trees: Vec<v8::Local<v8::Value>> = path.iter().map(|step| v8::Number::new(scope, step.tree.map_or(-1.0, f64::from)).into()).collect();
        let nodes = v8::Array::new_with_elements(scope, &nodes);
        let trees = v8::Array::new_with_elements(scope, &trees);
        evt_slot_write(scope, ev, "_pathNodes", nodes.into());
        evt_slot_write(scope, ev, "_pathTrees", trees.into());
    }
    // Only a path that crosses a shadow boundary, or an event with a related target, moves either slot.
    let retargets = related.is_some() || path.iter().any(|s| !s.target.strict_equals(this));
    // Capture pass: window -> ... -> target. A step whose own target is its node (the target, or a shadow host
    // the target was retargeted to) is AT_TARGET.
    for i in (0..path.len()).rev() {
        if keys.stopped(scope, ev) { break; }
        if retargets { at_step(scope, ev, &path[i]); }
        let at_target = path[i].target.strict_equals(path[i].node);
        keys.phase_now.set(if at_target { 2 } else { 1 });
        et_fire_at(scope, path[i].node, ev, &ev_type, Some(true), &keys);
    }
    // Bubble pass: target -> ... -> window. An AT_TARGET step runs its non-capturing listeners whether or not
    // the event bubbles; the others only for a bubbling event.
    let bubbles = flags & FLAG_BUBBLES != 0;
    for i in 0..path.len() {
        if keys.stopped(scope, ev) { break; }
        let at_target = path[i].target.strict_equals(path[i].node);
        if !at_target && !bubbles { continue; }
        if retargets { at_step(scope, ev, &path[i]); }
        keys.clear_immediate(scope, ev);
        keys.phase_now.set(if at_target { 2 } else { 3 });
        et_fire_at(scope, path[i].node, ev, &ev_type, Some(false), &keys);
    }
    // After the dispatch the target is the node it was dispatched at (Blink's `DispatchEventPostProcess`); the
    // related target keeps what the last step saw.
    if retargets {
        evt_slot_write(scope, ev, "target", this);
    }
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    evt_slot_write(scope, ev, "_pathNodes", undef);
    evt_slot_write(scope, ev, "_pathTrees", undef);
    keys.finish(scope, ev);
}
#[op2] pub(crate) fn op_et_dispatch<'s>(scope: &mut v8::PinScope<'s, '_>, #[string] fname: &str, #[smi] length: i32) -> v8::Local<'s, v8::Function> { build_fn(scope, cb_et_dispatch, fname, length) }

/// The callables registered for (target, type) right now: a function listener, an object listener's
/// `handleEvent`, the current `on<type>` handler. Lets a CookieStore-style target capture its listeners
/// at mutation time and deliver on a later task.
pub(crate) fn et_snapshot<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<v8::Value>, ty: &str) -> Vec<v8::Local<'s, v8::Value>> {
    let mut out = Vec::new();
    let Some(key) = et_key(scope, target) else { return out };
    let kinds: Vec<EtKind> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<EventListeners>().map.get(&key).and_then(|t| t.get(ty)).map(|l| l.iter().map(|e| e.kind.clone_kind()).collect()).unwrap_or_default()
    };
    for k in kinds {
        let f = match k {
            EtKind::Callback(g) => {
                let cb = v8::Local::new(scope, &g);
                if cb.is_function() { Some(cb) } else if let Ok(o) = v8::Local::<v8::Object>::try_from(cb) { get_prop(scope, o, "handleEvent").filter(|h| h.is_function()) } else { None }
            }
            EtKind::Handler => handler_current(scope, key, ty).filter(|h| h.is_function()),
            EtKind::Observer(_) | EtKind::SubscriberAbort(_) => None,
        };
        if let Some(f) = f { out.push(f); }
    }
    out
}

/// A window's CURRENT EVENT (DOM): held on the global of the realm whose listener is running, as a private (the
/// isolate's cached key: every listener call reads it twice).
fn current_event_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    crate::binding::protos::private_key(scope, "korvus.currentEvent")
}

/// DOM "inner invoke" steps 8-10: the global of the LISTENER's realm -- not the realm that
/// dispatched, and not every window -- gets `event` as its current event for the call (not when the invocation
/// target is in a shadow tree: Blink's `invocationTargetInShadowTree`), its previous one saved. Before, one stack
/// served every window: the challenge's blank probe frame read its parent's message event as its own `event`,
/// where Chrome 150 reads `undefined`.
fn current_event_enter(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, target: v8::Local<v8::Value>, ev: v8::Local<v8::Value>) -> Option<(v8::Global<v8::Object>, Option<v8::Global<v8::Value>>)> {
    let ctx = f.get_creation_context(scope)?;
    let global = ctx.global(scope);
    let k = current_event_key(scope);
    let prev = global.get_private(scope, k).filter(|v| !v.is_undefined()).map(|v| v8::Global::new(scope, v));
    let in_shadow = crate::binding::protos::unwrap_id(scope, target).is_some_and(|id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<crate::binding::HostDom>().0;
        dom.is_shadow_root(dom.root_of(id))
    });
    if !in_shadow {
        global.set_private(scope, k, ev);
    }
    Some((v8::Global::new(scope, global), prev))
}

/// DOM "inner invoke" step 12: the listener's global's current event back to what it was.
fn current_event_leave(scope: &mut v8::PinScope, saved: &Option<(v8::Global<v8::Object>, Option<v8::Global<v8::Value>>)>) {
    let Some((g, prev)) = saved else { return };
    let global = v8::Local::new(scope, g);
    let k = current_event_key(scope);
    match prev {
        Some(p) => {
            let p = v8::Local::new(scope, p);
            global.set_private(scope, k, p);
        }
        // `undefined` is what every reader takes for no current event; deleting a property of the global -- a
        // dictionary-mode object -- per listener was 1.9% of a `dispatchEvent` loop
        None => {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            global.set_private(scope, k, undef);
        }
    }
}

/// `window.event`: THIS window's current event -- `undefined` unless a listener of its own realm is running.
///
/// The `[Replaceable]` `event` ROW of the `Window` table (core/html/window.rs) is this callback
/// directly; the op below is the same body for whatever JS still reaches for it.
pub(crate) fn cb_window_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let k = current_event_key(scope);
    match args.this().get_private(scope, k).filter(|v| !v.is_undefined()) {
        Some(v) => rv.set(v),
        None => rv.set_undefined(),
    }
}

/// `window.event`: the current realm's window's current event, `undefined` outside its own listeners.
#[op2]
pub(crate) fn op_window_event<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let global = scope.get_current_context().global(scope);
    let k = current_event_key(scope);
    global.get_private(scope, k).filter(|v| !v.is_undefined()).unwrap_or_else(|| v8::undefined(scope).into())
}

/// Does `target` carry ANY registered listener, of any type? XHR's "upload listener flag" asks exactly
/// this of the XMLHttpRequestUpload object at `send()` time, and the answer decides whether the upload
/// half of the event model runs at all. The store is private to this file, so the question is answered
/// here rather than by re-deriving the key elsewhere. An activated `on<type>` handler IS a registered
/// listener (its entry is in the same list), so it counts.
pub(crate) fn et_any_listener(scope: &mut v8::PinScope, target: v8::Local<v8::Value>) -> bool {
    let Some(k) = et_key(scope, target) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.borrow::<EventListeners>().map.get(&k).map(|t| t.values().any(|l| !l.is_empty())).unwrap_or(false)
}

/// Drop every listener and event handler registered on `target`. What a frame's window loses when its
/// context navigates to a NEW document: HTML gives that document a new Window, and this engine keeps
/// one window object per frame (`core/html/browsing_context.rs`, `frame_navigated`).
pub(crate) fn et_forget(scope: &mut v8::PinScope, target: v8::Local<v8::Value>) {
    let Some(k) = et_key(scope, target) else { return };
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let s = st.borrow_mut::<EventListeners>();
        if let Some(t) = s.map.remove(&k) {
            t.values().flatten().for_each(|e| e.removed.set(true));
        }
        s.handlers.remove(&k);
    }
    // `DidRemoveAllEventListeners`: the window's device sensor controllers stop.
    crate::core::html::device_sensors::did_remove_all_listeners(scope, k);
}

/// [`et_forget`] for node `id`, whose key needs no wrapper.
pub(crate) fn et_forget_node(scope: &mut v8::PinScope, id: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let s = st.borrow_mut::<EventListeners>();
    let k = EtKey::Node(id);
    if let Some(t) = s.map.remove(&k) {
        t.values().flatten().for_each(|e| e.removed.set(true));
    }
    s.handlers.remove(&k);
}

/// Is `obj.on<ty>` a function? (XHR's upload listener flag asks this of the upload object.)
pub(crate) fn et_on_handler_set(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, ty: &str) -> bool {
    let Some(key) = et_key(scope, obj.into()) else { return false };
    handler_current(scope, key, ty).map(|v| v.is_function()).unwrap_or(false)
}

/// `this.on<ty>` as the store answers it (`null` when unset) -- the getter's body, for a member whose
/// accessor a bespoke callback wraps (MessagePort's setter also starts the port).
pub(crate) fn et_on_get<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, ty: &str) -> v8::Local<'s, v8::Value> {
    match et_key(scope, this).and_then(|key| handler_current(scope, key, ty)) {
        Some(v) => v,
        None => v8::null(scope).into(),
    }
}
/// `this.on<ty> = v` -- the setter's body: `[LegacyTreatNonObjectAsNull]`, activate on an object,
/// deactivate on null.
pub(crate) fn et_on_set(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, ty: &str, v: v8::Local<v8::Value>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(key) = et_key_add(scope, this, &op_state) else { return };
    if v.is_object() {
        let g = v8::Global::new(scope, v);
        handler_store(scope, key, ty, HandlerValue::Idl(g));
        // The FIRST non-null value adds the handler listener (`SetAttributeEventListener`), an added listener to
        // the window's observers as `addEventListener`'s is; a later value replaces it in place.
        if handler_activate(scope, key, ty) {
            crate::core::html::device_sensors::did_add_listener(scope, key, this, ty);
        }
    } else {
        handler_deactivate(scope, key, ty);
    }
}
fn cb_et_on_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ty = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this: v8::Local<v8::Value> = args.this().into();
    let v = et_on_get(scope, this, &ty);
    rv.set(v);
}
/// `[LegacyTreatNonObjectAsNull] callback Function?` -- what an `EventHandler` attribute really is.
///
/// A non-object assigned to an event handler is converted to null before it is stored, which is why
/// `el.onclick = 5` never throws, never runs, and reads back `null`. A non-callable OBJECT is a
/// different case and is kept -- it is a legal callback value that simply never runs (the handler
/// algorithm calls only a callable). Setting a non-null value activates the event handler listener
/// (once; it keeps its list position across later assignments); setting null deactivates it.
fn cb_et_on_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let ty = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this: v8::Local<v8::Value> = args.this().into();
    let v = args.get(0);
    et_on_set(scope, this, &ty, v);
}
/// Rust-callable core of `op_et_on_getter` -- the real event-handler getter (`get on<type>`, length
/// 0), so the installer builds the identical accessor for a shell `on*` member.
pub(crate) fn make_et_on_getter<'s>(scope: &mut v8::PinScope<'s, '_>, event_type: &str) -> v8::Local<'s, v8::Function> {
    let d = str_val(scope, event_type);
    build_fn_data(scope, cb_et_on_get, &format!("get on{event_type}"), 0, d)
}
#[op2]
pub(crate) fn op_et_on_getter<'s>(scope: &mut v8::PinScope<'s, '_>, #[string] event_type: &str) -> v8::Local<'s, v8::Function> {
    make_et_on_getter(scope, event_type)
}
/// Rust-callable core of `op_et_on_setter` (`set on<type>`, length 1).
pub(crate) fn make_et_on_setter<'s>(scope: &mut v8::PinScope<'s, '_>, event_type: &str) -> v8::Local<'s, v8::Function> {
    let d = str_val(scope, event_type);
    build_fn_data(scope, cb_et_on_set, &format!("set on{event_type}"), 1, d)
}
#[op2]
pub(crate) fn op_et_on_setter<'s>(scope: &mut v8::PinScope<'s, '_>, #[string] event_type: &str) -> v8::Local<'s, v8::Function> {
    make_et_on_setter(scope, event_type)
}

// ── EventTarget.prototype, the table ───────────────────────────────────────────────────────────

/// One method row of the `EventTarget` table, mounted with the row's name, length and flags.
fn et_method_row<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let f = build_fn(scope, cb, member.name, member.length as i32);
    crate::install::kits::define_value(scope, proto, member, f.into(), true);
}
/// The table body of every `EventTarget` method row.
pub fn install_event_target(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    match member.name {
        "addEventListener" => et_method_row(scope, proto, member, cb_et_add),
        "dispatchEvent" => et_method_row(scope, proto, member, cb_et_dispatch),
        "removeEventListener" => et_method_row(scope, proto, member, cb_et_remove),
        "when" => et_method_row(scope, proto, member, crate::core::events::observable::cb_et_when),
        other => debug_assert!(false, "install_event_target: unexpected member {other}"),
    }
}

// The one place the EventTarget interface is defined (goal invariant I1). Rows are the capture's,
// in its order: addEventListener, dispatchEvent, removeEventListener, when. The prototype is the JS
// `EventTarget` class's (core/korvus-runtime/src/js/events.js, until W4.b), made the table's by
// `installer::overwrite_prototype` from `events::install_event_model`.
webapi::interface! {
    "EventTarget" [value, both] {
        ctor   "EventTarget"                            len 0 = stub,
        method "addEventListener"    : Undefined len 2 required 2 = install_event_target,
        method "dispatchEvent"       : Boolean   len 1 required 1 = install_event_target,
        method "removeEventListener" : Undefined len 2 required 2 = install_event_target,
        method "when"                : Object    len 1 required 1 = install_event_target
    }
}

// The native ops this file owns — declared here, beside their definitions, so that adding one
// touches no shared file (binding/exposure.rs). build.rs collects every such declaration into the
// extension's op table on each build.
crate::webapi_ops![
    op_et_add, op_et_remove, op_et_dispatch, op_et_on_getter, op_et_on_setter, op_window_event
];
