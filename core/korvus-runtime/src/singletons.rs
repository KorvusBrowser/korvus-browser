//! `singletons.rs` -- the `History` table, and the IDENTITY of `location` and `document`
//! (the last of `core/korvus-webapi/src/js/dom/09i-singletons.js`).
//!
//! WHY THE RUNTIME CRATE, like `navigator_table.rs`. `History`'s bodies navigate, and the page's URL
//! is `HostLocation` -- a private of this crate, which `op_nav_to` is the only other writer of.
//! `History`'s bodies navigate and the page's URL is `HostLocation`, an op_state type
//! `korvus-webapi` cannot name.
//! The `interface!` scanner reads `core/*/src/**/*.rs`, so a table here is the same single definition
//! site as one over there.
//!
//! WHY AT CONTEXT CREATION AND NOT WHERE THE OBJECTS ARE BUILT -- the constraint the deleted module's
//! header stated. `host.js` and `shared-globals.js` construct `location`, `history` and `performance`
//! long before any interface object exists, and they are shared with the WORKER realm where the same
//! names mean different interfaces (`WorkerGlobalScope` has no `History` at all). The reshaping is
//! window-only and needs the shells, so it belongs after both -- which is where every table install of
//! this lane runs.
//!
//! WHAT WAS STILL host.js's WHEN THIS MODULE LANDED -- the performance timeline -- is gone:
//! made `Performance` and its six neighbours `interface!` tables in `korvus-webapi`, and
//! `rehome_performance` went with `KB.__createPerformance`, exactly as this header predicted.

use deno_core::v8;

use webapi::binding::protos::{build_fn, str_val, throw_type_error};
use webapi::install::installer::install_singleton;
use webapi::install::live::{parent_of, strip_own};
use webapi::install::MemberDef;

use webapi::page::{HistoryEntry as Entry, HistoryState};


// ── History ──────────────────────────────────────────────────────────────────────────────────────

/// The session history this realm moves through: `webapi::page::HistoryState`, one list shared with
/// the Navigation API. Every WRITE is here.
///
/// IT WAS NOT A STACK: `length` was a counter `pushState` bumped and `back()`,
/// `forward()` and `go()` did nothing. Measured on all four stock browsers of the family
/// (`tests/fixtures/p11a/fragment.*.txt`): a fragment navigation adds an entry (`replace()` does not),
/// and a traversal moves the URL, restores the entry's `state`, and fires `popstate` -- then
/// `hashchange` when the fragment differs -- in one task after the call returns.
fn with_history<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut HistoryState) -> R) -> R {
    let href = crate::location::current_href(scope);
    webapi::page::with_history(scope, href, f)
}

/// A new document was installed (`wire_document_identity`, per navigation): its entry is the current
/// one. The very first document REPLACES the initial `about:blank` entry, as a popup's first load
/// does -- a new tab's own `about:blank` only the tab's first URL replaces (`HistoryEntry::seeded`). After that the
/// history is the realms' together (`Engine::successor` carries it) and the document enters it as its
/// navigation decided (`Arrival::history`, D88 step 2): after the current entry, in its place, or -- a traversal --
/// at the entry it went to, whose document's other entries are this document's now.
pub(crate) fn history_document_committed(scope: &mut v8::PinScope) {
    use webapi::page::HistoryCommit as C;
    webapi::navigation_document_committed(scope);
    let href = crate::location::current_href(scope);
    let arrival = webapi::page::arrival(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<HistoryState>().is_none() {
        st.put(HistoryState::default());
    }
    let h = st.borrow_mut::<HistoryState>();
    h.generation += 1;
    let generation = h.generation;
    let transition = if arrival.transition.is_empty() { "link".to_string() } else { arrival.transition.clone() };
    let fresh = |url: String| {
        let mut e = Entry::new(url, None, generation);
        e.transition = transition.clone();
        e
    };
    let initial = h.entries.len() <= 1
        && h.entries.first().map(|e| e.generation == 0 && e.state.is_none() && (!e.seeded || transition == "auto_toplevel")).unwrap_or(true);
    let traversed = match arrival.history {
        C::Traverse(serial) => h.entries.iter().position(|e| e.serial == serial),
        _ => None,
    };
    if initial {
        h.entries = vec![fresh(href)];
        h.index = 0;
        h.activation = Some(if arrival.reload { "reload" } else { "replace" });
    } else if let Some(i) = traversed.or_else(|| (arrival.history == C::Reload).then_some(h.index)) {
        let old = h.entries[i].generation;
        for e in h.entries.iter_mut().filter(|e| e.generation == old) {
            e.generation = generation;
        }
        h.entries[i].url = href;
        if arrival.history == C::Reload {
            h.entries[i].transition = "reload".to_string();
        }
        h.index = i;
        h.activation = Some(if traversed.is_some() { "traverse" } else { "reload" });
    } else if arrival.history == C::Replace {
        let mut e = h.entries[h.index].replacement(href, None, generation);
        e.transition = transition.clone();
        let i = h.index;
        h.entries[i] = e;
        h.activation = Some("replace");
    } else {
        h.entries.truncate(h.index + 1);
        h.entries.push(fresh(href));
        h.index = h.entries.len() - 1;
        h.activation = Some("push");
    }
}

fn without_fragment(url: &str) -> Option<String> {
    url::Url::parse(url).ok().map(|mut u| {
        u.set_fragment(None);
        u.to_string()
    })
}

/// The URL's fragment, none and empty alike (WTF's null and empty strings compare equal, so Blink's
/// `FragmentIdentifier() !=` does not tell `page` from `page#`).
fn fragment_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok().map(|u| u.fragment().unwrap_or("").to_string())
}

