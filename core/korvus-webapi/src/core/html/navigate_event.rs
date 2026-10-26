//! `core/html/navigate_event.rs` — the Navigation API's events, and the promises of the navigations they
//! belong to: `navigate` (a `NavigateEvent` with its `NavigationDestination`,
//! `intercept()` and `scroll()`), `currententrychange`, `navigatesuccess` / `navigateerror`, an entry's
//! `dispose`, `navigation.transition`, and the `{committed, finished}` that `navigation.navigate()`,
//! `reload()`, `back()`, `forward()` and `traverseTo()` hand back -- Blink 150's `NavigationApi`,
//! `NavigateEvent` and `NavigationApiMethodTracker`, step by step.
//!
//! Measured on stock Chrome 150 first (41 same-document navigations of every kind a page can make, three runs
//! alike, `fixtures/navigation-events`): a fragment navigation -- `location.hash = "d"`, `navigation.navigate("#a")`,
//! a link -- fires `navigate`, `currententrychange`, `popstate`, returns, then `navigatesuccess` in a microtask and
//! `hashchange` a task later; `pushState` fires `navigate` and `currententrychange` and no `popstate`; a traversal
//! fires them in its own task with the promises settled before `popstate`; `preventDefault()` aborts the navigation
//! (`navigateerror`, both promises rejected); `intercept()` commits at once and finishes when its handlers have.
//! Korvus fired none of these, `navigation.navigate("#a")` loaded the page again, and `history.pushState`
//! kept the page's own object as the state (Chrome: a structured clone, read back the same until it changes).
//!
//! WHO CALLS. The navigations are the runtime's -- `korvus-runtime`'s `location.rs` and `singletons.rs` own the
//! page's URL and its session history. It fires the event ([`dispatch_navigate_event`]) before it commits a
//! navigation and reports the commit ([`update_for_navigation`]) after its history list changed. What this crate
//! starts -- `navigation.navigate()`, `reload()`, a link, a form, `window.open(url, "_self")` -- goes through
//! the runtime's "navigate" ([`PageHooks::navigate`]), and an intercepted navigation is committed by its URL and
//! history update steps ([`PageHooks::commit`]).
//!
//! WHAT IS NOT. The events of a FRAME's navigations (a frame's same-document navigations are
//! `browsing_context.rs`'s and fire `hashchange` alone), a cross-document traversal's `navigate`, scrolling after an
//! intercepted navigation (this engine scrolls for no fragment navigation), `deferPageSwap()` (not enabled in
//! Chrome 150), and an autofocus delegate taking the focus a finished interception resets.

use deno_core::v8;

use crate::binding::protos::{build_fn, build_fn_data, get_prop, make_dom_exc, set_prop, str_val, throw_dom, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::core::html::session_history::with_history;
use crate::install::kits::{define_accessor, define_value};
use crate::install::MemberDef;

// ── the kinds ─────────────────────────────────────────────────────────────────────────────────────

/// `NavigationType`: what a navigation does to the session history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavigationType {
    Push,
    Replace,
    Reload,
    Traverse,
}

impl NavigationType {
    pub fn name(self) -> &'static str {
        match self {
            NavigationType::Push => "push",
            NavigationType::Replace => "replace",
            NavigationType::Reload => "reload",
            NavigationType::Traverse => "traverse",
        }
    }
    fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "push" => NavigationType::Push,
            "replace" => NavigationType::Replace,
            "reload" => NavigationType::Reload,
            "traverse" => NavigationType::Traverse,
            _ => return None,
        })
    }
}

/// Blink's `NavigateEventType`: a navigation to a fragment (and a same-document traversal), the history API's
/// `pushState` / `replaceState`, or one that loads a new document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavigateEventType {
    Fragment,
    HistoryApi,
    CrossDocument,
}

impl NavigateEventType {
    fn code(self) -> f64 {
        match self {
            NavigateEventType::Fragment => 0.0,
            NavigateEventType::HistoryApi => 1.0,
            NavigateEventType::CrossDocument => 2.0,
        }
    }
    fn from_code(n: f64) -> Self {
        match n as i32 {
            0 => NavigateEventType::Fragment,
            1 => NavigateEventType::HistoryApi,
            _ => NavigateEventType::CrossDocument,
        }
    }
}

/// What the `navigate` event is fired for (Blink's `NavigateEventDispatchParams`).
pub struct NavigateParams {
    /// The destination, absolute.
    pub url: String,
    pub event_type: NavigateEventType,
    pub nav_type: NavigationType,
    /// A traversal's destination entry.
    pub destination_key: Option<String>,
    /// The link or form that started it.
    pub source_element: Option<v8::Global<v8::Object>>,
    pub user_initiated: bool,
    /// A `method=post` form's entry list.
    pub form_data: Option<v8::Global<v8::Object>>,
    /// `pushState` / `replaceState`'s state, cloned.
    pub classic_state: Option<v8::Global<v8::Value>>,
}

impl NavigateParams {
    pub fn new(url: impl Into<String>, event_type: NavigateEventType, nav_type: NavigationType) -> Self {
        NavigateParams {
            url: url.into(),
            event_type,
            nav_type,
            destination_key: None,
            source_element: None,
            user_initiated: false,
            form_data: None,
            classic_state: None,
        }
    }
}

/// What the `navigate` event decided: go on, stop, or -- intercepted -- already committed as a same-document one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatchResult {
    Continue,
    Abort,
    Intercept,
}

/// An intercepted navigation's commit (Blink `NavigateEvent::CommitNow`): the runtime's URL and history update
/// steps for `url`, then `popstate` when `fire_popstate`.
pub struct NavigateCommit {
    pub url: String,
    pub nav_type: NavigationType,
    pub destination_key: Option<String>,
    pub classic_state: Option<v8::Global<v8::Value>>,
    pub fire_popstate: bool,
}

/// How a navigation started here treats the session history (`NavigationHistoryBehavior`, and a reload).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HistoryHandling {
    Auto,
    Push,
    Replace,
    Reload,
}

/// Why it started -- Blink's `ClientNavigationReason`, as far as it decides anything here: a link click and a
/// navigation with no reason are not made replacing before the document has loaded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClientReason {
    /// `location.*`, `navigation.*`, `window.open(url, "_self")`.
    FrameNavigation,
    AnchorClick,
    FormSubmission,
    None,
}

/// A navigation of the page that this crate starts, handed to the runtime's "navigate".
pub struct PageNavigate {
    pub nav: crate::core::html::browsing_context::PageNavigation,
    pub history: HistoryHandling,
    pub reason: ClientReason,
    pub source_element: Option<v8::Global<v8::Object>>,
    pub user_initiated: bool,
    pub form_data: Option<v8::Global<v8::Object>>,
}

/// The two steps the runtime performs for this crate.
pub struct PageHooks {
    /// HTML's "navigate" of the page (the runtime's `location::navigate_page`).
    pub navigate: fn(&mut v8::PinScope, PageNavigate),
    /// The URL and history update steps of an intercepted navigation.
    pub commit: fn(&mut v8::PinScope, NavigateCommit),
    /// A traversal to the entry a key names (`navigation.back()` / `forward()` / `traverseTo()`).
    pub traverse: fn(&mut v8::PinScope, String),
}

static HOOKS: std::sync::OnceLock<PageHooks> = std::sync::OnceLock::new();

/// Registered once, by the runtime.
pub fn set_page_hooks(hooks: PageHooks) {
    let _ = HOOKS.set(hooks);
}

/// Traverse to the entry `key` names, through the runtime (nothing without one).
pub(crate) fn traverse_to(scope: &mut v8::PinScope, key: String) {
    if let Some(h) = HOOKS.get() {
        (h.traverse)(scope, key);
    }
}

/// Navigate the page through the runtime -- or, with no runtime (a bare webapi test), ask for it.
pub(crate) fn navigate_page(scope: &mut v8::PinScope, p: PageNavigate) {
    match HOOKS.get() {
        Some(h) => (h.navigate)(scope, p),
        None => crate::core::html::browsing_context::request_top_navigation(scope, p.nav),
    }
}

// ── the state: the method trackers, the event in flight, the transition ────────────────────────────

/// One navigation method's promises (Blink `NavigationApiMethodTracker`).
struct Tracker {
    serial: u64,
    committed: v8::Global<v8::PromiseResolver>,
    finished: v8::Global<v8::PromiseResolver>,
    result: v8::Global<v8::Object>,
    info: Option<v8::Global<v8::Value>>,
    /// The Navigation API state the committed entry takes (`navigate()` / `reload()`'s `state`, cloned).
    state: Option<v8::Global<v8::Value>>,
    /// A traversal's destination.
    key: Option<String>,
    committed_to: Option<v8::Global<v8::Object>>,
}

struct Transition {
    object: v8::Global<v8::Object>,
    committed: v8::Global<v8::PromiseResolver>,
    finished: v8::Global<v8::PromiseResolver>,
    committed_settled: bool,
}

