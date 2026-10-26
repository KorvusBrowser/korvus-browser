//! `core/html/navigation_api.rs` — `window.navigation`, the Navigation API's read side.
//!
//! Chrome 102+ and Brave expose `navigation` as an own accessor of `window`; korvus had the
//! `Navigation`/`NavigationHistoryEntry` interfaces as registry shells and no `navigation` at all, one
//! of the five own names `Object.getOwnPropertyNames(window)` was short by against Brave 151.
//!
//! WHAT IS HERE. The session history as the API shows it, read off the ONE list `History` keeps
//! (`session_history.rs`): `currentEntry` and `entries()` (the same-origin run around the current
//! entry, one `NavigationHistoryEntry` object per entry), `canGoBack` / `canGoForward`, each entry's
//! `url` / `key` / `id` / `index` / `sameDocument` / `getState()`, `activation`, and the navigating methods:
//! `navigate(url)` and `reload()` start the runtime's navigation of the page, `back` / `forward` / `traverseTo`
//! make `history.go`'s traversal, `updateCurrentEntry({state})` replaces the current entry's state. Their events
//! and promises -- `navigate`, `currententrychange`, `navigatesuccess` / `navigateerror`, `transition` -- are
//! `navigate_event.rs`'s.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, get_prop, global_get, str_val, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::core::html::session_history::{with_history, HistoryState};
use crate::install::kits::{define_accessor, define_value};
use crate::install::MemberDef;

const P_NAV: &str = "korvus_navigation";
const P_ENTRY: &str = "korvus_nav_entry";
const P_ENTRY_URL: &str = "korvus_nav_entry_url";
const P_ENTRY_KEY: &str = "korvus_nav_entry_key";
const P_ENTRY_ID: &str = "korvus_nav_entry_id";
const P_ENTRY_GENERATION: &str = "korvus_nav_entry_generation";
const P_CACHE: &str = "korvus_nav_entries";
const P_ACTIVATION: &str = "korvus_nav_activation_of";

pub(crate) fn page_href(scope: &mut v8::PinScope) -> String {
    global_get(scope, "location")
        .and_then(|l| v8::Local::<v8::Object>::try_from(l).ok())
        .and_then(|l| get_prop(scope, l, "href"))
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default()
}

fn is_navigation(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if priv_get(scope, this, P_NAV).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

fn is_entry(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if priv_get(scope, this, P_ENTRY).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

// ── the entries, read off the session history ─────────────────────────────────────────────────────

/// What the Navigation API shows of the session history: the run of SAME-ORIGIN entries around the
/// current one, `lo..=hi`. An entry of another origin ends the run (HTML's "get session history
/// entries for the navigation API"), so `history.length` can be larger than `entries().length`.
pub(crate) fn visible(h: &HistoryState) -> (usize, usize) {
    fn origin(u: &str) -> String {
        url::Url::parse(u).map(|u| u.origin().ascii_serialization()).unwrap_or_default()
    }
    let cur = origin(&h.entries[h.index].url);
    let mut lo = h.index;
    while lo > 0 && origin(&h.entries[lo - 1].url) == cur {
        lo -= 1;
    }
    let mut hi = h.index;
    while hi + 1 < h.entries.len() && origin(&h.entries[hi + 1].url) == cur {
        hi += 1;
    }
    (lo, hi)
}

/// One shown entry, copied out of the history so no borrow of `OpState` is held while v8 runs.
struct Shown {
    url: String,
    key: String,
    id: String,
    generation: u64,
}

/// The shown entries, the current one's position among them, and the current document's generation.
fn shown(scope: &mut v8::PinScope) -> (Vec<Shown>, usize, u64) {
    let href = page_href(scope);
    with_history(scope, href, |h| {
        let (lo, hi) = visible(h);
        let list = h.entries[lo..=hi]
            .iter()
            .map(|e| Shown { url: e.url.clone(), key: e.key.clone(), id: e.id.clone(), generation: e.generation })
            .collect();
        (list, h.index - lo, h.generation)
    })
}

/// The `NavigationHistoryEntry` for `e`: ONE object per entry, so `navigation.currentEntry ===
/// navigation.entries()[navigation.currentEntry.index]`, as in Chrome. A replacement is a new entry
/// (new `id`), so it gets a new object with the old `key`.
fn entry_object<'s>(scope: &mut v8::PinScope<'s, '_>, nav: v8::Local<v8::Object>, e: &Shown) -> Option<v8::Local<'s, v8::Object>> {
    let cache = match priv_get(scope, nav, P_CACHE).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(c) => c,
        None => {
            let c = v8::Object::new(scope);
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            c.set_prototype(scope, null);
            priv_set(scope, nav, P_CACHE, c.into());
            c
        }
    };
    let k = v8::String::new(scope, &e.id)?;
    if let Some(o) = cache.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        let g = v8::Global::new(scope, o);
        return Some(v8::Local::new(scope, &g));
    }
    let proto = crate::binding::protos::shell_proto(scope, "NavigationHistoryEntry")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, o, P_ENTRY, t);
    let u = str_val(scope, &e.url);
    priv_set(scope, o, P_ENTRY_URL, u);
    let key = str_val(scope, &e.key);
    priv_set(scope, o, P_ENTRY_KEY, key);
    let id = str_val(scope, &e.id);
    priv_set(scope, o, P_ENTRY_ID, id);
    let g: v8::Local<v8::Value> = v8::Number::new(scope, e.generation as f64).into();
    priv_set(scope, o, P_ENTRY_GENERATION, g);
    cache.set(scope, k.into(), o.into());
    Some(o)
}