/// Fire `popstate` with `state` at the window.
fn fire_popstate(scope: &mut v8::PinScope, state: Option<v8::Global<v8::Value>>) {
    let init = webapi::binding::protos::init_dict(scope);
    let v: v8::Local<v8::Value> = match state {
        Some(g) => v8::Local::new(scope, &g),
        None => v8::null(scope).into(),
    };
    webapi::binding::protos::set_prop(scope, init, "state", v);
    webapi::install::live::fire_window_event(scope, "PopStateEvent", "popstate", init);
}

fn fire_hashchange(scope: &mut v8::PinScope, old: &str, new: &str) {
    let init = webapi::binding::protos::init_dict(scope);
    let o = str_val(scope, old);
    let n = str_val(scope, new);
    webapi::binding::protos::set_prop(scope, init, "oldURL", o);
    webapi::binding::protos::set_prop(scope, init, "newURL", n);
    webapi::install::live::fire_window_event(scope, "HashChangeEvent", "hashchange", init);
}

/// Run `cb` with `data` on a later task -- an ENGINE task (the history-traversal and DOM-manipulation task sources),
/// never a timer: the page's timer nesting does not clamp it (a `location.hash` set from a deeply nested timer chain
/// still gets its `hashchange` in the next turn, as Chrome's DOM-manipulation task does).
fn queue_task<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, data: v8::Local<'s, v8::Value>) {
    queue_task_after(scope, cb, data, 0.0);
}

/// [`queue_task`], due `ms` from now.
fn queue_task_after<'s>(scope: &mut v8::PinScope<'s, '_>, cb: impl v8::MapFnTo<v8::FunctionCallback>, data: v8::Local<'s, v8::Value>, ms: f64) {
    let f = webapi::binding::protos::build_fn_data(scope, cb, "", 0, data);
    crate::scope_timers::queue_engine_task(scope, f, ms);
}

/// The time a traversal takes through the BROWSER (Blink `History::go` -> the browser's `NavigationController` ->
/// `CommitSameDocumentNavigation` back in the renderer): Chrome 150 runs a message posted and a 0 ms timer armed right
/// after `history.back()` before the traversal's `navigate` event, and the next animation frame and a timer the nesting
/// clamp holds back (4 ms) after it (measured, `fixtures/navigation-events/order-*`). A round trip of 1 ms here.
const BROWSER_ROUND_TRIP_MS: f64 = 1.0;

fn cb_fire_hashchange(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(pair) = v8::Local::<v8::Array>::try_from(webapi::binding::protos::body_data(scope, &args)) else { return };
    let old = pair.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let new = pair.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    fire_hashchange(scope, &old, &new);
}

