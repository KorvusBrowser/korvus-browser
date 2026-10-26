//! `binding/protos.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use deno_core::OpState;
// The DOM engine state + wrapper types this prototype-assembly tier drives.
use crate::binding::host_dom::{ElementProto, HostDom, MathmlElementProto, MathmlTagProtos, NodeCache, NodeProtos, QNameMap, ShadowRootProto, SvgElementProto, SvgTagProtos, TagProtos, UnknownElementProto};
use crate::core::dom::element::JsElement;
use crate::core::html::html_element::{cb_ds_delete, cb_ds_enum, cb_ds_get, cb_ds_query, cb_ds_set};

pub(crate) fn arg_node_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    // A Document names ITS root: `ON_DOCUMENT_ID` for the page's, a real arena id for a nested
    // browsing context's (see `core::dom::shared::document_node_id`).
    if let Some(id) = crate::core::dom::shared::document_node_id(scope, v) { return Some(id); }
    deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, v).map(|p| unsafe { p.as_ref() }.id)
}

pub fn set_prop(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, key: &str, value: v8::Local<v8::Value>) {
    if let Some(k) = key_str(scope, key) { this.set(scope, k.into(), value); }
}
pub(crate) fn int_val<'s>(scope: &mut v8::PinScope<'s, '_>, n: i64) -> v8::Local<'s, v8::Value> { v8::Integer::new(scope, n as i32).into() }

pub fn call_method<'s>(scope: &mut v8::PinScope<'s, '_>, recv: v8::Local<v8::Value>, method: &str, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    let obj = v8::Local::<v8::Object>::try_from(recv).ok()?;
    let key = key_str(scope, method)?;
    let f = v8::Local::<v8::Function>::try_from(obj.get(scope, key.into())?).ok()?;
    f.call(scope, recv, args)
}
/// `recv.<method>(args)` for the engine's own use when the method is one the engine owns:
/// `dispatchEvent` runs the native dispatch and `setTimeout` the pristine timer, never whatever the
/// page stored under that name. Chrome's internals never look those up on the page's objects -- a
/// page that wraps them (zone.js wraps both) saw a call for every event and timer the engine made
/// on its own (the attribute half is `attr_call`). Anything else is `call_method`.
pub fn engine_call<'s>(scope: &mut v8::PinScope<'s, '_>, recv: v8::Local<v8::Value>, method: &str, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    match method {
        "dispatchEvent" => {
            let recv = v8::Local::new(scope, recv);
            let ev = args.first().and_then(|v| v8::Local::<v8::Object>::try_from(*v).ok())?;
            let ev = v8::Local::new(scope, ev);
            let r = crate::core::events::event_target::et_dispatch(scope, recv, ev);
            Some(v8::Boolean::new(scope, r).into())
        }
        "setTimeout" => {
            let f = args.first().and_then(|v| v8::Local::<v8::Function>::try_from(*v).ok());
            if let Some(f) = f {
                let f = v8::Local::new(scope, f);
                let ms = match args.get(1) { Some(v) => v.number_value(scope).unwrap_or(0.0), None => 0.0 };
                if let Some(id) = crate::core::intersection_observer::rendering::pristine_timeout(scope, f, ms) {
                    return Some(id);
                }
            }
            call_method(scope, recv, method, args)
        }
        _ => call_method(scope, recv, method, args),
    }
}
/// The isolate's `v8::Private` for `name`, made once and kept: `Private::for_api` needs a fresh V8
/// string and a registry lookup on every call, and the two private-slot helpers run it on the
/// hottest paths there are -- every collection index, every event slot.
///
/// IN AN ISOLATE SLOT, NOT `OpState`, because this is read on hot paths (every event getter reads two):
/// deno_core keeps `OpState`'s per-type state in an ordered map, so a lookup there costs whatever
/// position this type's `TypeId` happens to take among the others -- measured once, `event.type`
/// moved +390 instructions per call between two builds that changed nothing on its path (a scan of
/// 128-bit keys in `private_key`). An isolate slot is a hash map keyed by type: the same cost in every
/// build. Behind an `Rc`, so a hit hands back a handle without cloning a `Global` (a global-handle create
/// and release per call).
#[derive(Default, Clone)]
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct PrivateKeys(std::rc::Rc<std::cell::RefCell<PrivateKeyMaps>>);

#[derive(Default)]
pub(crate) struct PrivateKeyMaps {
    /// Every key made, by name: the authority the two memos below are filled from.
    by_name: std::collections::HashMap<String, v8::Global<v8::Private>>,
    /// In front of `by_name`, by the name's address ([`crate::binding::keys`]): `by_name` SipHashes
    /// every name (a page chooses the names `structured_clone` brands with), and the hot callers -- every
    /// list index and length, every brand check -- pass literals. Filled from literals only.
    /// 1024 entries: the engine names some 260 private keys (`binding::keys::AddrMemo`).
    recent: crate::binding::keys::AddrMemo<v8::Global<v8::Private>, 1024>,
    /// `korvus_brand_<iface>` by `iface`, for [`brand_private`]: the name was a `format!` per check.
    /// Only interface names the brand registry knows (blueprint names, a set the binary bounds) enter it,
    /// so a page-chosen name can neither grow it nor pick its keys; capped besides.
    brands: crate::install::templates::FxMap<Box<str>, v8::Global<v8::Private>>,
    /// [`key_str`]'s internalized property-name strings, by the literal's address.
    strs: crate::binding::keys::AddrMemo<v8::Global<v8::String>, 512>,
    /// Behind `strs`, the names its full groups turned away, by their TEXT: a memo group fills once and keeps what
    /// filled it, so a name that came later made and internalized a string on every call -- `EventInit`'s three
    /// members were 12.7% of a `new Event` loop. Never by address: before the binary's image is known every name is
    /// `fillable`, a heap one too, and a freed name's address comes back as another's (keyed by address and length,
    /// `Event` read `Fence` and `EventTarget` `PushManager` in another engine's realms). Filled under `strs`'s rule.
    strs_more: korvus_dom::FxHashMap<Box<str>, v8::Global<v8::String>>,
}

/// A property-name string for the engine's literal `s`, internalized once and kept: `v8::String::new`
/// made a fresh string per call, which V8 then looked up in its string table (hashing it) before every
/// Get, Set or Define. A name is a key by its characters, so the internalized string names the same
/// property, and as a value it is the same string. Only a literal of the binary is kept
/// ([`crate::binding::keys::fillable`]); any other name is made as before.
pub(crate) fn key_str<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> Option<v8::Local<'s, v8::String>> {
    type Memo = crate::binding::keys::AddrMemo<v8::Global<v8::String>, 512>;
    let i = Memo::entry_of(s);
    if let Some(k) = scope.get_slot::<PrivateKeys>() {
        let k = k.0.borrow();
        if let Some(g) = k.strs.get(i, s).or_else(|| k.strs_more.get(s)) {
            return Some(v8::Local::new(scope, g));
        }
    }
    if !crate::binding::keys::fillable(s) {
        return v8::String::new(scope, s);
    }
    let v = v8::String::new_from_utf8(scope, s.as_bytes(), v8::NewStringType::Internalized)?;
    let keys = match scope.get_slot::<PrivateKeys>() {
        Some(k) => k.clone(),
        None => {
            let k = PrivateKeys::default();
            scope.set_slot(k.clone());
            k
        }
    };
    let mut k = keys.0.borrow_mut();
    if !k.strs.fill(i, s, || v8::Global::new(scope, v)) {
        k.strs_more.insert(s.into(), v8::Global::new(scope, v));
    }
    Some(v)
}

/// [`key_str`] as a value (an engine literal stored or handed out as a string).
pub(crate) fn key_val<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::Value> {
    match key_str(scope, s) {
        Some(k) => k.into(),
        None => str_val(scope, s),
    }
}

pub(crate) fn private_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    type Memo = crate::binding::keys::AddrMemo<v8::Global<v8::Private>, 1024>;
    let i = Memo::entry_of(name);
    // The hit reads the slot in place: no `Rc` clone, no hash.
    if let Some(k) = scope.get_slot::<PrivateKeys>() {
        if let Some(g) = k.0.borrow().recent.get(i, name) {
            return v8::Local::new(scope, g);
        }
    }
    let keys = match scope.get_slot::<PrivateKeys>() {
        Some(k) => k.clone(),
        None => {
            let k = PrivateKeys::default();
            scope.set_slot(k.clone());
            k
        }
    };
    let found = keys.0.borrow().by_name.get(name).map(|g| v8::Local::new(scope, g));
    let key = match found {
        Some(k) => k,
        None => {
            let n = v8::String::new(scope, name).unwrap();
            let key = v8::Private::for_api(scope, Some(n));
            keys.0.borrow_mut().by_name.insert(name.to_string(), v8::Global::new(scope, key));
            key
        }
    };
    if crate::binding::keys::fillable(name) {
        keys.0.borrow_mut().recent.fill(i, name, || v8::Global::new(scope, key));
    }
    key
}

/// The WebIDL brand private of interface `iface` (`korvus_brand_<iface>`, [`crate::install::webidl`]):
/// [`private_key`] of that name, kept by the interface name so a brand check builds no string.
pub(crate) fn brand_private<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> v8::Local<'s, v8::Private> {
    if let Some(k) = scope.get_slot::<PrivateKeys>() {
        if let Some(g) = k.0.borrow().brands.get(iface) {
            return v8::Local::new(scope, g);
        }
    }
    let key = private_key(scope, &format!("korvus_brand_{iface}"));
    if crate::binding::brand::id_of(iface).is_some() {
        if let Some(k) = scope.get_slot::<PrivateKeys>() {
            let mut m = k.0.borrow_mut();
            if m.brands.len() < 4096 {
                m.brands.insert(iface.into(), v8::Global::new(scope, key));
            }
        }
    }
    key
}

/// What the CURRENT realm keeps for the engine under `name`: a private on the realm's global, so no
/// page can reach it. Set through the global, it lands on the global OBJECT and not the WindowProxy, so
/// a frame navigated to a new document -- a new realm behind the same proxy -- starts empty.
///
/// `OpState` is per ISOLATE and holds what every realm shares. A prototype the engine builds on first
/// use, a singleton, a document's own state belong to ONE realm and live here: kept in `OpState`, the
/// first realm to ask made them for every other (where every frame became a realm).
pub(crate) fn realm_slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let global = scope.get_current_context().global(scope);
    let key = private_key(scope, name);
    global.get_private(scope, key).filter(|v| !v.is_undefined())
}

/// Keep `value` under `name` for the current realm ([`realm_slot`]).
pub(crate) fn set_realm_slot(scope: &mut v8::PinScope, name: &str, value: v8::Local<v8::Value>) {
    let global = scope.get_current_context().global(scope);
    let key = private_key(scope, name);
    global.set_private(scope, key, value);
}

/// The current realm's null-prototype map object under `name` ([`realm_slot`]), made on first use.
pub(crate) fn realm_map<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Object> {
    if let Some(m) = realm_slot(scope, name).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return m;
    }
    let m = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    m.set_prototype(scope, null);
    set_realm_slot(scope, name, m.into());
    m
}

pub fn get_prop<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<v8::Object>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = key_str(scope, key)?;
    obj.get(scope, k.into())
}

pub fn str_val<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::Value> { v8::String::new(scope, s).unwrap().into() }


pub(crate) fn set_bool(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, key: &str, b: bool) {
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into(); set_prop(scope, this, key, v);
}

pub(crate) fn make_dom_exc<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let ctor = crate::binding::protos::iface_object(scope, "DOMException").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let m = str_val(scope, msg); let n = str_val(scope, name);
    let e = ctor.new_instance(scope, &[m, n])?;
    capture_native_stack(scope, e);
    Some(e.into())
}

/// A DOMException the PLATFORM throws carries the stack of the script that called it -- an own `stack`
/// whose frames are the caller's, as Blink attaches with `v8::Exception::CaptureStackTrace` (Chrome 150,
/// measured 2026-10-05: `try { document.querySelector('<<<') } catch (e) { e.stack }` is
/// `"SyntaxError: … \n    at f (…:3:31)\n    at …"`, own property). One the PAGE constructs has none
/// (`new DOMException('m').stack === undefined`), so this is called only where the platform mints one.
/// Without it an uncaught platform DOMException thrown from a timer was reported at line 0, and a
/// script reading `e.stack` found `undefined` where every browser has a string.
pub(crate) fn capture_native_stack(scope: &mut v8::PinScope, e: v8::Local<v8::Object>) {
    let ctx = scope.get_current_context();
    let _ = v8::Exception::capture_stack_trace(ctx, e);
}

pub(crate) fn resolved_promise<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    resolver.resolve(scope, value);
    resolver.get_promise(scope).into()
}
/// A promise nothing will ever settle -- the answer to a request that opens a prompt no one answers
/// (Brave 151's `clipboard.readText()`, `requestMIDIAccess()` and
/// `Notification.requestPermission()` stay pending behind theirs).
pub(crate) fn pending_promise<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    resolver.get_promise(scope).into()
}

/// When a promise the engine hands back settles. Chrome answers most of these from another
/// process: the answer arrives as a TASK, after a 0 ms timer the page queued right after the call or a few
/// milliseconds later; a few of its bindings settle from a MICROTASK. An already-settled promise -- what this engine
/// handed back for all of them -- runs its reactions before either, and a page that schedules its work by which
/// promise settles first (a bot challenge's VM does) runs it in another order. Measured on stock Chrome 150
/// against a microtask, a posted message and 0-64 ms timers queued right after each call.
#[derive(Clone, Debug)]
pub(crate) enum Settle {
    Microtask,
    Task(f64),
    /// A task `ms` from now -- or later, once `done` is set: work another thread does (the browser's storage service
    /// writing to disk) before the answer can come, as the answer waits for it in the browser.
    TaskGated(f64, std::sync::Arc<std::sync::atomic::AtomicBool>),
}

/// The flags [`Settle::TaskGated`] settles wait for, by settle id.
#[derive(Default)]
struct SettleGates(std::collections::HashMap<u64, std::sync::Arc<std::sync::atomic::AtomicBool>>);

/// How often a gated settle looks at its flag again.
const GATE_POLL_MS: f64 = 0.25;

/// The promises [`settle_later`] has yet to settle, by id: (resolver, value, reject).
#[derive(Default)]
struct LaterSettles(std::collections::HashMap<u64, (v8::Global<v8::PromiseResolver>, v8::Global<v8::Value>, bool)>, u64);

/// A promise settled `when` -- resolved with `value`, or rejected with it when `reject`. A rejection is NOT marked
/// handled: a page that never handles it reports it, as Chrome reports it.
pub(crate) fn settle_later<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<v8::Value>, reject: bool, when: Settle) -> v8::Local<'s, v8::Value> {
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    let (r, v) = (v8::Global::new(scope, resolver), v8::Global::new(scope, value));
    let id = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if !st.has::<LaterSettles>() {
            st.put(LaterSettles::default());
        }
        let l = st.borrow_mut::<LaterSettles>();
        l.1 += 1;
        l.0.insert(l.1, (r, v, reject));
        l.1
    };
    match when {
        Settle::Microtask => {
            let d = v8::Number::new(scope, id as f64);
            let f = build_fn_data(scope, cb_settle_later, "", 0, d.into());
            scope.enqueue_microtask(f);
        }
        Settle::Task(ms) => crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_settle_later, id, ms),
        Settle::TaskGated(ms, done) => {
            {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let mut st = op_state.borrow_mut();
                if !st.has::<SettleGates>() {
                    st.put(SettleGates::default());
                }
                st.borrow_mut::<SettleGates>().0.insert(id, done);
            }
            crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_settle_later, id, ms)
        }
    }
    promise.into()
}

fn cb_settle_later(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = body_data(scope, &args).number_value(scope).map(|n| n as u64) else { return };
    // a gated settle whose work is not done yet looks again shortly
    let waiting = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.try_borrow_mut::<SettleGates>() {
            Some(g) => match g.0.get(&id) {
                Some(done) if !done.load(std::sync::atomic::Ordering::Acquire) => true,
                Some(_) => {
                    g.0.remove(&id);
                    false
                }
                None => false,
            },
            None => false,
        }
    };
    if waiting {
        crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_settle_later, id, GATE_POLL_MS);
        return;
    }
    let entry = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<LaterSettles>().and_then(|l| l.0.remove(&id))
    };
    let Some((r, v, reject)) = entry else { return };
    let (r, v) = (v8::Local::new(scope, &r), v8::Local::new(scope, &v));
    if reject {
        r.reject(scope, v);
    } else {
        r.resolve(scope, v);
    }
}

pub(crate) fn rejected_promise<'s>(scope: &mut v8::PinScope<'s, '_>, reason: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    resolver.reject(scope, reason);
    resolver.get_promise(scope).into()
}
/// A promise rejected with a fresh `TypeError(msg)` -- the shape `pipeTo`/`cancel` need when the
/// failure is discovered after the call already committed to returning a promise (an argument-count
/// or wrong-type failure is a SYNCHRONOUS throw instead; see `throw_type_error`).
///
/// Made as [`throw_type_error`] makes its error, Blink's context applied after construction: Chrome 150's
/// rejections carry it that way too (`fetch()`, a locked stream's `cancel()`, `permissions.query({name: 'nope'})`,
/// `Response('').json()` -- `stack` the detail, `message` the whole text, measured).
pub(crate) fn rejected_type_error<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str) -> v8::Local<'s, v8::Value> {
    let e = deno_core::error::native_error(scope, "TypeError", msg);
    rejected_promise(scope, e)
}
pub(crate) fn new_global<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Object>> {
    // The realm's OWN interface object (its sealed constructor) before whatever the page now keeps under
    // `window[name]`: an event the engine fires is never built by a page's replacement
    // `MessageEvent`/`Event`, and a page that wrapped one saw a construction for every delivery.
    let ctor = iface_object(scope, name).or_else(|| global_get(scope, name));
    ctor.and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()).and_then(|c| c.new_instance(scope, args))
}

#[track_caller]
pub fn build_fn_data<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, name: &str, length: i32, data: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Function> {
    let raw = cb.map_fn_to();
    #[cfg(not(feature = "analysis"))]
    let f = v8::Function::builder_raw(crate::binding::refs::c(raw)).data(data).length(length).constructor_behavior(v8::ConstructorBehavior::Throw).build(scope).unwrap();
    #[cfg(feature = "analysis")]
    let f = {
        let (cb2, d2) = crate::install::templates::analysis_untemplated(scope, raw, Some(data), name);
        v8::Function::builder_raw(crate::binding::refs::c(cb2)).data(d2).length(length).constructor_behavior(v8::ConstructorBehavior::Throw).build(scope).unwrap()
    };
    let n = v8::String::new(scope, name).unwrap(); f.set_name(n);
    stamp_engine_fn(scope, f);
    if crate::install::templates::capturing() {
        let data = Some(v8::Global::new(scope, data));
        crate::install::templates::record(scope, f, crate::install::templates::Recipe { cb: Some(raw), data, length, name: name.to_string(), ctor: false, guard: crate::install::templates::Guard::None, inner: None });
    }
    crate::install::templates::note_untemplated(scope, "build_fn_data", name, f);
    f
}