#[derive(Default)]
struct NavApi {
    serial: u64,
    upcoming_non_traverse: Option<Tracker>,
    upcoming_traverse: Vec<Tracker>,
    ongoing: Option<Tracker>,
    ongoing_event: Option<v8::Global<v8::Object>>,
    transition: Option<Transition>,
    /// The document's `load` event has finished (`FrameLoader`'s `LoadEventFinished`).
    loaded: bool,
}

fn with_api<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut NavApi) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<NavApi>().is_none() {
        st.put(NavApi::default());
    }
    f(st.borrow_mut::<NavApi>())
}

/// The page's `load` event has finished: a navigation is no longer made replacing for having started before it.
pub fn document_loaded(scope: &mut v8::PinScope) {
    with_api(scope, |a| a.loaded = true);
}

/// A new document was committed: everything in flight belonged to the one before it.
pub fn document_committed(scope: &mut v8::PinScope) {
    with_api(scope, |a| *a = NavApi::default());
}

/// Has the document's `load` event finished.
pub fn load_event_finished(scope: &mut v8::PinScope) -> bool {
    with_api(scope, |a| a.loaded)
}

// ── private slots ─────────────────────────────────────────────────────────────────────────────────

const P_EV: &str = "korvus_nav_event";
const P_EV_STATE: &str = "korvus_nav_event_state";
const P_EV_HANDLERS: &str = "korvus_nav_event_handlers";
const P_EV_PRECOMMIT: &str = "korvus_nav_event_precommit";
const P_EV_PROMISES: &str = "korvus_nav_event_promises";
const P_EV_FOCUS: &str = "korvus_nav_event_focus";
const P_EV_SCROLL: &str = "korvus_nav_event_scroll";
const P_EV_FOCUSED: &str = "korvus_nav_event_focused";
const P_EV_URL: &str = "korvus_nav_event_url";
const P_EV_KIND: &str = "korvus_nav_event_kind";
const P_EV_KEY: &str = "korvus_nav_event_key";
const P_EV_CLASSIC: &str = "korvus_nav_event_classic";
const P_EV_SIGNAL: &str = "korvus_nav_event_signal";
const P_EV_DEST: &str = "korvus_nav_event_destination";

const P_DEST: &str = "korvus_nav_dest";
const P_DEST_URL: &str = "korvus_nav_dest_url";
const P_DEST_KEY: &str = "korvus_nav_dest_key";
const P_DEST_ID: &str = "korvus_nav_dest_id";
const P_DEST_SAME: &str = "korvus_nav_dest_same";
const P_DEST_STATE: &str = "korvus_nav_dest_state";

const P_TR: &str = "korvus_nav_transition";
const P_TR_TYPE: &str = "korvus_nav_transition_type";
const P_TR_FROM: &str = "korvus_nav_transition_from";
const P_TR_TO: &str = "korvus_nav_transition_to";
const P_TR_COMMITTED: &str = "korvus_nav_transition_committed";
const P_TR_FINISHED: &str = "korvus_nav_transition_finished";

const P_PC_EVENT: &str = "korvus_nav_precommit_event";

/// `NavigateEvent`'s interception state (Blink `InterceptState`), ordered as Blink compares it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum InterceptState {
    None = 0,
    Intercepted = 1,
    Committed = 2,
    Scrolled = 3,
    Finished = 4,
}

fn intercept_state(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) -> InterceptState {
    match priv_get(scope, ev, P_EV_STATE).and_then(|v| v.number_value(scope)).unwrap_or(0.0) as i32 {
        1 => InterceptState::Intercepted,
        2 => InterceptState::Committed,
        3 => InterceptState::Scrolled,
        4 => InterceptState::Finished,
        _ => InterceptState::None,
    }
}

fn set_intercept_state(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, s: InterceptState) {
    let v: v8::Local<v8::Value> = v8::Number::new(scope, s as i32 as f64).into();
    priv_set(scope, ev, P_EV_STATE, v);
}

fn slot_string(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str) -> Option<String> {
    priv_get(scope, o, key).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

fn slot_obj<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, key: &str) -> Option<v8::Local<'s, v8::Object>> {
    priv_get(scope, o, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// The list in slot `key` of `o`, made empty on first use.
fn slot_list<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, key: &str) -> v8::Local<'s, v8::Array> {
    if let Some(a) = priv_get(scope, o, key).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        return a;
    }
    let a = v8::Array::new(scope, 0);
    priv_set(scope, o, key, a.into());
    a
}

fn push(scope: &mut v8::PinScope, list: v8::Local<v8::Array>, v: v8::Local<v8::Value>) {
    let n = list.length();
    list.set_index(scope, n, v);
}

// ── the page's `navigation`, its entries, and the URLs ───────────────────────────────────────────────

/// The page's own `navigation` -- `None` when nothing has read it yet, in which case nothing can be listening.
fn page_navigation<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    if crate::core::html::browsing_context::current_realm_ctx(scope).is_some() {
        return None;
    }
    crate::core::html::window_globals::window_slot_get(scope, "navigation").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

fn document_href(scope: &mut v8::PinScope) -> String {
    crate::core::html::navigation_api::page_href(scope)
}

/// HTML's "has entries and events disabled": the initial `about:blank`, an opaque origin.
fn events_disabled(scope: &mut v8::PinScope) -> bool {
    let href = document_href(scope);
    match url::Url::parse(&href) {
        Ok(u) => !u.origin().is_tuple() && u.scheme() != "file",
        Err(_) => true,
    }
}

fn same_ignoring_fragment(a: &url::Url, b: &url::Url) -> bool {
    let (mut a, mut b) = (a.clone(), b.clone());
    a.set_fragment(None);
    b.set_fragment(None);
    a == b
}

/// Blink's `CanChangeToUrlForHistoryApi` (HTML "can have its URL rewritten"): whether a document at `document` whose
/// origin is opaque or not may move its URL to `url` without a navigation -- what `pushState` allows and a `navigate`
/// event can intercept. The ORIGIN is the document's, not its URL's: an `about:blank` frame has its creator's.
pub fn can_change_to_url_for_history_api(url: &url::Url, document: &url::Url, opaque_origin: bool) -> bool {
    if url.scheme() != document.scheme()
        || url.username() != document.username()
        || url.password() != document.password()
        || url.host_str() != document.host_str()
        || url.port() != document.port()
    {
        return false;
    }
    if matches!(url.scheme(), "http" | "https") {
        return true;
    }
    let differ_in_path = url.path() != document.path();
    let differ_in_query = url.query() != document.query();
    if url.scheme() == "file" && differ_in_path {
        return false;
    }
    // an opaque origin (`data:`, a sandboxed document) may change its query and fragment
    if opaque_origin {
        return !differ_in_path;
    }
    // the "standard" schemes besides http(s) and file (`filesystem:`, ws(s), ftp) may change their path too
    if matches!(url.scheme(), "file" | "filesystem" | "ftp" | "ws" | "wss") {
        return true;
    }
    !(differ_in_path || differ_in_query)
}

/// The text Chrome's URL parser leaves between the quotes of "Invalid URL '…'." / "'…' is not a valid URL." -- the
/// input as KURL canonicalised it before giving up, which is not the input. Every rule below is a measured case
/// (`fixtures/p11a/location.*.txt`): `http://[` and `//[` both read `http://[/`, `http://[::1` reads
/// `http://[::1/`, `https://` reads `https:`, and `http://%zz/` reads `http://%25zz/`.
pub fn invalid_url_text(input: &str, base: Option<&url::Url>) -> String {
    let mut s = input.trim().to_string();
    if s.starts_with("//") {
        if let Some(b) = base {
            s = format!("{}:{s}", b.scheme());
        }
    }
    let mut escaped = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        let hex = |j: usize| bytes.get(j).map(|b| b.is_ascii_hexdigit()).unwrap_or(false);
        if c == '%' && !(hex(i + 1) && hex(i + 2)) {
            escaped.push_str("%25");
        } else {
            escaped.push(c);
        }
    }
    let s = escaped;
    if let Some((scheme, rest)) = s.split_once("://") {
        if rest.is_empty() {
            return format!("{scheme}:");
        }
        if !rest.contains('/') {
            return format!("{s}/");
        }
    }
    s
}

/// One entry of the session history, copied out so no borrow of `OpState` is held while v8 runs.
#[derive(Clone)]
struct EntryInfo {
    url: String,
    key: String,
    id: String,
    generation: u64,
}

/// The entries the Navigation API shows (the same-origin run around the current one) and the current one's place.
fn shown_entries(scope: &mut v8::PinScope) -> (Vec<EntryInfo>, usize) {
    let href = document_href(scope);
    with_history(scope, href, |h| {
        let (lo, hi) = crate::core::html::navigation_api::visible(h);
        let list = h.entries[lo..=hi]
            .iter()
            .map(|e| EntryInfo { url: e.url.clone(), key: e.key.clone(), id: e.id.clone(), generation: e.generation })
            .collect();
        (list, h.index - lo)
    })
}