/// The URL and history update steps of a same-document navigation (Blink
/// `DocumentLoader::UpdateForSameDocumentNavigation`): the URL moves, the session history takes the entry -- a new
/// one, the current one replaced, or the traversal's destination made current -- and the Navigation API reports the
/// commit (`currententrychange`, the method's promises). A fragment navigation carries the Navigation API state over
/// to its entry (HTML's "navigate to a fragment"; Blink `SetHistoryItemStateForCommit`); the history API's and an
/// interception's do not.
///
/// `kind` is what `Page.navigatedWithinDocument` reports it as (D88 step 2): `fragment`, `historyApi` or `other`.
pub(crate) fn url_and_history_update(
    scope: &mut v8::PinScope,
    url: &str,
    nav_type: webapi::NavigationType,
    key: Option<&str>,
    classic: Option<v8::Global<v8::Value>>,
    carry_nav_state: bool,
    kind: &'static str,
) {
    use webapi::NavigationType as T;
    let before = webapi::navigation_before_update(scope);
    crate::location::move_url(scope, url);
    // a same-document entry is titled by its document, as Chrome titles it (`Page.getNavigationHistory`)
    let title = webapi::page::page_title(&deno_core::JsRuntime::op_state_from(scope));
    with_history(scope, |h| {
        let generation = h.generation;
        match nav_type {
            T::Traverse => {
                if let Some(i) = key.and_then(|k| h.entries.iter().position(|e| e.key == k)) {
                    h.index = i;
                }
            }
            T::Reload => {}
            T::Push | T::Replace => {
                let nav_state = if carry_nav_state { h.entries[h.index].nav_state.clone() } else { None };
                let mut entry = if nav_type == T::Replace {
                    h.entries[h.index].replacement(url.to_string(), classic, generation)
                } else {
                    Entry::new(url.to_string(), classic, generation)
                };
                entry.nav_state = nav_state;
                entry.title = title.clone();
                if nav_type == T::Replace {
                    h.entries[h.index] = entry;
                } else {
                    h.entries.truncate(h.index + 1);
                    h.entries.push(entry);
                    h.index = h.entries.len() - 1;
                }
            }
        }
    });
    webapi::update_for_navigation(scope, nav_type, before);
    webapi::page::note_same_document(&deno_core::JsRuntime::op_state_from(scope), url, kind);
}

/// HTML's "navigate to a fragment" (Blink `DocumentLoader::CommitSameDocumentNavigation`): the `navigate` event, then
/// -- unless it was canceled or intercepted -- `hashchange` queued when only the fragment changed, the URL and
/// history update steps, and `popstate` with a null state. The order is Chrome 150's
/// (`fixtures/navigation-events`, `fixtures/p11a/fragment.*.txt`): `navigate`, `currententrychange`, `popstate`, the
/// call returns, `navigatesuccess` in a microtask, `hashchange` a task later.
pub(crate) fn fragment_navigate(
    scope: &mut v8::PinScope,
    url: &str,
    nav_type: webapi::NavigationType,
    source_element: Option<v8::Global<v8::Object>>,
    user_initiated: bool,
) {
    let mut p = webapi::NavigateParams::new(url, webapi::NavigateEventType::Fragment, nav_type);
    p.source_element = source_element;
    p.user_initiated = user_initiated;
    if webapi::dispatch_navigate_event(scope, p) != webapi::DispatchResult::Continue {
        return;
    }
    let old = crate::location::current_href(scope);
    if fragment_of(&old) != fragment_of(url) && without_fragment(&old) == without_fragment(url) {
        let pair = v8::Array::new(scope, 2);
        let o = str_val(scope, &old);
        let n = str_val(scope, url);
        pair.set_index(scope, 0, o);
        pair.set_index(scope, 1, n);
        queue_task(scope, cb_fire_hashchange, pair.into());
    }
    url_and_history_update(scope, url, nav_type, None, None, true, "fragment");
    fire_popstate(scope, None);
}

/// An intercepted navigation's commit ([`webapi::PageHooks::commit`]): the URL and history update steps, and
/// `popstate` when Blink fires one (a fragment navigation no navigation method started, a traversal) -- a traversal's
/// as the next task, after the microtasks its commit queued ([`cb_traverse`]).
pub(crate) fn commit_intercepted(scope: &mut v8::PinScope, c: webapi::NavigateCommit) {
    url_and_history_update(scope, &c.url, c.nav_type, c.destination_key.as_deref(), c.classic_state, false, "other");
    if !c.fire_popstate {
        return;
    }
    if c.nav_type == webapi::NavigationType::Traverse {
        let state = with_history(scope, |h| h.entries[h.index].state.clone());
        queue_popstate_next(scope, state);
    } else {
        fire_popstate(scope, None);
    }
}

/// `popstate` with `state`, as the very next task.
fn queue_popstate_next(scope: &mut v8::PinScope, state: Option<v8::Global<v8::Value>>) {
    let s: v8::Local<v8::Value> = match state {
        Some(g) => v8::Local::new(scope, &g),
        None => v8::null(scope).into(),
    };
    let f = webapi::binding::protos::build_fn_data(scope, cb_popstate_task, "", 0, s);
    crate::scope_timers::queue_next_task(scope, f);
}

fn cb_popstate_task(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = webapi::binding::protos::body_data(scope, &args);
    let state = (!s.is_null()).then(|| v8::Global::new(scope, s));
    fire_popstate(scope, state);
}

/// A traversal waiting in HTML's session history traversal queue: `history.go(delta)`'s delta, resolved when its
/// step runs, or the Navigation API's key, chosen when it was called.
enum Traversal {
    Delta(i64),
    Key(String),
}