/// [`build_fn_data`] FROM A TEMPLATE: for a function no interface's template makes -- an
/// unforgeable `Location` member, a `[LegacyUnforgeable]` window accessor. One template per isolate
/// (`install::templates::standalone_fn`), whose instance in each realm is that realm's function: ONE
/// function per realm for each callback, name, length and data, so it serves a member a realm defines once.
/// `data` is a string every realm shares, or none.
pub fn template_fn<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, name: &str, length: i32, data: Option<&str>) -> v8::Local<'s, v8::Function> {
    crate::install::templates::standalone_fn(scope, cb.map_fn_to(), name, length, data, true, false)
}

/// A constructor from a template ([`template_fn`], `ConstructorBehavior::Allow`) that no interface's template makes -- a legacy
/// factory, whose `prototype` the caller redefines in each realm (as Blink does for `Image`, `Audio` and
/// `Option`).
pub(crate) fn template_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, name: &str, length: i32, data: Option<&str>) -> v8::Local<'s, v8::Function> {
    crate::install::templates::standalone_fn(scope, cb.map_fn_to(), name, length, data, false, true)
}

/// [`build_fn`] for a function no ROW builds: the kernel's factories (the timer quartet,
/// `queueMicrotask`, the worker scope's services), which `korvus-runtime` makes for a table row to mount.
/// RECORDED while a template capture runs, like every builder here, but WITHOUT the
/// engine brand: that brand marks a row-built body, which the mount wraps in its receiver check, and the
/// kernel's functions never had it -- stamped, `setTimeout.call({})` in a worker became `Illegal
/// invocation` (measured by the surface lock, six members in every worker realm).
#[track_caller]
pub fn build_kernel_fn<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, name: &str, length: i32) -> v8::Local<'s, v8::Function> {
    kernel_fn(scope, cb.map_fn_to(), name, length, None, "build_kernel_fn")
}

/// [`build_kernel_fn`] with call data (the holder name the timer bodies throw with).
#[track_caller]
pub fn build_kernel_fn_data<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, name: &str, length: i32, data: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Function> {
    kernel_fn(scope, cb.map_fn_to(), name, length, Some(data), "build_kernel_fn_data")
}

#[track_caller]
fn kernel_fn<'s>(scope: &mut v8::PinScope<'s, '_>, raw: v8::FunctionCallback, name: &str, length: i32, data: Option<v8::Local<'s, v8::Value>>, builder: &str) -> v8::Local<'s, v8::Function> {
    // the release statements exactly as they were (A3: even a block around them moved release codegen)
    #[cfg(not(feature = "analysis"))]
    let mut b = v8::Function::builder_raw(crate::binding::refs::c(raw)).length(length).constructor_behavior(v8::ConstructorBehavior::Throw);
    #[cfg(not(feature = "analysis"))]
    if let Some(d) = data {
        b = b.data(d);
    }
    #[cfg(not(feature = "analysis"))]
    let f = b.build(scope).unwrap();
    #[cfg(feature = "analysis")]
    let f = {
        let (cb2, d2) = crate::install::templates::analysis_untemplated(scope, raw, data, name);
        v8::Function::builder_raw(crate::binding::refs::c(cb2)).data(d2).length(length).constructor_behavior(v8::ConstructorBehavior::Throw).build(scope).unwrap()
    };
    let n = v8::String::new(scope, name).unwrap();
    f.set_name(n);
    if crate::install::templates::capturing() {
        let data = data.map(|d| v8::Global::new(scope, d));
        crate::install::templates::record(scope, f, crate::install::templates::Recipe { cb: Some(raw), data, length, name: name.to_string(), ctor: false, guard: crate::install::templates::Guard::None, inner: None });
    }
    crate::install::templates::note_untemplated(scope, builder, name, f);
    f
}

/// The private every function THIS ENGINE builds for a member body carries.
///
/// WHY IT EXISTS. The installer's receiver check wraps the function a row's body
/// mounted, and it must not wrap a function the row did not build. One kit, `kits::array_proto_fn`,
/// deliberately mounts `Array.prototype`'s OWN function object -- WebIDL gives an interface with an
/// indexed property getter the array value iterator, and the capture records the identity
/// (`NodeList.prototype.values === Array.prototype.values` is `true` in Chrome 150.0.7871.46,
/// measured). Wrapping that would break the identity AND add a brand check to a member the browser
/// leaves generic (the bundle's oracle recorded `XRInputSourceArray.values` answering
/// `[object Array Iterator]` for a foreign receiver, not throwing).
///
/// So "did this engine build the function" is the derived question, and this answers it without a
/// list of exempt members: a `v8::Private` is invisible to every JS reflection path, and V8's own
/// builtins never carry one.
fn engine_fn_brand<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    private_key(scope, "korvus.engineFn")
}

pub(crate) fn stamp_engine_fn(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) {
    let key = engine_fn_brand(scope);
    let t = v8::Boolean::new(scope, true);
    let o: v8::Local<v8::Object> = f.into();
    o.set_private(scope, key, t.into());
}

/// Whether `v` is a function this engine built (see [`stamp_engine_fn`]).
pub(crate) fn is_engine_fn(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return false };
    if !v.is_function() {
        return false;
    }
    let key = engine_fn_brand(scope);
    o.get_private(scope, key).map(|x| x.is_true()).unwrap_or(false)
}

/// The persona this realm wears: the kernel `Context`'s `PersonaHandle` service, and THE one read of it
/// for a Web API (every read is routed here). A read while a
/// template capture runs MARKS the capture (`install::templates::note_persona_read`): what that install
/// built may depend on the persona, so its blueprint is kept for this persona only. Invariant I9 says an
/// install reads the persona only at call time; this is what checks it.
pub fn persona_handle(st: &deno_core::OpState) -> Option<std::sync::Arc<spec::PersonaHandle>> {
    crate::install::templates::note_persona_read();
    st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::PersonaHandle>())
}

/// A callback's own data: `args.data()`, except on a FUSED member (`install::templates`) --
/// one native call for its guards and body, whose V8 data is the fused record -- where it is the data the
/// body was built with (`undefined` for a body built without). Every callback reads its data through here,
/// so a body cannot tell whether it was fused. Public: the
/// runtime crate's table bodies (`History`, `Location`, the timers) read theirs through it too.
pub fn body_data<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> v8::Local<'s, v8::Value> {
    let d = args.data();
    match crate::install::templates::fused_body_data(scope, d) {
        Some(v) => v,
        None => v8::Local::new(scope, d),
    }
}

pub fn cb_noop(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}
// THE SHELL INVENTORY IS GONE (goal invariant I7). It answered "which members were installed as a
// shape" by pushing a label every time `make_shell_method` built one, and `tools/status/data/status.json`
// read its `layer` field off it -- which the D2 stub never registered into, so a hand table's
// `= stub` row reported `implemented`. The layer is a COLUMN of the row now: the one member mount
// records it (`install::installer::mounted_layer` -> `behavior_rt::record_layer`), a page load dumps
// it to $KORVUS_LAYERS_OUT, and `tools/sync/gen_layers.py` derives the same fact statically into
// tools/status/data/layers.json. Coverage recording moved with it: one `record_stub_call` per stub hit,
// into the same map the corpus worklist reads, with no second label table to intern into.



/// The receiver check every shell member now performs before it answers.
///
/// A shell member used to answer `undefined` no matter what it was called on, so
/// `GPUAdapter.prototype.features` -- read off the PROTOTYPE, which is not an instance -- answered
/// `undefined` where every browser throws. That is 532 recorded disagreements in one shape, and it is
/// a fingerprint as much as a correctness gap: an object whose members answer for any receiver at all
/// is not a platform object.
///
/// The check is the one WebIDL specifies: the receiver must have the interface's prototype in its
/// chain and must not BE that prototype. The interface prototype travels as the function's hidden
/// data alongside the instrumentation index, so the fast path is still an integer read.
///
/// `cb_noop`'s own comment said "making shells THROW is a separate decision that needs this call data
/// first". The call data exists now, and so do the browser's recorded answers.

/// The private brand every shell INSTANCE carries, and no other object can forge.
///
/// A prototype-chain test is not what a browser does. `Object.create(GPUAdapter.prototype)` has the
/// prototype in its chain and Chrome still throws "Illegal invocation" on it, because the check is
/// for an internal slot the object either has or does not. The recorded case says so directly:
///     Object.getOwnPropertyDescriptor(GPUAdapter.prototype, "features").get
///         .call(Object.create(GPUAdapter.prototype))     ->  TypeError
/// A v8 Private is the closest thing this engine has to an internal slot: no JS reflection path --
/// getOwnPropertyNames, getOwnPropertySymbols, JSON, a Proxy trap -- can see or set it.
fn shell_is_branded(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> bool {
    let key = private_key(scope, "korvus.shellInstance");
    obj.get_private(scope, key).map(|v| v.is_true()).unwrap_or(false)
}

/// Mark an object as a real instance of a shell interface. Called from the shell constructor.
// `op_brand_shell_instance` IS GONE. It stamped the brand on an object built in JS and
// returned it, so that `js/dom/09g-navigator.js` could wrap its `Object.create(Iface.prototype)`
// mint of `navigator.mediaSession` / `navigator.geolocation` in one call. That file is deleted and
// those singletons are minted in Rust, which calls `brand_shell_instance` below directly -- as do
// `css/highlight.rs`, `dom/svg_string_list.rs`, `navigator/devices.rs` and `cb_shell_ctor`.
// Measured: `navigator.geolocation instanceof Geolocation` is still true.

pub(crate) fn brand_shell_instance(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) {
    // Its only reader (`shell_is_branded`) is asked only beside `is_api_wrapper`, and either one makes a real
    // instance: an API wrapper -- a template-minted event, record, DOMException, Blob -- answers yes without it,
    // and an object's instance type never changes, so the stamp would be a map transition per mint for nothing.
    if obj.is_api_wrapper() {
        return;
    }
    let key = private_key(scope, "korvus.shellInstance");
    let t = v8::Boolean::new(scope, true);
    obj.set_private(scope, key, t.into());
}

/// [`shell_receiver_ok`] by interface NAME instead of by prototype object: a shell-branded object (or
/// an API wrapper) whose chain holds a prototype stamped `iface`. The prototype itself is not an
/// instance -- the walk starts at its [[Prototype]]. Needs no realm's object, so a V8 template can
/// carry the member that asks it.
pub(crate) fn shell_receiver_ok_named(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, iface: &str) -> bool {
    // the brand, as [`table_receiver_ok`], the interface by name
    if let Some(target) = crate::binding::brand::id_of(iface) {
        return crate::binding::brand::implements_id(scope, this, target);
    }
    // a name no template brands: this realm's interface object, by `instanceof`
    let Ok(obj) = v8::Local::<v8::Object>::try_from(this) else { return false };
    if !obj.is_api_wrapper() && !shell_is_branded(scope, obj) {
        return false;
    }
    iface_object(scope, iface).and_then(|c| v8::Local::<v8::Object>::try_from(c).ok()).and_then(|c| this.instance_of(scope, c)).unwrap_or(false)
}

pub(crate) fn shell_receiver_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, proto: v8::Local<v8::Value>) -> bool {
    // the brand, as [`table_receiver_ok`]
    match crate::binding::brand::proto_iface(scope, proto) {
        Some(target) => crate::binding::brand::implements_id(scope, this, target),
        None => {
            report_unbranded_check(scope, "shell", proto);
            shell_receiver_walk(scope, this, proto)
        }
    }
}

fn shell_receiver_walk(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, proto: v8::Local<v8::Value>) -> bool {
    let (Ok(obj), Ok(pr)) = (v8::Local::<v8::Object>::try_from(this), v8::Local::<v8::Object>::try_from(proto)) else {
        return false; // a primitive receiver is never an instance
    };
    if obj == pr {
        return false; // the prototype itself is not an instance
    }
    // The prototype must be in the chain AND the object must carry the brand a shell constructor
    // stamps on it. The chain alone is not enough -- see `shell_brand`.
    // Two ways to be a real instance, and a plain object is neither:
    //   * the brand a shell constructor stamps on what it builds, or
    //   * a cppgc-backed platform object, which v8 marks as an API WRAPPER. `Document`, `Element`
    //     and every other implemented interface reaches its SHELL members through one of these --
    //     `Document.customElementRegistry` is a shell on a real document -- and requiring the brand
    //     alone cost 70 of them before this clause existed.
    // `Object.create(GPUAdapter.prototype)` has neither, which is the case the browser rejects.
    if !obj.is_api_wrapper() && !shell_is_branded(scope, obj) {
        return false;
    }
    let mut cur = obj.get_prototype(scope);
    while let Some(c) = cur {
        let Ok(co) = v8::Local::<v8::Object>::try_from(c) else { break };
        if co == pr {
            return true;
        }
        cur = co.get_prototype(scope);
    }
    false
}



/// WebIDL's interface-type conversion: is `v` a platform object implementing `iface` -- of ANY realm? Its
/// brand (`binding::brand`): another realm's object implements the interface as much as this
/// realm's does, and Chrome converts it (a frame's `File` to the page's `DataTransferItemList.add`, a
/// frame's `EditContext` to the page's `el.editContext`: B27), while `Object.create(I.prototype)` does not
/// (B31). A name no template brands -- a JavaScript builtin -- is `instanceof` this realm's constructor.
pub(crate) fn implements(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str) -> bool {
    if !v.is_object() {
        return false;
    }
    if let Some(target) = crate::binding::brand::id_of(iface) {
        return crate::binding::brand::implements_id(scope, v, target);
    }
    iface_object(scope, iface)
        .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
        .and_then(|c| v.instance_of(scope, c))
        .unwrap_or(false)
}

/// Under `KORVUS_TPL_REPORT`: a receiver check asked about a prototype no interface template made (no brand to
/// ask for), so it walked the prototype chain as. One `CHECK-NOID <how> <stamp>` line per
/// distinct prototype.
fn report_unbranded_check(scope: &mut v8::PinScope, how: &str, proto: v8::Local<v8::Value>) {
    if !crate::install::templates::reporting() {
        return;
    }
    let n = v8::Local::<v8::Object>::try_from(proto).ok().and_then(|p| iface_name_of(scope, p)).map(|n| n.to_rust_string_lossy(scope));
    let key = format!("{how} {n:?}");
    thread_local! {
        static SEEN: std::cell::RefCell<std::collections::HashSet<String>> = std::cell::RefCell::new(std::collections::HashSet::new());
    }
    if SEEN.with(|s| s.borrow_mut().insert(key.clone())) {
        crate::install::templates::report(&format!("CHECK-NOID {key}"));
    }
}

/// The HAND-TABLE receiver check: `proto` must be in the receiver's prototype chain, and the receiver
/// must not BE the prototype. Weaker than [`shell_receiver_ok`] on purpose.
///
/// WHY IT IS WEAKER. `shell_receiver_ok` also demands the brand a shell constructor stamps (or a cppgc
/// wrapper), which every registry instance carries. A HAND TABLE's instances are minted by the table's
/// own file -- `core/animation`'s `new_on_proto`, the WebRTC graph's plain objects -- and carry
/// neither, so requiring the brand there rejects genuine instances: `anim_live` and `rtc_graph_live`
/// both threw `Illegal invocation` reading a stub row off a real one. The chain alone cannot make that
/// mistake, and it still rejects the case every browser rejects and this path used to answer for --
/// `Notification.prototype.close.call({})`, `HighlightRegistry.prototype.highlightsFromPoint.call({})`.
///
/// What it does NOT reject, and the difference is recorded rather than hidden:
/// `Object.create(X.prototype)` passes the chain and Chrome throws for it. The event family does not
/// have that gap -- its rows check a per-interface BRAND (`core/events/ctor.rs`) -- and closing it for
/// the other hand tables means branding their mint sites, which is each table's own work.
pub(crate) fn table_receiver_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, proto: v8::Local<v8::Value>) -> bool {
    // THE BRAND: the interface that made the receiver is `proto`'s or inherits from it, in
    // any realm -- which refuses `Object.create(I.prototype)` and accepts another realm's instance, as
    // Chrome does, where the chain walk below did neither on its own
    match crate::binding::brand::proto_iface(scope, proto) {
        Some(target) => crate::binding::brand::implements_id(scope, this, target),
        None => {
            report_unbranded_check(scope, "table", proto);
            table_receiver_walk(scope, this, proto)
        }
    }
}

fn table_receiver_walk(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, proto: v8::Local<v8::Value>) -> bool {
    let (Ok(obj), Ok(pr)) = (v8::Local::<v8::Object>::try_from(this), v8::Local::<v8::Object>::try_from(proto)) else {
        return false; // a primitive receiver is never an instance
    };
    if obj == pr {
        return false; // the prototype itself is not an instance
    }
    let mut cur = obj.get_prototype(scope);
    while let Some(c) = cur {
        let Ok(co) = v8::Local::<v8::Object>::try_from(c) else { break };
        if co == pr {
            return true;
        }
        cur = co.get_prototype(scope);
    }
    false
}

/// What a PROMISE member's receiver refusal rejects with, as Chrome 150 words it (measured):
/// "Failed to execute '<m>' on '<I>': Illegal invocation" for an operation, "Failed to read the '<a>' property
/// from '<I>': Illegal invocation" for an attribute (`member` is the function's name, `get <a>` for a getter).
/// `<I>` is the interface `proto` belongs to. A synchronous member throws the bare "Illegal invocation".
pub(crate) fn illegal_invocation_message(scope: &mut v8::PinScope, proto: v8::Local<v8::Value>, member: &str) -> String {
    let iface = v8::Local::<v8::Object>::try_from(proto).ok().and_then(|p| iface_name_of(scope, p)).map(|n| n.to_rust_string_lossy(scope));
    let Some(iface) = iface else { return "Illegal invocation".to_string() };
    if let Some(a) = member.strip_prefix("get ") {
        format!("Failed to read the '{a}' property from '{iface}': Illegal invocation")
    } else if let Some(a) = member.strip_prefix("set ") {
        format!("Failed to set the '{a}' property on '{iface}': Illegal invocation")
    } else {
        format!("Failed to execute '{member}' on '{iface}': Illegal invocation")
    }
}

/// `TypeError: Illegal invocation` -- what every browser answers when a platform member is called on
/// a receiver that is not an instance of its interface. The one stub (`install::stub`) throws it after
/// `shell_receiver_ok` rejects the receiver.
pub(crate) fn illegal_invocation(scope: &mut v8::PinScope) {
    if let Some(m) = v8::String::new(scope, "Illegal invocation") {
        let e = v8::Exception::type_error(scope, m);
        scope.throw_exception(e);
    }
}