fn entry_object<'s>(scope: &mut v8::PinScope<'s, '_>, nav: v8::Local<v8::Object>, e: &EntryInfo) -> Option<v8::Local<'s, v8::Object>> {
    crate::core::html::navigation_api::entry_object_for(scope, nav, &e.url, &e.key, &e.id, e.generation)
}

/// The Navigation API state of the entry `key` names, else of the current one.
fn entry_nav_state(scope: &mut v8::PinScope, key: Option<&str>) -> Option<v8::Global<v8::Value>> {
    let href = document_href(scope);
    with_history(scope, href, |h| match key {
        Some(k) => h.entries.iter().find(|e| e.key == k).and_then(|e| e.nav_state.clone()),
        None => h.entries[h.index].nav_state.clone(),
    })
}

/// What the session history looked like before a commit changed it: its entries and the current one.
pub struct Before {
    current: Option<EntryInfo>,
    entries: Vec<EntryInfo>,
}

/// The session history as it is, for [`update_for_navigation`] once the runtime has changed it.
pub fn before_update(scope: &mut v8::PinScope) -> Before {
    let (entries, cur) = shown_entries(scope);
    Before { current: entries.get(cur).cloned(), entries }
}

// ── the tracker of a navigation method ───────────────────────────────────────────────────────────

/// A `NavigationResult` dictionary over two new promises, `finished` marked handled (Blink: a page that awaits
/// only `committed` is not told about `finished` too).
fn new_tracker(scope: &mut v8::PinScope, info: Option<v8::Local<v8::Value>>, state: Option<v8::Global<v8::Value>>, key: Option<String>) -> Tracker {
    let committed = v8::PromiseResolver::new(scope).unwrap();
    let finished = v8::PromiseResolver::new(scope).unwrap();
    let o = v8::Object::new(scope);
    let cp: v8::Local<v8::Value> = committed.get_promise(scope).into();
    set_prop(scope, o, "committed", cp);
    let fp = finished.get_promise(scope);
    fp.mark_as_handled();
    let fpv: v8::Local<v8::Value> = fp.into();
    set_prop(scope, o, "finished", fpv);
    let serial = with_api(scope, |a| {
        a.serial += 1;
        a.serial
    });
    Tracker {
        serial,
        committed: v8::Global::new(scope, committed),
        finished: v8::Global::new(scope, finished),
        result: v8::Global::new(scope, o),
        info: info.map(|i| v8::Global::new(scope, i)),
        state,
        key,
        committed_to: None,
    }
}

/// Blink's `EarlyErrorResult`: both promises rejected with `e`.
pub(crate) fn early_error_result<'s>(scope: &mut v8::PinScope<'s, '_>, e: v8::Local<v8::Value>) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    for k in ["committed", "finished"] {
        let r = v8::PromiseResolver::new(scope).unwrap();
        r.reject(scope, e);
        let p: v8::Local<v8::Value> = r.get_promise(scope).into();
        set_prop(scope, o, k, p);
    }
    o
}

/// [`early_error_result`] with a new `DOMException`.
pub(crate) fn early_dom_error<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str, name: &str) -> v8::Local<'s, v8::Object> {
    let e = make_dom_exc(scope, msg, name).unwrap_or_else(|| v8::undefined(scope).into());
    early_error_result(scope, e)
}

/// Blink's `EarlySuccessResult`: both promises resolved with `entry`.
pub(crate) fn early_success_result<'s>(scope: &mut v8::PinScope<'s, '_>, entry: v8::Local<v8::Value>) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    for k in ["committed", "finished"] {
        let r = v8::PromiseResolver::new(scope).unwrap();
        r.resolve(scope, entry);
        let p: v8::Local<v8::Value> = r.get_promise(scope).into();
        set_prop(scope, o, k, p);
    }
    o
}

fn reject_tracker(scope: &mut v8::PinScope, t: &Tracker, e: v8::Local<v8::Value>) {
    let c = v8::Local::new(scope, &t.committed);
    c.reject(scope, e);
    let f = v8::Local::new(scope, &t.finished);
    f.reject(scope, e);
}

/// `navigation.navigate()` / `reload()`: a tracker for the navigation it is about to start. With events disabled
/// the promises never settle (Blink `CleanupForWillNeverSettle`). Returns the tracker's serial and its result.
pub(crate) fn begin_non_traverse<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    info: Option<v8::Local<v8::Value>>,
    state: Option<v8::Global<v8::Value>>,
) -> (u64, v8::Local<'s, v8::Object>) {
    let t = new_tracker(scope, info, state, None);
    let serial = t.serial;
    let result = v8::Local::new(scope, &t.result);
    if !events_disabled(scope) {
        with_api(scope, |a| a.upcoming_non_traverse = Some(t));
    }
    (serial, result)
}

/// After the navigation was asked for: a tracker no `navigate` event took was never started -- "Navigation was
/// aborted" (Blink: blocked before `DispatchNavigateEvent`).
pub(crate) fn end_non_traverse<'s>(scope: &mut v8::PinScope<'s, '_>, serial: u64, result: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Object> {
    let left = with_api(scope, |a| {
        if a.upcoming_non_traverse.as_ref().is_some_and(|t| t.serial == serial) {
            a.upcoming_non_traverse.take()
        } else {
            None
        }
    });
    if left.is_some() {
        return early_dom_error(scope, "Navigation was aborted", "AbortError");
    }
    result
}

/// `traverseTo(key)` once the key is known to be a different entry's: the tracker of an upcoming traversal to `key`,
/// or the one already upcoming (the same promises).
pub(crate) fn begin_traverse<'s>(scope: &mut v8::PinScope<'s, '_>, key: &str, info: Option<v8::Local<v8::Value>>) -> (v8::Local<'s, v8::Object>, bool) {
    let existing = with_api(scope, |a| a.upcoming_traverse.iter().find(|t| t.key.as_deref() == Some(key)).map(|t| t.result.clone()));
    if let Some(r) = existing {
        return (v8::Local::new(scope, &r), false);
    }
    let t = new_tracker(scope, info, None, Some(key.to_string()));
    let result = v8::Local::new(scope, &t.result);
    with_api(scope, |a| a.upcoming_traverse.push(t));
    (result, true)
}

/// A traversal that did not happen: its entry was gone when its task ran (Blink `TraverseCancelled`).
pub fn traverse_cancelled(scope: &mut v8::PinScope, key: &str) {
    let t = with_api(scope, |a| {
        let i = a.upcoming_traverse.iter().position(|t| t.key.as_deref() == Some(key))?;
        Some(a.upcoming_traverse.remove(i))
    });
    if let Some(t) = t {
        let e = make_dom_exc(scope, "Navigation was aborted", "AbortError").unwrap_or_else(|| v8::undefined(scope).into());
        reject_tracker(scope, &t, e);
    }
}

fn promote_upcoming(scope: &mut v8::PinScope, key: Option<&str>) {
    with_api(scope, |a| {
        a.ongoing = match key {
            Some(k) => a.upcoming_traverse.iter().position(|t| t.key.as_deref() == Some(k)).map(|i| a.upcoming_traverse.remove(i)),
            None => a.upcoming_non_traverse.take(),
        };
    });
}

// ── firing ───────────────────────────────────────────────────────────────────────────────────────

/// A trusted `iface` event of type `ty` from `init`, dispatched at `target`; whether it was not canceled.
fn fire(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, iface: &str, ty: &str, init: Option<v8::Local<v8::Object>>) -> bool {
    let target = v8::Local::new(scope, target);
    let t = str_val(scope, ty);
    let mut args = vec![t];
    if let Some(i) = init {
        let i: v8::Local<v8::Value> = v8::Local::new(scope, i).into();
        args.push(i);
    }
    let Some(ev) = crate::binding::protos::new_global(scope, iface, &args) else { return true };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    crate::core::events::event_target::et_dispatch(scope, target.into(), ev)
}

fn new_destination<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    url: &str,
    entry: Option<&EntryInfo>,
    same_document: bool,
    state: Option<v8::Global<v8::Value>>,
) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "NavigationDestination")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, o, P_DEST, t);
    let u = str_val(scope, url);
    priv_set(scope, o, P_DEST_URL, u);
    let k = str_val(scope, entry.map(|e| e.key.as_str()).unwrap_or(""));
    priv_set(scope, o, P_DEST_KEY, k);
    let i = str_val(scope, entry.map(|e| e.id.as_str()).unwrap_or(""));
    priv_set(scope, o, P_DEST_ID, i);
    let s: v8::Local<v8::Value> = v8::Boolean::new(scope, same_document).into();
    priv_set(scope, o, P_DEST_SAME, s);
    if let Some(g) = state {
        let v = v8::Local::new(scope, &g);
        priv_set(scope, o, P_DEST_STATE, v);
    }
    Some(o)
}