/// [`entry_object`] for an entry described by its fields -- `navigate_event.rs`'s way in.
pub(crate) fn entry_object_for<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    nav: v8::Local<v8::Object>,
    url: &str,
    key: &str,
    id: &str,
    generation: u64,
) -> Option<v8::Local<'s, v8::Object>> {
    let e = Shown { url: url.to_string(), key: key.to_string(), id: id.to_string(), generation };
    entry_object(scope, nav, &e)
}

/// The object the page already holds for entry `id`, if it holds one (an entry nobody read has none).
pub(crate) fn cached_entry_object<'s>(scope: &mut v8::PinScope<'s, '_>, nav: v8::Local<v8::Object>, id: &str) -> Option<v8::Local<'s, v8::Object>> {
    let cache = priv_get(scope, nav, P_CACHE).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let k = v8::String::new(scope, id)?;
    cache.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

fn current_entry<'s>(scope: &mut v8::PinScope<'s, '_>, nav: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let (list, cur, _) = shown(scope);
    entry_object(scope, nav, &list[cur])
}

fn cb_nav_current_entry(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !is_navigation(scope, this) {
        return;
    }
    match current_entry(scope, this) {
        Some(e) => rv.set(e.into()),
        None => rv.set_null(),
    }
}

fn cb_nav_entries(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !is_navigation(scope, this) {
        return;
    }
    let (list, _, _) = shown(scope);
    let mut items: Vec<v8::Local<v8::Value>> = Vec::with_capacity(list.len());
    for e in &list {
        if let Some(o) = entry_object(scope, this, e) {
            items.push(o.into());
        }
    }
    let arr = v8::Array::new_with_elements(scope, &items);
    rv.set(arr.into());
}

fn cb_nav_can_go(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !is_navigation(scope, args.this()) {
        return;
    }
    let back = crate::binding::protos::body_data(scope, &args).is_true();
    let (list, cur, _) = shown(scope);
    rv.set_bool(if back { cur > 0 } else { cur + 1 < list.len() });
}

fn cb_nav_transition(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !is_navigation(scope, args.this()) {
        return;
    }
    match crate::core::html::navigate_event::current_transition(scope) {
        Some(t) => rv.set(t.into()),
        None => rv.set_null(),
    }
}