/// The session history traversal queue (HTML "append session history traversal steps"): ONE traversal at a time, the
/// next starting a browser round trip after the one before it committed. Chrome 150: `history.back()` asked again from
/// a 0 ms timer that runs before the first traversal arrives goes back a second step, from where the first arrived,
/// and only after the first one's `hashchange` (measured; WPT `history-traversal/popstate_event.html`).
#[derive(Default)]
struct TraversalQueue {
    pending: std::collections::VecDeque<Traversal>,
    in_flight: bool,
}

fn queue_traversal(scope: &mut v8::PinScope, t: Traversal) {
    let start = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<TraversalQueue>().is_none() {
            st.put(TraversalQueue::default());
        }
        let q = st.borrow_mut::<TraversalQueue>();
        q.pending.push_back(t);
        !std::mem::replace(&mut q.in_flight, true)
    };
    if start {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        queue_task_after(scope, cb_traverse, u, BROWSER_ROUND_TRIP_MS);
    }
}

/// The traversal task (HTML "apply the traverse history step") for the traversal at the head of the queue: a DELTA
/// resolved NOW, against the entry that is current when the step runs, or a KEY; nothing when there is no entry there
/// any more. The next traversal in the queue starts a round trip after this one.
///
/// A same-document traversal fires `navigate` and commits in this task, queueing `hashchange` (when the two URLs
/// differ in their fragment ALONE: `go(-2)` from `?s=1#q` to `#e` fires `popstate` and no `hashchange` on all four
/// browsers, measured) as Blink's `CommitSameDocumentNavigationInternal` does, before the commit -- so a message or a
/// timer the `navigate` listener queued runs first. Its `popstate` runs as the very next task, which is where Chrome
/// 150 has it: after the microtask checkpoint that follows its Navigation API commit (`committed`, `navigatesuccess`
/// and `finished` before `popstate`, measured) -- a task here runs inside the pump's own microtask checkpoint, so the
/// checkpoint cannot run inside it.
fn cb_traverse(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (head, more) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(q) = st.try_borrow_mut::<TraversalQueue>() else { return };
        let head = q.pending.pop_front();
        let more = !q.pending.is_empty();
        q.in_flight = more;
        (head, more)
    };
    if let Some(t) = head {
        traverse_step(scope, t);
    }
    if more {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        queue_task_after(scope, cb_traverse, u, BROWSER_ROUND_TRIP_MS);
    }
}

fn traverse_step(scope: &mut v8::PinScope, t: Traversal) {
    let step = with_history(scope, |h| {
        let i = match &t {
            Traversal::Key(k) => h.entries.iter().position(|e| &e.key == k)?,
            Traversal::Delta(d) => {
                let t = h.index as i64 + d;
                (t >= 0 && (t as usize) < h.entries.len()).then_some(t as usize)?
            }
        };
        let e = &h.entries[i];
        Some((e.key.clone(), e.url.clone(), e.state.clone(), e.generation == h.generation, e.serial))
    });
    let Some((key, url, state, same_document, serial)) = step else {
        if let Traversal::Key(k) = &t {
            webapi::traverse_cancelled(scope, k);
        }
        return;
    };
    if !same_document {
        crate::location::request_traversal(scope, &url, serial);
        return;
    }
    let mut p = webapi::NavigateParams::new(url.clone(), webapi::NavigateEventType::Fragment, webapi::NavigationType::Traverse);
    p.destination_key = Some(key.clone());
    if webapi::dispatch_navigate_event(scope, p) != webapi::DispatchResult::Continue {
        return;
    }
    let old = crate::location::current_href(scope);
    if fragment_of(&old) != fragment_of(&url) && without_fragment(&old) == without_fragment(&url) {
        let pair = v8::Array::new(scope, 2);
        let o = str_val(scope, &old);
        let n = str_val(scope, &url);
        pair.set_index(scope, 0, o);
        pair.set_index(scope, 1, n);
        queue_task(scope, cb_fire_hashchange, pair.into());
    }
    // a traversal between two entries that differ in their fragment alone is a fragment navigation (Chrome 150)
    let kind = if without_fragment(&old) == without_fragment(&url) { "fragment" } else { "historyApi" };
    url_and_history_update(scope, &url, webapi::NavigationType::Traverse, Some(&key), None, false, kind);
    queue_popstate_next(scope, state);
}

/// `back()` / `forward()` / `go(delta)`: a traversal by `delta` in the queue; `go(0)` (and `go()`) reloads. Nothing
/// happens synchronously -- measured.
fn traverse(scope: &mut v8::PinScope, delta: i64) {
    if delta == 0 {
        crate::location::request_reload(scope);
        return;
    }
    queue_traversal(scope, Traversal::Delta(delta));
}

/// The Navigation API's traversal to the entry `key` names ([`webapi::PageHooks::traverse`]).
pub(crate) fn traverse_to_key(scope: &mut v8::PinScope, key: String) {
    queue_traversal(scope, Traversal::Key(key));
}