// The shell stub family that used to live here -- `shell_data`, `cb_shell_noop`, `cb_shell_null`,
// `make_shell_method` and the `shell_labels` inventory -- is GONE (goal invariant I7, "one stub").
// There is one stub now, `install::stub`, and THE installer mounts it for every `Stub` row of every
// table in either flavor; it carries the receiver check with it (`shell_receiver_ok` above,
// `illegal_invocation` to throw), answers from the row's own type + nullability instead of a bool
// argument, and records into the coverage map through `behavior_rt::record_stub_call` rather than a
// second label inventory.
//
// AND SO IS THE LAST JS-FACING SPELLING OF "GIVE ME A STUB": `op_noop_method` and its
// helper `js_named_stub`. The block that stood here listed the op's three remaining call sites --
// `js/generated_shells.js`, `js/dom/08j-model-element.js` and `js/worker-finalize.js` -- and said
// "they go when those files do". The files went (Phase 4 deleted both generated shell layers) and
// the op did not, so it sat registered with no caller in either language. Its nullable twin
// `op_null_method` had already gone the same way.
//
// INVARIANT I7 IS NOW WHOLE: there is exactly one stub in the engine, `install::stub::cb_stub`, and
// exactly one way to reach it -- a `Stub` row of a table. `interned_member` and `stub_data_recv`,
// which `js_named_stub` used, are NOT gone: `install::live::foreign` re-mints two descriptors
// through them and `installer::member` builds every table stub with `stub_data_recv`.

// ── the interface object's two messages ────────────────────────────────────────────
//
// MEASURED on `google-chrome --headless=new` 150.0.7871.46 over a loopback server, 2026-09-12, by
// probing EVERY uppercase-initial global of both realms (949 window, 294 worker) for both call
// shapes. The two shapes DISAGREE, and the disagreement is the reason this pair exists:
//
//   interface WebIDL gives NO constructor (Selection, AbortSignal, Element, Navigator, ... 537
//   window rows + 147 worker rows):
//       new X()  ->  TypeError: Failed to construct 'X': Illegal constructor
//       X()      ->  TypeError: Illegal constructor                      <- NO PREFIX
//
//   interface WebIDL gives `[HTMLConstructor]` (the 73 HTML element interfaces; 72 of them shipped
//   by Chrome 150 -- `HTMLPortalElement` is declared by webref and absent from the browser):
//       new X()  ->  TypeError: Failed to construct 'X': Illegal constructor
//       X()      ->  TypeError: Failed to construct 'X': Please use the 'new' operator, this DOM
//                    object constructor cannot be called as a function.
//
// THE DISCRIMINATOR IS `new.target`, NOT THE SYNTAX. Measured on the same run:
// `Reflect.construct(Selection, [])` gets the prefix, `Reflect.apply(Selection, null, [])` and
// `Selection.call(null)` do not, and `class Q extends Selection {}; new Q()` gets it with
// `Selection` -- the INTERFACE's name, never `new.target`'s. So the name is carried in the
// callback's `data` (the `make_arity_guard` pattern), not read off
// `new.target`, and the branch is `args.new_target().is_undefined()`.
//
// `NodeFilter` is deliberately NOT one of these: it is a legacy callback interface with no
// `[[Construct]]` at all, so Chrome answers `new NodeFilter()` with V8's own "NodeFilter is not a
// constructor" and a bare call with "Illegal constructor". `core/dom/spine.rs` therefore keeps
// building it with `build_fn` (`ConstructorBehavior::Throw`) and only the CALL path reaches the body.

/// The interface name an interface-object callback was built with, from its `data` slot.
///
/// TWO SHAPES, because the arity message needs a second fact and a V8 callback has no route back to
/// the function object that is running it (there is no `Callee`, and `new.target` is the SUBCLASS on
/// `class Q extends CloseEvent {}; new Q()` -- measured: Chrome still names `CloseEvent` and its
/// count there). So [`build_interface_ctor`] packs `[name, requiredArgs]` and the pair travels
/// together. A `RowCtor::named` body binds the bare NAME, because its arity is its own body's
/// business; both shapes read their name here.
pub(crate) fn ctor_name(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> String {
    let d = crate::binding::protos::body_data(scope, args);
    // (The `[name, requiredArgs, classTemplate]` Array a cppgc class's interface object carried is gone,
    // with the `build_class_ctor` that packed it -- the deno_core class adoption is deleted,
    // `32b46d9d` -- so only the two string shapes below are left to read.)
    // NOT a `debug_assert!`: this runs inside a V8 callback, and a Rust panic unwinding through
    // `extern "C"` aborts the process with the message still in libtest's per-test capture buffer --
    // which is exactly how it presented while this unit was being wired (SIGABRT, no output). A
    // mis-wired caller is caught by the gate instead, and loudly: the message becomes
    // `Failed to construct '': …` and `tests/installer_parity` compares it to the row's name for
    // every interface of both realms.
    if !d.is_string() {
        eprintln!("[korvus] interface-object callback built without its interface name as `data`");
        return String::new();
    }
    // `"<name>|<required>"` from [`build_interface_ctor`], or a bare `"<name>"` (a `RowCtor`'s shape)
    let s = d.to_rust_string_lossy(scope);
    match s.split_once('|') {
        Some((name, _)) => name.to_string(),
        None => s,
    }
}

/// How many arguments this interface's constructor REQUIRES, from the same `data` slot.
///
/// Zero when the callback was built with the bare-name shape, which is the right answer for it: a
/// `RowCtor` body checks its own arguments.
pub(crate) fn ctor_required(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> i32 {
    let d = crate::binding::protos::body_data(scope, args);
    if !d.is_string() {
        return 0;
    }
    d.to_rust_string_lossy(scope).split_once('|').and_then(|(_, n)| n.parse().ok()).unwrap_or(0)
}

/// WebIDL's "not enough arguments", with Chrome's construction-context prefix.
///
/// MEASURED, headless Chrome 150.0.7871.46 over a loopback server, 2026-09-12: `new XRWebGLLayer()`
/// answers `Failed to construct 'XRWebGLLayer': 2 arguments required, but only 0 present.` and
/// `new XRWebGLLayer(1)` the same sentence with `only 1 present.`. `argument` is singular at 1 and
/// `present` never is. The count is the interface object's own `length` in all 273 rows of the class.
pub(crate) fn arity_msg(name: &str, required: i32, present: i32) -> String {
    format!(
        "Failed to construct '{name}': {required} argument{} required, but only {present} present.",
        if required == 1 { "" } else { "s" }
    )
}

/// The interface object of an interface WebIDL gives NO constructor. See the block comment above.
pub(crate) fn cb_illegal_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        throw_type_error(scope, "Illegal constructor");
        return;
    }
    let msg = format!("Failed to construct '{}': Illegal constructor", ctor_name(scope, &args));
    throw_type_error(scope, &msg);
}

/// The interface object of an `[HTMLConstructor]` interface that is not `HTMLElement` itself.
///
/// Constructing one outside a custom-element upgrade is an "Illegal constructor" -- measured:
/// `new (class extends HTMLButtonElement {})()` answers
/// `Failed to construct 'HTMLButtonElement': Illegal constructor`, as does
/// `Reflect.construct(HTMLDivElement, [], Object)`. Calling one without `new` takes the WebIDL
/// "this is a constructor" path instead, because the interface DOES declare one.
/// `HTMLElement`'s own object is `core/html/html_element.rs`'s `cb_html_element_ctor`, which
/// really does construct for a registered definition and throws these same two messages otherwise.
pub(crate) fn cb_element_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = ctor_name(scope, &args);
    let msg = if args.new_target().is_undefined() { use_new_msg(&name) } else { format!("Failed to construct '{name}': Illegal constructor") };
    throw_type_error(scope, &msg);
}

/// WebIDL's "you called a constructor without `new`", with Chrome's construction-context prefix.
/// Measured on every one of the 365 window / 103 worker interface objects whose interface declares a
/// constructor: the prefix is there even though nothing is being constructed.
pub(crate) fn use_new_msg(name: &str) -> String {
    format!("Failed to construct '{name}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.")
}

/// A real `[[Construct]]` that throws the interface object's two messages, carrying its own name.
///
/// `ConstructorBehavior::Allow` is not a detail: with `Throw`, `new X()` fails inside V8 before any
/// callback runs and the message is V8's "<the binding> is not a constructor" -- which names the
/// EXPRESSION rather than the interface, and which Chrome answers for exactly one thing on the whole
/// surface (`NodeFilter`). Every other non-newable interface object in Chrome is constructible and
/// throws from its body.
#[track_caller]
pub(crate) fn build_interface_ctor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    name: &str,
    length: i32,
) -> v8::Local<'s, v8::Function> {
    // `"<name>|<required>"` -- see `ctor_name` / `ctor_required`: ONE string, a primitive a V8 template
    // can carry (it was a two-element Array). `length` IS the required-argument count:
    // that is what WebIDL defines an interface object's `length` to be, it is what the capture recorded
    // off the browser, and Chrome's arity message quotes the same number (verified on all 273 rows).
    let data: v8::Local<v8::Value> = str_val(scope, &format!("{name}|{length}"));
    let raw = cb.map_fn_to();
    let f = v8::Function::builder_raw(crate::binding::refs::c(raw)).data(data).length(length).constructor_behavior(v8::ConstructorBehavior::Allow).build(scope).unwrap();
    let n = v8::String::new(scope, name).unwrap();
    f.set_name(n);
    if crate::install::templates::capturing() {
        let data = Some(v8::Global::new(scope, data));
        crate::install::templates::record(scope, f, crate::install::templates::Recipe { cb: Some(raw), data, length, name: name.to_string(), ctor: true, guard: crate::install::templates::Guard::None, inner: None });
    }
    crate::install::templates::note_untemplated(scope, "build_interface_ctor", name, f);
    f
}

/// [`build_interface_ctor`] for an interface WebIDL gives no constructor.
pub(crate) fn build_illegal_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, length: i32) -> v8::Local<'s, v8::Function> {
    build_interface_ctor(scope, cb_illegal_ctor, name, length)
}

/// The hand-table `Model::Value` PLACEHOLDER interface object.
///
/// `new X()` returns the object V8 built from the interface object's `.prototype` -- the shape gate,
/// which is all a placeholder promises, and the live layer replaces it with the real body where the
/// engine has one. `X()` throws, because a WebIDL constructor is not callable, and a table that
/// carries a `ctor` row is a table whose IDL declares one.
///
/// It was `fn cb_ctor_noop(..) {}` before, and a no-op body cannot tell the two apart: it returned
/// `undefined` for the call. See `install::installer::create_interface_in` for the seven interfaces
/// this was measured on.
pub(crate) fn cb_table_value_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = use_new_msg(&ctor_name(scope, &args));
        throw_type_error(scope, &msg);
        return;
    }
    // WebIDL's overload resolution before any constructor step (the Chrome oracle ratchet):
    // the placeholder's `length` IS the required count, and `new ReadableStreamDefaultReader()`,
    // `new WritableStreamDefaultWriter()` and `new ClipboardItem()` minted objects where Chrome 150
    // throws "1 argument required, but only 0 present.".
    let required = ctor_required(scope, &args);
    if args.length() < required {
        let msg = arity_msg(&ctor_name(scope, &args), required, args.length());
        throw_type_error(scope, &msg);
    }
}

// `op_stub_ctor` IS GONE. It was the non-newable interface object named from JS, and it
// had had no caller since the JS that spelled it was deleted. What it built is untouched:
// `build_illegal_ctor` above is named directly by `install::installer::create_interface_in`,
// `core/dom/spine.rs` and `core/navigator/devices.rs`, which is invariant I7's "one stub runtime".

/// A shell constructor for an interface WebIDL DOES give one.
///
/// [`build_illegal_ctor`] throws "Illegal constructor" for every shell, which is right for the
/// interfaces that declare no constructor and wrong for the 114 that do -- and the wrong answer is
/// indistinguishable from the right one, so it went unnoticed across the whole shell surface.
///
/// With `ConstructorBehavior::Allow`, V8 has already built `this` from the function's `.prototype`
/// before the callback runs, so returning nothing returns that object: `new TransformStream()` yields
/// an object whose prototype chain and `Symbol.toStringTag` are the interface's, which is exactly as
/// real as the rest of a shell. Calling WITHOUT `new` still throws, because a WebIDL constructor is
/// not callable.
///
/// IT CHECKS ARITY FIRST. WebIDL's overload-resolution step runs before any constructor
/// body, so an interface whose constructor requires arguments rejects a short call whatever the body
/// would have done -- 206 window and 67 worker interface objects in Chrome 150, every one of which
/// this minted a real object for until the row learned to carry the count. Minting where the browser
/// throws is a worse defect than a wrong message, because a page can branch on it.
pub(crate) fn cb_shell_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        // WITH the construction-context prefix. Measured -- `Blob()` answers
        // `Failed to construct 'Blob': Please use the 'new' operator, ...` in both realms, and so
        // does every other interface object whose interface declares a constructor.
        let msg = use_new_msg(&ctor_name(scope, &args));
        throw_type_error(scope, &msg);
        return;
    }
    let required = ctor_required(scope, &args);
    if args.length() < required {
        // BEFORE the branding and the Event steps below, because that is where WebIDL puts it: the
        // arity check is the overload-resolution step, and Chrome answers it for a subclass and for
        // `Reflect.construct(X, [], Object)` too -- with the INTERFACE's name and count, not
        // `new.target`'s (measured on `class Q extends CloseEvent {}; new Q()`).
        let msg = arity_msg(&ctor_name(scope, &args), required, args.length());
        throw_type_error(scope, &msg);
        return;
    }
    // ~80 of the shells are Event subclasses (AnimationEvent, BlobEvent, ...). `isTrusted` is
    // [LegacyUnforgeable], so in a browser it is an OWN accessor on every event object and lives on no
    // prototype -- including Event.prototype, which is why Korvus does not put it there either. These
    // shells never run the JS `Event` constructor that installs it, so install it here for anything that
    // really is an Event; a shell for a non-event interface is left alone.
    let this = args.this();
    brand_shell_instance(scope, this);
    // ... and the INTERFACE's OWN brand. A row whose body is a branded slot getter
    // checks `korvus_brand_<Iface>`, which only a table constructor used to stamp -- so the moment
    // `SubmitEvent.submitter` stopped being a D2 stub, `new SubmitEvent("submit").submitter` would
    // have thrown `Illegal invocation` where Chrome answers `null`. A shell instance IS an instance
    // of its interface; `has_brand`'s own doc says so ("or of a Rust mint site that ran its init").
    let iface = ctor_name(scope, &args);
    crate::install::webidl::brand(scope, this, &iface);
    // The CSS Typed OM's values (`core/css/typed_om.rs`): their arguments' TYPES are the checks.
    if crate::core::css::typed_om::construct(scope, &iface, this, &args) {
        return;
    }
    // The shells with Rust bodies over their constructor arguments.
    if iface == "VTTCue" {
        crate::core::html::media_objects::vtt_cue_init(scope, this, &args);
        return;
    }
    if iface == "EditContext" {
        crate::core::html::edit_context::edit_context_init(scope, this, &args);
        return;
    }
    // WebXR's two math constructors: overloads and normalization the generic conversion cannot express.
    if iface == "XRRigidTransform" {
        crate::core::xr::rigid_init(scope, this, &args);
        return;
    }
    if iface == "XRRay" {
        crate::core::xr::ray_init(scope, this, &args);
        return;
    }
    if iface == "PasswordCredential" || iface == "FederatedCredential" {
        crate::core::navigator::credentials::credential_init(scope, &iface, this, &args);
        return;
    }
    // The constructor's arguments -- its dictionary included -- converted as WebIDL converts them
    // (Blink's IDL): a missing `required` member, an object of the wrong interface or a bad enum value
    // throws here, before any constructor step -- `new BlobEvent("x", {})` and
    // `new USBInTransferResult("bogus")` are TypeErrors in Chrome (the oracle ratchet).
    if crate::core::misc_ifaces::pre_ctor(scope, &iface, &args) {
        return;
    }
    let converted = match crate::install::ctor_dicts::convert_ctor_args(scope, &iface, &args) {
        Ok(c) => c,
        Err(()) => return,
    };
    // What Blink's constructor refuses in its CONVERTED arguments, before it makes the object.
    if crate::core::misc_ifaces::refused_ctor(scope, &iface, converted.as_ref()) {
        return;
    }
    let is_event =crate::binding::protos::iface_object(scope, "Event")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|ctor| this.instance_of(scope, ctor))
        .unwrap_or(false);
    if is_event {
        // THE `Event(type, eventInitDict)` STEPS, for a shell that IS an Event.
        //
        // ~80 registry interfaces inherit from `Event` -- BlobEvent, CompositionEvent, GamepadEvent,
        // StorageEvent, PopStateEvent, ... -- and their constructor is this one. It used to install
        // `isTrusted` and nothing else, so `new BlobEvent("x").type` was `undefined`, `.bubbles`
        // `undefined` and `.timeStamp` `undefined`, where every browser answers "x", false and a
        // positive DOMHighResTimeStamp. WPT said so in two places this unit measured:
        // `dom/events/Event-subclasses-constructors.html` failed all seven CompositionEvent subtests
        // on `data`/`type`, and `Event-timestamp-high-resolution.https.html` failed on a
        // GamepadEvent whose timeStamp was not a number at all.
        //
        // The steps are `core/events/ctor.rs`'s -- the SAME ones the nineteen hand tables run -- so
        // there is one definition of what constructing an Event does, whichever layer built the
        // interface. They also stamp the `Event` brand `Event.prototype`'s rows check.
        //
        // The interface's OWN members are untouched: a shell has D2 stub rows for those and they keep
        // answering their shape default. This makes the INHERITED half real, not the whole interface.
        let type_str = args.get(0).to_rust_string_lossy(scope);
        let dict = match converted.as_ref().and_then(|c| c.dict) {
            Some((1, c)) => Some(c),
            _ => v8::Local::<v8::Object>::try_from(args.get(1)).ok(),
        };
        // Chrome 150 ignores `bubbles` / `cancelable` / `composed` in this one's dictionary (the probe).
        let base = if iface == "VirtualKeyboardGeometryChangeEvent" { None } else { dict };
        crate::core::events::ctor::init_event(scope, this, &type_str, base);
        // A shell under UIEvent / MouseEvent runs their init steps too (CompositionEvent's
        // `detail`, every one of DragEvent's MouseEvent members).
        let is_a = |scope: &mut v8::PinScope, name: &str| {
            crate::binding::protos::iface_object(scope, name)
                .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
                .and_then(|ctor| this.instance_of(scope, ctor))
                .unwrap_or(false)
        };
        if is_a(scope, "MouseEvent") {
            if crate::core::events::ui_event::init_ui_event(scope, this, dict).is_err()
                || crate::core::events::mouse_event::init_mouse_event(scope, this, dict, &iface).is_err()
            {
                return;
            }
            crate::install::webidl::brand(scope, this, &iface);
        } else if is_a(scope, "UIEvent") && crate::core::events::ui_event::init_ui_event(scope, this, dict).is_err() {
            return;
        }
        // The interface's own dictionary members.
        if iface == "StorageEvent" {
            crate::core::events::event::storage_event_from_dict(scope, this, dict);
        } else {
            crate::core::events::event::shell_members_from_dict(scope, this, &iface, dict);
        }
    } else {
        let dict = converted.as_ref().and_then(|c| c.dict).map(|(_, d)| d).or_else(|| v8::Local::<v8::Object>::try_from(args.get(0)).ok());
        if let Some(c) = converted.as_ref() {
            crate::install::ctor_dicts::ctor_values_to_slots(scope, this, &iface, c);
        }
        // State Blink's constructor computes from those arguments (`core/misc_ifaces.rs`).
        crate::core::misc_ifaces::post_ctor(scope, &iface, this, &args, dict, converted.as_ref().map(|c| c.named));
    }
}
// `op_shell_ctor` IS GONE TOO, for the same reason as `op_stub_ctor`: it was the JS
// spelling of "build me this shell's constructor" and no JS spells anything any more. `cb_shell_ctor`
// is untouched -- `install::installer` names it for every `(Flavor::Shell, Model::Value)` row, and
// `install::live::media` for `VTTCue`, through `build_interface_ctor`: the callback reads its interface
// name AND its required argument count out of `data`, which that builder binds.

