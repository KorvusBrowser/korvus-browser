//! `modules/cookie_store/jar.rs` — what `document.cookie` and `cookieStore` share on this side of the store: the
//! document's cookie context, the harness document's own store, the `CookieListItem` a cookie becomes, and the
//! change event a write makes.
//!
//! ONE STORE. `cookieStore` used to keep a third copy of the cookies here, in
//! OpState -- a list of its own and `document.cookie` writes copied into it -- while the page's real cookies lived in
//! the transport; `cookieStore.getAll()` saw neither a response's cookies nor the page's attributes. Every cookie
//! is now `transport::cookies`' (Chrome's `CookieMonster`), and a change event says what that store changed.

use deno_core::v8;
use transport::cookies::{Change, CookieItem, CookieMonster, ScriptContext};
use crate::binding::protos::{build_fn_data, set_prop, str_val};

/// The cookies of a document the engine was handed as a string (`Engine::load_html`): no URL, so not the process's
/// store (`transport::cookies`, "the harness").
#[derive(Default)]
pub(crate) struct HarnessCookies(pub(crate) CookieMonster);

/// Run `f` with this realm's harness store (a fresh one if the realm has none).
pub(crate) fn with_harness<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut CookieMonster) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    match st.try_borrow_mut::<HarnessCookies>() {
        Some(h) => f(&mut h.0),
        None => f(&mut CookieMonster::default()),
    }
}

/// The cookie context of the RUNNING realm's document -- `cookieStore`'s default cookie URL, site for cookies and
/// top-frame origin (Blink's `DefaultCookieURL` & co.): a frame's, or the page's.
pub(crate) fn realm_script_context(scope: &mut v8::PinScope) -> Option<ScriptContext> {
    let (url, cross_site, top) = match crate::core::html::browsing_context::realm_cookie_context(scope) {
        Some(c) => c,
        None => {
            let page = page_cookie_url(scope);
            (page.clone(), false, page)
        }
    };
    ScriptContext::new(&url, cross_site, Some(top.as_str()))
}

/// The page document's cookie URL (`about:blank` until a navigation: the harness).
pub(crate) fn page_cookie_url(scope: &mut v8::PinScope) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_else(|| "about:blank".to_string())
}

/// A member of a dictionary converted to JS, defined as WebIDL's dictionary-to-ES conversion defines it
/// (`CreateDataProperty`): no setter on `Object.prototype` runs.
fn define(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str, v: v8::Local<v8::Value>) {
    if let Some(k) = v8::String::new(scope, key) {
        let _ = o.create_data_property(scope, k.into(), v);
    }
}

/// A cookie as the JS `CookieListItem` `cookieStore` hands out (Blink's `ToCookieListItem` and its dictionary's
/// conversion): the members it has, in the dictionary's member order -- `domain`, `expires`, `name`, `partitioned`,
/// `path`, `sameSite`, `secure`, `value` (Chrome 150, `tests/fixtures/cookies`).
pub(crate) fn cookie_to_obj<'s>(scope: &mut v8::PinScope<'s, '_>, c: &CookieItem) -> v8::Local<'s, v8::Value> {
    let o = v8::Object::new(scope);
    let domain: v8::Local<v8::Value> = match &c.domain {
        Some(d) => str_val(scope, d),
        None => v8::null(scope).into(),
    };
    define(scope, o, "domain", domain);
    if let Some(expires) = c.expires {
        let v: v8::Local<v8::Value> = match expires {
            Some(ms) => v8::Number::new(scope, ms).into(),
            None => v8::null(scope).into(),
        };
        define(scope, o, "expires", v);
    }
    let name = str_val(scope, &c.name);
    define(scope, o, "name", name);
    let partitioned: v8::Local<v8::Value> = v8::Boolean::new(scope, c.partitioned).into();
    define(scope, o, "partitioned", partitioned);
    let path = str_val(scope, &c.path);
    define(scope, o, "path", path);
    if let Some(ss) = c.same_site {
        let v = str_val(scope, ss);
        define(scope, o, "sameSite", v);
    }
    let secure: v8::Local<v8::Value> = v8::Boolean::new(scope, c.secure).into();
    define(scope, o, "secure", secure);
    if let Some(value) = &c.value {
        let v = str_val(scope, value);
        define(scope, o, "value", v);
    }
    o.into()
}