/// `activation`: how this document was reached. Its `entry` is the entry the document was activated
/// into -- the first of its generation, which a later `pushState` does not move -- and `from` is null:
/// this realm does not carry the previous document's entry across a navigation.
///
/// `navigationType` is `"replace"` for the realm's first document (it takes the initial `about:blank`
/// entry's slot, as a new tab's first load does; `history_document_committed`) and `"push"` after.
/// NOT MEASURED: the capture bundles record only `NavigationActivation`'s constructor.
fn cb_nav_activation(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !is_navigation(scope, this) {
        return;
    }
    let href = page_href(scope);
    let (generation, first, activation) = with_history(scope, href, |h| {
        let first = h.entries.iter().find(|e| e.generation == h.generation).map(|e| Shown {
            url: e.url.clone(),
            key: e.key.clone(),
            id: e.id.clone(),
            generation: e.generation,
        });
        (h.generation, first, h.activation)
    });
    // One activation per document: minted on the first read after the document was installed.
    if let Some(a) = priv_get(scope, this, P_ACTIVATION).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        let of = priv_get(scope, a, "korvus_nav_activation_generation").and_then(|v| v.number_value(scope)).unwrap_or(-1.0);
        if of == generation as f64 {
            rv.set(a.into());
            return;
        }
    }
    let Some(proto) = crate::binding::protos::shell_proto(scope, "NavigationActivation") else {
        rv.set_null();
        return;
    };
    let a = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, a);
    if let Some(e) = first.as_ref().and_then(|f| entry_object(scope, this, f)) {
        priv_set(scope, a, "korvus_nav_activation_entry", e.into());
    }
    // How the document's commit entered the session history (`HistoryState::activation`); a realm that never
    // committed one: a reloaded document was activated by the reload (Chrome 150: `reload`), else the first
    // document replaced the initial one.
    let reloaded = generation <= 1 && crate::page::arrival(scope).reload;
    let ty = str_val(scope, activation.unwrap_or(if reloaded { "reload" } else if generation <= 1 { "replace" } else { "push" }));
    priv_set(scope, a, "korvus_nav_activation_type", ty);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, a, "korvus_nav_activation", t);
    let g: v8::Local<v8::Value> = v8::Number::new(scope, generation as f64).into();
    priv_set(scope, a, "korvus_nav_activation_generation", g);
    priv_set(scope, this, P_ACTIVATION, a.into());
    rv.set(a.into());
}

fn activation_this(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    if priv_get(scope, args.this(), "korvus_nav_activation").is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}
fn cb_activation_entry(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !activation_this(scope, &args) {
        return;
    }
    match priv_get(scope, args.this(), "korvus_nav_activation_entry") {
        Some(e) => rv.set(e),
        None => rv.set_null(),
    }
}
fn cb_activation_from(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if activation_this(scope, &args) {
        rv.set_null();
    }
}
fn cb_activation_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !activation_this(scope, &args) {
        return;
    }
    if let Some(t) = priv_get(scope, args.this(), "korvus_nav_activation_type") {
        rv.set(t);
    }
}

// ── the navigating methods ────────────────────────────────────────────────────────────────────────

/// The state a `navigate()` / `reload()` carries, serialized now: a refusal is the result's (both promises
/// rejected with the `DataCloneError`), as Blink 150 answers it.
fn serialize_state<'s>(scope: &mut v8::PinScope<'s, '_>, opts: Option<v8::Local<v8::Object>>, op: &'static str) -> Result<Option<v8::Global<v8::Value>>, v8::Local<'s, v8::Value>> {
    let Some(state) = opts.and_then(|o| get_prop(scope, o, "state")).filter(|v| !v.is_undefined()) else { return Ok(None) };
    let outcome = {
        v8::tc_scope!(let tc, scope);
        match crate::modules::structured_clone::clone_for(tc, state, "Navigation", op) {
            Some(c) => Ok(v8::Global::new(tc, c)),
            None => {
                let e = tc.exception().map(|e| v8::Global::new(tc, e));
                tc.reset();
                Err(e)
            }
        }
    };
    match outcome {
        Ok(g) => Ok(Some(g)),
        Err(Some(e)) => Err(v8::Local::new(scope, &e)),
        Err(None) => Err(v8::undefined(scope).into()),
    }
}