/// HTML's "fire a navigate event" (Blink `NavigationApi::DispatchNavigateEvent`): abort the navigation in flight,
/// give the method's tracker to this one, fire `navigate` -- cancelable unless it is a traversal the page may not
/// cancel -- and, intercepted, commit it now.
pub fn dispatch_navigate_event(scope: &mut v8::PinScope, p: NavigateParams) -> DispatchResult {
    if events_disabled(scope) {
        return DispatchResult::Continue;
    }
    while with_api(scope, |a| a.ongoing_event.is_some()) {
        abort_ongoing_navigation(scope);
    }
    let key = p.destination_key.clone();
    let (entries, cur) = shown_entries(scope);
    let target = key.as_deref().and_then(|k| entries.iter().find(|e| e.key == k).cloned());
    if p.nav_type == NavigationType::Traverse && p.event_type == NavigateEventType::Fragment && target.is_none() {
        if let Some(k) = key.as_deref() {
            traverse_cancelled(scope, k);
        }
        return DispatchResult::Abort;
    }
    promote_upcoming(scope, key.as_deref());
    let Some(nav) = page_navigation(scope) else {
        // nobody has `navigation`, so nobody listens -- and no method of it is waiting either
        with_api(scope, |a| a.ongoing = None);
        return DispatchResult::Continue;
    };
    // the destination's state: the traversal's entry's, the method's, or the reloaded entry's
    let (method_state, info) = with_api(scope, |a| match a.ongoing.as_ref() {
        Some(t) => (t.state.clone(), t.info.clone()),
        None => (None, None),
    });
    let has_method = with_api(scope, |a| a.ongoing.is_some());
    let dest_state = if p.nav_type == NavigationType::Traverse {
        entry_nav_state(scope, key.as_deref())
    } else if has_method {
        method_state
    } else if p.nav_type == NavigationType::Reload {
        entry_nav_state(scope, None)
    } else {
        None
    };
    let doc = url::Url::parse(&document_href(scope)).ok();
    let dest_url = url::Url::parse(&p.url).ok();
    let traverse = p.nav_type == NavigationType::Traverse;
    let Some(dest) = new_destination(scope, &p.url, target.as_ref(), p.event_type != NavigateEventType::CrossDocument, dest_state) else {
        return DispatchResult::Continue;
    };
    // a script-initiated traversal of the main frame may be canceled; this engine's are all script-initiated
    let cancelable = !traverse || p.event_type != NavigateEventType::CrossDocument;
    let can_intercept = match (&dest_url, &doc) {
        (Some(u), Some(d)) => can_change_to_url_for_history_api(u, d, !d.origin().is_tuple()),
        _ => false,
    } && (p.event_type != NavigateEventType::CrossDocument || !traverse);
    let hash_change = p.event_type == NavigateEventType::Fragment
        && match (&dest_url, &doc) {
            (Some(u), Some(d)) => u != d && same_ignoring_fragment(u, d),
            _ => false,
        };
    let Some(signal) = crate::core::dom::abort_controller::mint_signal(scope) else { return DispatchResult::Continue };

    let init = crate::binding::protos::init_dict(scope);
    crate::binding::protos::set_bool(scope, init, "cancelable", cancelable);
    let ty = str_val(scope, p.nav_type.name());
    set_prop(scope, init, "navigationType", ty);
    set_prop(scope, init, "destination", dest.into());
    crate::binding::protos::set_bool(scope, init, "canIntercept", can_intercept);
    crate::binding::protos::set_bool(scope, init, "userInitiated", p.user_initiated);
    crate::binding::protos::set_bool(scope, init, "hashChange", hash_change);
    set_prop(scope, init, "signal", signal.into());
    if let Some(f) = p.form_data.as_ref() {
        let f: v8::Local<v8::Value> = v8::Local::new(scope, f).into();
        set_prop(scope, init, "formData", f);
    }
    if let Some(i) = info.as_ref() {
        let i = v8::Local::new(scope, i);
        set_prop(scope, init, "info", i);
    }
    if let Some(s) = p.source_element.as_ref() {
        let s: v8::Local<v8::Value> = v8::Local::new(scope, s).into();
        set_prop(scope, init, "sourceElement", s);
    }
    let t = str_val(scope, "navigate");
    let Some(ev) = crate::binding::protos::new_global(scope, "NavigateEvent", &[t, init.into()]) else {
        with_api(scope, |a| a.ongoing = None);
        return DispatchResult::Continue;
    };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let yes: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, ev, P_EV, yes);
    set_intercept_state(scope, ev, InterceptState::None);
    let u = str_val(scope, &p.url);
    priv_set(scope, ev, P_EV_URL, u);
    let kind: v8::Local<v8::Value> = v8::Number::new(scope, p.event_type.code()).into();
    priv_set(scope, ev, P_EV_KIND, kind);
    if let Some(k) = key.as_deref() {
        let k = str_val(scope, k);
        priv_set(scope, ev, P_EV_KEY, k);
    }
    if let Some(c) = p.classic_state.as_ref() {
        let c = v8::Local::new(scope, c);
        priv_set(scope, ev, P_EV_CLASSIC, c);
    }
    priv_set(scope, ev, P_EV_SIGNAL, signal.into());
    priv_set(scope, ev, P_EV_DEST, dest.into());
    let g = v8::Global::new(scope, ev);
    with_api(scope, |a| a.ongoing_event = Some(g));

    let _ = cur;
    crate::core::events::event_target::et_dispatch(scope, nav.into(), ev);

    if crate::core::events::event::evt_flag_get(scope, ev, "defaultPrevented") {
        if crate::core::dom::abort_controller::aborted_reason(scope, signal.into()).is_none() {
            abort_ongoing_navigation(scope);
        }
        return DispatchResult::Abort;
    }
    if intercept_state(scope, ev) != InterceptState::None {
        // the transition: from the entry that is current now, to the destination
        let from = {
            let (entries, cur) = shown_entries(scope);
            entries.get(cur).cloned().and_then(|e| entry_object(scope, nav, &e))
        };
        begin_transition(scope, p.nav_type, from, dest);
        maybe_commit_immediately(scope, ev);
        return DispatchResult::Intercept;
    }
    DispatchResult::Continue
}

fn begin_transition(scope: &mut v8::PinScope, nav_type: NavigationType, from: Option<v8::Local<v8::Object>>, to: v8::Local<v8::Object>) {
    let Some(proto) = crate::binding::protos::shell_proto(scope, "NavigationTransition") else { return };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, o, P_TR, t);
    let ty = str_val(scope, nav_type.name());
    priv_set(scope, o, P_TR_TYPE, ty);
    if let Some(f) = from {
        priv_set(scope, o, P_TR_FROM, f.into());
    }
    priv_set(scope, o, P_TR_TO, to.into());
    let committed = v8::PromiseResolver::new(scope).unwrap();
    let finished = v8::PromiseResolver::new(scope).unwrap();
    let cp: v8::Local<v8::Value> = committed.get_promise(scope).into();
    priv_set(scope, o, P_TR_COMMITTED, cp);
    let fp = finished.get_promise(scope);
    fp.mark_as_handled();
    priv_set(scope, o, P_TR_FINISHED, fp.into());
    let tr = Transition {
        object: v8::Global::new(scope, o),
        committed: v8::Global::new(scope, committed),
        finished: v8::Global::new(scope, finished),
        committed_settled: false,
    };
    with_api(scope, |a| a.transition = Some(tr));
}

/// Blink `NavigateEvent::MaybeCommitImmediately`: commit now, or once the precommit handlers have all fulfilled.
fn maybe_commit_immediately(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let ev = v8::Local::new(scope, ev);
    let pre = slot_list(scope, ev, P_EV_PRECOMMIT);
    if pre.length() == 0 {
        commit_now(scope, ev);
        return;
    }
    priv_set(scope, ev, P_EV_PRECOMMIT, v8::Array::new(scope, 0).into());
    let Some(controller) = new_precommit_controller(scope, ev) else { return };
    let promises = v8::Array::new(scope, 0);
    for i in 0..pre.length() {
        let Some(f) = pre.get_index(scope, i).and_then(|f| v8::Local::<v8::Function>::try_from(f).ok()) else { continue };
        let p = invoke_handler(scope, f, ev.into(), &[controller.into()]);
        push(scope, promises, p.into());
    }
    let ok = build_fn_data(scope, cb_precommit_done, "", 0, ev.into());
    let bad = build_fn_data(scope, cb_precommit_failed, "", 1, ev.into());
    wait_for_all(scope, promises, ok, bad);
}

fn cb_precommit_done(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Ok(ev) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) {
        commit_now(scope, ev);
    }
}

fn cb_precommit_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Ok(ev) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) {
        react_done(scope, ev, false, args.get(0));
    }
}