// `op_nativize`, `op_nativize_ctor` AND THEIR TWO CALLBACKS ARE GONE.
//
// `op_nativize` wrapped a JS function in a native-looking one so that
// `Function.prototype.toString` would print `[native code]` over a body that was not native. That
// is a lie told to the page, and it was kept because W4.f measured that 44 functions stopped
// reporting `[native code]` without the sweep that used it. All 44 lived in
// `core/korvus-runtime/src/js/`, which no longer exists; W9.23 re-measured with the sweep removed
// and the whole-surface diff was 2 leaves of 94,425, both clocks. It masked nothing, and a patched
// `toString` is itself a first-order tell.
//
// `op_nativize_ctor` was the newable variant, documented as serving the three legacy factory
// constructors. It had NO caller at all by the time it was deleted: `core/html/legacy_factories.rs`
// builds `Audio` and `Option` (and `install::live::html_image` builds `Image`) over their own Rust
// callbacks -- from templates (`template_ctor`). The doc comment outlived the fact.
//
// `cb_call_js` and `cb_construct_js` went with them -- each had exactly one caller, the op above it.
// Nothing in the tree hands a JS function to the page as a native one now, which is what invariant
// I3 says in the first place.

// ---------------------------------------------------------------------------------------------
// The SVG element FACTORY used to be here: an op walking a SECOND generated table
// (core/dom/generated_svg.rs::SVG_FACTORY, local tag -> interface) into `SvgTagProtos`, called from
// dom/09k-svg.js during the ESM layer. It was a duplicate of `korvus_dom::tags::SVG_TAGS`, which
// `install::dom_chain::fill_dom_protos` walks into the SAME map -- and the same pass sets
// `SvgElementProto` -- so the table stated tag -> interface twice.
// deleted the table, the op and the generator's Rust output; `fill_dom_protos` is the one element
// factory for BOTH namespaces, and nothing mints an element wrapper before it runs.

/// F3 glue-body: the recorded media-codec support grid (`MEDIA_CONTAINERS`) as its JSON string, for the
/// JS codec grammar (dom/00c-media-codecs.js) to `JSON.parse` once and reduce to its query TABLE. The
/// DATA moved to Rust (generated_media_codecs.rs); the parser/answer logic stays JS (algorithm, not a
/// table). Retires generated_media_codecs.js.

// `op_refresh_node_proto` IS GONE. It re-ran [`node_proto`] for an element whose
// wrapper was minted BEFORE its namespace was known -- the JS `createElementNS`, which picked a
// prototype while the node still looked HTML-namespaced and then called this once it did not. The
// Rust `createElementNS` needs no second pass: it creates the node in its namespace, so
// `node_proto` reads the right answer the first time, and no LATER wrapper for the same node
// (from `querySelector` or a collection walk) needs one either. Measured: `createElementNS(svg,
// "svg")` is [object SVGSVGElement] and `createElementNS(svg, "rect")`'s prototype IS
// `SVGRectElement.prototype`, with the op deleted.


/// A tag map's entry for `tag`, whose interface the page put lazily: the interface made now (its
/// first read, `shell_proto` -> `templates::realm_interface`), its prototype kept in the map (`map` 0 HTML, 1 SVG,
/// 2 MathML) as `install::dom_chain::fill_dom_protos` keeps an eager one's.
///
/// MADE IN THE PAGE'S REALM, whichever context asks. The map is the page's, and a frame or an isolated
/// world translates the page's prototype into its own (`proto_in_realm`). Made under an isolated world's context --
/// Playwright's utility world wrapping a parsed <input> before the page did -- the WORLD's prototype went into the
/// page's map. The world then minted the <input> as an `HTMLElement` (no `type`: `page.fill` threw), and the page's own
/// wrappers took the world's prototype.
fn lazy_tag_proto<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, map: u8, tag: &'static str, iface: &'static str) -> Option<v8::Local<'s, v8::Object>> {
    let page = crate::core::html::browsing_context::main_context(scope);
    let g = {
        let scope = &mut v8::ContextScope::new(scope, page);
        let p = shell_proto(scope, iface)?;
        v8::Global::new(scope, p)
    };
    let p = v8::Local::new(scope, &g);
    let st = op_state.borrow();
    match map {
        0 => {
            st.borrow::<TagProtos>().0.borrow_mut().insert(tag.to_ascii_lowercase(), g);
        }
        1 => {
            st.borrow::<SvgTagProtos>().0.borrow_mut().insert(tag.to_string(), g);
        }
        _ => {
            st.borrow::<MathmlTagProtos>().0.borrow_mut().insert(tag.to_string(), g);
        }
    }
    Some(p)
}

/// The prototype node `id`'s wrapper takes -- its interface's, by node type, namespace and local name -- or
/// `None` for the class prototype's default (an undefined custom element name).
pub(crate) fn node_proto<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    // Every prototype below is read IN PLACE, under one borrow of the state: cloning a cached `Global`
    // creates and releases a global handle per wrapper, which was ~17 % of `createElement`
    // (measured 2026-09-29), and the local name is lowercased only when it has an upper-case letter.
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    // A shadow root reuses nodeType 9 (Document) but must carry ShadowRoot.prototype, not the default
    // HTMLElement chain -- check is_shadow_root FIRST (Korvus GS ②-A3), since nodeType can't tell a
    // shadow root apart from a detached document root.
    if dom.is_shadow_root(id) {
        if let Some(pg) = st.borrow::<ShadowRootProto>().0.borrow().as_ref() {
            return Some(v8::Local::new(scope, pg));
        }
    }
    let nt = dom.node_type(id);
    if nt != 1 {
        return st.borrow::<NodeProtos>().0.borrow().get(&nt).map(|pg| v8::Local::new(scope, pg));
    }
    // The local name, not the tag: `createElementNS` keeps the whole QUALIFIED name as the tag
    // (`svg:rect`), and the factories are keyed by the local half -- `QNameMap`'s when
    // `createElementNS` recorded one, the arena tag otherwise.
    let qnames = st.borrow::<QNameMap>().0.borrow();
    let local = qnames.get(&id).map(|(_, l)| l.as_str()).or_else(|| dom.tag_name(id));
    // A FOREIGN-namespaced element selects from its own namespace's factory, NOT the HTML tag map.
    // This has to come before the HTML lookup because local names live in more than one namespace and
    // mean different interfaces there: `createElementNS(svgNS,"script")` is an SVGScriptElement while
    // `<script>` is an HTMLScriptElement, and the same holds for `a`, `style`, `title` and `text`.
    // Keyed by the as-authored local name, since foreign names are case-sensitive (`clipPath`,
    // `feBlend`, `annotation-xml`).
    //
    // The namespace is a field on the arena node, so this answers for a PARSER-created element --
    // an inline `<svg><rect>` or `<math><mi>` -- exactly as it does for a `createElementNS` one.
    // While the fact lived in a side map only the factory wrote, every `<svg>` in markup came out
    // `HTMLUnknownElement`.
    let ns = dom.element_ns(id);
    let (is_svg, is_mathml) = (matches!(ns, Some(korvus_dom::ElementNs::Svg)), matches!(ns, Some(korvus_dom::ElementNs::MathMl)));
    if is_svg || is_mathml {
        let tags = if is_svg { st.borrow::<SvgTagProtos>().0.borrow() } else { st.borrow::<MathmlTagProtos>().0.borrow() };
        if let Some(pg) = local.and_then(|l| tags.get(l)) {
            return Some(v8::Local::new(scope, pg));
        }
        // A tag whose interface the page put lazily: made now, and kept in the map.
        let table = if is_svg { korvus_dom::tags::SVG_TAGS } else { korvus_dom::tags::MATHML_TAGS };
        if let Some((tag, iface)) = local.and_then(|l| table.iter().find(|(t, _)| *t == l)).copied() {
            if crate::install::templates::lazy_tag_iface(scope, iface) {
                drop(tags);
                drop(qnames);
                drop(st);
                return lazy_tag_proto(scope, op_state, if is_svg { 1 } else { 2 }, tag, iface);
            }
        }
        // The namespace's own `fallbackJSInterfaceName` is the MISS path -- SVGElement /
        // MathMLElement, never HTMLUnknownElement (headless Chrome 150, measured).
        let fallback = if is_svg { st.borrow::<SvgElementProto>().0.borrow() } else { st.borrow::<MathmlElementProto>().0.borrow() };
        return fallback.as_ref().map(|pg| v8::Local::new(scope, pg));
    }
    // NO namespace, or one that is not HTML: an `Element`, whatever the local name (Chrome 150:
    // `createElementNS("http://example.com/x", "div")`, `createElementNS(null, "foo")`, an XML parser's
    // `<div/>` in `urn:x`). The HTML tag map below answered them -- HTMLUnknownElement, or HTMLDivElement.
    if matches!(ns, Some(korvus_dom::ElementNs::Null) | Some(korvus_dom::ElementNs::Other(_))) {
        return st.borrow::<ElementProto>().0.borrow().as_ref().map(|pg| v8::Local::new(scope, pg));
    }
    // THE LOCAL NAME, NOT THE TAG -- the same correction the SVG/MathML arm above already
    // carries, and for the same reason: `createElementNS` keeps the whole QUALIFIED name as the
    // arena tag, so `createElementNS(htmlNS, "foo:table")` looked itself up in `TagProtos` under
    // `"foo:table"`, missed, and came back an `HTMLUnknownElement` -- `table.createTHead is not
    // a function`, which is how five WPT subtests across `tHead`, `tFoot`, `insertCell`,
    // `createTBody` and `table-insertRow` report it. Chrome answers `HTMLTableElement`
    // (measured here, headless 150). `local` is `QNameMap`'s local half when `createElementNS`
    // recorded one and the arena tag otherwise, so a PARSED `<a:table>` -- which is one element
    // whose local name really is `a:table` -- still misses and still comes back
    // `HTMLUnknownElement`, exactly as Chrome has it.
    let local = local?;
    let tag: std::borrow::Cow<str> = if local.bytes().any(|b| b.is_ascii_uppercase()) { local.to_ascii_lowercase().into() } else { local.into() };
    // A tag with no interface of its own is `HTMLUnknownElement`, not `HTMLElement` -- except when
    // its name is a valid custom element name (it contains a hyphen), which IS `HTMLElement` even
    // before anything is defined for it. Recorded from Brave 151 on this machine:
    //
    //   createElement("zzznotatag") -> HTMLUnknownElement      createElement("foo-bar") -> HTMLElement
    //   createElement("blink")      -> HTMLUnknownElement      createElement("marquee") -> HTMLMarqueeElement
    //   createElement("isindex")    -> HTMLUnknownElement      createElement("xmp")     -> HTMLPreElement
    //
    // A miss used to set NO prototype at all, leaving the wrapper on HTMLElement.prototype, so
    // `document.createElement(x).constructor.name` differed from the reference browser for every
    // unrecognised tag -- a one-expression probe, and one a page hits by accident on any markup
    // with a tag this engine does not know.
    if let Some(pg) = st.borrow::<TagProtos>().0.borrow().get(tag.as_ref()) {
        return Some(v8::Local::new(scope, pg));
    }
    // a tag whose interface the page put lazily, as above
    if let Some((t, iface)) = korvus_dom::tags::HTML_TAGS.iter().find(|(t, _)| t.eq_ignore_ascii_case(tag.as_ref())).copied() {
        if crate::install::templates::lazy_tag_iface(scope, iface) {
            drop(qnames);
            drop(st);
            return lazy_tag_proto(scope, op_state, 0, t, iface);
        }
    }
    // The custom-element-name test is the HTML spec's: a hyphen, and a letter first.
    if tag.contains('-') && tag.starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    let unknown = st.borrow::<UnknownElementProto>();
    let proto = unknown.0.borrow();
    proto.as_ref().map(|pg| v8::Local::new(scope, pg))
}

// THE WHOLE `KB.__*` HELPER-MASK FAMILY IS GONE: `op_helper_get`, `op_helper_get_cfg`,
// `op_helper_set`, `op_helper_set_cfg`, `op_helper_method` and their five callbacks. Each op built a
// genuine native function -- right `name`, `length`, no own `prototype`, `[native code]` -- whose
// body did nothing but look a name up on the hidden bridge and call the JS behind it. That is the
// mask invariant I3 deletes, and it is what the contract means by "no new native mask".
//
// They survived the JS deletion because a mask is invisible: each callback had exactly ONE caller,
// the op above it, and each op had none, so the family compiled and registered and answered nothing.
// `kb_slots` has been 0 since the JS that wrote those slots went, so there was not even a bridge
// entry left to find. `bridge_get` STAYS -- `core/dom/document.rs` (`__curScript`),
// `core/workers/worker.rs` (`__workerScripts`) and `core/html/html_script_element.rs` read it from
// Rust, which is a host->page read and not a member body.
//
// The 281 SVG animated-attribute getters that `op_helper_get_cfg` existed for (one helper, a
// per-member config in the function's hidden `data`, instead of 281 names on the bridge) are table
// rows now; `install::live::svg` mounts them from Rust.
//
// The members whose doc comments around the tree still name one of these ops --
// `getBoundingClientRect`, `Document.elementFromPoint`, `getElementsByTagName`,
// `HTMLLinkElement.sheet`, the CSSOM cssText pair, the anchor/area/option/label reflections -- are
// Rust table rows and have been for some time. Those comments are history, not a call graph.

/// A `TypeError` with `msg`, thrown. A message that begins with Blink's operation context ("Failed to execute 'm'
/// on 'T': ...") is made as Blink makes it (`deno_core::error::native_error`): the error made with the
/// detail, then `message` redefined to the whole text by `CreateDataProperty` -- V8's exception-propagation callback
/// does that in Chrome, after the binding threw -- so `stack` reads "TypeError: <detail>", `message` the whole text,
/// and `Object.keys(e)` is `["message"]`, as stock Chrome 150's (`tests/stack_format_live.rs`).
pub fn throw_type_error(scope: &mut v8::PinScope, msg: &str) {
    let e = deno_core::error::native_error(scope, "TypeError", msg);
    scope.throw_exception(e);
}

/// WebIDL arity: a non-optional argument that is absent is a TypeError, not a defaulted value.
///
/// Returns `true` when the call may proceed; when it may not, the exception is already thrown and the
/// caller must return immediately.
///
/// Missing this is a whole CLASS of divergence rather than a bug in one member: `usp.get()` returned
/// null, `headers.set()` silently stored the string "undefined", `fd.has()` answered false. Every one
/// looked like a working member to a hand-written behavior case, because nobody writes a case that
/// calls a getter with no arguments. The generated corpus calls everything with none, and found 17 of
/// these at once.
///
/// The message is Chrome's, verbatim, down to the pluralisation and the trailing period -- a page can
/// read `e.message`, and "1 argument required, but only 0 present." is as much a fingerprint as the
/// error type is.
pub(crate) fn require_args(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    need: usize,
    iface: &str,
    method: &str,
) -> bool {
    let have = args.length() as usize;
    if have >= need {
        return true;
    }
    // `exec_arity_msg` is THE place this sentence is spelled; this used to spell it inline, and a
    // second spelling of a Chrome message is how the first one drifts (the ruling, applied
    // to the METHOD form by ARITY1 so the reject path below can answer the identical bytes).
    throw_type_error(scope, &exec_arity_msg(iface, method, need, have));
    false
}

/// WebIDL's "not enough arguments" for a METHOD, with Chrome's EXECUTION-context prefix -- the twin
/// of [`arity_msg`], which carries the CONSTRUCTION-context prefix for `new X()`.
///
/// The message is Chrome's, verbatim, down to the pluralisation and the trailing period -- a page can
/// read `e.message`, and "1 argument required, but only 0 present." is as much a fingerprint as the
/// error type is. Measured on Chrome 150.0.7871.46 `--headless=new`, 2026-09-23: `document
/// .getElementById()` answers `Failed to execute 'getElementById' on 'Document': 1 argument
/// required, but only 0 present.` and `crypto.subtle.digest()` `... 2 arguments required ...`.
///
/// ONE PRODUCER, because the sentence now has two DELIVERIES: thrown ([`require_args`]) for an
/// ordinary member, and carried by a rejected promise ([`raise_webidl_type_error`]) for a
/// promise-returning one. Two deliveries of one sentence must not become two spellings of it.
pub(crate) fn exec_arity_msg(iface: &str, method: &str, need: usize, have: usize) -> String {
    let plural = if need == 1 { "argument" } else { "arguments" };
    format!("Failed to execute '{method}' on '{iface}': {need} {plural} required, but only {have} present.")
}

/// Deliver a WebIDL `TypeError` the way the MEMBER'S OWN RETURN TYPE requires: thrown, or -- when the
/// operation returns a promise -- as a rejection of that promise.
///
/// `rejects` is [`crate::install::TypeTag::rejects_rather_than_throws`], which is where the rule
/// itself is written down and measured. This function is only the DELIVERY.
///
/// `install::receiver::cb_receiver_guard` deliberately does NOT call it and spells its three arms
/// inline instead -- one extra native frame on that callback's deeply recursive path is enough to
/// turn a WPT file into V8's `Check failed: IsOnCentralStack()` process abort in a debug build. That
/// is measured, and the reason is written at that function. Do not "unify" it back without re-running
/// the file named there.
pub(crate) fn raise_webidl_type_error(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str, rejects: bool) {
    if rejects {
        let p = rejected_type_error(scope, msg);
        rv.set(p);
    } else {
        throw_type_error(scope, msg);
    }
}

/// [`require_args`] for a CONSTRUCTOR: Chrome says "Failed to **construct** 'X'", never "Failed to
/// execute 'X' on 'X'".
///
/// Lifted here from `core/html/webgpu.rs`, which had the only copy, when a change made a generated
/// row's constructor body visible to `tests/installer_parity`'s golden and the second constructor
/// using the wrong one showed up in it: `new OffscreenCanvas()` answered "Failed to execute
/// 'OffscreenCanvas' on 'OffscreenCanvas': 2 arguments required, but only 0 present." where headless
/// Chrome 150.0.7871.46 answers "Failed to construct 'OffscreenCanvas': 2 arguments required, but
/// only 0 present." (measured 2026-09-12). One copy, so the third constructor cannot pick the wrong
/// sibling.
/// The `new`-operator check every WebIDL constructor owes, and it comes FIRST.
///
/// Chrome 150.0.7871.46, measured headless 2026-09-12 over a loopback server: `OffscreenCanvas()`,
/// `ChannelMergerNode()`, `PeriodicWave()` and `GPUValidationError()` all answer **"Failed to
/// construct 'X': Please use the 'new' operator, this DOM object constructor cannot be called as a
/// function."** — the new-target check before the argument count, and the interface name in front of
/// it. Twelve hand tables already spell that exact sentence inline; this is the same sentence in one
/// place, for the bodies that reached a generated row's `ctor` and had only an arity check
/// at the top, so `OffscreenCanvas()` answered "2 arguments required" where Chrome answers this.
pub(crate) fn require_ctor_new_target(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str) -> bool {
    if !args.new_target().is_undefined() {
        return true;
    }
    // `use_new_msg` is THE place this sentence is spelled; this used to respell it.
    let msg = use_new_msg(iface);
    throw_type_error(scope, &msg);
    false
}