/// `navigate(url, options)` (Blink `NavigationApi::navigate`): the arguments converted, an unparsable URL, a
/// `javascript:` URL and an uncloneable `state` answered as rejected promises, then the page's navigation through
/// the runtime, whose `navigate` event takes this call's promises. A navigation no event took was aborted.
fn cb_nav_navigate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::html::navigate_event as ne;
    let this = args.this();
    if !is_navigation(scope, this) {
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'navigate' on 'Navigation': 1 argument required, but only 0 present.");
        return;
    }
    let prefix = "Failed to execute 'navigate' on 'Navigation': ";
    let Ok(Some(vals)) = crate::install::ctor_dicts::convert_method_args_values(scope, "Navigation::navigate", &args, prefix) else { return };
    let raw = vals.first().copied().flatten().map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let opts = vals.get(1).copied().flatten().and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let base = url::Url::parse(&crate::core::dom::base_url(scope)).ok();
    let Some(abs) = base.as_ref().and_then(|b| b.join(&raw).ok()).or_else(|| url::Url::parse(&raw).ok()) else {
        let r = ne::early_dom_error(scope, &format!("Invalid URL '{}'.", ne::invalid_url_text(&raw, base.as_ref())), "SyntaxError");
        rv.set(r.into());
        return;
    };
    if abs.scheme().eq_ignore_ascii_case("javascript") {
        let r = ne::early_dom_error(scope, "The javascript: protocol is not supported", "NotSupportedError");
        rv.set(r.into());
        return;
    }
    let state = match serialize_state(scope, opts, "navigate") {
        Ok(s) => s,
        Err(e) => {
            let r = ne::early_error_result(scope, e);
            rv.set(r.into());
            return;
        }
    };
    let history = opts.and_then(|o| get_prop(scope, o, "history")).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let info = opts.and_then(|o| get_prop(scope, o, "info")).filter(|v| !v.is_undefined());
    // a FRAME's navigation navigates the frame (and fires none of the page's events: module header of navigate_event.rs)
    if let Some(id) = crate::core::html::browsing_context::current_realm_ctx(scope) {
        let nav = crate::core::html::browsing_context::PageNavigation::get(abs.to_string(), crate::core::html::browsing_context::NavigationReason::ScriptInitiated);
        crate::core::html::browsing_context::navigate_target(scope, Some(id), crate::core::html::browsing_context::NavigationTarget::Context(id), nav);
        let (_, r) = ne::begin_non_traverse(scope, info, None);
        rv.set(r.into());
        return;
    }
    let (serial, result) = ne::begin_non_traverse(scope, info, state);
    let handling = match history.as_str() {
        "push" => ne::HistoryHandling::Push,
        "replace" => ne::HistoryHandling::Replace,
        _ => ne::HistoryHandling::Auto,
    };
    let nav = crate::core::html::browsing_context::PageNavigation::get(abs.to_string(), crate::core::html::browsing_context::NavigationReason::ScriptInitiated);
    ne::navigate_page(scope, ne::PageNavigate { nav, history: handling, reason: ne::ClientReason::FrameNavigation, source_element: None, user_initiated: false, form_data: None });
    let r = ne::end_non_traverse(scope, serial, result);
    rv.set(r.into());
}

/// `reload(options)`: a reload of the page, whose `navigate` event takes this call's promises -- the state the
/// options give, else the current entry's.
fn cb_nav_reload(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::html::navigate_event as ne;
    let this = args.this();
    if !is_navigation(scope, this) {
        return;
    }
    let prefix = "Failed to execute 'reload' on 'Navigation': ";
    let Ok(Some(vals)) = crate::install::ctor_dicts::convert_method_args_values(scope, "Navigation::reload", &args, prefix) else { return };
    let opts = vals.first().copied().flatten().and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let state = match serialize_state(scope, opts, "reload") {
        Ok(Some(s)) => Some(s),
        Ok(None) => {
            let href = page_href(scope);
            with_history(scope, href, |h| h.entries[h.index].nav_state.clone())
        }
        Err(e) => {
            let r = ne::early_error_result(scope, e);
            rv.set(r.into());
            return;
        }
    };
    let info = opts.and_then(|o| get_prop(scope, o, "info")).filter(|v| !v.is_undefined());
    let (serial, result) = ne::begin_non_traverse(scope, info, state);
    let href = page_href(scope);
    let nav = crate::core::html::browsing_context::PageNavigation::reload(href, crate::core::html::browsing_context::NavigationReason::ScriptInitiated);
    ne::navigate_page(scope, ne::PageNavigate { nav, history: ne::HistoryHandling::Reload, reason: ne::ClientReason::FrameNavigation, source_element: None, user_initiated: false, form_data: None });
    let r = ne::end_non_traverse(scope, serial, result);
    rv.set(r.into());
}