/// Blink `NavigateEvent::CommitNow`: the runtime's URL and history update steps for the intercepted navigation.
fn commit_now(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    if event_aborted(scope, ev) {
        return;
    }
    set_intercept_state(scope, ev, InterceptState::Committed);
    let kind = NavigateEventType::from_code(priv_get(scope, ev, P_EV_KIND).and_then(|v| v.number_value(scope)).unwrap_or(2.0));
    let nav_type = nav_type_of(scope, ev);
    let has_method = with_api(scope, |a| a.ongoing.is_some());
    let fire_popstate = kind == NavigateEventType::Fragment && (!has_method || nav_type == NavigationType::Traverse);
    let commit = NavigateCommit {
        url: slot_string(scope, ev, P_EV_URL).unwrap_or_default(),
        nav_type,
        destination_key: slot_string(scope, ev, P_EV_KEY),
        classic_state: priv_get(scope, ev, P_EV_CLASSIC).map(|v| v8::Global::new(scope, v)),
        fire_popstate,
    };
    if let Some(h) = HOOKS.get() {
        (h.commit)(scope, commit);
    }
}

fn nav_type_of(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) -> NavigationType {
    crate::core::events::event::evt_slot_read(scope, ev, "navigationType")
        .map(|v| v.to_rust_string_lossy(scope))
        .and_then(|s| NavigationType::from_name(&s))
        .unwrap_or(NavigationType::Push)
}

fn event_aborted(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) -> bool {
    match slot_obj(scope, ev, P_EV_SIGNAL) {
        Some(s) => crate::core::dom::abort_controller::aborted_reason(scope, s.into()).is_some(),
        None => true,
    }
}

// ── the commit ───────────────────────────────────────────────────────────────────────────────────

/// Blink `NavigationApi::UpdateForNavigation`, after the runtime moved the URL and changed the history: the method's
/// `committed` with its entry, `currententrychange`, the interception's handlers, `dispose` at every entry that left
/// the list, and the reaction that finishes the navigation -- with the microtask checkpoint Blink runs here when no
/// script is on the stack.
pub fn update_for_navigation(scope: &mut v8::PinScope, nav_type: NavigationType, before: Before) {
    if events_disabled(scope) {
        return;
    }
    let Some(nav) = page_navigation(scope) else {
        with_api(scope, |a| a.ongoing = None);
        return;
    };
    let (entries, cur) = shown_entries(scope);
    let Some(current) = entries.get(cur).cloned() else { return };
    let current_obj = entry_object(scope, nav, &current);
    // the method's tracker: the entry takes its state (not a traversal's), and `committed` resolves with it
    let tracker = with_api(scope, |a| a.ongoing.as_mut().map(|t| (t.committed.clone(), t.state.take())));
    if let Some((committed, state)) = tracker {
        if nav_type != NavigationType::Traverse {
            let href = document_href(scope);
            with_history(scope, href, |h| {
                let i = h.index;
                h.entries[i].nav_state = state;
            });
        }
        if let Some(o) = current_obj {
            let r = v8::Local::new(scope, &committed);
            r.resolve(scope, o.into());
            let g = v8::Global::new(scope, o);
            with_api(scope, |a| {
                if let Some(t) = a.ongoing.as_mut() {
                    t.committed_to = Some(g);
                }
            });
        }
    }
    let tr = with_api(scope, |a| {
        a.transition.as_mut().map(|t| {
            t.committed_settled = true;
            t.committed.clone()
        })
    });
    if let Some(r) = tr {
        let r = v8::Local::new(scope, &r);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        r.resolve(scope, u);
    }
    let ongoing = with_api(scope, |a| a.ongoing_event.clone()).map(|g| v8::Local::new(scope, &g));

    // `currententrychange`, the handlers, `dispose` and the reaction, in Blink's one MicrotasksScope: a listener's
    // microtasks wait for the checkpoint below
    crate::core::html::task_queue::hold_checkpoints(scope, |scope| {
        // currententrychange
        let from = before.current.as_ref().and_then(|e| entry_object(scope, nav, e));
        if let Some(from) = from {
            let init = crate::binding::protos::init_dict(scope);
            let t = str_val(scope, nav_type.name());
            set_prop(scope, init, "navigationType", t);
            set_prop(scope, init, "from", from.into());
            fire(scope, nav, "NavigationCurrentEntryChangeEvent", "currententrychange", Some(init));
        }
        if let Some(ev) = ongoing {
            finalize_action_promises(scope, ev);
        }
        // dispose: the entries that left the list -- only those a page could hold
        let gone: Vec<EntryInfo> = before.entries.iter().filter(|e| !entries.iter().any(|n| n.id == e.id)).cloned().collect();
        for e in gone {
            if let Some(o) = crate::core::html::navigation_api::cached_entry_object(scope, nav, &e.id) {
                fire(scope, o, "Event", "dispose", None);
            }
        }
        if let Some(ev) = ongoing {
            react(scope, ev);
        }
    });
    if v8::StackTrace::current_stack_trace(scope, 1).map_or(0, |st| st.get_frame_count()) == 0 {
        scope.perform_microtask_checkpoint();
    }
}

/// `navigation.updateCurrentEntry()`'s `currententrychange`: no navigation type, from the current entry.
pub(crate) fn current_entry_state_changed(scope: &mut v8::PinScope, nav: v8::Local<v8::Object>, from: v8::Local<v8::Object>) {
    let from = v8::Local::new(scope, from);
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "from", from.into());
    fire(scope, nav, "NavigationCurrentEntryChangeEvent", "currententrychange", Some(init));
}

/// Blink `FinalizeNavigationActionPromisesList`: run each `intercept()` handler, with the event as `this`.
fn finalize_action_promises(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let handlers = slot_list(scope, ev, P_EV_HANDLERS);
    priv_set(scope, ev, P_EV_HANDLERS, v8::Array::new(scope, 0).into());
    let promises = slot_list(scope, ev, P_EV_PROMISES);
    for i in 0..handlers.length() {
        let Some(f) = handlers.get_index(scope, i).and_then(|f| v8::Local::<v8::Function>::try_from(f).ok()) else { continue };
        let p = invoke_handler(scope, f, ev.into(), &[]);
        push(scope, promises, p.into());
    }
}

/// A `Promise<undefined>`-returning callback, invoked: its promise, a resolved one for any other value, a
/// rejected one when it threw (WebIDL: a promise-returning callback's exception is its rejection).
fn invoke_handler<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<v8::Function>, this: v8::Local<v8::Value>, args: &[v8::Local<v8::Value>]) -> v8::Local<'s, v8::Promise> {
    let outcome = {
        v8::tc_scope!(let tc, scope);
        match f.call(tc, this, args) {
            Some(v) => Ok(v8::Global::new(tc, v)),
            None => {
                let e = tc.exception().map(|e| v8::Global::new(tc, e));
                tc.reset();
                Err(e)
            }
        }
    };
    match outcome {
        Ok(g) => {
            let v = v8::Local::new(scope, &g);
            if let Ok(p) = v8::Local::<v8::Promise>::try_from(v) {
                return p;
            }
            let r = v8::PromiseResolver::new(scope).unwrap();
            r.resolve(scope, v);
            r.get_promise(scope)
        }
        Err(e) => {
            let e = match e {
                Some(e) => v8::Local::new(scope, &e),
                None => v8::undefined(scope).into(),
            };
            let r = v8::PromiseResolver::new(scope).unwrap();
            r.reject(scope, e);
            r.get_promise(scope)
        }
    }
}

/// WebIDL "wait for all": `ok` once every promise of `list` has fulfilled (a microtask later for none), `bad` with
/// the first rejection.
fn wait_for_all(scope: &mut v8::PinScope, list: v8::Local<v8::Array>, ok: v8::Local<v8::Function>, bad: v8::Local<v8::Function>) {
    let n = list.length();
    if n == 0 {
        scope.enqueue_microtask(ok);
        return;
    }
    let state = v8::Object::new(scope);
    let left: v8::Local<v8::Value> = v8::Number::new(scope, n as f64).into();
    priv_set(scope, state, "korvus_all_left", left);
    priv_set(scope, state, "korvus_all_ok", ok.into());
    priv_set(scope, state, "korvus_all_bad", bad.into());
    for i in 0..n {
        let Some(p) = list.get_index(scope, i).and_then(|p| v8::Local::<v8::Promise>::try_from(p).ok()) else { continue };
        let on_ok = build_fn_data(scope, cb_all_fulfilled, "", 1, state.into());
        let on_bad = build_fn_data(scope, cb_all_rejected, "", 1, state.into());
        p.then2(scope, on_ok, on_bad);
    }
}

fn cb_all_fulfilled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(state) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    if priv_get(scope, state, "korvus_all_settled").is_some_and(|v| v.is_true()) {
        return;
    }
    let left = priv_get(scope, state, "korvus_all_left").and_then(|v| v.number_value(scope)).unwrap_or(1.0) - 1.0;
    let lv: v8::Local<v8::Value> = v8::Number::new(scope, left).into();
    priv_set(scope, state, "korvus_all_left", lv);
    if left > 0.0 {
        return;
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, state, "korvus_all_settled", t);
    if let Some(f) = priv_get(scope, state, "korvus_all_ok").and_then(|f| v8::Local::<v8::Function>::try_from(f).ok()) {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        f.call(scope, u, &[]);
    }
}