pub(crate) fn require_ctor_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, need: usize, iface: &str) -> bool {
    let have = args.length() as usize;
    if have >= need {
        return true;
    }
    // `arity_msg` is THE place this sentence is spelled; this used to respell it, and a
    // second spelling of a Chrome message is how the first one drifts.
    let msg = arity_msg(iface, need as i32, have as i32);
    throw_type_error(scope, &msg);
    false
}

/// Wrap an existing native method in a WebIDL arity check, keeping it native.
///
/// `#[op2] impl` methods cannot see `arguments.length`: deno_ops has no varargs, and a missing
/// `#[string] k: String` arrives as V8 `undefined`, which stringifies to "undefined". So
/// `headers.get()` looked up a header named "undefined" and answered null where Chrome throws, and
/// `fd.has()` answered false. `Option<String>` does not fix it either -- that would make an EXPLICIT
/// `get(undefined)` throw, which Chrome does not, trading one divergence for another.
///
/// This takes the already-installed method as the wrapper's `data`, checks the count, and forwards.
/// Redefining an existing prototype key keeps its position, so the baseline enumeration order is
/// untouched, and the wrapper is a real native function with the member's own name and length.
fn cb_arity_guard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // data = [need, "Iface", "member", originalFn, tyOrdinal] -- packed at install time, read per
    // call. Slot 4 was a bare `isVoid` boolean until ARITY1; it is the ROW'S TYPE TAG now, because
    // the return type answers two questions here and a bool could only carry one of them.
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let need = d.get_index(scope, 0).and_then(|v| v.integer_value(scope)).unwrap_or(0) as usize;
    let ty = crate::install::TypeTag::from_ordinal(d.get_index(scope, 4).and_then(|v| v.integer_value(scope)).unwrap_or(9));
    let have = args.length() as usize;
    if have < need {
        let iface = d.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        let member = d.get_index(scope, 2).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        // A PROMISE-RETURNING OPERATION DOES NOT THROW -- it rejects. Measured (see
        // `TypeTag::rejects_rather_than_throws`): `navigator.permissions.query()` answers a promise
        // rejected with this exact sentence in Chrome 150, where this guard used to throw it.
        // Same sentence from `exec_arity_msg` either way; only the delivery differs.
        let msg = exec_arity_msg(&iface, &member, need, have);
        raise_webidl_type_error(scope, &mut rv, &msg, ty.rejects_rather_than_throws());
        return;
    }
    let Some(orig) = d.get_index(scope, 3).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    call_through(scope, &args, rv, ty, orig);
}

/// Call the guarded function with this call's receiver and arguments, and hand back what it returns.
fn call_through(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, ty: crate::install::TypeTag, orig: v8::Local<v8::Function>) {
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let this: v8::Local<v8::Value> = args.this().into();
    let is_void = ty.is_undefined_return();
    let r = orig.call(scope, this, &argv);
    // A WebIDL `undefined` return is not `null`. op2 renders a Rust `()` as null, so
    // `headers.append(k, v)` answered null where Chrome answers undefined -- a difference a page sees
    // and one no hand-written case looked for. Leaving `rv` untouched IS undefined.
    if is_void {
        return;
    }
    if let Some(r) = r {
        rv.set(r);
    }
}

/// Build the arity-enforcing wrapper: a native function that enforces the member's WebIDL arity,
/// forwards to `orig`, and returns `undefined` rather than whatever op2 rendered when the IDL return
/// type is `undefined`.
///
/// The table installer's arity pass is the ONE caller shape now. There used to be a second, the
/// `#[op2]` wrapper `op_arity_guard` that JS named by string; it is deleted, because the JS
/// that spelled it is gone and a guard has to be built exactly one way to be the same function.
/// `ty` IS THE ROW'S OWN `TypeTag`, not a derived bool. It used to be `is_void: bool`, and that bool
/// was the only type information the guard carried -- so the guard could render a WebIDL `undefined`
/// return correctly but had nowhere to learn that a `Promise` row must REJECT its argument-count
/// refusal rather than throw it (ARITY1). Both facts come off the one tag now:
/// [`crate::install::TypeTag::is_undefined_return`] and
/// [`crate::install::TypeTag::rejects_rather_than_throws`]. A caller with no table row behind it
/// passes `TypeTag::Any`, which is this tree's tag for "no IDL type recorded".
pub fn make_arity_guard<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    member: &str,
    need: i32,
    length: i32,
    orig: v8::Local<'s, v8::Value>,
    ty: crate::install::TypeTag,
) -> v8::Local<'s, v8::Function> {
    let d = v8::Array::new(scope, 5);
    let n = v8::Integer::new(scope, need);
    let i = v8::String::new(scope, iface).unwrap();
    let m = v8::String::new(scope, member).unwrap();
    d.set_index(scope, 0, n.into());
    d.set_index(scope, 1, i.into());
    d.set_index(scope, 2, m.into());
    d.set_index(scope, 3, orig);
    let vd: v8::Local<v8::Value> = v8::Integer::new(scope, ty.ordinal()).into();
    d.set_index(scope, 4, vd);
    let g = build_fn_data(scope, cb_arity_guard, member, length, d.into());
    if crate::install::templates::capturing() {
        if let Ok(inner) = v8::Local::<v8::Function>::try_from(orig) {
            let inner = Some(v8::Global::new(scope, inner));
            let guard = crate::install::templates::Guard::Arity { need: need.max(0) as usize, iface: iface.to_string(), member: member.to_string(), ty };
            crate::install::templates::record(scope, g, crate::install::templates::Recipe { cb: None, data: None, length, name: member.to_string(), ctor: false, guard, inner });
        }
    }
    // Brand the wrapper so a re-run of the arity pass (INSTALL_DOCUMENT re-guards the few prototypes
    // finished after boot) skips it instead of double-wrapping -- the Rust equivalent of the JS
    // `guarded` WeakSet. Double-wrapping is observably identical (same name/length/throw), so this is
    // an idempotency optimisation, not a correctness fix.
    let go: v8::Local<v8::Object> = g.into();
    let key = arity_guard_brand(scope);
    let t = v8::Boolean::new(scope, true);
    go.set_private(scope, key, t.into());
    g
}

fn arity_guard_brand<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    private_key(scope, "korvus.arityGuarded")
}

/// Mark `f` as carrying the WebIDL argument-count check. `make_arity_guard` stamps what it builds;
/// `install::receiver`'s wrapper stamps itself when it wraps an already-guarded function, because the
/// guarded PAIR still counts arguments and the arity pass must skip it rather than wrap again from
/// the outside — which would put the count before the receiver and reverse Chrome's order.
pub(crate) fn stamp_arity_guarded(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) {
    let o: v8::Local<v8::Object> = f.into();
    let key = arity_guard_brand(scope);
    let t = v8::Boolean::new(scope, true);
    o.set_private(scope, key, t.into());
}

fn receiver_guard_brand<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    private_key(scope, "korvus.receiverGuarded")
}

/// Mark `f` as `install::receiver`'s wrapper, so a second install pass over the same slot (the
/// backfill, or `INSTALL_DOCUMENT`'s re-run over the prototypes finished after boot) recognises its
/// own work instead of wrapping it twice.
pub(crate) fn stamp_receiver_guarded(scope: &mut v8::PinScope, f: v8::Local<v8::Function>) {
    let o: v8::Local<v8::Object> = f.into();
    let key = receiver_guard_brand(scope);
    let t = v8::Boolean::new(scope, true);
    o.set_private(scope, key, t.into());
}

/// Whether `v` already carries `install::receiver`'s check.
pub(crate) fn is_receiver_guarded(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let key = receiver_guard_brand(scope);
    o.get_private(scope, key).map(|x| x.is_true()).unwrap_or(false)
}

/// Whether `v` is an arity-guard wrapper this module built (see `make_arity_guard`). The arity pass
/// checks this to stay idempotent across the boot pass and the INSTALL_DOCUMENT re-run.
pub(crate) fn is_arity_guarded(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let key = arity_guard_brand(scope);
    o.get_private(scope, key).map(|x| x.is_true()).unwrap_or(false)
}

/// Create-or-return the canonical wrapper for `id` (native intern primitive), retyped per nodeType./// Create-or-return the canonical wrapper for `id` (native intern primitive), retyped per nodeType./// Create-or-return the canonical wrapper for `id` (native intern primitive), retyped per nodeType./// Create-or-return the canonical wrapper for `id` (native intern primitive), retyped per nodeType.
/// The constructor node and document wrappers are instantiated from: an API function whose template
/// has NO class name. V8 names a receiver (stack frames, `at X.get`) after its map's constructor when
/// that constructor has a name, and deno_core's per-type template carries the Rust struct's --
/// korvus printed `at JsElement.get` where Chrome prints `at HTMLBodyElement.get`. With
/// the name empty V8 falls back to the prototype chain (`Symbol.toStringTag`, then `constructor`),
/// which is the interface's: Chrome's answer for every built-in node, and for a `new`-constructed
/// custom element too.
struct AnonWrapperCtor(v8::Global<v8::ObjectTemplate>);

/// Per wrapped type, the prototype its deno_core class template gives an instance -- `HTMLElement
/// .prototype` for `JsElement`, `Document.prototype` for `JsDocument` (the spine installs the tables
/// onto those class prototypes). A wrapper starts there exactly as before; only the name its map
/// carries changed.
#[derive(Default)]
struct ClassProtos(std::cell::RefCell<std::collections::HashMap<std::any::TypeId, v8::Global<v8::Value>>>);

/// An object the engine made some other way -- its own template with interceptors, a realm's global, one made
/// before its interface existed -- given interface prototype `proto` AND that interface's brand
/// (`binding::brand`), which an instance of the interface's template carries from birth.
/// The interface is `proto`'s or the first one up its chain (a page's subclass); a prototype that belongs to no
/// interface brands nothing, and another interface's prototype is never branded.
pub fn adopt_as(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, proto: v8::Local<v8::Value>) {
    obj.set_prototype(scope, proto);
    if !brand_as(scope, obj, proto) && crate::install::templates::reporting() {
        let n = v8::Local::<v8::Object>::try_from(proto).ok().and_then(|p| iface_name_of(scope, p)).map(|n| n.to_rust_string_lossy(scope));
        crate::install::templates::report(&format!("ADOPT-NOID stamp={n:?}"));
    }
}

/// [`adopt_as`]'s brand half, for `obj` already on `proto`: false when `proto` belongs to no interface.
fn brand_as(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, proto: v8::Local<v8::Value>) -> bool {
    // the interface: `proto`'s, else the first up its chain (a page's subclass prototype sits above it)
    let mut cur = Some(proto);
    let mut id = None;
    for _ in 0..16 {
        let Some(c) = cur.filter(|c| c.is_object()) else { break };
        if let Some(i) = crate::binding::brand::proto_iface(scope, c) {
            id = Some(i);
            break;
        }
        cur = v8::Local::<v8::Object>::try_from(c).ok().and_then(|o| o.get_prototype(scope));
    }
    let Some(id) = id else { return false };
    // another interface's prototype re-parented this way is not an instance
    if crate::binding::brand::proto_iface(scope, obj.into()).is_some() {
        return true;
    }
    if crate::install::templates::reporting() {
        let n = v8::Local::<v8::Object>::try_from(proto).ok().and_then(|p| iface_name_of(scope, p)).map(|n| n.to_rust_string_lossy(scope));
        match crate::binding::brand::brand_of(scope, obj.into()) {
            // an object no interface template made, branded here (D1: what is still made some other way) -- or a
            // realm's GLOBAL, which deno_core makes before this crate can reach the isolate's templates: a named D1
            // exception (the human's ruling, 2026-09-29), reported as such
            None => {
                let how = if obj.is_api_wrapper() { "api" } else { "plain" };
                let global: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
                let this: v8::Local<v8::Value> = obj.into();
                let kind = if global == this { "ADOPT-GLOBAL" } else { "ADOPT-UNTEMPLATED" };
                crate::install::templates::report(&format!("{kind} {how} {}", n.unwrap_or_default()));
            }
            // one ANOTHER interface's template made -- a parent's -- re-branded as this one
            Some(b) if b != id => {
                let from = crate::binding::brand::name_of(b).unwrap_or_default();
                crate::install::templates::report(&format!("ADOPT-REBRAND {from} -> {}", n.unwrap_or_default()));
            }
            Some(_) => {}
        }
    }
    crate::binding::brand::set_brand(scope, obj, id);
    true
}

/// A new platform object whose prototype is `proto`, made from the template of the interface `proto` belongs
/// to, in `proto`'s realm (see [`mint_wrapper_on`]) -- or, without a stamp or a template, an
/// ordinary object given `proto` and, when `proto` belongs to an interface, that interface's brand: what
/// `Object::new` + [`adopt_as`] made, and what `Object::new` + `set_prototype` made for a prototype that
/// belongs to none (those `set_prototype` pairs became this
/// call).
pub fn mint_on<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Object> {
    if let Some(inst) = v8::Local::<v8::Object>::try_from(proto).ok().and_then(|p| mint_from_template(scope, p)) {
        return inst;
    }
    let obj = v8::Object::new(scope);
    obj.set_prototype(scope, proto);
    brand_as(scope, obj, proto);
    obj
}

/// A new platform object -- one cppgc will wrap -- whose prototype is `proto`, MADE FROM THE TEMPLATE of the
/// interface `proto` belongs to, in `proto`'s realm (the Signature family): V8 then knows it as an instance of that
/// interface in every realm of the isolate (`FunctionTemplate::HasInstance` asks the map which template
/// made it), which is the brand Chrome's platform objects carry. The interface is `proto`'s own stamp,
/// else the first stamped prototype up its chain (a custom element's class prototype sits above its
/// platform interface's); the instance takes `proto` when its template's own prototype is another.
/// Without a stamp, or a template for the interface in this isolate, it is made as before -- the nameless
/// wrapper template's instance given `proto` (`deno_core::cppgc::wrap_object` needs an API object), which
/// carries no brand, so every receiver check refuses it: a mint must reach a template.
pub(crate) fn mint_wrapper_on<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Object> {
    if let Some(inst) = v8::Local::<v8::Object>::try_from(proto).ok().and_then(|p| mint_from_template(scope, p)) {
        return inst;
    }
    let obj = anon_wrapper_object(scope);
    obj.set_prototype(scope, proto);
    obj
}

/// [`mint_wrapper_on`]'s template half: the instance of `proto`'s interface template in `proto`'s realm, or `None`.
fn mint_from_template<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let mut cur = proto;
    let mut name = None;
    for _ in 0..16 {
        if let Some(n) = iface_name_of(scope, cur).and_then(|n| v8::Local::<v8::String>::try_from(n).ok()) {
            name = Some(n);
            break;
        }
        cur = cur.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())?;
    }
    let Some(name) = name else {
        if crate::install::templates::reporting() {
            crate::install::templates::report("MINT-FALLBACK no-stamp");
        }
        return None;
    };
    let it = mint_template(scope, name)?;
    let Some(ctx) = proto.get_creation_context(scope) else {
        if crate::install::templates::reporting() {
            let n = name.to_rust_string_lossy(scope);
            crate::install::templates::report(&format!("MINT-FALLBACK no-context {n}"));
        }
        return None;
    };
    let inst = {
        let scope = &mut v8::ContextScope::new(scope, ctx);
        it.new_instance(scope)
    };
    let Some(inst) = inst else {
        if crate::install::templates::reporting() {
            let n = name.to_rust_string_lossy(scope);
            crate::install::templates::report(&format!("MINT-FALLBACK no-instance {n}"));
        }
        return None;
    };
    let pv: v8::Local<v8::Value> = proto.into();
    if inst.get_prototype(scope) != Some(pv) {
        inst.set_prototype(scope, pv);
    }
    if crate::install::templates::reporting() {
        let n = name.to_rust_string_lossy(scope);
        let ok = crate::binding::brand::id_of(&n).is_some_and(|t| crate::binding::brand::implements_id(scope, inst.into(), t));
        crate::install::templates::report(&format!("MINT-OK {n} branded={ok}"));
    }
    Some(inst)
}

/// The instance template interface `name` (a prototype's stamp) mints from, per isolate, cached by the
/// name's identity in an isolate slot: a node wrapper is minted per `createElement`, and `OpState`'s map
/// is ordered by type, so its reads cost what the type sorts to. Keyed by the NAME, not the prototype, so
/// the cache holds no realm's object alive.
#[derive(Clone, Default)]
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct MintTemplates(std::rc::Rc<std::cell::RefCell<std::collections::HashMap<i32, Vec<(v8::Global<v8::String>, v8::Global<v8::ObjectTemplate>)>>>>);

fn mint_template<'s>(scope: &mut v8::PinScope<'s, '_>, name: v8::Local<'s, v8::String>) -> Option<v8::Local<'s, v8::ObjectTemplate>> {
    let h = name.get_identity_hash().get();
    // The hit reads the slot in place and makes the Local straight from the cached handle: cloning the
    // `Rc` and the `Global` first cost a global-handle create and release per mint, every node, list,
    // record and event wrapper (with `iface_name_key`'s clone: createTextNode 9,650 -> 9,150 instructions,
    // an observed setAttribute 19,600 -> 18,600, 2026-10-23).
    if let Some(c) = scope.get_slot::<MintTemplates>() {
        let hit = c.0.borrow().get(&h).and_then(|v| {
            v.iter().find(|(n, _)| v8::Local::new(scope, n) == name).map(|(_, t)| v8::Local::new(scope, t))
        });
        if hit.is_some() {
            return hit;
        }
    }
    let cache = match scope.get_slot::<MintTemplates>() {
        Some(c) => c.clone(),
        None => {
            let c = MintTemplates::default();
            scope.set_slot(c.clone());
            c
        }
    };
    let rust = name.to_rust_string_lossy(scope);
    let Some(tpl) = crate::install::templates::iface_template(scope, &rust) else {
        if crate::install::templates::reporting() {
            crate::install::templates::report(&format!("MINT-FALLBACK no-template {rust}"));
        }
        return None;
    };
    let it = tpl.instance_template(scope);
    cache.0.borrow_mut().entry(h).or_default().push((v8::Global::new(scope, name), v8::Global::new(scope, it)));
    Some(it)
}

pub(crate) fn anon_wrapper_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let cached = op_state.borrow().try_borrow::<AnonWrapperCtor>().map(|c| c.0.clone());
    let templ = match cached {
        Some(g) => v8::Local::new(scope, &g),
        None => {
            let ft = v8::FunctionTemplate::new(scope, crate::binding::refs::f(cb_anon_wrapper));
            let t = ft.instance_template(scope);
            op_state.borrow_mut().put(AnonWrapperCtor(v8::Global::new(scope, t)));
            t
        }
    };
    templ.new_instance(scope).expect("wrapper instance")
}