/// `back()`, `forward()`, `traverseTo(key)` (Blink `NavigationApi::traverseTo`). With no entry to go to, both
/// promises reject with `InvalidStateError` ("Cannot go back" / "Cannot go forward" / "Invalid key"); the current
/// entry's own key resolves both with it at once; a traversal already on its way to the key hands back its own
/// promises; anything else is the SAME traversal `history.go(delta)` makes, whose `navigate` event takes them.
fn cb_nav_traverse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::html::navigate_event as ne;
    let this = args.this();
    if !is_navigation(scope, this) {
        return;
    }
    let which = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if which == "traverseTo" && args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'traverseTo' on 'Navigation': 1 argument required, but only 0 present.");
        return;
    }
    // the arguments, converted by the binding first: `key` a DOMString, `NavigationOptions` (`info`)
    let label = match which.as_str() {
        "back" => "Navigation::back",
        "forward" => "Navigation::forward",
        _ => "Navigation::traverseTo",
    };
    let Ok(Some(vals)) = crate::install::ctor_dicts::convert_method_args_values(scope, label, &args, &format!("Failed to execute '{which}' on 'Navigation': ")) else { return };
    let (key_arg, opts) = if which == "traverseTo" {
        (vals.first().copied().flatten().map(|v| v.to_rust_string_lossy(scope)), vals.get(1).copied().flatten())
    } else {
        (None, vals.first().copied().flatten())
    };
    let info = opts.and_then(|o| v8::Local::<v8::Object>::try_from(o).ok()).and_then(|o| get_prop(scope, o, "info")).filter(|v| !v.is_undefined());
    let (list, cur, _) = shown(scope);
    let target = match which.as_str() {
        "back" => (cur > 0).then(|| cur - 1).ok_or("Cannot go back"),
        "forward" => (cur + 1 < list.len()).then_some(cur + 1).ok_or("Cannot go forward"),
        _ => {
            let key = key_arg.unwrap_or_default();
            list.iter().position(|e| e.key == key).ok_or("Invalid key")
        }
    };
    let t = match target {
        Ok(t) => t,
        Err(msg) => {
            let r = ne::early_dom_error(scope, msg, "InvalidStateError");
            rv.set(r.into());
            return;
        }
    };
    if t == cur {
        let Some(dest) = entry_object(scope, this, &list[t]) else { return };
        let r = ne::early_success_result(scope, dest.into());
        rv.set(r.into());
        return;
    }
    // by KEY, chosen now (Blink `NavigationApi::traverseTo`), where `history.go(delta)` resolves its delta when the
    // step runs
    let (result, fresh) = ne::begin_traverse(scope, &list[t].key, info);
    if fresh {
        ne::traverse_to(scope, list[t].key.clone());
    }
    rv.set(result.into());
}

/// `updateCurrentEntry({state})`: `state` is a REQUIRED member (the capture: `updateCurrentEntry({})`
/// throws `TypeError`, as does no argument), structured-cloned into the current entry, then
/// `currententrychange` with no navigation type (Chrome 150, measured).
fn cb_nav_update_current_entry(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !is_navigation(scope, this) {
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'updateCurrentEntry' on 'Navigation': 1 argument required, but only 0 present.");
        return;
    }
    let prefix = "Failed to execute 'updateCurrentEntry' on 'Navigation': ";
    let Ok(Some(vals)) = crate::install::ctor_dicts::convert_method_args_values(scope, "Navigation::updateCurrentEntry", &args, prefix) else { return };
    let Some(opts) = vals.first().copied().flatten().and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let state = get_prop(scope, opts, "state").unwrap_or_else(|| v8::undefined(scope).into());
    let Some(cloned) = crate::modules::structured_clone::clone_for(scope, state, "Navigation", "updateCurrentEntry") else { return };
    let g = v8::Global::new(scope, cloned);
    let href = page_href(scope);
    with_history(scope, href, |h| {
        let i = h.index;
        h.entries[i].nav_state = Some(g);
    });
    if crate::core::html::browsing_context::current_realm_ctx(scope).is_none() {
        if let Some(cur) = current_entry(scope, this) {
            crate::core::html::navigate_event::current_entry_state_changed(scope, this, cur);
        }
    }
}