fn cb_all_rejected(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(state) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    if priv_get(scope, state, "korvus_all_settled").is_some_and(|v| v.is_true()) {
        return;
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, state, "korvus_all_settled", t);
    if let Some(f) = priv_get(scope, state, "korvus_all_bad").and_then(|f| v8::Local::<v8::Function>::try_from(f).ok()) {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        f.call(scope, u, &[args.get(0)]);
    }
}

/// Blink `NavigateEvent::React`: finish once the handlers' promises have all fulfilled, fail at the first rejection.
fn react(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let ev = v8::Local::new(scope, ev);
    let promises = slot_list(scope, ev, P_EV_PROMISES);
    let ok = build_fn_data(scope, cb_react_fulfilled, "", 0, ev.into());
    let bad = build_fn_data(scope, cb_react_rejected, "", 1, ev.into());
    wait_for_all(scope, promises, ok, bad);
}

fn cb_react_fulfilled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Ok(ev) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        react_done(scope, ev, true, u);
    }
}

fn cb_react_rejected(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Ok(ev) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) {
        react_done(scope, ev, false, args.get(0));
    }
}

/// Blink `NavigateEvent::ReactDone`.
fn react_done(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, did_fulfill: bool, value: v8::Local<v8::Value>) {
    if event_aborted(scope, ev) {
        return;
    }
    with_api(scope, |a| a.ongoing_event = None);
    let state = intercept_state(scope, ev);
    if state >= InterceptState::Committed {
        potentially_reset_the_focus(scope, ev);
        set_intercept_state(scope, ev, InterceptState::Finished);
    }
    if did_fulfill {
        did_finish_ongoing_navigation(scope);
    } else {
        event_abort(scope, ev, value);
    }
}

/// Blink `NavigateEvent::PotentiallyResetTheFocus`: after an interception, the focus goes back to the document --
/// unless the page moved it meanwhile or asked for `focusReset: "manual"`.
fn potentially_reset_the_focus(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) {
    let Some(at_intercept) = priv_get(scope, ev, P_EV_FOCUSED) else { return };
    let doc = crate::page::page_document(scope);
    let now = crate::binding::protos::pristine_get(scope, doc.into(), "Document.activeElement").unwrap_or_else(|| v8::null(scope).into());
    if !now.strict_equals(at_intercept) {
        return;
    }
    if slot_string(scope, ev, P_EV_FOCUS).as_deref() == Some("manual") {
        return;
    }
    let body = crate::binding::protos::pristine_get(scope, doc.into(), "Document.body").unwrap_or_else(|| v8::null(scope).into());
    if now.is_null_or_undefined() || now.strict_equals(body) {
        return;
    }
    if crate::binding::protos::pristine_call(scope, now, "HTMLElement.blur", &[]).is_none() {
        crate::binding::protos::pristine_call(scope, now, "SVGElement.blur", &[]);
    }
}

/// Blink `NavigationApi::DidFinishOngoingNavigation`: `finished`, `navigatesuccess`, the transition's `finished`.
fn did_finish_ongoing_navigation(scope: &mut v8::PinScope) {
    let tracker = with_api(scope, |a| a.ongoing.take());
    if let Some(t) = tracker {
        let r = v8::Local::new(scope, &t.finished);
        let v: v8::Local<v8::Value> = match t.committed_to.as_ref() {
            Some(e) => v8::Local::new(scope, e).into(),
            None => v8::undefined(scope).into(),
        };
        r.resolve(scope, v);
    }
    if let Some(nav) = page_navigation(scope) {
        fire(scope, nav, "Event", "navigatesuccess", None);
    }
    let tr = with_api(scope, |a| a.transition.take());
    if let Some(t) = tr {
        let r = v8::Local::new(scope, &t.finished);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        r.resolve(scope, u);
    }
}

/// Blink `NavigationApi::AbortOngoingNavigation`.
fn abort_ongoing_navigation(scope: &mut v8::PinScope) {
    let Some(ev) = with_api(scope, |a| a.ongoing_event.clone()) else { return };
    let ev = v8::Local::new(scope, &ev);
    let e = make_dom_exc(scope, "Navigation was aborted", "AbortError").unwrap_or_else(|| v8::undefined(scope).into());
    event_abort(scope, ev, e);
}

/// Blink `NavigateEvent::Abort`: cancel the event if it is still being dispatched, abort its signal, let it go, and
/// report it (`navigateerror`, the promises rejected).
fn event_abort(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, error: v8::Local<v8::Value>) {
    if crate::core::events::event::evt_flag_get(scope, ev, "_dispatching") && crate::core::events::event::evt_flag_get(scope, ev, "cancelable") {
        crate::core::events::event::evt_flag_set(scope, ev, "defaultPrevented", true);
    }
    let was = with_api(scope, |a| a.ongoing_event.clone()).map(|g| v8::Local::new(scope, &g));
    if let Some(sig) = slot_obj(scope, ev, P_EV_SIGNAL) {
        crate::core::dom::abort_controller::signal_abort(scope, sig, error);
    }
    let still = with_api(scope, |a| a.ongoing_event.clone()).map(|g| v8::Local::new(scope, &g));
    if let (Some(w), Some(s)) = (was, still) {
        if w == s {
            with_api(scope, |a| a.ongoing_event = None);
        }
    }
    did_abort(scope, error);
}

/// Blink `NavigationApi::DidAbort`: the method's promises rejected, `navigateerror` (an `ErrorEvent` built as an
/// uncaught exception's is), the transition's `finished` rejected.
fn did_abort(scope: &mut v8::PinScope, error: v8::Local<v8::Value>) {
    let tracker = with_api(scope, |a| a.ongoing.take());
    if let Some(t) = tracker {
        reject_tracker(scope, &t, error);
    }
    if let Some(nav) = page_navigation(scope) {
        let (message, filename, lineno, colno) = error_site(scope, error);
        let init = crate::binding::protos::init_dict(scope);
        crate::binding::protos::set_bool(scope, init, "cancelable", true);
        let m = str_val(scope, &message);
        set_prop(scope, init, "message", m);
        let f = str_val(scope, &filename);
        set_prop(scope, init, "filename", f);
        let l: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, lineno).into();
        set_prop(scope, init, "lineno", l);
        let c: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, colno).into();
        set_prop(scope, init, "colno", c);
        set_prop(scope, init, "error", error);
        fire(scope, nav, "ErrorEvent", "navigateerror", Some(init));
    }
    let tr = with_api(scope, |a| a.transition.take());
    if let Some(t) = tr {
        if !t.committed_settled {
            let r = v8::Local::new(scope, &t.committed);
            r.reject(scope, error);
        }
        let r = v8::Local::new(scope, &t.finished);
        r.reject(scope, error);
    }
}

/// Where `error` "happened", as Blink's `CaptureSourceLocation` reads it off `v8::Exception::CreateMessage`: V8's
/// "Uncaught …" text, and the message's script, line and column -- the document's URL when the script has no name,
/// line and column 0 when no script was running.
fn error_site(scope: &mut v8::PinScope, error: v8::Local<v8::Value>) -> (String, String, u32, u32) {
    let msg = v8::Exception::create_message(scope, error);
    let message = msg.get(scope).to_rust_string_lossy(scope);
    let name = msg.get_script_resource_name(scope).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let (lineno, colno) = match msg.get_line_number(scope) {
        Some(l) if l > 0 => (l as u32, msg.get_start_column() as u32 + 1),
        _ => (0, 0),
    };
    let filename = if name.is_empty() || lineno == 0 { document_href(scope) } else { name };
    (message, filename, lineno, colno)
}

// ── NavigateEvent's methods ──────────────────────────────────────────────────────────────────────

/// Blink `NavigateEvent::PerformSharedChecks`.
fn shared_checks(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, op: &str, prefix: &str) -> bool {
    let key = crate::core::events::event::trusted_key(scope);
    let trusted = ev.get_private(scope, key).is_some_and(|v| v.is_true());
    if !trusted {
        throw_dom(scope, &format!("{prefix}{op}() may only be called on a trusted event."), "SecurityError");
        return false;
    }
    if crate::core::events::event::evt_flag_get(scope, ev, "defaultPrevented") {
        throw_dom(scope, &format!("{prefix}{op}() may not be called if the event has been canceled."), "InvalidStateError");
        return false;
    }
    true
}

fn navigate_event_this<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Option<v8::Local<'s, v8::Object>> {
    let this = v8::Local::new(scope, args.this());
    if crate::install::webidl::has_brand(scope, this.into(), "NavigateEvent") {
        return Some(this);
    }
    throw_type_error(scope, "Illegal invocation");
    None
}