fn cb_anon_wrapper(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// A Document wrapper with the Document's named properties: an instance of `iface`'s template
/// -- `HTMLDocument`, `XMLDocument` or `Document`, the REALM's -- whose instance template
/// the builder gives the named interceptor (D1: `install::instance_shapes`). It had a wrapper template of its
/// own, cached per isolate, until then; and it was a `Document` re-parented afterwards.
pub(crate) fn make_document_wrapper<'s, T: deno_core::GarbageCollected + 'static>(scope: &mut v8::PinScope<'s, '_>, t: T, iface: &str) -> v8::Local<'s, v8::Object> {
    let proto = match shell_proto(scope, iface) {
        Some(p) => p.into(),
        None => class_proto::<T>(scope),
    };
    let obj = mint_wrapper_on(scope, proto);
    deno_core::cppgc::wrap_object(scope, obj, t)
}

fn class_proto<'s, T: deno_core::GarbageCollected + 'static>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    // THE NODE CLASSES' WRAPPERS WEAR THEIR INTERFACE'S PROTOTYPE, IN THIS REALM: the spine
    // builds `HTMLElement` and `Document` on fresh prototypes that take the template step, so the class
    // template's own prototype is no longer theirs. Before the spine has run, the class's.
    let key = std::any::TypeId::of::<T>();
    let iface = if key == std::any::TypeId::of::<JsElement>() {
        Some("HTMLElement")
    } else if key == std::any::TypeId::of::<crate::core::dom::document::JsDocument>() {
        Some("Document")
    } else {
        None
    };
    if let Some(p) = iface.and_then(|n| shell_proto(scope, n)) {
        return p.into();
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let cached = op_state.borrow().try_borrow::<ClassProtos>().and_then(|m| m.0.borrow().get(&key).map(|g| v8::Local::new(scope, g)));
    match cached {
        Some(p) => p,
        None => {
            let tmp = deno_core::cppgc::make_cppgc_empty_object::<T>(scope);
            let p = tmp.get_prototype(scope).unwrap_or_else(|| v8::null(scope).into());
            let mut st = op_state.borrow_mut();
            if st.try_borrow::<ClassProtos>().is_none() {
                st.put(ClassProtos::default());
            }
            st.borrow::<ClassProtos>().0.borrow_mut().insert(key, v8::Global::new(scope, p));
            p
        }
    }
}

pub(crate) fn make_wrapper<'s, T: deno_core::GarbageCollected + 'static>(scope: &mut v8::PinScope<'s, '_>, t: T) -> v8::Local<'s, v8::Object> {
    let proto = class_proto::<T>(scope);
    // The prototype of the realm doing the minting -- a frame's function runs in the
    // frame's context, and what it returns is the frame's.
    let ctx = scope.get_current_context();
    let proto = match v8::Local::<v8::Object>::try_from(proto) {
        Ok(p) => proto_in_realm(scope, ctx, p).into(),
        Err(_) => proto,
    };
    let obj = mint_wrapper_on(scope, proto);
    deno_core::cppgc::wrap_object(scope, obj, t)
}

pub(crate) fn wrap_cached<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    id: u32,
) -> v8::Local<'s, v8::Object> {
    // In an isolated world, the world's own object for the node (`core/html/isolated_world.rs`).
    if let Some(world) = crate::core::html::isolated_world::current(scope, op_state) {
        if let Some(w) = crate::core::html::isolated_world::wrap(scope, op_state, world, id) {
            return w;
        }
    }
    // the cached wrapper, read in place: cloning its Global created and released a global handle per call
    if let Some(w) = op_state.borrow().borrow::<NodeCache>().local(scope, id) {
        return w;
    }
    // One prototype write per wrapper: the node's own interface when `node_proto` knows it, the
    // `JsElement` class prototype (HTMLElement.prototype) otherwise. A `<select>`'s and a `<form>`'s
    // interfaces' templates carry their indexed / named access
    // (`install::instance_shapes`); each had a wrapper template of its own until then.
    let proto: v8::Local<v8::Value> = match node_proto(scope, op_state, id) {
        Some(p) => p.into(),
        None => class_proto::<JsElement>(scope),
    };
    let proto = match v8::Local::<v8::Object>::try_from(proto) {
        Ok(p) => crate::core::html::browsing_context::node_proto_in_its_realm(scope, p, id).into(),
        Err(_) => proto,
    };
    let obj = mint_wrapper_on(scope, proto);
    let obj = deno_core::cppgc::wrap_object(scope, obj, JsElement { id });
    op_state.borrow().borrow::<NodeCache>().insert(id, v8::Global::new(scope, &obj));
    obj
}

/// Generate a RAW v8 getter callback (bypassing the op2 #[this]+scope constraint) + a scope-only op
/// returning it, for a node-returning member. `$target` computes the target NodeId from `dom` + `id`.
/// Build a genuine native function (raw v8 callback) with the correct fn.name + fn.length set, so
/// it is indistinguishable from an op2-generated binding on the surface (name/length, not just
/// [native code]). name/length come from the install site (getter => "get x"/0, method => "x"/arity).
#[track_caller]
pub fn build_fn<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, name: &str, length: i32) -> v8::Local<'s, v8::Function> {
    // ConstructorBehavior::Throw -> a "concise" native function with NO own `.prototype`/arguments/caller
    // (real DOM methods carry none; the default Allow would add `.prototype`, a JS-shim tell).
    let raw = cb.map_fn_to();
    #[cfg(not(feature = "analysis"))]
    let f = v8::Function::builder_raw(crate::binding::refs::c(raw)).length(length).constructor_behavior(v8::ConstructorBehavior::Throw).build(scope).unwrap();
    #[cfg(feature = "analysis")]
    let f = {
        let (cb2, d2) = crate::install::templates::analysis_untemplated(scope, raw, None, name);
        v8::Function::builder_raw(crate::binding::refs::c(cb2)).data(d2).length(length).constructor_behavior(v8::ConstructorBehavior::Throw).build(scope).unwrap()
    };
    let n = v8::String::new(scope, name).unwrap();
    f.set_name(n);
    stamp_engine_fn(scope, f);
    if crate::install::templates::capturing() {
        crate::install::templates::record(scope, f, crate::install::templates::Recipe { cb: Some(raw), data: None, length, name: name.to_string(), ctor: false, guard: crate::install::templates::Guard::None, inner: None });
    }
    crate::install::templates::note_untemplated(scope, "build_fn", name, f);
    f
}

/// The page `document`, memoised in `DocumentGlobal`.
///
/// Identical result to `global_get(scope, "document")`, but that lookup goes through the window's
/// named-property machinery on every call, and the callers here (`Node.parentNode`, Range boundary
/// containers) run on every tree walk. See `DocumentGlobal`'s doc comment for the measurement.
pub(crate) fn document_val<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Option<v8::Local<'s, v8::Value>> {
    // An isolated world's `document` is its own object for the page's document.
    if let Some(world) = crate::core::html::isolated_world::current(scope, op_state) {
        let root = op_state.borrow().try_borrow::<crate::binding::host_dom::HostDom>().map(|h| h.0.root());
        if let Some(d) = root.and_then(|r| crate::core::html::isolated_world::wrap(scope, op_state, world, r)) {
            return Some(d.into());
        }
    }
    // `try_borrow`: a realm with no page document (the bare installer tests' runtime) has no memo,
    // and `EtKey::Document` asks through here from every EventTarget member.
    let has_memo = op_state.borrow().has::<crate::binding::host_dom::DocumentGlobal>();
    if !has_memo {
        let page = crate::core::html::browsing_context::main_global(scope);
        return get_prop(scope, page, "document").filter(|v| !v.is_null_or_undefined());
    }
    // Read in place: a cloned `Global` is a global-handle create and release per call.
    if let Some(g) = op_state.borrow().borrow::<crate::binding::host_dom::DocumentGlobal>().0.borrow().as_ref() {
        return Some(v8::Local::new(scope, g));
    }
    // The PAGE's global, whichever realm asks first: that can be a frame's script,
    // whose own `document` must never become the memo.
    let page = crate::core::html::browsing_context::main_global(scope);
    let v = get_prop(scope, page, "document")?;
    if v.is_null_or_undefined() { return None; }
    let g = v8::Global::new(scope, v);
    *op_state.borrow().borrow::<crate::binding::host_dom::DocumentGlobal>().0.borrow_mut() = Some(g);
    Some(v)
}

pub(crate) fn global_get<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let ctx = scope.get_current_context();
    let global = ctx.global(scope);
    let key = v8::String::new(scope, name)?;
    global.get(scope, key.into())
}
pub fn throw_dom(scope: &mut v8::PinScope, msg: &str, name: &str) {
    if let Some(exc) = make_dom_exc(scope, msg, name) { scope.throw_exception(exc); }
}

pub(crate) fn unwrap_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, v).map(|p| unsafe { p.as_ref() }.id)
}

/// A plain `RangeError` — what an out-of-range numeric argument raises where WebIDL says so
/// (`ctx.roundRect(0, 0, 10, 10, -1)`), as distinct from a `DOMException`.
pub(crate) fn throw_range_error(scope: &mut v8::PinScope, msg: &str) {
    // V8's own RangeError, never the page's replacement; Blink's context applied after it is made,
    // as `throw_type_error` applies it.
    let exc = deno_core::error::native_error(scope, "RangeError", msg);
    scope.throw_exception(exc);
}

/// A native getter that answers a CONSTANT string carried in its data -- an IDL attribute whose value
/// is fixed by the interface rather than read from the element (`textarea.type === "textarea"`).
/// `op_const_str_get`, the builder `dom/08y-output-fieldset.js` reached this through, is gone with that
/// file; the callback stays, named directly by the table rows that use it.
pub(crate) fn cb_const_str_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = scope;
    rv.set(crate::binding::protos::body_data(scope, &args));
}

/// A native function that returns `null`, whatever it is called with.
///
/// This is for the members whose answer in THIS engine is structurally null rather than merely
/// unimplemented: `object.getSVGDocument()` cannot return a document because Korvus has no nested
/// browsing contexts to hold one, and a real Chrome returns null there too for an `<object>` that
/// has loaded nothing. Every use must carry a comment saying which of those it is -- a null that
/// only happens to match a fixture does not belong here.
pub(crate) fn cb_null_get(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = scope;
    rv.set_null();
}
// `op_null_get`, the JS-facing builder for the callback above, IS GONE. The callback is
// not: `getSVGDocument` on `<object>` and `<iframe>`, `HTMLMediaElement.error`/`mediaKeys`,
// `Document.fullscreenElement`/`pointerLockElement`/`pictureInPictureElement` and
// `CharacterData`-adjacent `sheet` all name `cb_null_get` directly from their table rows.


/// The V8 Private the realm's interface prototypes are SEALED into, keyed by interface name.
///
/// A Private is not a property: `Object.getOwnPropertyNames`, `Reflect.ownKeys` and a `Proxy` all
/// miss it, and nothing in JS can name it. That is the same reason `binding::hidden` keeps
/// `Deno.core.ops` and the bridge holder here (`hidden::ops_object` is the worked example).
fn sealed_protos_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Private>> {
    Some(private_key(scope, "korvus_protos"))
}

/// This realm's sealed name -> prototype map, if [`seal_interface_protos`] has run here.
fn sealed_protos<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let global = scope.get_current_context().global(scope);
    let key = sealed_protos_key(scope)?;
    let v = global.get_private(scope, key)?;
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// SEAL this realm's interface prototypes, so the ~24 `shell_proto` mint sites stop resolving through
/// a name the page can write.
///
/// # The defect this closes
///
/// [`shell_proto`] read `globalThis[name].prototype` to find the prototype to MINT an instance on,
/// and `globalThis[name]` is an ordinary writable property. A page that ran
/// `globalThis.MediaList = function Hijacked(){}` and then read `sheet.media` on a fresh `<style>`
/// got an engine-minted object wearing ITS prototype: `[object Object]` instead of
/// `[object MediaList]`, no `MediaList` member on it, and an engine-minted object handed to
/// page-controlled code. A census measured 71 of 82
/// page-reachable mints doing exactly that, including one -- `ValidityState` -- where the lazy
/// install then defined twelve NATIVE getters onto the page's own object.
///
/// Headless Chrome 150.0.7871.46 accepts the same reassignment and mints on the real prototype
/// regardless: an interface's instances come from the realm's internal prototype, full stop.
///
/// # Why a SEAL rather than recording each prototype as it is built
///
/// Recording at `installer::create_interface_in` would be one line, but it is not the only writer of
/// an interface object: `rows::build_newable`, `rows::take_prototype`, `fetch_values::ensure_class`,
/// the phantom-parent pass and the native-class reconcile all mount or replace one, and a recorder
/// that misses any of them hands back a STALE prototype -- failure mode 1 of a table cutover, which
/// is the exact thing this unit must not cause. Reading the finished global once, at the end of the
/// realm's install, cannot be stale by construction: it is the same read `shell_proto` was already
/// doing, taken at the last moment before anything a page wrote could be in it.
///
/// # ORDER, which is the whole difficulty
///
/// `shell_proto`'s read of the live global is effectively a lazy lookup that works because the global
/// happens to be populated by the time a mint runs. The seal keeps that property rather than
/// replacing it: before the seal, [`shell_proto`] falls back to the live read and behaves EXACTLY as
/// it did; after it, the map answers. Boot-time install sites (`install_document_fonts`, the two canvas installs, `install_canvas_capture_stream`) run before the
/// seal and take the fallback -- which is correct, because no page script has run at that point and
/// there is nothing yet to hijack. Nothing needs a hand-maintained ordering list.
///
/// # No hand list
///
/// The names come from the GLOBAL's own keys, not from a roster: whatever the installer put there is
/// what gets sealed. The map has a NULL prototype, so a name that was never sealed reads `undefined`
/// rather than inheriting `Object.prototype.toString`.
///
/// # Descriptors, never `get`
///
/// The values are read from each key's property DESCRIPTOR. Reading the global by name would fire
/// every accessor on it -- `document.fonts`, `navigator.serviceWorker`, `performance.timing` -- and
/// mint their singletons during boot, which is a behaviour change this unit has no business making.
/// An interface object is always a data property, so the descriptor is also the complete answer.
pub fn seal_interface_protos(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    let map = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    map.set_prototype(scope, null);
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
        .index_filter(v8::IndexFilter::SkipIndices)
        .build();
    let Some(keys) = global.get_own_property_names(scope, args) else { return };
    // ...and the interface OBJECTS, for `iface_object`: the same names, sealed at the same
    // moment, for the engine's own lookups of a constructor.
    let ctors = v8::Object::new(scope);
    ctors.set_prototype(scope, null);
    let mut stamps = Vec::new();
    let any_lazy = crate::install::templates::has_lazy_globals(scope);
    for i in 0..keys.length() {
        let Some(k) = keys.get_index(scope, i) else { continue };
        let Ok(name) = v8::Local::<v8::Name>::try_from(k) else { continue };
        // a lazy interface object not made yet: reading its descriptor would make it -- it is stamped
        // when it is made ([`stamp_sealed`])
        if any_lazy && crate::install::templates::lazy_global(scope, global, &k.to_rust_string_lossy(scope)).is_some() {
            continue;
        }
        let Some(desc) = global.get_own_property_descriptor(scope, name).filter(|v| !v.is_undefined()) else { continue };
        let Ok(desc) = v8::Local::<v8::Object>::try_from(desc) else { continue };
        // An accessor descriptor has no `value`, so this is also the getter filter.
        let Some(ctor) = obj_get(scope, desc, "value").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { continue };
        let Some(proto) = obj_get(scope, ctor.into(), "prototype").filter(|v| v.is_object()) else { continue };
        map.set(scope, k, proto);
        ctors.set(scope, k, ctor.into());
        stamps.push((k, ctor, proto));
    }
    // the global itself is an instance of the first interface up its chain (`Window`, a worker's scope):
    // its brand, as a template-made global would carry it
    let mut cur = global.get_prototype(scope);
    for _ in 0..8 {
        let Some(c) = cur else { break };
        if let Some(id) = crate::binding::brand::proto_iface(scope, c) {
            crate::binding::brand::set_brand(scope, global, id);
            break;
        }
        cur = v8::Local::<v8::Object>::try_from(c).ok().and_then(|o| o.get_prototype(scope));
    }
    // the interfaces' parents, from the chains this install finished (`binding::brand`)
    let chain_protos: Vec<v8::Local<v8::Object>> = stamps.iter().filter_map(|(_, _, p)| v8::Local::<v8::Object>::try_from(*p).ok()).collect();
    crate::binding::brand::learn_parents_from_chains(scope, &chain_protos);
    // A prototype is stamped with its OWN interface's name first -- the global whose function carries that
    // name and is the prototype's `constructor` -- so a legacy factory (`Image`, which the capture lists
    // before `HTMLImageElement`) or an alias (`webkitURL`) never names another interface's prototype: a
    // realm built from templates makes the interface a stamp names. Then the first name
    // for any prototype no global names as its own.
    for canonical in [true, false] {
        for (k, ctor, proto) in &stamps {
            if canonical {
                let own = obj_get(scope, v8::Local::<v8::Object>::try_from(*proto).unwrap(), "constructor").is_some_and(|c| {
                    let cv: v8::Local<v8::Value> = (*ctor).into();
                    c == cv
                });
                if !own || ctor.get_name(scope).to_rust_string_lossy(scope) != k.to_rust_string_lossy(scope) {
                    continue;
                }
            }
            stamp_iface_name(scope, *proto, *k);
        }
    }
    let ck = private_key(scope, SEALED_CTORS);
    global.set_private(scope, ck, ctors.into());
    // The interfaces this device has no GLOBAL for, whose instances the engine still mints
    // ([`record_hidden_proto`]). A published name always wins: the global's own answer is the one a
    // page could have reached.
    if let Some(hidden) = hidden_protos(scope) {
        let args = v8::GetPropertyNamesArgsBuilder::new()
            .mode(v8::KeyCollectionMode::OwnOnly)
            .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
            .index_filter(v8::IndexFilter::SkipIndices)
            .build();
        if let Some(keys) = hidden.get_own_property_names(scope, args) {
            for i in 0..keys.length() {
                let Some(k) = keys.get_index(scope, i) else { continue };
                let Ok(name) = v8::Local::<v8::Name>::try_from(k) else { continue };
                if map.has_own_property(scope, name).unwrap_or(false) {
                    continue;
                }
                if let Some(proto) = hidden.get(scope, k) {
                    map.set(scope, k, proto);
                    stamp_iface_name(scope, proto, k);
                }
            }
        }
    }
    if let Some(key) = sealed_protos_key(scope) {
        global.set_private(scope, key, map.into());
    }
    seal_intrinsics(scope);
}

// ── the realm's intrinsics ─────────────────────────────────────────────────────────

const SEALED_INTRINSICS: &str = "korvus_intrinsics";