/// Is `this` the `history` singleton? Anything else throws Chrome's `Illegal invocation` -- probed on
/// headless Chrome 150.0.7871.46 over a loopback server, 2026-09-11:
/// `Object.getOwnPropertyDescriptor(History.prototype, "length").get.call({})` throws.
fn history_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    let ok = parent_of(scope, "History")
        .map(|(proto, _)| {
            if this == proto {
                return false;
            }
            let mut cur = this.get_prototype(scope);
            while let Some(c) = cur {
                let Ok(co) = v8::Local::<v8::Object>::try_from(c) else { break };
                if co == proto {
                    return true;
                }
                cur = co.get_prototype(scope);
            }
            false
        })
        .unwrap_or(false);
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

/// Run `f` in the realm the receiver belongs to: a `History` method called on another same-origin realm's `history`
/// acts on THAT realm's history, as Chrome's does (the bindings take the receiver's own `History`). YouTube binds a hidden
/// `about:blank` frame's `replaceState` and `pushState` to the page's `history` (its `bindHistoryStateFunctions`), and
/// Chrome 150 rewrites the page's URL with them; run in the frame's realm, the brand check (the frame's own
/// `History.prototype`) refused the page's object with `Illegal invocation`, and a search never left the home page.
fn in_receiver_realm<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, f: impl FnOnce(&mut v8::PinScope) -> R) -> R {
    match this.get_creation_context(scope).filter(|c| *c != scope.get_current_context()) {
        Some(ctx) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, ctx);
            f(scope)
        }
        None => f(scope),
    }
}

fn cb_hist_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    in_receiver_realm(scope, this, |scope| {
        if !history_recv(scope, args.this()) {
            return;
        }
        let n = with_history(scope, |h| h.entries.len());
        rv.set_double(n as f64);
    })
}

fn cb_hist_scroll_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    in_receiver_realm(scope, this, |scope| {
        if !history_recv(scope, args.this()) {
            return;
        }
        let s = with_history(scope, |h| h.scroll_restoration.clone());
        let v = str_val(scope, &s);
        rv.set(v);
    })
}

/// `scrollRestoration` is an enumerated attribute: a value outside the enum is IGNORED, not stored.
/// Chrome answers "auto" after `history.scrollRestoration = "bogus"` (same probe).
fn cb_hist_scroll_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    in_receiver_realm(scope, this, |scope| {
        if !history_recv(scope, args.this()) {
            return;
        }
        let w = args.get(0).to_rust_string_lossy(scope);
        if w == "auto" || w == "manual" {
            with_history(scope, |h| h.scroll_restoration = w);
        }
    })
}

fn cb_hist_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    in_receiver_realm(scope, this, |scope| {
        if !history_recv(scope, args.this()) {
            return;
        }
        // A frame's history.state is the frame's.
        if let Some(v) = webapi::frame_history_state(scope) {
            rv.set(v);
            return;
        }
        let g = with_history(scope, |h| h.entries[h.index].state.clone());
        match g {
            Some(g) => {
                let v = v8::Local::new(scope, &g);
                rv.set(v);
            }
            None => rv.set_null(),
        }
    })
}

/// `back()` / `forward()` / `go(delta)` -- a queued traversal of the session history ([`traverse`]).
/// `go`'s argument is WebIDL `long`, so `go("x")` is `go(0)`, a reload.
fn cb_hist_traverse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    in_receiver_realm(scope, this, |scope| {
        if !history_recv(scope, args.this()) {
            return;
        }
        let which = webapi::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
        let delta = match which.as_str() {
            "back" => -1,
            "forward" => 1,
            _ => {
                let v = args.get(0);
                if v.is_undefined() { 0 } else { v.int32_value(scope).unwrap_or(0) as i64 }
            }
        };
        traverse(scope, delta);
    })
}

/// `pushState(data, unused, url)` / `replaceState(...)` (Blink `History::pushState` / `StateObjectAdded`): the
/// arguments converted, the state STRUCTURED-CLONED (a refusal is the call's `DataCloneError`), the URL -- none or
/// empty is the document's own -- resolved against the document's base URL and refused as a `SecurityError` where
/// the document may not rewrite its URL to it, then the `navigate` event, and, unless it was canceled or intercepted,
/// the URL and history update steps. No `popstate`, no `hashchange`.
///
/// The state is a clone read back the same until it changes, as Chrome's is: `history.pushState(o, ""); history.state
/// === o` is false and `history.state === history.state` true (measured). It used to keep the page's own object.
fn cb_hist_push(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    in_receiver_realm(scope, this, |scope| hist_state_change(scope, &args, true));
}
fn cb_hist_replace(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    in_receiver_realm(scope, this, |scope| hist_state_change(scope, &args, false));
}