/// `intercept(options)`.
fn cb_intercept(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(ev) = navigate_event_this(scope, &args) else { return };
    let prefix = "Failed to execute 'intercept' on 'NavigateEvent': ";
    let opts = match crate::install::ctor_dicts::convert_dict(scope, args.get(0), "NavigationInterceptOptions", prefix) {
        Ok(o) => o,
        Err(()) => return,
    };
    if !shared_checks(scope, ev, "intercept", prefix) {
        return;
    }
    let engine = priv_get(scope, ev, P_EV).is_some();
    let can = crate::core::events::event::evt_slot_read(scope, ev, "canIntercept").is_some_and(|v| v.is_true());
    if !can || !engine {
        let url = slot_string(scope, ev, P_EV_URL).unwrap_or_default();
        let doc = document_href(scope);
        let origin = url::Url::parse(&doc).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".to_string());
        throw_dom(
            scope,
            &format!("{prefix}A navigation with URL '{url}' cannot be intercepted by in a window with origin '{origin}' and URL '{doc}'."),
            "SecurityError",
        );
        return;
    }
    if !crate::core::events::event::evt_flag_get(scope, ev, "_dispatching") {
        throw_dom(scope, &format!("{prefix}intercept() may only be called while the navigate event is being dispatched."), "InvalidStateError");
        return;
    }
    if let Some(pre) = get_prop(scope, opts, "precommitHandler").filter(|v| v.is_function()) {
        if !crate::core::events::event::evt_flag_get(scope, ev, "cancelable") {
            throw_dom(scope, &format!("{prefix}intercept() may only be called with a precommitHandler when thenavigate event is cancelable."), "InvalidStateError");
            return;
        }
        let list = slot_list(scope, ev, P_EV_PRECOMMIT);
        push(scope, list, pre);
    }
    if intercept_state(scope, ev) == InterceptState::None {
        // the focus as it is now, which a finished interception resets only if nothing moved it
        let doc = crate::page::page_document(scope);
        let focused = crate::binding::protos::pristine_get(scope, doc.into(), "Document.activeElement").unwrap_or_else(|| v8::null(scope).into());
        priv_set(scope, ev, P_EV_FOCUSED, focused);
    }
    if let Some(f) = get_prop(scope, opts, "focusReset").filter(|v| v.is_string()) {
        priv_set(scope, ev, P_EV_FOCUS, f);
    }
    if let Some(s) = get_prop(scope, opts, "scroll").filter(|v| v.is_string()) {
        priv_set(scope, ev, P_EV_SCROLL, s);
    }
    set_intercept_state(scope, ev, InterceptState::Intercepted);
    if let Some(h) = get_prop(scope, opts, "handler").filter(|v| v.is_function()) {
        let list = slot_list(scope, ev, P_EV_HANDLERS);
        push(scope, list, h);
    }
}

/// `scroll()`. The state checks are Blink's; the scroll itself is this engine's no-scroll (module header).
fn cb_scroll(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(ev) = navigate_event_this(scope, &args) else { return };
    let prefix = "Failed to execute 'scroll' on 'NavigateEvent': ";
    if !shared_checks(scope, ev, "scroll", prefix) {
        return;
    }
    let msg = match intercept_state(scope, ev) {
        InterceptState::Finished => Some("scroll() may not be called after transition completes"),
        InterceptState::Scrolled => Some("scroll() already called"),
        InterceptState::None => Some("intercept() must be called before scroll()"),
        InterceptState::Intercepted => Some("scroll() may not be called before commit."),
        InterceptState::Committed => None,
    };
    if let Some(m) = msg {
        throw_dom(scope, &format!("{prefix}{m}"), "InvalidStateError");
        return;
    }
    set_intercept_state(scope, ev, InterceptState::Scrolled);
}

// ── NavigationPrecommitController ────────────────────────────────────────────────────────────────

fn new_precommit_controller<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "NavigationPrecommitController")?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    priv_set(scope, o, P_PC_EVENT, ev.into());
    Some(o)
}

fn precommit_event<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Option<v8::Local<'s, v8::Object>> {
    let this = v8::Local::new(scope, args.this());
    let ev = slot_obj(scope, this, P_PC_EVENT);
    if ev.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    ev
}

/// `redirect(url, options)` (Blink `NavigateEvent::Redirect`).
fn cb_precommit_redirect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(ev) = precommit_event(scope, &args) else { return };
    let prefix = "Failed to execute 'redirect' on 'NavigationPrecommitController': ";
    if args.length() < 1 {
        throw_type_error(scope, &format!("{prefix}1 argument required, but only 0 present."));
        return;
    }
    let Ok(Some(vals)) = crate::install::ctor_dicts::convert_method_args_values(scope, "NavigationPrecommitController::redirect", &args, prefix) else { return };
    let raw = vals.first().copied().flatten().map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let opts = vals.get(1).copied().flatten().and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    if !shared_checks(scope, ev, "redirect", prefix) {
        return;
    }
    if intercept_state(scope, ev) > InterceptState::Intercepted {
        throw_dom(scope, &format!("{prefix}navigation has already committed."), "InvalidStateError");
        return;
    }
    let nav_type = nav_type_of(scope, ev);
    if !matches!(nav_type, NavigationType::Push | NavigationType::Replace) {
        throw_dom(scope, &format!("{prefix}redirect() may only be used on push and replace navigations."), "InvalidStateError");
        return;
    }
    let base = url::Url::parse(&crate::core::dom::base_url(scope)).ok();
    let doc_href = document_href(scope);
    let Some(u) = base.as_ref().and_then(|b| b.join(&raw).ok()).or_else(|| url::Url::parse(&raw).ok()) else {
        throw_dom(scope, &format!("{prefix}Invalid URL '{}'.", invalid_url_text(&raw, base.as_ref())), "SyntaxError");
        return;
    };
    let doc = url::Url::parse(&doc_href).ok();
    if !doc.as_ref().is_some_and(|d| can_change_to_url_for_history_api(&u, d, !d.origin().is_tuple())) {
        let origin = doc.as_ref().map(|d| d.origin().ascii_serialization()).unwrap_or_else(|| "null".to_string());
        throw_dom(scope, &format!("{prefix}Cannot redirect to '{u}' in a document with origin '{origin}' and URL '{doc_href}'."), "SecurityError");
        return;
    }
    let history = opts.and_then(|o| get_prop(scope, o, "history")).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    if let Some(state) = opts.and_then(|o| get_prop(scope, o, "state")).filter(|v| !v.is_undefined()) {
        let Some(cloned) = crate::modules::structured_clone::clone_for(scope, state, "NavigationPrecommitController", "redirect") else { return };
        let g = v8::Global::new(scope, cloned);
        if let Some(dest) = slot_obj(scope, ev, P_EV_DEST) {
            priv_set(scope, dest, P_DEST_STATE, cloned);
        }
        with_api(scope, |a| {
            if let Some(t) = a.ongoing.as_mut() {
                t.state = Some(g);
            }
        });
    }
    let us = str_val(scope, u.as_str());
    priv_set(scope, ev, P_EV_URL, us);
    if let Some(dest) = slot_obj(scope, ev, P_EV_DEST) {
        priv_set(scope, dest, P_DEST_URL, us);
    }
    if history == "push" || history == "replace" {
        let t = str_val(scope, &history);
        crate::core::events::event::evt_slot_write(scope, ev, "navigationType", t);
    }
    if let Some(info) = opts.and_then(|o| get_prop(scope, o, "info")).filter(|v| !v.is_undefined()) {
        crate::core::events::event::evt_slot_write(scope, ev, "info", info);
    }
}

/// `addHandler(handler)` (Blink `NavigateEvent::AddHandlerDuringPrecommit`).
fn cb_precommit_add_handler(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(ev) = precommit_event(scope, &args) else { return };
    let prefix = "Failed to execute 'addHandler' on 'NavigationPrecommitController': ";
    if crate::install::ctor_dicts::convert_method_args(scope, "NavigationPrecommitController::addHandler", &args, prefix).is_err() {
        return;
    }
    if !shared_checks(scope, ev, "addHandler", prefix) {
        return;
    }
    if intercept_state(scope, ev) > InterceptState::Intercepted {
        throw_dom(scope, &format!("{prefix}navigation has already committed."), "InvalidStateError");
        return;
    }
    let list = slot_list(scope, ev, P_EV_HANDLERS);
    push(scope, list, args.get(0));
}

// ── NavigationDestination ────────────────────────────────────────────────────────────────────────

fn destination_this<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Option<v8::Local<'s, v8::Object>> {
    let this = v8::Local::new(scope, args.this());
    if priv_get(scope, this, P_DEST).is_some() {
        return Some(this);
    }
    throw_type_error(scope, "Illegal invocation");
    None
}

fn cb_dest_slot(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(d) = destination_this(scope, &args) else { return };
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if let Some(v) = priv_get(scope, d, &key) {
        rv.set(v);
    }
}