fn entry_slot(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, key: &str, rv: &mut v8::ReturnValue) {
    let this = args.this();
    if !is_entry(scope, this) {
        return;
    }
    if let Some(v) = priv_get(scope, this, key) {
        rv.set(v);
    }
}
fn cb_entry_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    entry_slot(scope, &args, P_ENTRY_URL, &mut rv);
}
fn cb_entry_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    entry_slot(scope, &args, P_ENTRY_KEY, &mut rv);
}
fn cb_entry_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    entry_slot(scope, &args, P_ENTRY_ID, &mut rv);
}

fn entry_id(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    priv_get(scope, this, P_ENTRY_ID).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}

/// `index`: the entry's position among the shown entries, `-1` once it has left them (a later push
/// dropped it, or a replacement took its slot).
fn cb_entry_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !is_entry(scope, this) {
        return;
    }
    let id = entry_id(scope, this);
    let (list, _, _) = shown(scope);
    rv.set_int32(list.iter().position(|e| e.id == id).map(|i| i as i32).unwrap_or(-1));
}

/// `sameDocument`: the entry belongs to the document that is current now.
fn cb_entry_same_document(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !is_entry(scope, this) {
        return;
    }
    let of = priv_get(scope, this, P_ENTRY_GENERATION).and_then(|v| v.number_value(scope)).unwrap_or(-1.0);
    let (_, _, generation) = shown(scope);
    rv.set_bool(of == generation as f64);
}

/// `getState()`: a fresh copy of the entry's Navigation API state each call, `undefined` when none
/// was set.
fn cb_entry_get_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !is_entry(scope, this) {
        return;
    }
    let id = entry_id(scope, this);
    let href = page_href(scope);
    let state = with_history(scope, href, |h| h.entries.iter().find(|e| e.id == id).and_then(|e| e.nav_state.clone()));
    match state {
        Some(g) => {
            let v = v8::Local::new(scope, &g);
            if let Some(c) = crate::modules::structured_clone::clone_for(scope, v, "NavigationHistoryEntry", "getState") {
                rv.set(c);
            }
        }
        None => rv.set_undefined(),
    }
}

// ── the tables ─────────────────────────────────────────────────────────────────────────────────

/// Mount one `Navigation` row.
pub fn install_navigation(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let n = member.name;
    let len = member.length as i32;
    match n {
        "currentEntry" => getter_row(scope, proto, member, cb_nav_current_entry),
        "transition" => getter_row(scope, proto, member, cb_nav_transition),
        "activation" => getter_row(scope, proto, member, cb_nav_activation),
        "canGoBack" | "canGoForward" => {
            let data: v8::Local<v8::Value> = v8::Boolean::new(scope, n == "canGoBack").into();
            let g = build_fn_data(scope, cb_nav_can_go, &format!("get {n}"), 0, data);
            define_accessor(scope, proto, member, g, None);
        }
        "entries" => {
            let f = build_fn(scope, cb_nav_entries, n, len);
            define_value(scope, proto, member, f.into(), true);
        }
        "navigate" => {
            let f = build_fn(scope, cb_nav_navigate, n, len);
            define_value(scope, proto, member, f.into(), true);
        }
        "reload" => {
            let f = build_fn(scope, cb_nav_reload, n, len);
            define_value(scope, proto, member, f.into(), true);
        }
        "back" | "forward" | "traverseTo" => {
            let data = str_val(scope, n);
            let f = build_fn_data(scope, cb_nav_traverse, n, len, data);
            define_value(scope, proto, member, f.into(), true);
        }
        "updateCurrentEntry" => {
            let f = build_fn(scope, cb_nav_update_current_entry, n, len);
            define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_navigation: unexpected member {other}"),
    }
}

/// A readonly attribute row: `get <name>` over `cb`.
fn getter_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let g = build_fn(scope, cb, &format!("get {}", member.name), 0);
    define_accessor(scope, proto, member, g, None);
}

/// Mount one `NavigationHistoryEntry` row.
pub fn install_navigation_entry(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let n = member.name;
    match n {
        "key" => getter_row(scope, proto, member, cb_entry_key),
        "id" => getter_row(scope, proto, member, cb_entry_id),
        "url" => getter_row(scope, proto, member, cb_entry_url),
        "index" => getter_row(scope, proto, member, cb_entry_index),
        "sameDocument" => getter_row(scope, proto, member, cb_entry_same_document),
        "getState" => {
            let f = build_fn(scope, cb_entry_get_state, n, member.length as i32);
            define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_navigation_entry: unexpected member {other}"),
    }
}

/// Mount one `NavigationActivation` row.
pub fn install_navigation_activation(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "entry" => getter_row(scope, proto, member, cb_activation_entry),
        "from" => getter_row(scope, proto, member, cb_activation_from),
        "navigationType" => getter_row(scope, proto, member, cb_activation_type),
        other => debug_assert!(false, "install_navigation_activation: unexpected member {other}"),
    }
}