fn hist_state_change(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, push: bool) {
    if !history_recv(scope, args.this()) {
        return;
    }
    let method = if push { "pushState" } else { "replaceState" };
    // the binding's conversions: `DOMString unused`, `optional USVString? url = null`
    if args.get(1).to_string(scope).is_none() {
        return;
    }
    let url_arg = args.get(2);
    let url_s = if url_arg.is_null_or_undefined() {
        None
    } else {
        match url_arg.to_string(scope) {
            Some(s) => Some(s.to_rust_string_lossy(scope)),
            None => return,
        }
    };
    // Called in a FRAME's realm: the frame's URL and state, never the page's.
    if webapi::frame_history_change(scope, url_s.as_deref(), args.get(0), method).is_some() {
        return;
    }
    let data = v8::Local::new(scope, args.get(0));
    let Some(cloned) = webapi::structured_clone_for(scope, data, "History", method) else { return };
    let current = crate::location::current_href(scope);
    let doc = url::Url::parse(&current).ok();
    let full = match url_s.as_deref().filter(|s| !s.is_empty()) {
        None => doc.clone(),
        Some(s) => url::Url::parse(&webapi::page_base_url(scope)).ok().and_then(|b| b.join(s).ok()).or_else(|| url::Url::parse(s).ok()),
    };
    let allowed = match (&full, &doc) {
        (Some(u), Some(d)) => webapi::can_change_to_url_for_history_api(u, d, !d.origin().is_tuple()),
        _ => false,
    };
    if !allowed {
        let shown = match (&full, url_s.as_deref()) {
            (Some(u), _) => u.to_string(),
            (None, Some(s)) => webapi::invalid_url_text(s, doc.as_ref()),
            (None, None) => current.clone(),
        };
        let origin = doc.as_ref().map(|d| d.origin().ascii_serialization()).unwrap_or_else(|| "null".to_string());
        let msg = format!("Failed to execute '{method}' on 'History': A history state object with URL '{shown}' cannot be created in a document with origin '{origin}' and URL '{current}'.");
        webapi::binding::protos::throw_dom(scope, &msg, "SecurityError");
        return;
    }
    let Some(full) = full else { return };
    let nav_type = if push { webapi::NavigationType::Push } else { webapi::NavigationType::Replace };
    let state = v8::Global::new(scope, cloned);
    let mut p = webapi::NavigateParams::new(full.as_str(), webapi::NavigateEventType::HistoryApi, nav_type);
    p.classic_state = Some(state.clone());
    if webapi::dispatch_navigate_event(scope, p) != webapi::DispatchResult::Continue {
        return;
    }
    url_and_history_update(scope, full.as_str(), nav_type, None, Some(state), false, "historyApi");
}