/// `index`: the destination entry's place among the entries now, -1 without one (Blink reads the entry live).
fn cb_dest_index(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(d) = destination_this(scope, &args) else { return };
    let id = slot_string(scope, d, P_DEST_ID).unwrap_or_default();
    if id.is_empty() {
        rv.set_double(-1.0);
        return;
    }
    let (entries, _) = shown_entries(scope);
    rv.set_double(entries.iter().position(|e| e.id == id).map(|i| i as f64).unwrap_or(-1.0));
}

/// `getState()`: a fresh copy of the destination's state each call, `undefined` without one.
fn cb_dest_get_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(d) = destination_this(scope, &args) else { return };
    match priv_get(scope, d, P_DEST_STATE) {
        Some(v) => {
            if let Some(c) = crate::modules::structured_clone::clone_for(scope, v, "NavigationDestination", "getState") {
                rv.set(c);
            }
        }
        None => rv.set_undefined(),
    }
}

// ── NavigationTransition ─────────────────────────────────────────────────────────────────────────

fn cb_transition_slot(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if priv_get(scope, this, P_TR).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if let Some(v) = priv_get(scope, this, &key) {
        rv.set(v);
    }
}

/// `navigation.transition`: the intercepted navigation in flight, else null.
pub(crate) fn current_transition<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    with_api(scope, |a| a.transition.as_ref().map(|t| t.object.clone())).map(|g| v8::Local::new(scope, &g))
}

// ── the tables ───────────────────────────────────────────────────────────────────────────────────

fn getter(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>, slot: Option<&str>) {
    let name = format!("get {}", member.name);
    let g = match slot {
        Some(s) => {
            let d = str_val(scope, s);
            build_fn_data(scope, cb, &name, 0, d)
        }
        None => build_fn(scope, cb, &name, 0),
    };
    define_accessor(scope, proto, member, g, None);
}

/// Mount one `NavigateEvent` method row (its attributes are the event-init slots, `evt_get`).
pub fn install_navigate_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let len = member.length as i32;
    let f = match member.name {
        "intercept" => build_fn(scope, cb_intercept, member.name, len),
        "scroll" => build_fn(scope, cb_scroll, member.name, len),
        other => {
            debug_assert!(false, "install_navigate_event: unexpected member {other}");
            return;
        }
    };
    define_value(scope, proto, member, f.into(), true);
}

/// Mount one `NavigationDestination` row.
pub fn install_navigation_destination(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "key" => getter(scope, proto, member, cb_dest_slot, Some(P_DEST_KEY)),
        "id" => getter(scope, proto, member, cb_dest_slot, Some(P_DEST_ID)),
        "url" => getter(scope, proto, member, cb_dest_slot, Some(P_DEST_URL)),
        "sameDocument" => getter(scope, proto, member, cb_dest_slot, Some(P_DEST_SAME)),
        "index" => getter(scope, proto, member, cb_dest_index, None),
        "getState" => {
            let f = build_fn(scope, cb_dest_get_state, member.name, member.length as i32);
            define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_navigation_destination: unexpected member {other}"),
    }
}

/// Mount one `NavigationTransition` row.
pub fn install_navigation_transition(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let slot = match member.name {
        "navigationType" => P_TR_TYPE,
        "from" => P_TR_FROM,
        "to" => P_TR_TO,
        "committed" => P_TR_COMMITTED,
        "finished" => P_TR_FINISHED,
        other => {
            debug_assert!(false, "install_navigation_transition: unexpected member {other}");
            return;
        }
    };
    getter(scope, proto, member, cb_transition_slot, Some(slot));
}

/// Mount one `NavigationPrecommitController` row.
pub fn install_navigation_precommit_controller(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let len = member.length as i32;
    let f = match member.name {
        "redirect" => build_fn(scope, cb_precommit_redirect, member.name, len),
        "addHandler" => build_fn(scope, cb_precommit_add_handler, member.name, len),
        other => {
            debug_assert!(false, "install_navigation_precommit_controller: unexpected member {other}");
            return;
        }
    };
    define_value(scope, proto, member, f.into(), true);
}

// The rows are the generated registry's, verbatim and in the capture's order, which is where the four interfaces
// lived as shells until they had bodies.
webapi::interface! {
    "NavigationTransition" [interface, window] {
        ctor "NavigationTransition"                        len 0 = stub,
        attr "navigationType" : Any readonly                     = install_navigation_transition,
        attr "from"           : Any readonly                     = install_navigation_transition,
        attr "to"             : Any readonly                     = install_navigation_transition,
        attr "committed"      : Promise readonly                 = install_navigation_transition,
        attr "finished"       : Promise readonly                 = install_navigation_transition
    }
}

webapi::interface! {
    "NavigationPrecommitController" [interface, window] {
        ctor   "NavigationPrecommitController"                 len 0 = stub,
        method "redirect"   : Undefined len 1 required 1             = install_navigation_precommit_controller,
        method "addHandler" : Undefined len 1 required 1             = install_navigation_precommit_controller
    }
}

webapi::interface! {
    "NavigationDestination" [interface, window] {
        ctor   "NavigationDestination"                 len 0 = stub,
        attr   "key"          : DomString readonly           = install_navigation_destination,
        attr   "id"           : DomString readonly           = install_navigation_destination,
        attr   "url"          : UsvString readonly           = install_navigation_destination,
        attr   "index"        : Integer readonly             = install_navigation_destination,
        attr   "sameDocument" : Boolean readonly             = install_navigation_destination,
        method "getState"     : Any len 0 nullable           = install_navigation_destination
    }
}

webapi::interface! {
    "NavigateEvent" : "Event" [value, window] {
        ctor   "NavigateEvent"                                        len 2 = stub,
        attr   "navigationType"        : Any readonly                       = evt_get,
        attr   "destination"           : Any readonly                       = evt_get,
        attr   "canIntercept"          : Boolean readonly                   = evt_get,
        attr   "userInitiated"         : Boolean readonly                   = evt_get,
        attr   "hashChange"            : Boolean readonly                   = evt_get,
        attr   "signal"                : Any readonly                       = evt_get,
        attr   "formData"              : Any readonly nullable              = evt_get_null,
        attr   "downloadRequest"       : DomString readonly nullable        = evt_get_null,
        attr   "info"                  : Any readonly                       = evt_get,
        attr   "sourceElement"         : Any readonly nullable              = evt_get_null,
        method "intercept"             : Undefined len 0                    = install_navigate_event,
        method "scroll"                : Undefined len 0                    = install_navigate_event,
        attr   "hasUAVisualTransition" : Boolean readonly                   = evt_get
    }
}

/// `NavigationTransition`, created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn create_navigation_transition<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    create_plain(scope, global, &NAVIGATIONTRANSITION, ledger);
}

/// `NavigationPrecommitController`, created at its capture position by the registry walk.
pub(crate) fn create_navigation_precommit_controller<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    create_plain(scope, global, &NAVIGATIONPRECOMMITCONTROLLER, ledger);
}

/// `NavigationDestination`, created at its capture position by the registry walk.
pub(crate) fn create_navigation_destination<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    create_plain(scope, global, &NAVIGATIONDESTINATION, ledger);
}

/// `new NavigateEvent(type, eventInit)`: `type`, then the `NavigateEventInit` converted as WebIDL converts it (its
/// `destination` and `signal` required), the `Event` steps, and every member onto its slot -- `info` left out when
/// the dictionary has none (it reads `undefined`), the nullable ones `null`.
pub(crate) fn cb_navigate_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    use crate::core::events::ctor::{arg_string, brand, init_event, require_args, require_new};
    if !require_new(scope, &args, "NavigateEvent") || !require_args(scope, &args, "NavigateEvent", 2) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "NavigateEvent") else { return };
    let Ok(dict) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "NavigateEventInit", "Failed to construct 'NavigateEvent': ") else { return };
    let this = args.this();
    init_event(scope, this, &ty, Some(dict));
    for name in [
        "navigationType",
        "destination",
        "canIntercept",
        "userInitiated",
        "hashChange",
        "signal",
        "formData",
        "downloadRequest",
        "info",
        "hasUAVisualTransition",
        "sourceElement",
    ] {
        if let Some(v) = get_prop(scope, dict, name) {
            crate::core::events::event::evt_slot_write(scope, this, name, v);
        }
    }
    brand(scope, this, "NavigateEvent");
    rv.set(this.into());
}

/// `NavigateEvent`, created at its capture position by the registry walk.
pub(crate) fn create_navigate_event<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    crate::install::live::rows::build_newable(scope, global, &NAVIGATEEVENT, cb_navigate_event_ctor, Some("Event"), None, ledger);
}

fn create_plain<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, def: &'static crate::install::InterfaceDef, ledger: &mut crate::install::installer::InstallLedger) {
    use crate::install::installer::{self, Flavor};
    let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] navigation install gap: {}.{}", g.interface, g.member);
    }
}