/// The built-ins and window functions the engine's own algorithms call, by path. A page may replace
/// every one of them -- zone.js replaces `Promise` and `queueMicrotask`, old libraries `Array.from`,
/// stealth scripts wrap `getComputedStyle` -- and Chrome's algorithms never notice, because they use the
/// realm's intrinsics (`H-page-globals`: with all of these replaced by throwing functions, Chrome's
/// MutationObserver, innerText, Blob, pattern validation, lastModified and IndexedDB call none of them).
const INTRINSIC_PATHS: &[&str] = &[
    "Array", "Array.from",
    "Object", "Object.freeze", "Object.prototype",
    "Promise", "Promise.resolve", "Promise.all",
    "Date", "Date.now", "Date.parse",
    "RegExp", "MessageEvent", "WebAssembly",
    "queueMicrotask", "structuredClone", "getComputedStyle",
];

/// The prototype METHODS the engine calls, per constructor: sealed under the key
/// `<ctor>.prototype.<method>`, which is built here rather than written out (a property path is not
/// JavaScript, and the kernel's string ledger reads `prototype.` as the start of some). Read them with
/// [`intrinsic_method`].
const INTRINSIC_METHODS: &[(&str, &[&str])] = &[
    ("Object", &["valueOf"]),
    ("Date", &["getFullYear", "getMonth", "getDate", "getHours", "getMinutes", "getSeconds"]),
];

fn method_key(ctor: &str, method: &str) -> String {
    [ctor, "prototype", method].join(".")
}

/// One own DATA property's value: never an accessor's (reading one would fire a getter at boot -- see
/// [`seal_interface_protos`]'s "Descriptors, never `get`").
fn own_data_value<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = v8::String::new(scope, name)?;
    let desc = o.get_own_property_descriptor(scope, k.into()).filter(|v| !v.is_undefined())?;
    let desc = v8::Local::<v8::Object>::try_from(desc).ok()?;
    obj_get(scope, desc, "value")
}

/// Snapshot [`INTRINSIC_PATHS`] off the CURRENT realm's global into a V8 Private, under the same
/// contract as [`seal_interface_protos`] (which calls this): after the realm's last install, before any
/// script of the page's. A frame realm built from templates takes it at the end of its own install.
pub fn seal_intrinsics(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    let bag = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    bag.set_prototype(scope, null);
    let methods: Vec<String> = INTRINSIC_METHODS.iter().flat_map(|(c, ms)| ms.iter().map(move |m| method_key(c, m))).collect();
    for path in INTRINSIC_PATHS.iter().copied().chain(methods.iter().map(String::as_str)) {
        seal_path(scope, global, bag, path);
    }
    let key = private_key(scope, SEALED_INTRINSICS);
    global.set_private(scope, key, bag.into());
    seal_global_members(scope, global);
    seal_realm_members(scope, global);
}

/// One [`seal_intrinsics`] entry: the global's own data value at `path`, into `bag`.
fn seal_path<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, bag: v8::Local<'s, v8::Object>, path: &str) {
    let mut cur: Option<v8::Local<v8::Value>> = Some(global.into());
    for seg in path.split('.') {
        cur = cur
            .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
            .and_then(|o| own_data_value(scope, o, seg));
    }
    if let (Some(v), Some(k)) = (cur.filter(|v| !v.is_undefined()), v8::String::new(scope, path)) {
        bag.set(scope, k.into(), v);
    }
}

/// The context snapshot: [`seal_intrinsics`]'s entries rooted at `roots` -- V8's late names, which a snapshot
/// maker's realm lacked when it sealed (`install::installer::set_v8_late`) -- read off THIS realm's global into its
/// sealed bag, as a fresh realm's seal reads them.
#[cfg(not(feature = "analysis"))]
pub(crate) fn reseal_intrinsics(scope: &mut v8::PinScope, roots: &[String]) {
    let global = scope.get_current_context().global(scope);
    let key = private_key(scope, SEALED_INTRINSICS);
    let Some(bag) = global.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    for path in INTRINSIC_PATHS.iter().copied().filter(|p| roots.iter().any(|r| p.split('.').next() == Some(r.as_str()))) {
        seal_path(scope, global, bag, path);
    }
}

/// The current realm's pristine value at `path` (one of [`INTRINSIC_PATHS`]), never the page's
/// replacement. A realm sealed before this existed, or a path not in the list, reads the
/// live global as it did before.
pub fn intrinsic<'s>(scope: &mut v8::PinScope<'s, '_>, path: &str) -> Option<v8::Local<'s, v8::Value>> {
    let global = scope.get_current_context().global(scope);
    let key = private_key(scope, SEALED_INTRINSICS);
    if let Some(bag) = global.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        let k = v8::String::new(scope, path)?;
        if let Some(v) = bag.get(scope, k.into()).filter(|v| !v.is_undefined()) {
            return Some(v);
        }
    }
    let mut cur: Option<v8::Local<v8::Value>> = Some(global.into());
    for seg in path.split('.') {
        cur = cur.and_then(|c| v8::Local::<v8::Object>::try_from(c).ok()).and_then(|o| get_prop(scope, o, seg));
    }
    cur.filter(|v| !v.is_undefined())
}

/// The pristine `<ctor>.prototype.<method>` (one of [`INTRINSIC_METHODS`]), as a function.
pub fn intrinsic_method<'s>(scope: &mut v8::PinScope<'s, '_>, ctor: &str, method: &str) -> Option<v8::Local<'s, v8::Function>> {
    intrinsic_fn(scope, &method_key(ctor, method))
}

/// [`intrinsic`] as a function.
pub fn intrinsic_fn<'s>(scope: &mut v8::PinScope<'s, '_>, path: &str) -> Option<v8::Local<'s, v8::Function>> {
    intrinsic(scope, path).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
}

/// Call the pristine static `path` (`"Array.from"`) with its pristine owner (`Array`) as the receiver.
pub fn call_intrinsic<'s>(scope: &mut v8::PinScope<'s, '_>, path: &str, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    let f = intrinsic_fn(scope, path)?;
    let recv: v8::Local<v8::Value> = match path.rsplit_once('.') {
        Some((owner, _)) => intrinsic(scope, owner)?,
        None => scope.get_current_context().global(scope).into(),
    };
    f.call(scope, recv, args)
}

// ── the members the engine's own steps run ───────────────────────────────

const SEALED_MEMBERS: &str = "korvus_members";

/// The DOM members the engine's own algorithms run on a page's nodes, per interface. A page may wrap or
/// replace every one of them -- a bot-challenge frame wraps the tree's methods and accessors and
/// counts their calls -- and Chrome's algorithms never call them: Blink creates, inserts, clones and checks
/// in C++. A table's `insertRow`, a range's `extractContents`, a label's activation went through the page's
/// `createElement`, `appendChild`, `cloneNode`, `click` instead, so a page that wrapped those saw the
/// engine's calls as its own (`tests/engine_reads_live.rs`: Chrome 150 answers none). Each realm seals
/// these as its install left them ([`seal_members`]) and the engine runs the ORIGINAL on the receiver
/// ([`pristine_call`], [`pristine_get`], [`pristine_set`]): the same steps -- MutationObserver records,
/// live ranges, custom element reactions -- and nothing a page can see.
const ENGINE_MEMBERS: &[(&str, &[&str])] = &[
    ("Node", &["appendChild", "insertBefore", "removeChild", "replaceChild", "cloneNode", "textContent", "parentNode", "firstChild", "childNodes", "isConnected", "getRootNode", "contains"]),
    ("Element", &["remove", "getBoundingClientRect", "getClientRects", "outerHTML", "getAttribute", "matches", "closest", "querySelector", "querySelectorAll"]),
    ("DocumentFragment", &["getElementById", "querySelectorAll"]),
    ("NodeList", &["length"]),
    ("HTMLCollection", &["length"]),
    ("HTMLFormControlsCollection", &["namedItem"]),
    ("FormData", &["append"]),
    ("BroadcastChannel", &["postMessage"]),
    ("DOMParser", &["parseFromString"]),
    ("Response", &["text"]),
    ("CharacterData", &["remove"]),
    ("DocumentType", &["remove"]),
    ("Text", &["splitText"]),
    ("Document", &["createElement", "createElementNS", "createDocumentFragment", "createTextNode", "getElementById", "querySelector", "querySelectorAll", "head", "documentElement", "URL", "referrer"]),
    ("HTMLIFrameElement", &["contentWindow"]),
    ("MediaQueryList", &["matches"]),
    ("HTMLElement", &["click", "innerText"]),
    ("HTMLInputElement", &["checked", "indeterminate", "value", "type", "form", "checkValidity"]),
    ("HTMLButtonElement", &["type", "form", "value", "checkValidity"]),
    ("HTMLLabelElement", &["control"]),
    ("HTMLDetailsElement", &["open"]),
    ("HTMLFormElement", &["reset"]),
    ("HTMLDialogElement", &["close"]),
    ("HTMLImageElement", &["width", "height"]),
    ("HTMLMediaElement", &["preload", "src"]),
    ("HTMLOptionElement", &["text", "selected", "value"]),
    ("HTMLSelectElement", &["value", "selectedIndex", "checkValidity"]),
    ("HTMLTextAreaElement", &["value", "checkValidity"]),
    ("HTMLObjectElement", &["checkValidity"]),
    ("HTMLOutputElement", &["checkValidity"]),
    ("HTMLFieldSetElement", &["form", "checkValidity"]),
    ("Range", &["setStart", "setEnd", "selectNode", "toString", "deleteContents", "extractContents", "insertNode"]),
    ("Selection", &["collapse", "setBaseAndExtent"]),
];

/// Seal `iface`'s [`ENGINE_MEMBERS`] off `proto`, the realm's prototype for it as its install (or its
/// template) left it: a method under `Iface.member`, an accessor's halves under `Iface.member#get` and
/// `#set`. A realm built from templates seals an interface when it makes it (`templates::realm_interface`),
/// which is before a page can reach it; any other realm at [`seal_intrinsics`].
pub(crate) fn seal_members(scope: &mut v8::PinScope, iface: &str, proto: v8::Local<v8::Object>) {
    let Some((_, members)) = ENGINE_MEMBERS.iter().find(|(i, _)| *i == iface) else { return };
    let bag = realm_map(scope, SEALED_MEMBERS);
    for m in members.iter() {
        let Some(k) = v8::String::new(scope, m) else { continue };
        let Some(desc) = proto.get_own_property_descriptor(scope, k.into()).filter(|v| !v.is_undefined()) else { continue };
        let Ok(desc) = v8::Local::<v8::Object>::try_from(desc) else { continue };
        for (field, suffix) in [("value", ""), ("get", "#get"), ("set", "#set")] {
            let Some(f) = obj_get(scope, desc, field).filter(|v| v.is_function()) else { continue };
            if let Some(key) = v8::String::new(scope, &format!("{iface}.{m}{suffix}")) {
                bag.set(scope, key.into(), f);
            }
        }
    }
}

/// The realm global's own members the engine reads, sealed off the global (or the first holder up its chain --
/// a worker's are `WorkerGlobalScope`'s) under `self.<name>`. Both are `[Replaceable]`: a page's
/// `self.origin = ...` shadows the getter with a data property, and Blink's blob URLs and observers read the
/// realm's own state regardless (`createObjectURL` minted `blob:` + whatever `origin`
/// answered, and `new PerformanceObserver` read the page's `performance`).
const GLOBAL_MEMBERS: &[&str] = &["origin", "performance", "matchMedia"];

fn seal_global_members(scope: &mut v8::PinScope, global: v8::Local<v8::Object>) {
    let bag = realm_map(scope, SEALED_MEMBERS);
    for name in GLOBAL_MEMBERS {
        let Some(k) = v8::String::new(scope, name) else { continue };
        let mut holder = Some(global);
        for _ in 0..8 {
            let Some(h) = holder else { break };
            if let Some(desc) = h.get_own_property_descriptor(scope, k.into()).filter(|v| !v.is_undefined()) {
                if let Ok(desc) = v8::Local::<v8::Object>::try_from(desc) {
                    for (field, suffix) in [("value", ""), ("get", "#get"), ("set", "#set")] {
                        if let Some(f) = obj_get(scope, desc, field).filter(|v| v.is_function()) {
                            if let Some(key) = v8::String::new(scope, &format!("self.{name}{suffix}")) {
                                bag.set(scope, key.into(), f);
                            }
                        }
                    }
                }
                break;
            }
            holder = h.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
        }
    }
}

/// The current realm's own `self.<name>` ([`GLOBAL_MEMBERS`]), as its getter answers -- never a page's
/// replacement. Before the seal, the global's property.
pub fn realm_self_get<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let global: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    pristine_get(scope, global, &format!("self.{name}"))
}

/// [`seal_members`] for every interface a realm built by its install has: off its global's interface
/// objects, before any script of the page's. A realm made from templates seals each interface as it makes
/// it instead -- reading `Node` off its global here would make it, and every other, at birth.
fn seal_realm_members<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    if crate::install::templates::is_lazy_realm(scope) {
        return;
    }
    for (iface, _) in ENGINE_MEMBERS {
        let proto = own_data_value(scope, global, iface)
            .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
            .and_then(|c| own_data_value(scope, c, "prototype"))
            .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
        if let Some(proto) = proto {
            seal_members(scope, iface, proto);
        }
    }
}

/// The realm's sealed `key` (`Iface.member`, `Iface.member#get`); in a realm made from templates, the
/// interface is made (and so sealed) on first ask.
fn pristine_member<'s>(scope: &mut v8::PinScope<'s, '_>, key: &str) -> Option<v8::Local<'s, v8::Function>> {
    let bag = realm_map(scope, SEALED_MEMBERS);
    let k = v8::String::new(scope, key)?;
    if let Some(f) = bag.get(scope, k.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        return Some(f);
    }
    let iface = key.split('.').next().filter(|i| *i != "self")?;
    // or a page interface put lazily: made -- and so sealed -- on first ask, as in a template-built realm
    if crate::install::templates::is_lazy_realm(scope) || crate::install::templates::lazy_here(scope, iface) {
        crate::install::templates::realm_interface(scope, iface)?;
        return bag.get(scope, k.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    }
    None
}

/// `recv.<member>(args)` with the realm's ORIGINAL `member` (`"Node.appendChild"`, one of
/// [`ENGINE_MEMBERS`]) -- never what a page put on the prototype. A realm with no seal for it calls the
/// receiver's own, as [`call_method`] does.
pub fn pristine_call<'s>(scope: &mut v8::PinScope<'s, '_>, recv: v8::Local<v8::Value>, member: &str, args: &[v8::Local<v8::Value>]) -> Option<v8::Local<'s, v8::Value>> {
    match pristine_member(scope, member) {
        Some(f) => f.call(scope, recv, args),
        None => call_method(scope, recv, member.rsplit('.').next()?, args),
    }
}

/// `p.then(ok, err)` as the ENGINE chains its own promises: V8's `Promise::Then` -- the isolate's original
/// `then`, never one a page put on `Promise.prototype`. Blink chains its internal promises the same way; a
/// `then` looked up by name ran a page's replacement for every fetch, XHR and cache read the engine settled.
/// A value that is not a promise is chained as it always was.
pub fn promise_then<'s>(scope: &mut v8::PinScope<'s, '_>, p: v8::Local<v8::Value>, ok: v8::Local<v8::Function>, err: Option<v8::Local<v8::Function>>) -> Option<v8::Local<'s, v8::Value>> {
    let Ok(pr) = v8::Local::<v8::Promise>::try_from(p) else {
        let mut args: Vec<v8::Local<v8::Value>> = vec![ok.into()];
        if let Some(e) = err {
            args.push(e.into());
        }
        return call_method(scope, p, "then", &args);
    };
    let r = match err {
        Some(e) => pr.then2(scope, ok, e),
        None => pr.then(scope, ok),
    }?;
    Some(r.into())
}

/// A dictionary the ENGINE hands a constructor (an event's init): no prototype, so neither the members written
/// into it nor the members the constructor's WebIDL conversion reads -- every member of the dictionary, present
/// or not -- reach a getter or setter a page put on `Object.prototype`. Blink makes its events natively and
/// reads nothing there; an object literal's missing `bubbles` / `cancelable` / `composed` / `data` / ... went up
/// to the page's `Object.prototype` on every event the engine fired (an XHR's progress
/// events, a message, a click's `input` / `change`, a focus, a `popstate` -- Chrome 150: none).
pub fn init_dict<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    o.set_prototype(scope, null);
    o
}

/// `node.remove()` with the realm's ORIGINAL `remove` of the node's kind (`ChildNode` is mixed into three
/// interfaces, each with its own function and receiver check).
pub fn pristine_remove(scope: &mut v8::PinScope, node: v8::Local<v8::Value>) {
    let Some(id) = arg_node_id(scope, node) else { return };
    let kind = {
        let os = deno_core::JsRuntime::op_state_from(scope);
        let st = os.borrow();
        st.try_borrow::<crate::binding::host_dom::HostDom>().map(|d| d.0.node_type(id))
    };
    let member = match kind {
        Some(1) => "Element.remove",
        Some(10) => "DocumentType.remove",
        Some(3 | 4 | 7 | 8) => "CharacterData.remove",
        _ => return,
    };
    pristine_call(scope, node, member, &[]);
}

/// `root.querySelectorAll(selectors)` with the realm's ORIGINAL member for the root's kind (`ParentNode` is mixed
/// into Document, DocumentFragment and Element).
pub fn pristine_query_all<'s>(scope: &mut v8::PinScope<'s, '_>, root: v8::Local<v8::Value>, selectors: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let member = if crate::core::dom::shared::document_node_id(scope, root).is_some() {
        "Document.querySelectorAll"
    } else {
        let id = arg_node_id(scope, root)?;
        // a shadow root is a document-typed node in the arena, and a DocumentFragment to the page
        let kind = {
            let os = deno_core::JsRuntime::op_state_from(scope);
            let st = os.borrow();
            st.try_borrow::<crate::binding::host_dom::HostDom>().map(|d| (d.0.node_type(id), d.0.is_shadow_root(id)))
        };
        match kind {
            Some((1, _)) => "Element.querySelectorAll",
            Some((9, false)) => "Document.querySelectorAll",
            _ => "DocumentFragment.querySelectorAll",
        }
    };
    pristine_call(scope, root, member, &[selectors]).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// `recv.<member>` read with the realm's ORIGINAL getter ([`pristine_call`]'s rules).
pub fn pristine_get<'s>(scope: &mut v8::PinScope<'s, '_>, recv: v8::Local<v8::Value>, member: &str) -> Option<v8::Local<'s, v8::Value>> {
    match pristine_member(scope, &format!("{member}#get")) {
        Some(f) => f.call(scope, recv, &[]),
        None => {
            let o = v8::Local::<v8::Object>::try_from(recv).ok()?;
            get_prop(scope, o, member.rsplit('.').next()?)
        }
    }
}

/// `recv.<member> = v` with the realm's ORIGINAL setter ([`pristine_call`]'s rules). `false` when it
/// threw.
pub fn pristine_set(scope: &mut v8::PinScope, recv: v8::Local<v8::Value>, member: &str, v: v8::Local<v8::Value>) -> bool {
    match pristine_member(scope, &format!("{member}#set")) {
        Some(f) => f.call(scope, recv, &[v]).is_some(),
        None => {
            let Ok(o) = v8::Local::<v8::Object>::try_from(recv) else { return false };
            let Some(name) = member.rsplit('.').next() else { return false };
            let Some(k) = v8::String::new(scope, name) else { return false };
            o.set(scope, k.into(), v).is_some()
        }
    }
}