/// The `change` event the store's `changes` make for this realm's `cookieStore`: what its document may read of them
/// (`transport::cookies::change_items`), delivered on a task to the listeners registered NOW -- the one event
/// store's `et_snapshot`, the `onchange` handler among them -- so a listener added after the write but before the
/// task runs is not called. Nothing visible, or nobody listening: no event.
///
/// The realm's `cookieStore` is read from its window slot, never through `window.cookieStore`: an engine step does
/// not run a page's getter (a bot-detection script D2: every `document.cookie` write called a replaced `cookieStore` getter;
/// Chrome 150 calls none, `tests/fixtures/cookies`).
pub(crate) fn cookie_emit(scope: &mut v8::PinScope, changes: &[Change]) {
    if changes.is_empty() {
        return;
    }
    let Some(cs) = crate::core::html::window_globals::window_slot_get(scope, "cookieStore").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(listener) = realm_script_context(scope) else { return };
    let (changed, deleted) = transport::cookies::change_items(changes, &listener);
    if changed.is_empty() && deleted.is_empty() {
        return;
    }
    let csv: v8::Local<v8::Value> = cs.into();
    let listeners = crate::core::events::event_target::et_snapshot(scope, csv, "change");
    if listeners.is_empty() {
        return;
    }
    let snapshot = v8::Array::new(scope, 0);
    for (i, f) in listeners.into_iter().enumerate() {
        snapshot.set_index(scope, i as u32, f);
    }
    let changed_arr = v8::Array::new(scope, changed.len() as i32);
    for (i, c) in changed.iter().enumerate() {
        let o = cookie_to_obj(scope, c);
        changed_arr.set_index(scope, i as u32, o);
    }
    let deleted_arr = v8::Array::new(scope, deleted.len() as i32);
    for (i, c) in deleted.iter().enumerate() {
        let o = cookie_to_obj(scope, c);
        deleted_arr.set_index(scope, i as u32, o);
    }
    let data = v8::Array::new(scope, 4);
    data.set_index(scope, 0, csv);
    let cl: v8::Local<v8::Value> = changed_arr.into();
    let dl: v8::Local<v8::Value> = deleted_arr.into();
    let sl: v8::Local<v8::Value> = snapshot.into();
    data.set_index(scope, 1, cl);
    data.set_index(scope, 2, dl);
    data.set_index(scope, 3, sl);
    let cb = build_fn_data(scope, cb_cookie_deliver, "", 0, data.into());
    // The engine's task, not the page's `setTimeout`.
    crate::core::intersection_observer::rendering::engine_task(scope, cb, 0.0);
}

/// Deliver one queued change (setTimeout task): data is `[cookieStore, changed, deleted, snapshot]`.
pub(crate) fn cb_cookie_deliver(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = match v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) { Ok(a) => a, Err(_) => return };
    let cs = match data.get_index(scope, 0) { Some(c) => c, None => return };
    let changed = match data.get_index(scope, 1) { Some(c) => c, None => return };
    let deleted = match data.get_index(scope, 2) { Some(d) => d, None => return };
    let snapshot = match data.get_index(scope, 3).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) { Some(s) => s, None => return };
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "changed", changed);
    set_prop(scope, init, "deleted", deleted);
    let ty = str_val(scope, "change");
    let ev = match crate::binding::protos::iface_object(scope, "CookieChangeEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()).and_then(|c| c.new_instance(scope, &[ty, init.into()])) { Some(e) => e, None => return };
    let evv: v8::Local<v8::Value> = ev.into();
    for i in 0..snapshot.length() {
        if let Some(f) = snapshot.get_index(scope, i).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
            f.call(scope, cs, &[evv]); // this = cookieStore; a throwing listener returns None and is ignored
        }
    }
}