/// The body every row of [`HISTORY`] names.
pub fn install_history(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use webapi::install::kits::{define_accessor, define_value};
    match member.name {
        "length" => {
            let g = build_fn(scope, cb_hist_length, "get length", 0);
            define_accessor(scope, proto, member, g, None);
        }
        "scrollRestoration" => {
            let g = build_fn(scope, cb_hist_scroll_get, "get scrollRestoration", 0);
            let s = build_fn(scope, cb_hist_scroll_set, "set scrollRestoration", 1);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "state" => {
            let g = build_fn(scope, cb_hist_state, "get state", 0);
            define_accessor(scope, proto, member, g, None);
        }
        "back" | "forward" | "go" => {
            let d = str_val(scope, member.name);
            let f = webapi::binding::protos::build_fn_data(scope, cb_hist_traverse, member.name, member.length as i32, d);
            define_value(scope, proto, member, f.into(), true);
        }
        "pushState" | "replaceState" => {
            // The arity guard is INSIDE the receiver check, which is Chrome's order.
            let f = if member.name == "pushState" {
                build_fn(scope, cb_hist_push, member.name, member.length as i32)
            } else {
                build_fn(scope, cb_hist_replace, member.name, member.length as i32)
            };
            let guarded = webapi::binding::protos::make_arity_guard(
                scope, "History", member.name, member.required as i32, member.length as i32, f.into(), member.ty,
            );
            define_value(scope, proto, member, guarded.into(), true);
        }
        other => debug_assert!(false, "install_history: unexpected member {other}"),
    }
}

/// The capture's member order -- the `History` rows of `install::generated_registry`, verbatim. (It
/// was copied from `generated_order::ORDER`'s `History` row, which said the same thing in a second
/// table, since deleted.)
const HISTORY_MEMBERS: &[&str] = &["length", "scrollRestoration", "state", "back", "forward", "go", "pushState", "replaceState"];

// Rows VERBATIM from the `History` block of `install/generated_registry.rs`, in the capture's order,
// with its kinds, lengths, `required` counts and descriptor flags. `[singleton, window]`: the worker
// capture has no `History` at all.
webapi::interface! {
    "History" [singleton, window] {
        ctor   "History"                                   len 0 = stub,
        attr   "length"            : Any readonly                = install_history,
        attr   "scrollRestoration" : Any                         = install_history,
        attr   "state"             : Any readonly                = install_history,
        method "back"              : Any len 0                   = install_history,
        method "forward"           : Any len 0                   = install_history,
        method "go"                : Any len 0                   = install_history,
        method "pushState"         : Any len 2 required 2        = install_history,
        method "replaceState"      : Any len 2 required 2        = install_history
    }
}

// ── location: the [LegacyUnforgeable] reshape ────────────────────────────────────────────────────

/// Make `location` the shape WebIDL and the recorded browser give it: its members are
/// `[LegacyUnforgeable]`, so they are OWN, non-configurable properties of the object, and
/// `Location.prototype` carries nothing but `constructor`.
///
/// This engine had exactly the inverse -- zero own, twelve on the prototype -- so the two objects were
/// mirror images of each other. Headless Chrome 150.0.7871.46 on this machine answers
/// `Object.getOwnPropertyNames(location).length` 15 and
/// `Object.getOwnPropertyNames(Location.prototype)` `["constructor"]` (probed 2026-09-11).
/// gave it all fifteen (`ancestorOrigins`, `port`, `valueOf` were missing) and every setter, with the
/// bodies in `location.rs`; the capture rig still records no unforgeable own member, so the evidence
/// is `tests/fixtures/p11a/desc.*.txt`, one recording per browser of the family.
pub fn reshape_location<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let _ = global;
    if webapi::install::live::window_location_minted(scope) {
        return;
    }
    let Some((lp, _)) = parent_of(scope, "Location") else { return };
    // A FRAME realm's install: the page's `Location` is minted and is the one the members
    // below receiver-check against (`location::LocationObject`); a frame's own Location is the
    // browsing context's (`browsing_context::install_location_here`). Only the prototype is this
    // realm's to empty.
    if crate::location::page_location_minted(scope) {
        strip_location_proto(scope, lp);
        return;
    }
    // THE ONE `Location`, minted with Chrome's IMMUTABLE PROTOTYPE: `Object.setPrototypeOf
    // (location, x)` is `TypeError: Immutable prototype object '[object Location]' cannot have their
    // prototype set` on all four browsers of the family. The bit belongs to an object TEMPLATE -- the
    // prototype an instance is born with is the only one it can have -- and it is `Location`'s own
    // instance template's, set by the builder (`install::instance_shapes`), so this is
    // an instance of the interface from birth. It came from a FunctionTemplate of its own, whose
    // function's `prototype` was set to `Location.prototype` before its first instantiation, until then.
    let loc = webapi::binding::protos::mint_on(scope, lp.into());
    // Every member, own and non-configurable; the bodies are `location.rs`'s.
    crate::location::install_location_members(scope, loc);
    strip_location_proto(scope, lp);
    webapi::install::live::set_window_location(scope, loc.into());
}

/// Whatever the shell stubbed onto `Location.prototype` is answered by the instance, and a member on
/// both would contradict the capture, which records the prototype as empty.
fn strip_location_proto(scope: &mut v8::PinScope, lp: v8::Local<v8::Object>) {
    for name in own_string_keys(scope, lp) {
        if name == "constructor" {
            continue;
        }
        if let Some(nk) = v8::String::new(scope, &name) {
            lp.delete(scope, nk.into());
        }
    }
}