// The rows are the generated registry's, verbatim and in the capture's order -- kinds, lengths,
// `required` counts, flags -- which is where the three interfaces lived as shells once
// gave them bodies. The `on*` rows are the shared event-handler kit: nothing fires them yet (header).
webapi::interface! {
    "Navigation" : "EventTarget" [interface, window] {
        ctor    "Navigation"                                         len 0 = stub,
        attr    "currentEntry"         : Any readonly nullable             = install_navigation,
        attr    "transition"           : Any readonly nullable             = install_navigation,
        attr    "activation"           : Any readonly nullable             = install_navigation,
        attr    "canGoBack"            : Boolean readonly                  = install_navigation,
        attr    "canGoForward"         : Boolean readonly                  = install_navigation,
        handler "onnavigate"                                               = event_handler,
        handler "onnavigatesuccess"                                        = event_handler,
        handler "onnavigateerror"                                          = event_handler,
        handler "oncurrententrychange"                                     = event_handler,
        method  "back"                 : Any len 0                         = install_navigation,
        method  "entries"              : Sequence len 0                    = install_navigation,
        method  "forward"              : Any len 0                         = install_navigation,
        method  "navigate"             : Any len 1 required 1              = install_navigation,
        method  "reload"               : Any len 0                         = install_navigation,
        method  "traverseTo"           : Any len 1 required 1              = install_navigation,
        method  "updateCurrentEntry"   : Undefined len 1 required 1        = install_navigation
    }
}

webapi::interface! {
    "NavigationHistoryEntry" : "EventTarget" [interface, window] {
        ctor    "NavigationHistoryEntry"                       len 0 = stub,
        attr    "key"          : DomString readonly                  = install_navigation_entry,
        attr    "id"           : DomString readonly                  = install_navigation_entry,
        attr    "url"          : UsvString readonly nullable         = install_navigation_entry,
        attr    "index"        : Integer readonly                    = install_navigation_entry,
        attr    "sameDocument" : Boolean readonly                    = install_navigation_entry,
        handler "ondispose"                                          = event_handler,
        method  "getState"     : Any len 0                           = install_navigation_entry
    }
}

webapi::interface! {
    "NavigationActivation" [interface, window] {
        ctor "NavigationActivation"                     len 0 = stub,
        attr "entry"          : Any readonly                  = install_navigation_activation,
        attr "from"           : Any readonly nullable         = install_navigation_activation,
        attr "navigationType" : Any readonly                  = install_navigation_activation
    }
}

/// The three interfaces from their tables, and `window.navigation`, a `[Replaceable]` own accessor
/// (Brave 151: `get navigation` / `set navigation`, and an assignment replaces the accessor with a
/// data property). The singleton is a no-op in a realm whose capture does not publish the global.
pub(crate) fn install_window_navigation<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::exposure::{gate, Exposure, Publishes};
    use crate::core::html::window_globals::{define_window_singleton, Mint, WindowSetter};
    use crate::install::installer::{self, Flavor};

    let mut ledger = installer::take_ledger(scope);
    let event_target = crate::install::live::parent_of(scope, "EventTarget");
    for (def, parent) in [(&NAVIGATIONHISTORYENTRY, event_target), (&NAVIGATIONACTIVATION, None), (&NAVIGATION, event_target)] {
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] navigation install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);

    if !gate(scope, Publishes::Global, "navigation", Exposure::Unconditional) {
        return;
    }
    let Some(o) = mint_navigation(scope) else { return };
    define_window_singleton(scope, global, "navigation", o, WindowSetter::Replaceable, true, Mint::With(mint_navigation));
}

/// This realm's `navigation`: a `Navigation`, marked as the window's own.
fn mint_navigation<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let o = crate::core::html::window_globals::mint_shell(scope, "Navigation")?;
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, o, P_NAV, t);
    Some(o.into())
}