/// The V8 Private an interface prototype carries its interface NAME under, stamped by
/// [`seal_interface_protos`] -- the key [`proto_in_realm`] translates a prototype of one realm into the
/// same interface's prototype of another with. An alias global (`webkitURL`) names a
/// prototype a second time; the first name stamped stays.
fn iface_name_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Private>> {
    // once per isolate, in a slot: every template mint reads the stamp, and making the key
    // is a string plus a lookup in V8's API-private registry each time
    if let Some(k) = scope.get_slot::<IfaceNameKey>() {
        return Some(v8::Local::new(scope, &k.0));
    }
    let n = v8::String::new(scope, "korvus_iface")?;
    let k = v8::Private::for_api(scope, Some(n));
    scope.set_slot(IfaceNameKey(v8::Global::new(scope, k)));
    Some(k)
}

/// [`iface_name_key`]'s private, once per isolate.
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct IfaceNameKey(v8::Global<v8::Private>);

/// The private an interface prototype carries its interface's name under (the seal's stamp), for the
/// builder's templates to set on every prototype they make.
pub(crate) fn iface_name_private<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Private>> {
    iface_name_key(scope)
}

pub(crate) fn stamp_iface_name(scope: &mut v8::PinScope, proto: v8::Local<v8::Value>, name: v8::Local<v8::Value>) {
    let (Ok(p), Some(key)) = (v8::Local::<v8::Object>::try_from(proto), iface_name_key(scope)) else { return };
    if p.has_private(scope, key).unwrap_or(false) {
        return;
    }
    // A prototype no interface template made -- `RegExp.prototype`, `Array.prototype`, every ECMAScript
    // built-in the global carries -- is named BESIDE it, never on it: one more property is a new hidden
    // class, and V8's fast paths ask a built-in prototype for its initial one (`String.prototype.replace`
    // with a RegExp took the generic `[Symbol.replace]` path, an extra stack frame and slower, which a
    // page can time and read in `Error.stack`; Chrome's built-ins carry nothing of Blink's).
    if crate::binding::brand::proto_iface(scope, proto).is_none() {
        let global = scope.get_current_context().global(scope);
        let lk = private_key(scope, BUILTIN_IFACES);
        let list = match global.get_private(scope, lk).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
            Some(l) => l,
            None => {
                let l = v8::Array::new(scope, 0);
                global.set_private(scope, lk, l.into());
                l
            }
        };
        if builtin_iface_name(scope, list, p).is_none() {
            let n = list.length();
            list.set_index(scope, n, p.into());
            list.set_index(scope, n + 1, name);
        }
        return;
    }
    p.set_private(scope, key, name);
}

const BUILTIN_IFACES: &str = "korvus_builtin_ifaces";

/// `proto`'s name in a realm's list of built-in prototypes (`[proto, name, proto, name, ...]`).
fn builtin_iface_name<'s>(scope: &mut v8::PinScope<'s, '_>, list: v8::Local<'s, v8::Array>, proto: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let pv: v8::Local<v8::Value> = proto.into();
    let mut i = 0;
    while i + 1 < list.length() {
        if list.get_index(scope, i).is_some_and(|v| v.strict_equals(pv)) {
            return list.get_index(scope, i + 1);
        }
        i += 2;
    }
    None
}

/// The interface name a sealed prototype was stamped with, if any -- on the prototype, or, for a built-in
/// one, in its realm's list ([`stamp_iface_name`]).
pub(crate) fn iface_name_of<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let key = iface_name_key(scope)?;
    if let Some(n) = proto.get_private(scope, key).filter(|v| !v.is_undefined()) {
        return Some(n);
    }
    let ctx = proto.get_creation_context(scope)?;
    let global = ctx.global(scope);
    let lk = private_key(scope, BUILTIN_IFACES);
    let list = global.get_private(scope, lk).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
    builtin_iface_name(scope, list, proto)
}

/// `proto`, a sealed interface prototype of ANY realm, as the same interface's prototype in `ctx`'s
/// realm -- or `proto` itself when `ctx` has no such interface. An object belongs to ONE
/// realm, and its prototypes are that realm's, so `frame.contentDocument instanceof
/// frame.contentWindow.Document`.
pub(crate) fn proto_in_realm<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ctx: v8::Local<'s, v8::Context>,
    proto: v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Object> {
    let Some(name) = iface_name_of(scope, proto) else { return proto };
    let Some(key) = sealed_protos_key(scope) else { return proto };
    let global = ctx.global(scope);
    let Some(map) = global.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        // A realm built from templates: the same interface, made there from its template.
        let n = name.to_rust_string_lossy(scope);
        let scope = &mut v8::ContextScope::new(scope, ctx);
        if crate::install::templates::is_lazy_realm(scope) {
            if let Some((_, p)) = crate::install::templates::realm_interface(scope, &n) {
                return v8::Local::new(scope, p);
            }
            // a JavaScript intrinsic the page's seal stamped too (`Object`, `Array`): the realm's own
            let own = global
                .get(scope, name)
                .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
                .and_then(|f| obj_get(scope, f.into(), "prototype"))
                .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
            if let Some(p) = own {
                return v8::Local::new(scope, p);
            }
        }
        return proto;
    };
    if let Some(p) = map.get(scope, name).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return p;
    }
    // a page interface put lazily and not made yet: made there now
    let n = name.to_rust_string_lossy(scope);
    let scope = &mut v8::ContextScope::new(scope, ctx);
    if crate::install::templates::lazy_global(scope, global, &n).is_some() {
        if let Some((_, p)) = crate::install::templates::realm_interface(scope, &n) {
            return v8::Local::new(scope, p);
        }
    }
    proto
}

/// Re-point `obj`'s prototype into `ctx`'s realm ([`proto_in_realm`]).
pub(crate) fn rehome_in_realm(scope: &mut v8::PinScope, ctx: v8::Local<v8::Context>, obj: v8::Local<v8::Object>) {
    let (ctx, obj) = (v8::Local::new(scope, ctx), v8::Local::new(scope, obj));
    let Some(cur) = obj.get_prototype(scope).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let there = proto_in_realm(scope, ctx, cur);
    if there != cur {
        obj.set_prototype(scope, there.into());
    }
}

fn hidden_protos_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Private>> {
    Some(private_key(scope, "korvus_hidden_protos"))
}

fn hidden_protos<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let global = scope.get_current_context().global(scope);
    let key = hidden_protos_key(scope)?;
    let v = global.get_private(scope, key)?;
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// Keep the prototype of an interface the ACTIVE capture does not publish as a global, for the
/// engine's own mints.
///
/// Measured in Chrome 150 (the chrome-150 persona, `tools/realweb/seed/ffs`): `typeof FontFaceSet` is
/// "undefined", yet `document.fonts` is an object tagged `[object FontFaceSet]` whose prototype owns
/// the sixteen FontFaceSet members and NO `constructor` -- `document.fonts.constructor` is
/// `EventTarget`, found up the chain. WebIDL says the same: an interface without an interface object
/// has no `constructor` on its prototype object. korvus built that prototype (the installer builds it
/// whether or not it publishes), but [`shell_proto`] only knew prototypes reachable from the GLOBAL,
/// so the mint found nothing and `document.fonts` was `undefined` -- six `Cannot read properties of
/// undefined (reading 'load')` on angular.dev under that persona.
pub(crate) fn record_hidden_proto(scope: &mut v8::PinScope, global: v8::Local<v8::Object>, name: &str, proto: v8::Local<v8::Object>) {
    if let Some(k) = v8::String::new(scope, "constructor") {
        proto.delete(scope, k.into());
    }
    let Some(key) = hidden_protos_key(scope) else { return };
    let map = match global.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(m) => m,
        None => {
            let m = v8::Object::new(scope);
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            m.set_prototype(scope, null);
            global.set_private(scope, key, m.into());
            m
        }
    };
    if let Some(k) = v8::String::new(scope, name) {
        map.set(scope, k.into(), proto.into());
    }
}

/// An interface's prototype, for minting an instance on it or mounting a member onto it.
///
/// The SEALED map first ([`seal_interface_protos`]), which is where the answer comes from for every
/// call a page can trigger; `globalThis[name].prototype` only as the pre-seal fallback, which is what
/// the boot-time install sites take and what this function did unconditionally.
pub(crate) fn shell_proto<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    let k = key_str(scope, name)?;
    if let Some(map) = sealed_protos(scope) {
        if let Some(p) = map.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            return Some(p);
        }
    }
    // A realm built from templates seals nothing: its interface is the one its template
    // makes for it, once -- never a name the page can write. Nor does a page interface put lazily.
    let ctx = scope.get_current_context();
    let global = ctx.global(scope);
    if crate::install::templates::is_lazy_realm(scope) || crate::install::templates::lazy_global(scope, global, name).is_some() {
        if let Some((_, p)) = crate::install::templates::realm_interface(scope, name) {
            return Some(p);
        }
    }
    let from_global = global
        .get(scope, k.into())
        .and_then(|c| v8::Local::<v8::Object>::try_from(c).ok())
        .and_then(|c| {
            let pk = v8::String::new(scope, "prototype")?;
            c.get(scope, pk.into())
        })
        .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
    if from_global.is_some() {
        return from_global;
    }
    // Pre-seal, an interface this device has no global for ([`record_hidden_proto`]).
    let hidden = hidden_protos(scope)?;
    hidden.get(scope, k.into()).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())
}

/// The private the sealed interface-object map lives under on the realm's global.
const SEALED_CTORS: &str = "korvus_ctors";

/// The PAGE realm's own interface object for `name`, made by its install: the sealed one, or, before the
/// seal, the global's own data property -- never a lazy one ([`crate::install::templates::lazy_global`]), which the
/// caller makes from its template. `None` when the page has none.
pub(crate) fn page_eager_interface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Function>> {
    let k = key_str(scope, name)?;
    let ck = private_key(scope, SEALED_CTORS);
    if let Some(map) = global.get_private(scope, ck).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return map.get(scope, k.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    }
    let d = global.get_own_property_descriptor(scope, k.into()).filter(|v| !v.is_undefined())?;
    let d = v8::Local::<v8::Object>::try_from(d).ok()?;
    obj_get(scope, d, "value").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
}

/// A lazy page interface was made (`templates::realm_interface`): into the sealed maps, as
/// [`seal_interface_protos`] puts every eager one, so the engine's later lookups find it there.
pub(crate) fn stamp_sealed(scope: &mut v8::PinScope, name: &str, ctor: v8::Local<v8::Function>, proto: v8::Local<v8::Object>) {
    let global = scope.get_current_context().global(scope);
    let Some(k) = key_str(scope, name) else { return };
    if let Some(map) = sealed_protos(scope) {
        map.set(scope, k.into(), proto.into());
    }
    let ck = private_key(scope, SEALED_CTORS);
    if let Some(map) = global.get_private(scope, ck).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        map.set(scope, k.into(), ctor.into());
    }
}

/// The realm's interface object for `name` AS ITS INSTALL LEFT IT -- not whatever a page has since
/// assigned to the global, which Chrome's own lookups never read (the builder's lookup,
/// replacing `global_get` of a Web API name). Sealed with
/// the prototypes ([`seal_interface_protos`]); before the seal, and for a name the seal does not hold, the
/// global itself -- exactly what `global_get` answered.
pub fn iface_object<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let global = scope.get_current_context().global(scope);
    let ck = private_key(scope, SEALED_CTORS);
    if let Some(map) = global.get_private(scope, ck).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        let k = key_str(scope, name)?;
        if let Some(c) = map.get(scope, k.into()).filter(|v| v.is_function()) {
            return Some(c);
        }
    }
    // A realm built from templates, or a page interface put lazily: its interface
    // object, made from its template -- never what the page now keeps under the name.
    if crate::install::templates::is_lazy_realm(scope) || crate::install::templates::lazy_global(scope, global, name).is_some() {
        if let Some((c, _)) = crate::install::templates::realm_interface(scope, name) {
            return Some(c.into());
        }
    }
    global_get(scope, name)
}

/// Define a method on a prototype (writable, enumerable, configurable -- matching a real DOM method and
/// the shell stub it replaces, so no SHAPE regression).
pub(crate) fn define_method(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, name: &str, f: v8::Local<v8::Function>) {
    let k = member_key(scope, name);
    proto.set(scope, k.into(), f.into());
}

/// A member's property key from its table name: `@@iterator` and the other well-known symbols spelled
/// `@@name` resolve to the symbol (a table row can name `[Symbol.iterator]`); anything else is the
/// string key.
pub(crate) fn member_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Name> {
    match name {
        "@@iterator" => v8::Symbol::get_iterator(scope).into(),
        "@@asyncIterator" => v8::Symbol::get_async_iterator(scope).into(),
        "@@toPrimitive" => v8::Symbol::get_to_primitive(scope).into(),
        "@@hasInstance" => v8::Symbol::get_has_instance(scope).into(),
        "@@isConcatSpreadable" => v8::Symbol::get_is_concat_spreadable(scope).into(),
        "@@unscopables" => v8::Symbol::get_unscopables(scope).into(),
        "@@toStringTag" => v8::Symbol::get_to_string_tag(scope).into(),
        _ => key_str(scope, name).unwrap().into(),
    }
}

pub(crate) fn obj_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = key_str(scope, key)?;
    o.get(scope, k.into())
}

pub(crate) fn obj_set_num(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: f64) {
    if let Some(k) = key_str(scope, key) {
        let n = v8::Number::new(scope, v);
        o.create_data_property(scope, k.into(), n.into());
    }
}
pub(crate) fn obj_set_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: &str) {
    if let (Some(k), Some(s)) = (key_str(scope, key), v8::String::new(scope, v)) {
        o.create_data_property(scope, k.into(), s.into());
    }
}
pub(crate) fn obj_set_obj(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: v8::Local<v8::Object>) {
    if let Some(k) = key_str(scope, key) {
        o.create_data_property(scope, k.into(), v.into());
    }
}

// THE INTERFACE-OBJECT SWAP IS GONE. `make_constructable`, `make_constructable_with`,
// `make_constructable_with_data` and `cb_construct_noop` lived here. They took an interface the ONE
// installer had already mounted, built a SECOND function beside it with a raw `v8::Function::builder`,
// copied the old one's `length` off the live object, pointed it at the old prototype and assigned it
// over `globalThis[name]` -- and everything else the installer had put on the object it replaced went
// with the object.
//
// Measured on the release binary, twice (`e1aa5070`, then `e394d00e`): `EventSource.CLOSED` answered
// `undefined` where both captures record the enumerable, non-configurable number `2`, because the
// swap ran after `create_interface` had mounted the three constants and after `seal_constants` (a pass
// deleted) would have sealed them. The install ledger reported all sixteen users as `clobber:unbranded` second
// writers for the same reason: the replacement carried no `korvus.installedBy` brand, so the
// prototype's `constructor` was no longer the function any row had mounted.
//
// A constructor body is a column of the ROW now (`install::ctors::row_ctor`, read by
// `installer::create_interface_in`), so the interface object is built once, constructable, with the
// row's `length`, the row's statics and the row's brand.

/// Milliseconds since epoch via the JS `Date.now()` (matches the JS these replace; 0.0 on failure).
pub(crate) fn js_now(scope: &mut v8::PinScope) -> f64 {
    let now_fn = match intrinsic_fn(scope, "Date.now") { Some(f) => f, None => return 0.0 };
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let r = now_fn.call(scope, u, &[]);
    r.and_then(|v| v.number_value(scope)).unwrap_or(0.0)
}

// `bridge_get` AND `bridge_set` ARE GONE, and with them invariant I3's last open clause
// -- "the bridge ops and `bridge_get` are deleted". They were one untyped pair over a NAME, and that
// is how the page realm came to hold four pieces of engine state as ordinary properties of an
// ordinary object: `__curScript`, `__bodyHandlerAttr`, `__loadFired`, `__workerScripts`. None of them
// was a Web API member body -- the bodies half of I3 has held for a while -- but each was engine state
// a page could read, overwrite or freeze, and two of them were reachable for no reader at all.
//
// Where the four went, and what each cost to move:
//
//   __curScript        a V8 PRIVATE on the global (`core/html/html_script_element.rs`). Both ends
//                      were already Rust; the page was a detour between two Rust statements. Five
//                      touches, not two, because `document.currentScript` NESTS -- a script that
//                      inserts a script must restore the outer one when the inner returns.
//   __loadFired        `OpState` (`korvus-runtime`'s `LoadFired`). Its reader was a JS string, so the
//                      kernel's declared JavaScript fell 10 rows to 9 in the same commit, and
//                      `Engine::eval_bool` -- which had no other caller -- went with it.
//   __bodyHandlerAttr  DELETED OUTRIGHT. It mounted a native function under a name that had had no
//                      reader since W5.1 L19: both callers call the Rust function directly.
//   __workerScripts    NOT MOVED, and that is the ruling. It is a plain global the test harness and
//                      the CDP loader write; nothing ever put it on the holder, so the bridge read
//                      beside the global read could not have answered. See `core/workers/worker.rs`.
//
// THE WINDOW REALM NOW HAS NO HOLDER AT ALL, so `binding::hidden` no longer mints one, no longer
// stashes one on the `korvus_bridge` private (these two functions were its only readers), and no
// longer rewrites `Object.getOwnPropertySymbols`/`Reflect.ownKeys` to hide the symbol it left on the
// global. `no_mask_live::the_bridge_symbol_is_filtered_by_native_functions` keeps its every assertion
// through that deletion, which is the proof it was the right one: the answers that used to need a
// filter are what V8's own intrinsics say once there is nothing on the global to find.
//
// What remains is the WORKER realm's, and it is declared rather than hidden: `worker_recv.rs` mounts
// two native functions (three in a ServiceWorker) on a holder of its own, because their callers are
// the injected boot scripts, which run after the `Deno` strip and so have no op table to reach.
// `korvus-runtime/tests/bridge_slots.rs` reads the holder's own keys in every realm and fails in both
// directions against `tests/fixtures/bridge_slots.manifest` -- which is the check `freeze.sh`'s
// `kb_slots` ratchet structurally cannot be: that ratchet greps `KB.__x =` out of `.js` files and
// kernel Rust STRINGS, so a `bridge_set(scope, "__x", …)` call read as nothing at all. It sat at 0
// and was honest about what it measured, and this whole residue lived under it for months.

/// `DOMStringMap`'s instance template -- `el.dataset`'s named access -- applied once per isolate by the
/// builder (`install::instance_shapes`); a dataset had a cached `ObjectTemplate` of its
/// own until then.
pub(crate) fn configure_dataset(it: v8::Local<v8::ObjectTemplate>) {
    use deno_core::v8::MapFnTo;
    let config = v8::NamedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(named_get, "DOMStringMap", cb_ds_get).map_fn_to()))
        .setter_raw(crate::binding::refs::c(icpt!(named_set, "DOMStringMap", cb_ds_set).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(named_query, "DOMStringMap", cb_ds_query).map_fn_to()))
        .deleter_raw(crate::binding::refs::c(icpt!(named_delete, "DOMStringMap", cb_ds_delete).map_fn_to()))
        .enumerator_raw(crate::binding::refs::c(icpt!(named_keys, "DOMStringMap", cb_ds_enum).map_fn_to()));
    it.set_named_property_handler(config);
}