fn own_string_keys(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Vec<String> {
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
        .index_filter(v8::IndexFilter::IncludeIndices)
        .build();
    let mut out = Vec::new();
    if let Some(keys) = o.get_own_property_names(scope, args) {
        for i in 0..keys.length() {
            if let Some(k) = keys.get_index(scope, i) {
                out.push(k.to_rust_string_lossy(scope));
            }
        }
    }
    out
}

// `rehome_performance` (and `install_memory_info` / `cb_heap_number` / `cb_memory` /
// `cb_value_getter` / `set_fn_length`) WERE HERE, and this module's header said what would end them:
// "When W4.c deletes `KB.__createPerformance`, `Performance` becomes a table like the others and this
// function goes." It is deleted. `Performance` and its six neighbours are `interface!` tables
// in `core/korvus-webapi/src/core/timing/performance.rs`, mounted by
// `install::live::install_window_performance` from the same slot in the order this function ran from,
// and `performance` is minted there wearing the table's prototype -- so there is no object literal
// left to relocate members off, and no shell descriptor to reconcile a JS body against.

/// `document` is an `HTMLDocument`.
///
/// The interface object existed and the shells had already chained `HTMLDocument.prototype` onto
/// `Document.prototype` -- the one thing missing was the document itself being on the far end of it.
/// Hanging straight off `Document.prototype` made `document.constructor.name` read "Document" and
/// `Object.prototype.toString.call(document)` read `[object Document]`, where every browser says
/// HTMLDocument.
///
/// Runs PER NAVIGATION, not at context creation: the document is built by `INSTALL_DOCUMENT`, and this
/// is called from there in place of `KB.__wireDocumentIdentity`.
pub fn wire_document_identity<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    // A new document is a new session-history entry.
    history_document_committed(scope);
    let Some(dk) = v8::String::new(scope, "document") else { return };
    let Some(doc) = global.get(scope, dk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let (Some((hdp, _)), Some((dp, _))) = (parent_of(scope, "HTMLDocument"), parent_of(scope, "Document")) else { return };
    if hdp.get_prototype(scope).map(|p| p != Into::<v8::Local<v8::Value>>::into(dp)).unwrap_or(true) {
        hdp.set_prototype(scope, dp.into());
    }
    if doc.get_prototype(scope).map(|p| p == Into::<v8::Local<v8::Value>>::into(dp)).unwrap_or(false) {
        doc.set_prototype(scope, hdp.into());
    }
    // `location` is the document's ONE own member in a browser -- `[LegacyUnforgeable]`, like
    // Location's own members are. Chrome answers `Object.getOwnPropertyNames(document)` `["location"]`
    // and `hasOwnProperty.call(Document.prototype, "location")` false (same probe).
    let Some(lk) = v8::String::new(scope, "location") else { return };
    let desc = dp.get_own_property_descriptor(scope, lk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    if let Some(d) = desc {
        if !doc.has_own_property(scope, lk.into()).unwrap_or(false) {
            let get = webapi::binding::protos::get_prop(scope, d, "get").unwrap_or_else(|| v8::undefined(scope).into());
            let set = webapi::binding::protos::get_prop(scope, d, "set").unwrap_or_else(|| v8::undefined(scope).into());
            if get.is_function() || set.is_function() {
                let mut pd = v8::PropertyDescriptor::new_from_get_set(get, set);
                pd.set_enumerable(true);
                pd.set_configurable(false);
                doc.define_property(scope, lk.into(), &pd);
            } else if let Some(value) = webapi::binding::protos::get_prop(scope, d, "value") {
                let mut pd = v8::PropertyDescriptor::new_from_value_writable(value, false);
                pd.set_enumerable(true);
                pd.set_configurable(false);
                doc.define_property(scope, lk.into(), &pd);
            }
        }
        // ...and off the prototype, where the recorded browser has nothing: `[LegacyUnforgeable]`
        // members exist only on instances. Every other document in this engine is a facade that
        // defines its own `location` (core/xml/), including a frame's, so nothing loses it.
        dp.delete(scope, lk.into());
    }
}

// ── the one install ──────────────────────────────────────────────────────────────────────────────

/// `History` from its table and `location`'s unforgeable reshape, at context creation -- after the
/// whole ESM layer, so these are the LAST writers.
/// The navigation hooks webapi calls into this crate through (a process static): set by the page's install, and by a
/// page restored from a snapshot, whose process has none yet.
pub(crate) fn set_page_hooks() {
    webapi::set_page_hooks(webapi::PageHooks { navigate: crate::location::navigate_page, commit: commit_intercepted, traverse: traverse_to_key });
}

pub fn install_window_singletons<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    // the page's navigations, as `korvus-webapi`'s Navigation API starts and commits them
    set_page_hooks();
    let (proto, gaps) = install_singleton(scope, global, &HISTORY, "History", HISTORY_MEMBERS);
    for g in &gaps {
        eprintln!("[korvus] history install gap: {}.{}", g.interface, g.member);
    }
    // The singleton is MINTED here. `host.js` used to build an object literal of eight members
    // that this very code then deleted (`strip_own` runs before the re-parent), so the literal was
    // dead code kept alive only by the object's identity -- the last JS `history` in the kernel. An
    // object already on the global is still adopted, so an embedder that provides one keeps working.
    if let Some(k) = v8::String::new(scope, "history") {
        let existing = global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
        // from the interface's template
        let h = existing.unwrap_or_else(|| webapi::binding::protos::mint_on(scope, proto.into()));
        strip_own(scope, h);
        webapi::binding::protos::adopt_as(scope, h, proto.into());
        // An own getter-only ACCESSOR on the window, as in every browser of the family.
        webapi::install::live::define_window_getter_singleton(scope, global, "history", h.into());
    }
    reshape_location(scope, global);
}
