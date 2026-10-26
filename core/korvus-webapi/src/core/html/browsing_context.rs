//! `core/html/browsing_context.rs` -- nested and auxiliary browsing contexts.
//!
//! ONE V8 CONTEXT PER BROWSING CONTEXT'S DOCUMENT. A nested browsing context
//! (`<iframe>`/`<frame>`) and an auxiliary one (`window.open`) each get their own `v8::Context` in the
//! page's isolate: their own intrinsics (`Array`, `Object`, `Function`, `eval`, `JSON`), their own
//! global object and global scope. The context's WINDOW is that context's global proxy, reparented
//! onto `Window.prototype` and carrying the members a child's scripts and its parent's scripts read,
//! over a REAL parsed document (a `DOMParser` document over arena nodes: one arena, several document
//! roots, exactly what DOMParser has always done). A navigation that replaces a navigated document
//! makes a new context behind the SAME global proxy, so `iframe.contentWindow` keeps its identity.
//!
//! WHAT A FRAME SHARES WITH THE PAGE. Every Web API function and interface object on a frame's window
//! is the page realm's own (the page's install-time surface, [`seal_frame_surface`]), so
//! `contentWindow.Node === Node` where Chrome's two realms answer `false`; the JavaScript intrinsics
//! are the frame's. The frame shares the page's security token and deno_core's per-context state
//! ([`new_realm`]). Until F9 a frame was a plain object reached through `with` scopes in the page's
//! context, so `Function('return this')()` in a frame returned the PAGE's window and the frame's
//! `Array` was the page's -- which is how a challenge provider a challenge widget's challenge frame came to look its
//! elements up in the page's document.
//!
//! The state is [`BrowsingContexts`] in `OpState`. A child window carries its context id in a private
//! slot; a frame ELEMENT maps to its context by node id; a context's DOCUMENT maps back to the
//! context so that the Document TABLE's `defaultView` / `location` / `open` / `write` / `writeln` /
//! `close` rows answer for it (core/dom/document.rs) -- where the JS installed six own properties on
//! every frame document, the rows now ask [`context_of_document`].
//!
//! # Which realm is running
//!
//! A child context's scripts are entered by THIS module, so the engine knows which browsing context a
//! piece of script belongs to. That fact is what `postMessage` needs: a message posted from inside a
//! popup at `opener` must arrive at the opener carrying `source === thePopupWindow` and the POPUP's
//! origin, not the page's. [`running`] answers it: the context this module entered (a script, a timer
//! or microtask callback), else the one whose V8 context is entered or whose promise job is running.
//! `None` means "the page's own realm".
//!
//! # Where the model is thinner than a browser's
//!
//! A real browser has two WindowProxy objects after a COOP browsing-context-group switch: the
//! disowned one the opener holds and the live one the popup's own scripts see. This engine has ONE
//! object serving both realms, so `closed`/`length`/`name` answer by WHO IS ASKING (the current
//! realm), which is the pair of answers a real browser gives, off one object. `document.write()` on
//! a frame accumulates and builds the tree once at `close()` -- a real parser tokenizes each write
//! incrementally; no test in scope observes the difference. The frame window's prototype chain is the
//! PAGE's (`Window.prototype` ... `Object.prototype`), so `window instanceof Object` is false inside a
//! frame.

use std::collections::{HashMap, HashSet};

use deno_core::{v8, OpState};
use spec::DeliveredIsolation;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{build_fn, build_fn_data, call_method, get_prop, set_prop, str_val, template_fn, throw_dom, throw_type_error, unwrap_id, wrap_cached};
use crate::core::dom::shared::{document_node_id, set_attribute_notify};
use crate::core::events::event_target::et_dispatch;
use crate::core::events::report::ErrorSite;
use crate::core::html::html_dialog_element::mark_trusted;
use crate::core::html::isolation_policy::{coop_requires_switch, doc_policy, headers_from_js, resolve_for};

/// A frame's modules: its `import()` loads and evaluates in the frame's own realm.
#[path = "frame_modules.rs"]
mod frame_modules;

type OpStateRc = std::rc::Rc<std::cell::RefCell<OpState>>;

// ── state ─────────────────────────────────────────────────────────────────────────────────────────

/// One browsing context: its window object, its document, its Location, and the navigation state
/// the JS kept on `ctx`.
struct Ctx {
    iwin: v8::Global<v8::Object>,
    idoc: Option<v8::Global<v8::Object>>,
    loc: Option<v8::Global<v8::Object>>,
    /// `raw` parsed (the WHATWG URL parser `URL` itself is), or `None` when it does not parse -- the
    /// components are the URL parser's answers. It was a JS `URL` made through the page's global `URL`
    /// constructor and read and written through `URL.prototype`: a page that wrapped `URL` watched the
    /// engine parse every frame's and every message's URL (150 `URL` constructions
    /// inside `postMessage` where Chrome makes none).
    url: Option<url::Url>,
    raw: String,
    /// The frame element (node id) this context belongs to; `None` for an auxiliary context.
    el: Option<u32>,
    /// The `document.write()` buffer between `open()` and `close()`.
    writing: Option<String>,
    /// After an explicit `open()`: the document element the last `write()` produced, so
    /// `close()` can tell whether script replaced it since (html2canvas's clone does).
    written_el: Option<Option<u32>>,
    /// `close()` of a document script rebuilt after the last write: run the load steps, no scripts.
    skip_doc_scripts: bool,
    /// The document's end runs inside the `close()` that ends a script-created parser:
    /// its `load` fires before `close()` returns, not a task later.
    end_in_close: bool,
    /// Where its document's load event is (Blink's `load_event_progress_`): not run, being dispatched, done.
    load_event: FrameLoadEvent,
    popup: bool,
    /// The policy the context's CURRENT document runs under (a popup inherits its opener's until its
    /// first real response).
    policy: Option<DeliveredIsolation>,
    left_initial_about_blank: bool,
    disowned: bool,
    closed: bool,
    /// The window name it is registered under in its opening realm's named map.
    name: Option<String>,
    name_realm: Option<Option<u32>>,
    win_name: String,
    /// The V8 context the context's CURRENT document runs in: its own intrinsics and
    /// global scope, behind the one WindowProxy `iwin` is.
    realm: Option<v8::Global<v8::Context>>,
    /// A `blob:` URL this frame has ALREADY navigated to synchronously ([`frame_src_set`]), whose
    /// loader completion is therefore not a second navigation (see [`frame_navigated`]).
    blob_nav: Option<String>,
    /// The arena root of the context's CURRENT document. `by_doc` keeps every document a
    /// context ever held, which is right for `document.write()` and wrong for layout: only the
    /// document a frame is showing is laid out into it.
    doc_root: Option<u32>,
    /// How many `window[i]` this context's window holds for the frames of its own document
    /// ([`sync_child_window_frames`]).
    idx_count: u32,
    /// Which navigation of this context is the ONGOING one. HTML's "navigate" begins by
    /// aborting the context's ongoing navigation, so a second `form.submit()` before the first
    /// response arrives supersedes it -- `html/semantics/forms/form-submission-0/`'s
    /// `form-double-submit-preventdefault-click.html` submits twice from one click handler and asserts
    /// the SECOND one is the document that lands. Two in-flight fetches would otherwise race, and the
    /// slower one would win.
    nav_gen: u32,
    /// The frame document's `readyState`: `loading` while its parser scripts run,
    /// `interactive` at `DOMContentLoaded`, `complete` at `load`.
    readiness: Readiness,
    /// `document.currentScript` of the frame document: the classic script running in it (F3).
    current_script: Option<v8::Global<v8::Value>>,
    /// The `currentScript` to restore once a parser script's microtasks have run: set when the script
    /// ran inside a microtask job, where HTML's checkpoint cannot run, and restored by the continuation task.
    restore_script: Option<Option<v8::Global<v8::Value>>>,
    /// `document.referrer` of the frame document, fixed when it was created (F5).
    referrer: String,
    /// HTML's "about base URL": the base URL an `about:blank` / `about:srcdoc` document of
    /// this context falls back to -- its creator's document base URL when the context was made, or when its
    /// `srcdoc` navigation began, kept as it was then (Chrome 150: removing the parent's `<base>` afterwards
    /// changes neither). It was the PAGE's base: a bot-challenge frame's blank child read
    /// `document.baseURI` as the embedding site's URL (live, rateyourmusic, without OOPIF), where Chrome reads the
    /// challenge frame's.
    about_base: Option<String>,
    /// A navigation has installed a document here (see [`forget_previous_window`]).
    navigated: bool,
    /// This context's realm is its own, built from templates when the context was made
    /// ([`build_frame_realm`]); otherwise the window shares the page's surface.
    installed: bool,
    /// `history.state` of this context's document: a frame's own, never the page's.
    history_state: Option<v8::Global<v8::Value>>,
    /// The frame document's parser is running one of its scripts -- HTML's "active parser whose script
    /// nesting level is greater than 0": from before the script until its microtasks have run.
    parser_nesting: u32,
    /// `window.stop()` aborted the frame document's parser while it ran.
    parser_aborted: bool,
    /// The insertion point of a `document.write` from one of the document's parser scripts: the script, and the
    /// last node it wrote (the next write goes after it).
    write_anchor: Option<(u32, u32)>,
    /// The document's OPAQUE origin, when its frame is sandboxed without `allow-same-origin`: a
    /// number no other document's origin shares. Set when the frame is bound to its element and again for each
    /// document it loads -- the sandbox flags are the element's at that moment -- and inherited by a frame
    /// nested in such a document.
    opaque: Option<u32>,
    /// The document's SANDBOXED SCRIPTS flag (HTML "sandboxed scripts browsing context flag"): no script of it runs --
    /// parser, inserted or fetched, module, event handler content attribute, string timer, `javascript:` URL. Set
    /// with [`opaque`](Self::opaque), from the same flag set ([`sandbox_of`]).
    scripts_sandboxed: bool,
    /// The loads that delay THIS frame document's load event (HTML "delay the load event": its scripts, images,
    /// sheets, fonts, nested frames), as the page's set delays the page's: tokens taken while the frame's realm
    /// runs, before its load event has run ([`delay_load`]). Its load steps park while any is out
    /// (`load_parked`) and run once the last is released ([`undelay_load`]). Chrome 150: a frame's `load` waits
    /// for a script its own script inserted (a bot-detection script's sync frame).
    delays: std::collections::HashSet<u32>,
    load_parked: bool,
    /// The policies the frame document's RESPONSE delivered: its `Referrer-Policy`, its
    /// `Permissions-Policy`/`Feature-Policy` headers, and its `Origin-Agent-Cluster`. Set when the
    /// navigation's response arrives ([`cb_nav_ok`]); a document with no response (`about:`) has none.
    delivered: FrameDelivered,
    /// The id of the child engine that runs this frame's document on its own thread, when the
    /// committed document is cross-site to the top (`core/html/remote_frame.rs`). The local context then
    /// shows an empty document at the frame's URL -- so the parent sees the cross-origin WindowProxy --
    /// and its `postMessage` crosses to the child.
    remote: Option<u64>,
    /// A popup open as a target of its own (`core/html/popup.rs`, D74 step 3): `remote` is its id there, and this is
    /// the context whose script opened it -- the window its `opener.postMessage` reaches.
    popup_opener: Option<Option<u32>>,
    /// The clock of the context's CURRENT document (HR Time): its time origin is the start of
    /// the navigation that produced it -- the frame's own -- or the moment it was made, for a document no
    /// navigation fetched. A frame made 500 ms after its page read `fetchStart` 510 and `performance.now()`
    /// 562 on the page's clock where Chrome 150 reads 2 and 58.
    clock: spec::PerfClock,
    /// The document's lifecycle milestones on that clock (`domInteractive`, ...), for its navigation entry
    /// -- which read the PAGE's.
    milestones: Vec<(&'static str, f64)>,
    /// When the frame's pending `srcdoc` / `about:blank` navigation BEGAN (a [`spec::process_ms`] reading): the
    /// time origin of the document it commits, which no response carries (HTML: a document's time origin is its
    /// navigation's start). The commit comes [`SRCDOC_COMMIT_MS`] later; the clock started at the commit, so a
    /// script-less srcdoc document reached `interactive` and `complete` at 0.0 -- "not yet" to every reader -- where
    /// Chrome 150 reads them 2-10 ms in.
    inline_nav_start: Option<f64>,
    /// The document's navigation entry once a script asked for it, so a later milestone updates it (it
    /// replaced the page's own, which then took the frame's milestones).
    nav_entry: Option<v8::Global<v8::Object>>,
    /// The extension bindings (`chrome.app`) taken off the initial about:blank's window because the context
    /// was made while its frame was requesting a navigation, to go back when that navigation commits
    /// ([`defer_extension_bindings`]).
    deferred_bindings: Option<v8::Global<v8::Value>>,
    /// The isolated worlds a DevTools client's new-document scripts named, made over the CURRENT document as it started
    /// ([`devtools_frame_document_start`]): each world's name and index, with the document root they are over.
    devtools_worlds: Vec<(String, usize, u32)>,
    /// A navigation has started and no document has arrived for it yet (a DevTools client is not told of the initial
    /// `about:blank` a popup or frame shows meanwhile as if it were the page's document).
    nav_pending: bool,
}

/// What a frame document's response said about the document's policies (see `Ctx::delivered`).
#[derive(Default, Clone)]
pub(crate) struct FrameDelivered {
    pub referrer_policy: Option<transport::ReferrerPolicy>,
    /// The policy headers, lowercase name and value, for `core/feature_policy` to parse against the
    /// frame document's own origin.
    pub permissions_headers: Vec<(String, String)>,
    /// `Origin-Agent-Cluster: ?1` / `?0`, when the response sent one.
    pub origin_agent_cluster: Option<bool>,
    /// Every header the response carried, for a remote frame's child engine.
    pub all_headers: Vec<(String, String)>,
    /// The response's fetch timing, for a remote frame's navigation entry.
    pub timing: Option<spec::FetchTiming>,
    /// The document's CSP list as delivered (`core/html/csp.rs`): its response's policies, or -- for a
    /// document no response delivered (`about:blank`, `about:srcdoc`, `blob:`, `data:`) -- its parent's,
    /// cloned when it was created (HTML's policy container). Its own `<head>` metas are read on demand
    /// ([`ctx_csp`]).
    pub csp: crate::core::html::csp::CspList,
    /// The encoding the document was decoded with ([`frame_encoding_delivered`]).
    pub encoding: Option<String>,
}

impl FrameDelivered {
    fn from_headers(headers: &[(String, String)]) -> FrameDelivered {
        let oac = headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("origin-agent-cluster"))
            .filter_map(|(_, v)| match v.trim() {
                "?1" => Some(true),
                "?0" => Some(false),
                _ => None,
            })
            .last();
        FrameDelivered {
            referrer_policy: transport::ReferrerPolicy::from_headers(headers),
            permissions_headers: headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("permissions-policy") || k.eq_ignore_ascii_case("feature-policy"))
                .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                .collect(),
            origin_agent_cluster: oac,
            all_headers: headers.to_vec(),
            timing: None,
            csp: crate::core::html::csp::CspList::from_headers(headers),
            encoding: None,
        }
    }
}

impl Ctx {
    fn new(iwin: v8::Global<v8::Object>) -> Ctx {
        Ctx {
            iwin,
            history_state: None,
            idoc: None,
            loc: None,
            url: None,
            raw: "about:blank".to_string(),
            el: None,
            writing: None,
            popup: false,
            policy: None,
            left_initial_about_blank: false,
            disowned: false,
            closed: false,
            nav_gen: 0,
            written_el: None,
            skip_doc_scripts: false,
            end_in_close: false,
            load_event: FrameLoadEvent::NotRun,
            name: None,
            name_realm: None,
            win_name: String::new(),
            realm: None,
            blob_nav: None,
            doc_root: None,
            idx_count: 0,
            readiness: Readiness::Complete,
            current_script: None,
            restore_script: None,
            referrer: String::new(),
            about_base: None,
            navigated: false,
            installed: false,
            parser_nesting: 0,
            parser_aborted: false,
            write_anchor: None,
            opaque: None,
            scripts_sandboxed: false,
            delays: Default::default(),
            load_parked: false,
            delivered: FrameDelivered::default(),
            remote: None,
            popup_opener: None,
            clock: spec::PerfClock::default(),
            milestones: Vec::new(),
            inline_nav_start: None,
            nav_entry: None,
            deferred_bindings: None,
            devtools_worlds: Vec::new(),
            nav_pending: false,
        }
    }
}

/// Set on the isolate once some frame keeps a clock of its own ([`Ctx::clock`]): until then a realm's
/// clock is the page's, read straight from its isolate slot (`performance.now()` is a hot path).
pub(crate) struct FrameClocks;

/// The clock of the frame whose realm is current -- `None` in the page's realm.
pub(crate) fn realm_frame_clock(scope: &mut v8::PinScope) -> Option<spec::PerfClock> {
    scope.get_slot::<FrameClocks>()?;
    let id = realm_ctx(scope).filter(|&id| id != MAIN_CTX)?;
    with_ctx(scope, id, |c| c.clock)
}

/// The frame document's milestone `name` (0 until it has happened) -- `None` in the page's realm.
pub(crate) fn realm_frame_milestone(scope: &mut v8::PinScope, name: &str) -> Option<f64> {
    realm_frame_milestone_at(scope, name).map(|t| t.unwrap_or(0.0))
}

/// The frame document's milestone `name`: `Some(None)` until it has happened -- a milestone CAN be 0.0, reached in
/// the time origin's first quantum -- and `None` in the page's realm.
pub(crate) fn realm_frame_milestone_at(scope: &mut v8::PinScope, name: &str) -> Option<Option<f64>> {
    let id = realm_ctx(scope).filter(|&id| id != MAIN_CTX)?;
    with_ctx(scope, id, |c| c.milestones.iter().find(|(n, _)| *n == name).map(|(_, t)| *t))
}

/// Keep `entry` as the frame document's navigation entry, if the current realm is a frame's.
pub(crate) fn keep_realm_frame_nav_entry(scope: &mut v8::PinScope, entry: v8::Local<v8::Object>) -> bool {
    let Some(id) = realm_ctx(scope).filter(|&id| id != MAIN_CTX) else { return false };
    let g = v8::Global::new(scope, entry);
    with_ctx(scope, id, |c| c.nav_entry = Some(g)).is_some()
}

/// Frame `id`'s document reached milestone `name`, now on its clock: recorded once, and written into its
/// navigation entry if a script already holds it.
fn note_frame_milestone(scope: &mut v8::PinScope, id: u32, name: &'static str) {
    let entry = with_ctx(scope, id, |c| {
        if c.milestones.iter().any(|(n, _)| *n == name) {
            return None;
        }
        let t = c.clock.now_ms();
        c.milestones.push((name, t));
        Some((t, c.nav_entry.clone()))
    })
    .flatten();
    if let Some((t, Some(e))) = entry {
        let e = v8::Local::new(scope, &e);
        crate::core::timing::performance::set_entry_milestone(scope, e, name, t);
    }
}

/// One document's script run: the `<script>` elements in document order, which of them run at all,
/// their resolved `src`s, the in-flight fetches, and the cursor.
struct ScriptRun {
    ctx: u32,
    els: Vec<v8::Global<v8::Object>>,
    runs: Vec<bool>,
    abs: Vec<Option<String>>,
    pending: Vec<Option<v8::Global<v8::Promise>>>,
    i: usize,
}

/// HTML's "delaying the load event" set, for the page document.
#[derive(Default)]
struct LoadDelay {
    set: HashSet<u32>,
    /// The document's own load-event step, parked until the set drains.
    waiter: Option<v8::Global<v8::Function>>,
    /// How long the page may hold the isolate WITHOUT the host's event loop getting a turn while the
    /// load step is parked. `Some` exactly while a step is parked. This is not a deadline for the load
    /// event -- Chrome has none -- see [`when_load_delays_done`].
    starve_limit: Option<std::time::Duration>,
    /// The realm the parked load step belongs to. The interrupt path (below) is entered from a
    /// watchdog thread's V8 interrupt, where there is no calling frame to take a context from, so
    /// the context is recorded when the waiter parks.
    realm: Option<v8::Global<v8::Context>>,
    /// The thread that posts the V8 interrupt when the page has held the isolate past `starve_limit`.
    /// Created on first use and reused for the isolate's whole life.
    watchdog: Option<CapWatchdog>,
    /// The parked step's `cap_ms`, for the check that re-parks it ([`cb_load_step_check`]).
    cap_ms: i32,
}

/// When the host's event loop last got a turn, on the host's clock -- written by `Engine::pump`, read
/// by the load-delay watchdog thread.
///
/// A turn is the pump polling the event loop. While a page waits for a subresource the pump keeps
/// polling, and a beat future polled beside the loop stamps `last` every [`EventLoopTurns::BEAT`]. A
/// page running JavaScript that never returns holds the one thread both futures share, so the stamps
/// stop: THAT, and only that, is starvation. `in_loop` is false whenever nobody is pumping at all
/// (between two `pump` calls, inside a host `eval`), so a gap in stamps there reads as idle, not as
/// starved.
pub struct EventLoopTurns {
    base: std::time::Instant,
    in_loop: std::sync::atomic::AtomicBool,
    last_ms: std::sync::atomic::AtomicU64,
}

impl Default for EventLoopTurns {
    fn default() -> Self {
        Self { base: std::time::Instant::now(), in_loop: std::sync::atomic::AtomicBool::new(false), last_ms: std::sync::atomic::AtomicU64::new(0) }
    }
}

impl EventLoopTurns {
    /// How often the pump's beat future stamps a turn. Well below any starvation limit, so a page that
    /// is merely waiting is never mistaken for one that is spinning.
    pub const BEAT: std::time::Duration = std::time::Duration::from_millis(50);
    fn now_ms(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    /// The loop got a turn now.
    pub fn beat(&self) {
        self.last_ms.store(self.now_ms(), std::sync::atomic::Ordering::Relaxed);
    }
    /// The pump is about to poll the event loop.
    pub fn enter(&self) {
        self.beat();
        self.in_loop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    /// The pump stopped polling the event loop.
    pub fn leave(&self) {
        self.in_loop.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    /// How long the loop has gone without a turn while the pump was inside it; `None` when nobody is
    /// pumping, which is never starvation.
    fn held_for(&self) -> Option<std::time::Duration> {
        if !self.in_loop.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let last = self.last_ms.load(std::sync::atomic::Ordering::Relaxed);
        Some(std::time::Duration::from_millis(self.now_ms().saturating_sub(last)))
    }
}

// ── the starvation watchdog: a V8 interrupt, posted from off-thread ────────────────────────────────
//
// A parked load step runs when its set drains, and a set drains only when the page's event loop runs
// the completions that release it. A page that holds the isolate in JavaScript and never returns --
// `while (document.readyState !== "complete") {}` is the whole of it -- can therefore never load, and
// can never stop spinning either (the `rakuten.co.jp` shape). Nothing inside the page's task
// queue can break that, and `tokio::time::timeout` is cooperative, so the pump cannot preempt it.
// V8's `RequestInterrupt` runs a host callback at the isolate's next interrupt check, which a JS loop
// back-edge IS, and that is the only mechanism that reaches inside a synchronous spin.
//
// What it no longer does is fire on a CLOCK. It was a 1000 ms deadline from the moment
// the step parked, enforced both here and from `Engine::pump`, and it released `load` on every page
// whose subresources took longer than a second -- measured against Chrome 150, which waited 1.5, 3
// and 6 s for a slow script and was still waiting at 20 s for one that never answered. So the
// interrupt now fires only when the event loop has had NO TURN for the limit ([`EventLoopTurns`]):
// a page that is waiting is left waiting, as Chrome leaves it.
//
// One thread per isolate, parked on a condvar, spawned the first time a load step parks behind a
// non-empty set. It holds no V8 handles: the only V8 it touches is `IsolateHandle`, which is
// Send+Sync by construction (it is built for cross-thread termination) and refuses the call if the
// isolate is already gone.
struct CapWatchdog {
    shared: std::sync::Arc<CapShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct CapShared {
    state: std::sync::Mutex<CapWatchState>,
    wake: std::sync::Condvar,
}

#[derive(Default)]
struct CapWatchState {
    /// The starvation limit while a step is parked; `None` means nothing is parked.
    limit: Option<std::time::Duration>,
    stop: bool,
}

impl CapWatchdog {
    fn new(handle: v8::IsolateHandle, turns: std::sync::Arc<EventLoopTurns>) -> Self {
        let shared = std::sync::Arc::new(CapShared::default());
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("korvus-load-delay-cap".into())
            .spawn(move || cap_watchdog_loop(&s, &handle, &turns))
            .ok();
        Self { shared, thread }
    }
    fn arm(&self, limit: std::time::Duration) {
        if let Ok(mut g) = self.shared.state.lock() {
            g.limit = Some(limit);
        }
        self.shared.wake.notify_all();
    }
    fn disarm(&self) {
        if let Ok(mut g) = self.shared.state.lock() {
            g.limit = None;
        }
        self.shared.wake.notify_all();
    }
}

/// Signal the thread and DETACH it; do not join. This drop runs while the isolate is being torn down
/// (`BrowsingContexts` lives in the isolate's `OpState`), and the watchdog may at that instant be
/// inside `request_interrupt`, which takes the isolate's own teardown mutex. Joining there would be
/// this thread waiting on that mutex while that thread waits for it. The detached thread wakes on the
/// notify, sees `stop`, and exits; it holds nothing but its `Arc`s and an `IsolateHandle`, which
/// answers safely -- with `false` -- once the isolate is gone.
impl Drop for CapWatchdog {
    fn drop(&mut self) {
        if let Ok(mut g) = self.shared.state.lock() {
            g.stop = true;
            g.limit = None;
        }
        self.shared.wake.notify_all();
        drop(self.thread.take());
    }
}

/// While armed, look at the event loop's turns: sleep until the earliest moment the loop could have
/// gone `limit` without one, and post the interrupt only if it really has. It stays armed after
/// posting -- the callback may land where it cannot act (`op_state` borrowed) -- and re-checks one
/// limit later; the release is idempotent.
fn cap_watchdog_loop(shared: &std::sync::Arc<CapShared>, handle: &v8::IsolateHandle, turns: &EventLoopTurns) {
    let Ok(mut g) = shared.state.lock() else { return };
    loop {
        if g.stop {
            return;
        }
        let Some(limit) = g.limit else {
            let Ok(next) = shared.wake.wait(g) else { return };
            g = next;
            continue;
        };
        let wait = match turns.held_for() {
            Some(held) if held >= limit => {
                // NEVER hold the lock across the V8 call: `request_interrupt` takes the isolate's own
                // mutex, and the isolate thread takes ours from `arm`/`disarm`.
                drop(g);
                handle.request_interrupt(cb_cap_interrupt, std::ptr::null_mut());
                let Ok(next) = shared.state.lock() else { return };
                g = next;
                limit
            }
            Some(held) => limit - held,
            None => limit,
        };
        let Ok((next, _)) = shared.wake.wait_timeout(g, wait) else { return };
        g = next;
    }
}

/// The interrupt itself, on the isolate's thread, between two JS operations of whatever the page is
/// doing. It enters the realm the waiter was parked in and runs [`release_starved_load_step`], which
/// does nothing unless a waiter really is still parked and the loop really is still starved -- an
/// interrupt requested during a spin can land after it, inside an unrelated host `eval`.
///
/// `op_state` is borrowed non-blockingly and the callback returns empty-handed rather than panicking
/// if something up the stack already holds it: an interrupt that arrives at an awkward moment must
/// cost the page nothing, and the watchdog asks again one limit later.
unsafe extern "C" fn cb_cap_interrupt(mut raw: v8::UnsafeRawIsolatePtr, _data: *mut std::ffi::c_void) {
    if raw.is_null() {
        return;
    }
    // SAFETY: V8 hands the interrupt callback the isolate it was requested on, on that isolate's own
    // thread, with the isolate entered and no V8 API call in progress.
    let isolate = unsafe { v8::Isolate::ref_from_raw_isolate_ptr_mut(&mut raw) };
    let op_state = deno_core::JsRuntime::op_state_from(isolate);
    let realm = {
        let Ok(st) = op_state.try_borrow() else { return };
        let Some(b) = st.try_borrow::<BrowsingContexts>() else { return };
        if b.delay.waiter.is_none() {
            return;
        }
        match &b.delay.realm {
            Some(c) => c.clone(),
            None => return,
        }
    };
    drop(op_state);
    v8::scope_with_context!(let scope, isolate, &realm);
    release_starved_load_step(scope);
}

/// HTML's "current document readiness" for the page document — the one fact `document.readyState`
/// reports. It was a string constant (`"complete"`, in two places in `core/dom/document.rs`) until
/// Which is why an inline `<head>` script saw `complete` where Chrome says `loading`
/// and every `DOMContentLoaded` handler saw `complete` where Chrome says `interactive`.
///
/// The default is `Complete`, not `Loading`: an engine that has not navigated is sitting on the
/// initial `about:blank`, whose readiness is complete in every browser, and `Engine::load_document`
/// (the harness entry point that parses without running a lifecycle) hands over a finished document.
/// A navigation sets `Loading` for itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Readiness {
    Loading,
    Interactive,
    #[default]
    Complete,
}

/// A frame document's load event (Blink's `Document::load_event_progress_`, the three states this engine needs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FrameLoadEvent {
    /// A new document, or one `open()` reset (`LoadEventStillNeeded`).
    NotRun,
    /// The window's `load` and the frame element's are being dispatched.
    InProgress,
    Completed,
}

impl Readiness {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Readiness::Loading => "loading",
            Readiness::Interactive => "interactive",
            Readiness::Complete => "complete",
        }
    }
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "loading" => Some(Readiness::Loading),
            "interactive" => Some(Readiness::Interactive),
            "complete" => Some(Readiness::Complete),
            _ => None,
        }
    }
}

/// Every nested and auxiliary browsing context of this isolate, and the realm bookkeeping.
#[derive(Default)]
pub struct BrowsingContexts {
    ctxs: Vec<Ctx>,
    by_frame: HashMap<u32, u32>,
    by_doc: HashMap<u32, u32>,
    /// The context whose script is running right now; `None` is the page's own realm.
    current: Option<u32>,
    /// Window name -> context, PER OPENING REALM: a name is looked up inside a browsing-context
    /// group, and a popup that a popup opened is not in the page's group at all. One global map
    /// meant twelve independent popups in WPT's `resource-popup` all resolved to ONE context.
    named: HashMap<Option<u32>, HashMap<String, u32>>,
    /// The page's own V8 context: what a frame context copies its security token and deno_core's two
    /// realm slots from, and what "the window" means to a Rust callback running in a frame context.
    main: Option<v8::Global<v8::Context>>,
    delay: LoadDelay,
    /// The host event loop's turns, shared with `Engine::pump` and the load-delay watchdog thread.
    turns: std::sync::Arc<EventLoopTurns>,
    /// Source of the anonymous delay tokens a non-frame subresource load takes (see
    /// [`new_load_delay_token`]).
    next_delay_token: u32,
    /// The page document's current readiness — see [`Readiness`].
    readiness: Readiness,
    frame_idx_count: u32,
    /// The page document's frame elements in tree order, as [`frames_now`] last walked them, with the
    /// document root, the arena instance and the journal position they hold at.
    frames_at: Option<(u32, u64, u64, Vec<u32>)>,
    runs: HashMap<u32, ScriptRun>,
    next_run: u32,
    /// The page realm's own global surface as the INSTALL left it -- every own string key of the
    /// global, in order, with its property descriptor -- taken by [`seal_frame_surface`] before any
    /// page script runs. What a nested context's window is built from.
    surface: Option<(v8::Global<v8::Array>, v8::Global<v8::Array>)>,
    /// How a frame realm built from templates takes each name of that surface ([`lazy_plan`]), worked out
    /// once per runtime.
    lazy_plan: Option<std::rc::Rc<LazyPlan>>,
}

/// A frame realm's global, built from templates: the install-time surface ([`frame_surface`])
/// from the first name the fresh frame global does not already have at the same place, each name in the
/// page's order and the way a realm built from templates takes it.
struct LazyPlan {
    /// from the first name a fresh frame global does not have at the page's place (the prefix before it
    /// is V8's intrinsics, in the same order), each name in the page's order
    rows: Vec<(v8::Global<v8::Name>, LazyRow)>,
    /// the names the page locks, locked once the frame has them (but those [`furnish_window`] locks)
    locks: Vec<v8::Global<v8::Name>>,
}

enum LazyRow {
    /// the frame's own, and it cannot move (a non-configurable intrinsic)
    Fixed,
    /// an interface object (an alias too), made from its template on first read; enumerable, writable
    Lazy(v8::Global<v8::String>, bool, bool),
    /// a member of `Window`'s template (`false`) or of the global's own (`true`), enumerable or not
    Templated(bool, bool),
    /// the frame's own V8 intrinsic, moved to the page's place
    Own,
    /// the page's install-time descriptor (what no template carries yet)
    Copy(v8::Global<v8::Object>),
}

const P_CTX: &str = "korvus_bctx";

fn bc<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut BrowsingContexts) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    f(st.borrow_mut::<BrowsingContexts>())
}
fn with_ctx<R>(scope: &mut v8::PinScope, id: u32, f: impl FnOnce(&mut Ctx) -> R) -> Option<R> {
    bc(scope, |b| b.ctxs.get_mut(id as usize).map(f))
}

// ── small helpers ─────────────────────────────────────────────────────────────────────────────────

/// The PAGE's window, whichever realm is running -- a frame's own functions run in the
/// frame's context, whose global is the frame's window, not "the window" every caller here means.
fn global_obj<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    main_global(scope)
}
fn name_key<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::Name> {
    v8::String::new(scope, s).unwrap().into()
}
fn js_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> String {
    v.to_rust_string_lossy(scope)
}
/// `v == null ? "" : String(v)`.
fn nullish_or_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> String {
    if v.is_null_or_undefined() { String::new() } else { js_string(scope, v) }
}
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}
fn u32_data<'s>(scope: &mut v8::PinScope<'s, '_>, n: u32) -> v8::Local<'s, v8::Value> {
    v8::Integer::new_from_unsigned(scope, n).into()
}
fn data_u32(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<u32> {
    data.uint32_value(scope)
}
/// Re-home an argument local on `scope` (a callback's `args.get(i)` carries the arguments' lifetime).
fn rescope<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let g = v8::Global::new(scope, v);
    v8::Local::new(scope, &g)
}
fn pack<'s>(scope: &mut v8::PinScope<'s, '_>, a: u32, b: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let arr = v8::Array::new(scope, 2);
    let av = u32_data(scope, a);
    arr.set_index(scope, 0, av);
    arr.set_index(scope, 1, b);
    arr.into()
}
fn unpack<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<v8::Value>) -> Option<(u32, v8::Local<'s, v8::Value>)> {
    let arr = v8::Local::<v8::Array>::try_from(data).ok()?;
    let a = arr.get_index(scope, 0)?.uint32_value(scope)?;
    let b = arr.get_index(scope, 1)?;
    Some((a, b))
}
fn pack_str<'s>(scope: &mut v8::PinScope<'s, '_>, a: u32, s: &str) -> v8::Local<'s, v8::Value> {
    let sv = str_val(scope, s);
    pack(scope, a, sv)
}
fn unpack_str(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<(u32, String)> {
    let (a, b) = unpack(scope, data)?;
    Some((a, js_string(scope, b)))
}
/// `globalThis.setTimeout(f, ms)` -- the page's own timer, as the JS used it.
fn set_timeout<'s>(scope: &mut v8::PinScope<'s, '_>, f: v8::Local<v8::Function>, ms: i32) -> Option<v8::Local<'s, v8::Value>> {
    let g: v8::Local<v8::Value> = global_obj(scope).into();
    let msv: v8::Local<v8::Value> = v8::Integer::new(scope, ms).into();
    crate::binding::protos::engine_call(scope, g, "setTimeout", &[f.into(), msv])
}
/// `new Event(ty)`, trusted: an event the engine fires (a frame's `load`, a script's `load`/`error`)
/// is `isTrusted === true` in a browser.
fn trusted_event<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let t = str_val(scope, ty);
    let ev = ctor.new_instance(scope, &[t])?;
    mark_trusted(scope, ev);
    Some(ev)
}
/// [`trusted_event`] with `bubbles` set -- `DOMContentLoaded` bubbles, so a listener on the frame's
/// window hears it.
fn trusted_event_with<'s>(scope: &mut v8::PinScope<'s, '_>, ty: &str, bubbles: bool) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let t = str_val(scope, ty);
    let init = crate::binding::protos::init_dict(scope);
    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, bubbles).into();
    set_prop(scope, init, "bubbles", b);
    let ev = ctor.new_instance(scope, &[t, init.into()])?;
    mark_trusted(scope, ev);
    Some(ev)
}

/// Fire a trusted `ty` at `target` -- the page lifecycle's events. `legacy_target` is HTML's "legacy
/// target override": fired at the Window, the event's `target` is the Document.
pub(crate) fn fire_lifecycle(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, ty: &str, bubbles: bool, legacy_target: Option<v8::Local<v8::Value>>) {
    let Some(ev) = trusted_event_with(scope, ty, bubbles) else { return };
    if let Some(t) = legacy_target {
        crate::core::events::event::evt_slot_write(scope, ev, "target", t);
        // The dispatch keeps a target the engine set this way: see `et_dispatch_inner`.
        crate::core::events::event::evt_flag_set(scope, ev, "_targetOverride", true);
    }
    let target = v8::Local::new(scope, target);
    et_dispatch(scope, target, ev);
}

/// `target.dispatchEvent(new Event(ty))` through the one dispatch algorithm.
pub(crate) fn fire_at<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Value>, ty: &str) {
    let Some(ev) = trusted_event(scope, ty) else { return };
    et_dispatch(scope, target, ev);
}
/// One component of a parsed URL, as the `URL` interface's getter of that name answers it (`modules/url/components`).
fn url_component(u: &url::Url, k: &str) -> String {
    use crate::modules::url::components as c;
    match k {
        "href" => c::href(u),
        "origin" => c::origin(u),
        "protocol" => c::protocol(u),
        "host" => c::host(u),
        "hostname" => c::hostname(u),
        "port" => c::port(u),
        "pathname" => c::pathname(u),
        "search" => c::search(u),
        "hash" => c::hash(u),
        _ => String::new(),
    }
}
fn prop_string(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str) -> Option<String> {
    // A context's URL object is read natively, never through the page-visible `URL.prototype` getters.
    if let Some(v) = crate::modules::url::url::native_component(scope, o, key) {
        return Some(v);
    }
    get_prop(scope, o, key).map(|v| js_string(scope, v))
}
/// `String(globalThis.location[k])`, or `dflt` when there is no location.
fn page_location_prop(scope: &mut v8::PinScope, k: &str, dflt: &str) -> String {
    // Read in the PAGE's context: the page Location's members resolve their receiver against the
    // current realm's prototypes, and from a frame realm `origin` answered nothing.
    // And AS the page: `location` answers the RUNNING realm's, so with a frame's script running
    // the page's origin read as the frame's -- a cross-origin frame then counted as same-origin with the page
    // and `postMessage` handed the page the frame's real Window as `source` (a challenge widget's parent dropped it).
    let prev = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<BrowsingContexts>().and_then(|b| b.current.take())
    };
    let main = main_context(scope);
    let out = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, main);
        let page = global_obj(scope);
        get_prop(scope, page, "location")
            .and_then(|l| v8::Local::<v8::Object>::try_from(l).ok())
            .and_then(|l| prop_string(scope, l, k))
            .unwrap_or_else(|| dflt.to_string())
    };
    if prev.is_some() {
        bc(scope, |b| b.current = prev);
    }
    out
}
fn tag_upper(op_state: &OpStateRc, id: u32) -> String {
    op_state.borrow().borrow::<HostDom>().0.tag_name(id).map(|t| t.to_ascii_uppercase()).unwrap_or_default()
}
/// FRAME answers alongside IFRAME because `HTMLFrameElement.contentWindow` is a Window in a real
/// browser too, and because `window.length` counts "iframe, frame" -- if the two used different
/// predicates then `window.length` and the `window[k]` set would disagree.
fn frameish_tag(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("iframe") || tag.eq_ignore_ascii_case("frame")
}
fn is_frameish(op_state: &OpStateRc, id: u32) -> bool {
    op_state.borrow().borrow::<HostDom>().0.tag_name(id).map(frameish_tag).unwrap_or(false)
}
/// Whether `id` is a frame element OR holds one anywhere in its subtree -- the test for whether a
/// childList-added node can introduce a browsing context. A frame nested inside appended markup
/// (`div.innerHTML='<iframe>'; body.appendChild(div)`, creepjs's getPhantomIframe) enters the tree as
/// a DESCENDANT of the added node, not the added node itself, so a shallow [`is_frameish`] on the
/// added node misses it and `window[i]` is never installed (the live `length` walk still counts it).
/// Same predicate as [`frames_now`]'s `holds`.
fn holds_frame(op_state: &OpStateRc, id: u32) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let frameish = |n: u32| dom.tag_name(n).map(frameish_tag).unwrap_or(false);
    frameish(id) || dom.descendant_elements(id).into_iter().any(frameish)
}
/// [`frames_under`] the page document, walked again only when the document can have gained, lost or
/// reordered a frame since the last walk: a journal entry since then inserts or removes a
/// subtree holding a frame element, replaces children around one, or is one the journal names only as
/// "other" -- or the journal dropped entries. A frame element's tag never changes and a move is a
/// removal and an insertion, so otherwise the list is the one a walk would make. Every tree mutation
/// journals itself (`korvus_dom::journal`), whichever notifier did or did not run. The walk, once a
/// page had a frame, ran on every childList mutation: ~10 % of www.atlassian.com's load.
fn frames_now(op_state: &OpStateRc, root: u32, forced: bool) -> Vec<u32> {
    use korvus_dom::journal::Change;
    let cached = op_state.borrow_mut().borrow_mut::<BrowsingContexts>().frames_at.take();
    let (instance, seq, frames) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let frameish = |n: u32| dom.tag_name(n).map(frameish_tag).unwrap_or(false);
        let holds = |n: u32| frameish(n) || dom.descendant_elements(n).into_iter().any(frameish);
        let moved = |c: &Change| match c {
            Change::Inserted { node, .. } | Change::Removed { node, .. } => holds(*node),
            Change::ChildrenReplaced { parent, old } => dom.journal().nodes(*old).iter().any(|&o| holds(o)) || holds(*parent),
            Change::Other { .. } => true,
            _ => false,
        };
        let journal = dom.journal();
        let kept = cached.filter(|(r, i, at, _)| {
            !forced && *r == root && *i == dom.instance() && journal.since(*at).is_some_and(|entries| !entries.iter().any(moved))
        });
        let frames = match kept {
            Some((_, _, _, frames)) => frames,
            None => dom.descendant_elements(root).into_iter().filter(|&n| frameish(n)).collect(),
        };
        (dom.instance(), journal.seq(), frames)
    };
    op_state.borrow_mut().borrow_mut::<BrowsingContexts>().frames_at = Some((root, instance, seq, frames.clone()));
    frames
}

/// The frame elements under `root`, in tree order -- `querySelectorAll("iframe, frame")` off the arena.
fn frames_under(op_state: &OpStateRc, root: u32) -> Vec<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    dom.descendant_elements(root).into_iter().filter(|&n| dom.tag_name(n).map(frameish_tag).unwrap_or(false)).collect()
}
/// The node ids in a JS array of nodes (a childList notification's `addedNodes`).
fn array_ids(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(arr) = v8::Local::<v8::Array>::try_from(v) else { return out };
    for i in 0..arr.length() {
        if let Some(id) = arr.get_index(scope, i).and_then(|n| unwrap_id(scope, n)) {
            out.push(id);
        }
    }
    out
}

// ── the context registry ───────────────────────────────────────────────────────────────────────────

/// The context a child window object belongs to (its private slot); `None` for the page window and
/// for anything that is not a child window.
pub(crate) fn ctx_of_window(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Option<u32> {
    // the memoised key: a string and a registry lookup (`Private::ForApi`) per call were 2.6% of a dispatch loop
    let key = crate::binding::protos::private_key(scope, P_CTX);
    obj.get_private(scope, key).filter(|v| !v.is_undefined()).and_then(|v| v.uint32_value(scope))
}
pub(crate) fn ctx_of_value(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    v8::Local::<v8::Object>::try_from(v).ok().and_then(|o| ctx_of_window(scope, o))
}
/// The context whose realm a frame's OWN member runs in: every frame's members come from
/// templates all frames share, instantiated in the frame's realm, and V8 runs a template's function in the
/// realm it was made in -- so the realm's global, which carries its context id ([`P_CTX`]), names the
/// frame the member belongs to. Each member used to carry the id as its data, which no template can.
pub(crate) fn realm_ctx(scope: &mut v8::PinScope) -> Option<u32> {
    let g = scope.get_current_context().global(scope);
    ctx_of_window(scope, g)
}
/// The page's own window, where a context id is expected: the target of a cross-origin frame's
/// `parent`/`top` view. Never an index into `ctxs`.
const MAIN_CTX: u32 = u32::MAX;

fn window_of<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    if id == MAIN_CTX {
        return Some(global_obj(scope));
    }
    let g = bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.iwin.clone()))?;
    Some(v8::Local::new(scope, &g))
}
fn doc_of<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    let g = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.idoc.clone()))?;
    Some(v8::Local::new(scope, &g))
}
fn loc_of<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    let g = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.loc.clone()))?;
    Some(v8::Local::new(scope, &g))
}
/// The context whose document `doc` is (or was: every document a context ever held stays mapped,
/// so a `write()` after `close()` still reaches the context's buffer).
pub(crate) fn context_of_document(scope: &mut v8::PinScope, doc: v8::Local<v8::Value>) -> Option<u32> {
    let root = document_node_id(scope, doc)?;
    bc(scope, |b| b.by_doc.get(&root).copied())
}
/// Make `d` the context's document: registered for the Document rows, and `window.document`.
///
/// It used to RE-INSTALL the document's own `location` accessor first, because the document arrives
/// from `DOMParser` and `dom/05-xml.js`'s facade copied its own `get location() { return null; }`
/// over the real one -- right for a created document, wrong for a frame's, whose `location` IS its
/// window's. W5.0e deleted that facade, and the accessor the mint installs already answers this
/// context's own Location (`cb_doc_location` -> `frame_document_location`), so the re-install
/// corrected nothing and cost the document its `configurable: false`: the accessor had to be left
/// redefinable for a redefinition that no longer happens. Gone, and with it the `configurable`
/// parameter -- every document now carries the unforgeable descriptor Chrome gives it.
fn set_ctx_document(scope: &mut v8::PinScope, id: u32, d: v8::Local<v8::Object>) {
    let root = document_node_id(scope, d.into());
    let g = v8::Global::new(scope, d);
    bc(scope, |b| {
        if let Some(r) = root {
            b.by_doc.insert(r, id);
        }
        if let Some(c) = b.ctxs.get_mut(id as usize) {
            c.idoc = Some(g);
            c.doc_root = root;
            c.nav_pending = false;
        }
    });
    if let Some(iwin) = window_of(scope, id) {
        set_prop(scope, iwin, "document", d.into());
    }
    // A frame realm's own `document` accessor reads its global's slot.
    if realm_installed(scope, id) {
        if let Some(realm) = realm_of(scope, id) {
            // The document is the frame realm's object: it wears that realm's `Document` prototypes.
            crate::binding::protos::rehome_in_realm(scope, realm, d);
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            crate::core::html::window_globals::window_slot_set(scope, "document", d.into());
        }
    }
}

/// The Document table's `defaultView` for a nested context's document: its window.
pub(crate) fn frame_document_window<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let id = context_of_document(scope, doc)?;
    window_of(scope, id)
}
/// The Document table's `location` for a nested context's document: its window's Location.
pub(crate) fn frame_document_location<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let id = context_of_document(scope, doc)?;
    loc_of(scope, id)
}
/// `document.location = url` on a nested context's document navigates the context
/// (`[PutForwards=href]`). `false` when `doc` is no context's.
pub(crate) fn frame_document_navigate(scope: &mut v8::PinScope, doc: v8::Local<v8::Value>, href: v8::Local<v8::Value>) -> bool {
    let Some(id) = context_of_document(scope, doc) else { return false };
    let h = js_string(scope, href);
    navigate_context(scope, id, &h);
    true
}
/// `document.open()` / `write()` / `writeln()` / `close()` on a frame document. The write buffer
/// accumulates and the tree is built once at `close()`; `close()` ENDS A NAVIGATION, so it fires the
/// frame's load event -- asynchronously, because the page installs `iframe.onload` before calling
/// `open()`/`write()`/`close()` and expects to be called back after the call returns. `None` when
/// `doc` is no context's (the row then takes its ordinary path).
pub(crate) fn frame_document_write_family<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<v8::Value>, name: &str, args: &v8::FunctionCallbackArguments) -> Option<v8::Local<'s, v8::Value>> {
    let id = context_of_document(scope, doc)?;
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    match name {
        "open" => {
            // HTML's document open steps bail out, with no side effects at all, while the document's own parser
            // is running one of its scripts or after `window.stop()` aborted that parser (WPT
            // `opening-the-input-stream/aborted-parser.window.js`, `bailout-side-effects-synchronous-script`).
            if with_ctx(scope, id, |c| c.parser_nesting > 0 || c.parser_aborted).unwrap_or(false) {
                return Some(doc_of(scope, id).map(|d| d.into()).unwrap_or(undef));
            }
            frame_document_open(scope, id);
            Some(doc_of(scope, id).map(|d| d.into()).unwrap_or(undef))
        }
        "write" | "writeln" => {
            // Blink's overloads: a TrustedHTML first is read as itself, nothing on it called
            // (a throw is this frame's: answered here, not handed on to the page document's write)
            let Some(mut s) = crate::core::dom::document::write_args_text(scope, args, name) else { return Some(undef) };
            if name == "writeln" {
                s.push('\n');
            }
            // No open() before it, and no script of the document's own parser running (the insertion point is
            // undefined): the document open steps run first -- the SAME Document, emptied -- or, after its parser
            // was aborted, nothing at all (HTML's document.write steps 4-5). Chrome 150: `d.write(...)` from the
            // parent keeps `d === iframe.contentDocument`, `readyState` reads `loading`, the body's computed style
            // is real. It used to buffer the markup and load it at close() as a NEW document, so an interactive-widget challenge
            // challenge frame's `getComputedStyle` of the body it had just written read "" for all 400 properties.
            match with_ctx(scope, id, |c| (c.written_el.is_some(), c.parser_nesting > 0, c.parser_aborted)) {
                Some((false, false, true)) => return Some(undef),
                Some((false, false, false)) => frame_document_open(scope, id),
                // One of the document's own parser scripts writing: the markup goes in at the insertion point --
                // after the script, or after what it wrote before -- and its scripts run as they arrive (HTML's
                // document.write steps 6-7). It was buffered for a close() that never came: Naver's ad frames
                // write their creative from `gfp-display-safeframe.js`, and stayed empty and 0px tall.
                Some((false, true, false)) => {
                    frame_write_at_script(scope, id, &s);
                    return Some(undef);
                }
                _ => {}
            }
            let (written, opened) = with_ctx(scope, id, |c| {
                let mut w = c.writing.take().unwrap_or_default();
                w.push_str(&s);
                c.writing = Some(w.clone());
                (w, c.written_el.is_some())
            })
            .unwrap_or_default();
            // After an explicit open(): what has been written is the document's content NOW, parsed into the
            // same Document (its scripts run at close()).
            if opened {
                if let Some(tmp) = parse_frame_doc(scope, id, &written) {
                    let from = document_node_id(scope, tmp.into());
                    let el = frame_replace_children(scope, id, from);
                    // The document's mode is the written markup's doctype verdict (Chrome 150 / Brave 151,
                    // measured 2026-10-05: `open(); write('<p>x')` is `BackCompat`, with `<!DOCTYPE html>`
                    // first `CSS1Compat`), not whatever the document was before `open()`.
                    if let (Some(src), Some(root)) = (from, frame_document_root(scope, id)) {
                        let op_state = deno_core::JsRuntime::op_state_from(scope);
                        let mut st = op_state.borrow_mut();
                        let dom = &mut st.borrow_mut::<HostDom>().0;
                        let mode = dom.document_mode(src);
                        dom.set_document_mode(root, mode);
                    }
                    with_ctx(scope, id, |c| c.written_el = Some(el));
                }
            }
            Some(undef)
        }
        "close" => {
            let (html, written_el) = with_ctx(scope, id, |c| (c.writing.take().unwrap_or_default(), c.written_el.take())).unwrap_or_default();
            match written_el {
                // Opened: the content is in place. A document script rebuilt since the last write runs the load
                // steps and none of the scripts it now holds (html2canvas's clone carries the page's). The
                // document's end runs before close() returns (Chrome 150: `interactive`, DOMContentLoaded,
                // `complete`, the window's load, the frame element's -- all inside the call); the document keeps
                // its clock and timing, it was never navigated.
                Some(el) => {
                    let now = frame_document_root(scope, id).and_then(|r| {
                        let op_state = deno_core::JsRuntime::op_state_from(scope);
                        let st = op_state.borrow();
                        let dom = &st.borrow::<HostDom>().0;
                        dom.children(r).into_iter().find(|&c| dom.node_type(c) == 1)
                    });
                    with_ctx(scope, id, |c| {
                        c.skip_doc_scripts = now != el;
                        c.current_script = None;
                        c.end_in_close = true;
                    });
                    run_doc_scripts(scope, id);
                }
                // No script-created parser: close() does nothing (Chrome 150: the same document, no load). It
                // loaded what had been buffered -- nothing -- as a new document.
                None => drop(html),
            }
            Some(undef)
        }
        _ => None,
    }
}

/// A `document.write` of `markup` from the parser script running in context `id`'s document: parsed in the script's
/// parent, inserted after the script (or after what the script wrote before it), and connected -- so its scripts
/// run, an external one when it has loaded, as a written script would after the writing one returns.
fn frame_write_at_script(scope: &mut v8::PinScope, id: u32, markup: &str) {
    if markup.is_empty() {
        return;
    }
    let Some(script) = with_ctx(scope, id, |c| c.current_script.clone()).flatten() else { return };
    let script = v8::Local::new(scope, &script);
    let Some(script_id) = crate::binding::protos::unwrap_id(scope, script) else { return };
    let previous = with_ctx(scope, id, |c| c.write_anchor).flatten().filter(|(s, _)| *s == script_id).map(|(_, after)| after);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (parent, ids) = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        // a script removed from the tree by the time it writes has no insertion point
        let anchor = match previous {
            Some(a) if dom.parent(a).is_some() => a,
            _ => script_id,
        };
        let Some(parent) = dom.parent(anchor) else { return };
        let ids = dom.parse_fragment_in(parent, markup);
        let mut after = anchor;
        for &n in &ids {
            match dom.next_sibling(after) {
                Some(next) => dom.insert_before(parent, n, next),
                None => dom.append_child(parent, n),
            }
            after = n;
        }
        (parent, ids)
    };
    if let Some(&last) = ids.last() {
        with_ctx(scope, id, |c| c.write_anchor = Some((script_id, last)));
        crate::core::dom::node::notify_childlist_batch(scope, &op_state, parent, &ids, &[]);
    }
}

/// HTML's document open steps for context `id`'s document (after the bail-outs): its listeners erased, the SAME
/// Document emptied for a script-created parser (html2canvas holds `iframe.contentWindow.document`
/// and puts its clone into it), `loading`, and the URL of the document whose script opened it -- the entry
/// document -- when that is another document (step 12; Chrome 150: a blank frame opened from the page reads the
/// page's URL in `document.URL`, `location.href` and `baseURI`).
fn frame_document_open(scope: &mut v8::PinScope, id: u32) {
    erase_listeners_and_handlers(scope, id);
    with_ctx(scope, id, |c| {
        c.writing = Some(String::new());
        c.written_el = Some(None);
        c.readiness = Readiness::Loading;
        // Blink's `ImplicitOpen`: the load event may run again -- unless it is running now (an `open()` from a `load`
        // handler), which `frame_loaded_now` keeps from firing twice
        if c.load_event != FrameLoadEvent::InProgress {
            c.load_event = FrameLoadEvent::NotRun;
        }
        c.delays.clear();
        c.load_parked = false;
    });
    frame_replace_children(scope, id, None);
    let entry = running(scope).unwrap_or(MAIN_CTX);
    if entry != id {
        let href = loc_href(scope, entry);
        if !href.is_empty() && href != loc_href(scope, id) {
            set_location(scope, id, &href);
        }
    }
}

/// `window.stop()` on a frame's window: while the frame document's parser is running its scripts,
/// it is aborted -- the only part of stopping this engine models, and what the document open steps read back.
pub(crate) fn frame_window_stop(scope: &mut v8::PinScope, win: v8::Local<v8::Object>) {
    let Some(id) = ctx_of_window(scope, win) else { return };
    let parsing = bc(scope, |b| b.runs.values().any(|r| r.ctx == id));
    if parsing {
        with_ctx(scope, id, |c| c.parser_aborted = true);
    }
}

/// Replace every child of context `id`'s document with the children of the document rooted at `from` (none
/// for `open()`), moved in the arena -- no insertion steps, so a moved `<script>` does not run until `close()`
/// runs the document's scripts. Answers the document element the document now has.
fn frame_replace_children(scope: &mut v8::PinScope, id: u32, from: Option<u32>) -> Option<u32> {
    let root = frame_document_root(scope, id)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let dom = &mut st.borrow_mut::<HostDom>().0;
    for c in dom.children(root) {
        dom.remove_child(root, c);
    }
    if let Some(src) = from {
        for c in dom.children(src) {
            dom.remove_child(src, c);
            dom.append_child(root, c);
        }
    }
    dom.children(root).into_iter().find(|&c| dom.node_type(c) == 1)
}

/// HTML's document open steps for context `id`'s document: "erase all event listeners and handlers given"
/// every shadow-including inclusive descendant of the document and -- the document being its window's --
/// given the window (`event-listeners.window.js`: `body.onfocus` is null after `open()`).
fn erase_listeners_and_handlers(scope: &mut v8::PinScope, id: u32) {
    if let Some(w) = window_of(scope, id) {
        crate::core::events::event_target::et_forget(scope, w.into());
    }
    let Some(root) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.doc_root)) else { return };
    let ids: Vec<u32> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            out.push(n);
            if let Some(sr) = dom.shadow_root_of(n) {
                stack.push(sr);
            }
            stack.extend(dom.children(n));
        }
        out
    };
    for n in ids {
        crate::core::events::event_target::et_forget_node(scope, n);
    }
}

// ── which realm is running ─────────────────────────────────────────────────────────────────────────

fn enter_realm(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    bc(scope, |b| std::mem::replace(&mut b.current, Some(id)))
}
/// An element load is its NODE DOCUMENT's request, also when no script of that document is running: a
/// frame document's `<img>` the parser met, which the page's realm starts. Runs the frame's context as the running
/// one until [`leave_element_context`]; `None` (nothing entered) for an element of the page's document or of none.
pub(crate) fn enter_element_context(scope: &mut v8::PinScope, el: u32) -> Option<Option<u32>> {
    let id = context_of_element(scope, el)?;
    Some(enter_realm(scope, id))
}
pub(crate) fn leave_element_context(scope: &mut v8::PinScope, entered: Option<Option<u32>>) {
    if let Some(prev) = entered {
        leave_realm(scope, prev);
    }
}
fn leave_realm(scope: &mut v8::PinScope, prev: Option<u32>) {
    bc(scope, |b| b.current = prev);
}

/// Run `f` as the realm of the browsing context whose V8 context created `callback`: a
/// frame's event listener, called from the page's dispatch, belongs to the frame -- a bare
/// `addEventListener` inside it is the frame's, and so is its uncaught error. Anything else runs as
/// whatever realm is running already.
pub(crate) fn as_callback_realm<'s, R>(scope: &mut v8::PinScope<'s, '_>, callback: v8::Local<v8::Function>, f: impl FnOnce(&mut v8::PinScope<'s, '_>) -> R) -> R {
    let frame = {
        let has = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<BrowsingContexts>().is_some_and(|b| !b.ctxs.is_empty())
        };
        if has {
            callback.get_creation_context(scope).and_then(|c| {
                let g = c.global(scope);
                ctx_of_window(scope, g)
            })
        } else {
            None
        }
    };
    let Some(id) = frame else { return f(scope) };
    let prev = enter_realm(scope, id);
    let r = f(scope);
    leave_realm(scope, prev);
    r
}
/// The window whose script is running: a child window, or `None` for the page's own realm.
pub(crate) fn current_realm_window<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let cur = running(scope)?;
    window_of(scope, cur)
}
/// [`current_realm_window`] for a caller that may run in a realm with no browsing contexts at all (a
/// worker): `core/events/report.rs` dispatches an uncaught error at the running frame's window.
pub(crate) fn running_frame_window<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let cur = running(scope)?;
    window_of(scope, cur)
}

/// The browsing context whose realm is running: the one this module entered (its script, its timer
/// or microtask callback), else the one whose V8 context is entered or whose microtask is running --
/// a frame's promise continuation, a frame listener. `None` is the page's own realm, and
/// the answer in a realm with no browsing contexts at all (a worker).
/// Whether this realm holds a document at all (a window realm), as against a worker's.
pub(crate) fn has_document(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<HostDom>().is_some()
}

/// The nested context whose realm is the CURRENT one -- the realm of the function being called, not of the
/// script that called it (`None` = the page's realm, or a realm with no browsing contexts).
/// Is the running realm's document a frame CROSS-ORIGIN to the top-level document (Blink's
/// `!frame->IsSameOrigin()`)? A remote frame is one by construction.
pub(crate) fn realm_cross_origin_to_top(scope: &mut v8::PinScope) -> bool {
    if let Some(id) = current_realm_ctx(scope) {
        return document_origin(scope, Some(id)) != document_origin(scope, None);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::core::html::remote_frame::embedding(&st).is_some()
}

pub(crate) fn current_realm_ctx(scope: &mut v8::PinScope) -> Option<u32> {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if st.try_borrow::<BrowsingContexts>().map_or(true, |b| b.ctxs.is_empty()) {
            return None;
        }
    }
    let ctx = scope.get_current_context();
    let g = ctx.global(scope);
    ctx_of_window(scope, g).filter(|&id| id != MAIN_CTX)
}

/// The navigation response timing of the FRAME whose realm is current, for its own
/// `PerformanceNavigationTiming` -- `None` in the page's realm (whose document's timing is the
/// isolate's `NavTiming`), `Some(None)` for a frame no navigation fetched (`about:blank`, `srcdoc`).
/// A frame's entry used to report the PAGE's response: a bot-challenge frame read
/// haveibeenpwned.com's sizes for its own document (korvus-analysis U-5).
pub(crate) fn realm_frame_nav_timing(scope: &mut v8::PinScope) -> Option<Option<spec::FetchTiming>> {
    let id = realm_ctx(scope)?;
    Some(with_ctx(scope, id, |c| c.delivered.timing.clone()).flatten())
}

pub(crate) fn running(scope: &mut v8::PinScope) -> Option<u32> {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let b = st.try_borrow::<BrowsingContexts>()?;
        if b.current.is_some() || b.ctxs.is_empty() {
            return b.current;
        }
    }
    let ctx = scope.get_entered_or_microtask_context();
    let g = ctx.global(scope);
    ctx_of_window(scope, g)
}
fn realm_or_global<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    match current_realm_window(scope) {
        Some(w) => w.into(),
        None => global_obj(scope).into(),
    }
}

/// The href a context's Location reads: the parsed URL's `href` when it parsed, else the raw string.
fn loc_href(scope: &mut v8::PinScope, id: u32) -> String {
    if id == MAIN_CTX {
        return page_location_prop(scope, "href", "");
    }
    let (raw, url) = bc(scope, |b| b.ctxs.get(id as usize).map(|c| (c.raw.clone(), c.url.clone()))).unwrap_or_default();
    match url {
        Some(u) => url_component(&u, "href"),
        None => raw,
    }
}
/// The origin a message sent from `win` carries. A context still on `about:blank`/`about:srcdoc`
/// inherits the page's origin, which is what the spec's "inherit the origin" rule says for both.
pub(crate) fn realm_origin(scope: &mut v8::PinScope, win: v8::Local<v8::Value>) -> String {
    if realm_is_sandboxed(scope, win) {
        return "null".to_string();
    }
    let h = match ctx_of_value(scope, win) {
        Some(id) => loc_href(scope, id),
        None => page_location_prop(scope, "href", ""),
    };
    if h.is_empty() || starts_with_ci(&h, "about:") {
        // Its CREATOR's origin, which is the page's only for a frame the page made.
        return match ctx_of_value(scope, win) {
            Some(id) => document_origin(scope, Some(id)),
            None => page_location_prop(scope, "origin", "null"),
        };
    }
    url_origin(&h).unwrap_or_else(|| "null".to_string())
}
/// The origin of the document whose arena root is `root`: the origin of the frame that shows it (an `about:` document's
/// is its creator's), or the page's for one no frame shows (`document.domain`).
pub(crate) fn doc_root_origin(scope: &mut v8::PinScope, root: u32) -> String {
    let id = bc(scope, |b| {
        let id = *b.by_doc.get(&root)?;
        b.ctxs.get(id as usize).filter(|c| c.doc_root == Some(root)).map(|_| id)
    });
    document_origin(scope, id)
}

/// The origin an `about:blank` / `about:srcdoc` document of context `id` INHERITS: its creator's --
/// the document holding its frame element, walked up while that is `about:` too; the page's for a
/// frame the page made, and for a popup. It was always the PAGE's: a cross-origin
/// frame's own blank child was cross-origin to it, so `a.contentDocument` read null inside the frame
/// and the child's `parent.eval` threw SecurityError -- what a challenge provider a challenge widget's challenge frame
/// does (seek, apmex, tanitjobs, ichancy200, poliisi; all challenged). Chrome 150, measured
/// (`tools/realweb/seed/nab`): both children's `parent` is same-origin, `self.origin` is the frame's.
fn inherited_origin(scope: &mut v8::PinScope, id: u32) -> String {
    match creator_ctx(scope, id) {
        Some(p) => document_origin(scope, Some(p)),
        None => page_location_prop(scope, "origin", "null"),
    }
}

/// The context whose document created context `id`'s `about:` document -- the one holding its frame
/// element (`None`: the page, or no parent at all, a popup).
fn creator_ctx(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    frame_parent_ctx(scope, id).filter(|&p| p != MAIN_CTX)
}

/// The ORIGIN OF A CONTEXT'S DOCUMENT (`None` = the page's own), for the "same origin-domain" test
/// `contentDocument` makes. Not [`realm_origin`], which answers `event.origin` and serialises what
/// `new URL(href).origin` does: that is `"null"` for a `blob:null/<uuid>` an opaque-origin page
/// mints, and a frame navigated to its own page's Blob is SAME-origin -- a blob URL's origin is its
/// creator's. Measured on Chrome 150: `iframe.src = URL.createObjectURL(blob)` leaves
/// `contentDocument` readable; a frame on another port of the same host answers `null`.
///
/// * `about:blank` / `about:srcdoc` and an empty URL inherit the page's origin (the existing rule).
/// * `blob:<origin>/<uuid>` is `<origin>`; a `blob:` whose inner part is not a tuple origin was
///   minted by this realm and is the page's.
/// * everything else is its URL's origin (`data:` is opaque: `"null"`, cross-origin to everyone).
fn document_origin(scope: &mut v8::PinScope, ctx: Option<u32>) -> String {
    let page = page_location_prop(scope, "origin", "null");
    let Some(id) = ctx.filter(|&c| c != MAIN_CTX) else { return page };
    // A sandboxed document's origin is opaque: its own, same-origin with nothing else.
    if let Some(n) = with_ctx(scope, id, |c| c.opaque).flatten() {
        return format!("null#{n}");
    }
    let h = loc_href(scope, id);
    if h.is_empty() || starts_with_ci(&h, "about:") {
        return inherited_origin(scope, id);
    }
    if starts_with_ci(&h, "blob:") {
        return match url::Url::parse(&h["blob:".len()..]).ok().map(|u| u.origin()) {
            Some(o) if o.is_tuple() => o.ascii_serialization(),
            _ => page,
        };
    }
    match url::Url::parse(&h) {
        Ok(u) => u.origin().ascii_serialization(),
        Err(_) => "null".to_string(),
    }
}
/// `new URL(href).origin`, parsed natively (no page-visible `URL` is constructed or read).
fn url_origin(href: &str) -> Option<String> {
    url::Url::parse(href).ok().map(|u| url_component(&u, "origin"))
}
/// The base URL a child context resolves its own relative URLs against. `about:blank`/`about:srcdoc`
/// inherit their creator's BASE URL, exactly as the spec's "about base URL" rule requires -- the
/// page's `<base>` included (with `<base href=/sub/>` on the parent, Chrome 150 requests
/// `/sub/bb.js` for `bb.js` appended to an initial `about:blank` frame).
fn ctx_base(scope: &mut v8::PinScope, id: u32) -> String {
    let h = loc_href(scope, id);
    if h.is_empty() || starts_with_ci(&h, "about:") {
        return frame_request_client(scope, id).1;
    }
    h
}

/// Context `id`'s about base URL ([`Ctx::about_base`]), when it has one.
pub(crate) fn about_base(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    with_ctx(scope, id, |c| c.about_base.clone()).flatten()
}

/// The document base URL of context `id`'s creator, now: its parent frame's document base, or the page's.
fn creator_base(scope: &mut v8::PinScope, id: u32) -> String {
    match frame_parent_ctx(scope, id) {
        Some(p) if p != MAIN_CTX => frame_request_client(scope, p).1,
        _ => crate::core::dom::base_url(scope),
    }
}

/// Take context `id`'s about base URL from its creator now (the context was made, or a `srcdoc` navigation began).
fn note_about_base(scope: &mut v8::PinScope, id: u32) {
    let b = creator_base(scope, id);
    with_ctx(scope, id, |c| c.about_base = Some(b));
}

// ── What `core/html/subresource.rs` needs to load into a nested document ─────────────

/// The nested context whose document has arena root `root`, if any.
pub(crate) fn frame_of_document_root(scope: &mut v8::PinScope, root: u32) -> Option<u32> {
    bc(scope, |b| b.by_doc.get(&root).copied())
}

/// Is the nested context `id` still shown by a CONNECTED frame element -- its document "fully active"?
/// A frame removed from the document keeps its context object here (a page may still hold its
/// `contentDocument`), and the Storage Access API rejects on such a document with `InvalidStateError`
/// (WPT `storage-access-api/requestStorageAccess-non-fully-active`). An auxiliary context has no frame
/// element and is fully active while it exists.
pub(crate) fn context_fully_active(scope: &mut v8::PinScope, id: u32) -> bool {
    // A frame element inside ANOTHER frame's document is connected to that document, and the outer
    // frame has to be fully active in its turn -- up to the page. Bounded by the context count.
    let mut id = id;
    for _ in 0..=bc(scope, |b| b.ctxs.len()) {
        let Some(el) = bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.el)) else { return false };
        let Some(el) = el else { return true };
        let (root, page_root, is_doc) = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let Some(h) = st.try_borrow::<HostDom>() else { return false };
            let root = h.0.shadow_including_root(el);
            (root, h.0.root(), h.0.node_type(root) == 9)
        };
        if root == page_root {
            return true;
        }
        if !is_doc {
            return false;
        }
        match frame_of_document_root(scope, root) {
            Some(outer) => id = outer,
            None => return false,
        }
    }
    false
}
/// Is the document of `obj`'s realm fully active -- `obj`'s relevant global a Window whose associated
/// Document is ([`context_fully_active`] for a frame's realm; the page's always is)?
pub(crate) fn object_realm_fully_active(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> bool {
    let Some(ctx) = obj.get_creation_context(scope) else { return true };
    let n = bc(scope, |b| b.ctxs.len());
    for id in 0..n as u32 {
        if id == MAIN_CTX {
            continue;
        }
        if realm_of(scope, id).is_some_and(|r| r == ctx) {
            return context_fully_active(scope, id);
        }
    }
    true
}

/// The URL of the realm whose code is running, for the call recorder's realm record:
/// a worker's own script URL, a nested context's current URL, else the page's. Never a borrow that could fail
/// inside a callback.
#[cfg(feature = "analysis")]
pub(crate) fn realm_url_for_analysis(scope: &mut v8::PinScope) -> String {
    // a worker realm: its own script's URL
    let w = crate::core::workers::worker_global_scope::worker_href_for_analysis(scope);
    if !w.is_empty() {
        return w;
    }
    let ctx = scope.get_current_context();
    let g = ctx.global(scope);
    match ctx_of_window(scope, g) {
        Some(id) if id != MAIN_CTX => frame_context_url(scope, id),
        _ if main_global(scope) == g => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let Ok(st) = op_state.try_borrow() else { return String::new() };
            st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default()
        }
        // a frame's realm before its window carries its context id: not known yet (the recorder asks
        // again on a later call)
        _ => String::new(),
    }
}

/// The context's current URL (`about:blank` for an initial empty document).
pub(crate) fn frame_context_url(scope: &mut v8::PinScope, id: u32) -> String {
    loc_href(scope, id)
}

/// The running window realm's URL, and the URL whose SITE names the renderer process Chrome runs that realm
/// in under site isolation: a nested context's own; an `about:` one's creator's, walked up; the page's for the
/// page's realm. Read off engine state -- no page-visible member runs. For a realm with no browsing context of
/// its own yet, the page's.
pub(crate) fn realm_process_urls(scope: &mut v8::PinScope) -> (String, String) {
    let page = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let Ok(st) = op_state.try_borrow() else { return (String::new(), String::new()) };
        st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default()
    };
    let ctx = scope.get_current_context();
    let g = ctx.global(scope);
    let Some(id) = ctx_of_window(scope, g).filter(|&id| id != MAIN_CTX) else { return (page.clone(), page) };
    let own = loc_href(scope, id);
    let mut site = page;
    let mut cur = Some(id);
    for _ in 0..32 {
        let Some(c) = cur else { break };
        let h = loc_href(scope, c);
        if !h.is_empty() && !starts_with_ci(&h, "about:") {
            site = h;
            break;
        }
        cur = creator_ctx(scope, c);
    }
    (own, site)
}
/// The V8 context of the running realm's LOCAL ROOT, as Chrome's site isolation makes one:
/// the nearest frame, from the running realm's up, whose document is cross-site to its parent's -- an
/// out-of-process frame, which renders on a widget of its own -- else the page's. A same-site frame renders with
/// its parent: Chrome 150 reports an 80 ms rendering task of a same-origin child of a cross-site frame as a
/// `long-animation-frame` of the cross-site frame, and none of the child's. The running realm itself outside a
/// frame tree (the page, a worker).
pub(crate) fn realm_local_root<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Context> {
    let Some(mut cur) = current_realm_ctx(scope) else { return scope.get_current_context() };
    for _ in 0..32 {
        // a popup, or a frame whose element is in no document: a root of its own
        let Some(parent) = frame_parent_ctx(scope, cur) else { break };
        let up = (parent != MAIN_CTX).then_some(parent);
        if ctx_site(scope, Some(cur)) != ctx_site(scope, up) {
            break;
        }
        match up {
            Some(p) => cur = p,
            None => return main_context(scope),
        }
    }
    realm_of(scope, cur).unwrap_or_else(|| scope.get_current_context())
}

/// The site (`scheme://registrable-domain`) of context `ctx`'s document (`None`: the page's) -- an `about:`
/// document's is its creator's.
fn ctx_site(scope: &mut v8::PinScope, ctx: Option<u32>) -> String {
    let origin = document_origin(scope, ctx);
    url::Url::parse(&origin).ok().and_then(|u| transport::schemeful_site(&u)).unwrap_or(origin)
}

/// The nested context whose script is running (`None` in the page's own realm, and in a realm with no
/// browsing contexts at all -- a worker).
pub(crate) fn running_frame(scope: &mut v8::PinScope) -> Option<u32> {
    running(scope)
}

/// A request a nested context makes: `(the client's URL, the base a relative URL resolves against)`.
///
/// The client URL is what the request's initiator is -- `Referer`, `Origin` and `Sec-Fetch-Site` are
/// computed from it -- and it is the FRAME DOCUMENT'S, measured on Chrome 150.0.7871.46 over loopback
/// (`fetch` from inside each frame):
///
/// ```text
/// <iframe src=/sub/f.html>, fetch same-origin    Referer http://127.0.0.1/sub/f.html   (not the page's)
/// <iframe src=//localhost/sub/f.html>, fetch 127.0.0.1
///                                                Origin http://localhost  Sec-Fetch-Site cross-site
/// <iframe srcdoc>, fetch same-origin             Referer http://127.0.0.1/req.html     (its creator's)
/// ```
///
/// An `about:blank`/`about:srcdoc` document's URL is not a referrer; its client is its creator's,
/// the page's, which is the same inheritance [`realm_origin`] applies. The base is the frame
/// document's own `<base href>` when it has one, else its URL (for an `about:` frame, the page's base).
pub(crate) fn frame_request_client(scope: &mut v8::PinScope, id: u32) -> (String, String) {
    let href = loc_href(scope, id);
    // The document's fallback base: its URL, or for an about:blank / about:srcdoc document its about base URL --
    // the creator's base when the context was made (a cross-origin frame's blank child requests as
    // that frame, not as the page) -- and its client the creator's.
    let (client, fallback) = if href.is_empty() || starts_with_ci(&href, "about:") {
        let snapshot = about_base(scope, id);
        match creator_ctx(scope, id) {
            Some(p) => {
                let (client, base) = frame_request_client(scope, p);
                (client, snapshot.unwrap_or(base))
            }
            None => {
                let page = page_location_prop(scope, "href", "");
                (page, snapshot.unwrap_or_else(|| crate::core::dom::base_url(scope)))
            }
        }
    } else {
        (href.clone(), href.clone())
    };
    // ... then its own `<base href>`, resolved against that (an about:srcdoc document's `<base>` is its own too:
    // Chrome 150 resolves a blank child of `<base href="https://x/dir/">` against `https://x/dir/`)
    let root = frame_document_root(scope, id);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let base = match (root, st.try_borrow::<HostDom>()) {
        (Some(root), Some(host)) => {
            let dom = &host.0;
            dom.get_elements_by_tag_name_in(root, "base")
                .iter()
                .find_map(|b| dom.get_attribute(*b, "href"))
                .and_then(|h| url::Url::parse(&fallback).ok().and_then(|u| u.join(&h).ok()))
                .map(|u| u.to_string())
                .unwrap_or_else(|| fallback.clone())
        }
        _ => fallback.clone(),
    };
    (client, base)
}
/// The base URL a relative URL handed to a Fetch-family API resolves against in the RUNNING realm: the
/// running frame's document base, or the page's. `fetch` asked the running frame
/// already; `XMLHttpRequest` and `new Request` read the page's base whatever realm called them, so a
/// frame's XHR to `/youtubei/v1/log_event` went to the embedding page's host (dev.to, parity replay).
pub(crate) fn realm_base_url(scope: &mut v8::PinScope) -> String {
    match running(scope) {
        Some(id) => frame_request_client(scope, id).1,
        None => crate::core::dom::base_url(scope),
    }
}

/// The client a request made in the running realm goes out as: the running frame's document URL, or
/// `""` for the page's own (what `op_fetch` reads as "this realm's own").
/// The SITE FOR COOKIES of the running realm's document, as a request carries it to the loader: `""`
/// for the page's own realm (the loader reads the page's URL), the top-level document's URL for a frame whose
/// chain up to it is all same-site with it, and `"null"` for a frame under a cross-site ancestor -- every request
/// it makes is then cross-site, and sends only `SameSite=None` cookies.
pub(crate) fn realm_site_for_cookies(scope: &mut v8::PinScope) -> String {
    match running(scope) {
        Some(id) => ctx_site_for_cookies(scope, id).unwrap_or_else(|| "null".to_string()),
        None => String::new(),
    }
}

/// [`realm_site_for_cookies`] of context `id`: `None` when some context from it up to its top-level one is not
/// same-site with that top-level document (the chain F34's `frame_doc_cookie_context` walks).
fn ctx_site_for_cookies(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    let mut chain = Vec::new();
    let mut cur = id;
    let top = loop {
        if cur == MAIN_CTX {
            break page_location_prop(scope, "href", "");
        }
        if bc(scope, |b| b.ctxs.get(cur as usize).map(|c| c.popup)).unwrap_or(true) {
            break frame_request_client(scope, cur).0;
        }
        chain.push(cur);
        match frame_parent_ctx(scope, cur) {
            Some(p) if chain.len() < 64 => cur = p,
            _ => break page_location_prop(scope, "href", ""),
        }
    };
    let top_url = url::Url::parse(&top).ok()?;
    for c in chain {
        // an opaque origin (a sandboxed document) is first-party with no site: every request under it is cross-site
        // (Chromium's `ComputeSiteForCookies`)
        if with_ctx(scope, c, |x| x.opaque.is_some()).unwrap_or(false) {
            return None;
        }
        let u = url::Url::parse(&frame_request_client(scope, c).0).ok()?;
        if !transport::same_site(&top_url, &u) {
            return None;
        }
    }
    Some(top)
}

pub(crate) fn realm_client(scope: &mut v8::PinScope) -> String {
    match running(scope) {
        Some(id) => request_client(scope, id),
        None => String::new(),
    }
}

/// The client a `fetch` / XHR / beacon of frame `id` names to the runtime: [`frame_request_client`]'s, marked
/// [`crate::OPAQUE_CLIENT`] when the frame's origin is opaque (a sandboxed document without `allow-same-origin`), whose
/// requests carry `Origin: null` (Chrome 150, measured).
pub(crate) fn request_client(scope: &mut v8::PinScope, id: u32) -> String {
    let client = frame_request_client(scope, id).0;
    if with_ctx(scope, id, |c| c.opaque.is_some()).unwrap_or(false) {
        format!("{}{client}", crate::OPAQUE_CLIENT)
    } else {
        client
    }
}

/// The frame ELEMENT showing the document whose arena root is `root` -- `None` when no frame is showing
/// it (a page's own document, a `DOMParser` document, a popup's, or one a frame has navigated away
/// from). What lays a frame's document out into that element's content box.
pub(crate) fn frame_element_showing(state: &OpState, root: u32) -> Option<u32> {
    let b = state.try_borrow::<BrowsingContexts>()?;
    let c = b.ctxs.get(*b.by_doc.get(&root)? as usize)?;
    if c.doc_root != Some(root) {
        return None;
    }
    c.el
}

/// Is `root` the current document of a popup this process shows (its viewport is a window's, not a frame element's)?
pub(crate) fn popup_document(state: &OpState, root: u32) -> bool {
    let Some(b) = state.try_borrow::<BrowsingContexts>() else { return false };
    b.by_doc.get(&root).and_then(|c| b.ctxs.get(*c as usize)).is_some_and(|c| c.popup && c.el.is_none() && c.doc_root == Some(root))
}

/// The arena root of popup `id`'s current document.
pub fn popup_document_root(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    bc(scope, |b| b.ctxs.get(id as usize).filter(|c| c.popup).and_then(|c| c.doc_root))
}

/// The arena root of the document frame element `el` shows in this process -- `None` for an element that is no
/// frame, a frame with no document yet, or one whose document lives in another process (`KORVUS_OOPIF`). The mouse
/// goes into this document when the pointer is over the frame (FIXER C6: Blink's `GetTargetSubframe`).
pub(crate) fn frame_content_root(state: &OpState, el: u32) -> Option<u32> {
    let b = state.try_borrow::<BrowsingContexts>()?;
    let c = b.ctxs.get(*b.by_frame.get(&el)? as usize)?;
    if c.el != Some(el) {
        return None;
    }
    c.doc_root
}

/// The frame element and current document root of context `id`.
pub(crate) fn frame_parts(state: &OpState, id: u32) -> Option<(u32, u32)> {
    let c = state.try_borrow::<BrowsingContexts>()?.ctxs.get(id as usize)?;
    Some((c.el?, c.doc_root?))
}

/// The arena root of the context's current document.
/// The document a realm's global reads its named properties from: a frame realm's own document root
/// (`None` before it has one), else the page's (`None` here, the caller takes `HostDom::root`).
pub(crate) fn realm_frame_document_root(scope: &mut v8::PinScope, global: v8::Local<v8::Object>) -> Option<Option<u32>> {
    match ctx_of_window(scope, global) {
        Some(id) if id != MAIN_CTX => Some(frame_document_root(scope, id)),
        _ => None,
    }
}
pub(crate) fn frame_document_root(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    let d = doc_of(scope, id)?;
    document_node_id(scope, d.into())
}

/// What frame `id`'s current document's response delivered (`Ctx::delivered`). `None` for the page.
pub(crate) fn frame_delivered(scope: &mut v8::PinScope, id: u32) -> Option<FrameDelivered> {
    if id == MAIN_CTX {
        return None;
    }
    bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.delivered.clone()))
}

/// The browsing context frame `id` is nested in, as `core/html/referrer_policy.rs` and the permissions
/// policy name it: `Some(None)` the page, `Some(Some(ctx))` another frame, `None` no parent (a popup).
pub(crate) fn frame_parent(scope: &mut v8::PinScope, id: u32) -> Option<Option<u32>> {
    frame_parent_ctx(scope, id).map(|p| (p != MAIN_CTX).then_some(p))
}

/// The referrer policy of frame `id`'s document: its `<meta name=referrer>`, else its
/// response's `Referrer-Policy`. An `about:` document has no response: it inherits its creator's policy,
/// as it inherits its creator's client ([`frame_request_client`]).
pub(crate) fn frame_document_referrer_policy(scope: &mut v8::PinScope, id: u32) -> Option<transport::ReferrerPolicy> {
    let meta = frame_document_root(scope, id).and_then(|root| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().and_then(|h| crate::core::html::referrer_policy::meta_policy(&h.0, root))
    });
    if meta.is_some() {
        return meta;
    }
    let href = loc_href(scope, id);
    if href.is_empty() || starts_with_ci(&href, "about:") {
        let parent = frame_parent(scope, id).flatten();
        return crate::core::html::referrer_policy::context_policy(scope, parent);
    }
    bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.delivered.referrer_policy))
}

/// The policy frame `id`'s OWN navigation request -- and so its document's `referrer` -- goes out under:
/// the `<iframe>`'s `referrerpolicy`, else the policy of the document holding the element.
fn frame_navigation_policy(scope: &mut v8::PinScope, id: u32) -> Option<transport::ReferrerPolicy> {
    let el = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el));
    let own = el.and_then(|el| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().and_then(|h| crate::core::html::referrer_policy::element_policy(&h.0, el))
    });
    if own.is_some() {
        return own;
    }
    let parent = frame_parent(scope, id).flatten();
    crate::core::html::referrer_policy::context_policy(scope, parent)
}
/// Run a classic script's source in the context's realm, as its own `<script>` elements run.
pub(crate) fn run_frame_script(scope: &mut v8::PinScope, id: u32, code: &str, url: &str, el: Option<u32>) {
    let el = el.map(|e| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let w: v8::Local<v8::Value> = wrap_cached(scope, &op_state, e).into();
        w
    });
    run_script(scope, id, code, url, el);
}
fn resolve_in(scope: &mut v8::PinScope, id: u32, u: &str) -> Option<String> {
    let base = ctx_base(scope, id);
    // `new URL(u, base)`, natively: an unparsable base, like an unparsable `u`, is no URL
    let url = if base.is_empty() {
        url::Url::parse(u).ok()?
    } else {
        let base = url::Url::parse(&base).ok()?;
        url::Url::options().base_url(Some(&base)).parse(u).ok()?
    };
    Some(url_component(&url, "href"))
}

// ── the child's Location ───────────────────────────────────────────────────────────────────────────
//
// A child context's `location` is a LOCATION, and it NAVIGATES. It was a `URL` instance: every
// component read correctly and not one of them did anything. `popup.location.href = url`,
// `popup.location = url` and `frame.contentWindow.location.replace(url)` are how a page drives a
// second browsing context. The object is created ONCE per context and mutated, because that is what
// a browser does: `w.location` is the same object across navigations. `[LegacyUnforgeable]`, so the
// members are own and non-configurable, which is what the capture records for the page's own.

const LOC_PARTS: [&str; 7] = ["protocol", "host", "hostname", "port", "pathname", "search", "hash"];

/// Set the URL WITHOUT navigating: the caller has just loaded, or is about to load, the document.
/// The page's `window[i]` / named frames are re-pointed, since a document of another origin makes the
/// page's view of that frame its cross-origin WindowProxy ([`sync_frames`]).
fn set_location(scope: &mut v8::PinScope, id: u32, href: &str) {
    let url = url::Url::parse(href).ok();
    with_ctx(scope, id, |c| {
        c.raw = href.to_string();
        c.url = url;
    });
    // Not re-entrantly: making a frame's window (inside the re-sync) sets ITS location in turn.
    thread_local!(static RESYNCING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if bc(scope, |b| b.frame_idx_count) > 0 && !RESYNCING.with(|r| r.replace(true)) {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        sync_frames(scope, false, undef);
        RESYNCING.with(|r| r.set(false));
    }
}

/// Frame `target`'s window as the PAGE's realm holds it -- `window[i]`, a named frame -- which is
/// the window `iframe.contentWindow` answers the page: the frame's own when its document is same
/// origin with the page's, else its cross-origin WindowProxy. `window[i]` held the frame's RAW window
/// whatever its origin: Chrome 150, measured with a frame on another origin, answers
/// `frames[0] === iframe.contentWindow` and `frames[0].location.href` a SecurityError; korvus answered
/// false and the URL (korvus-analysis U-7).
fn window_for_page<'s>(scope: &mut v8::PinScope<'s, '_>, target: u32) -> Option<v8::Local<'s, v8::Object>> {
    if document_origin(scope, Some(target)) != document_origin(scope, None) {
        xo_window(scope, target)
    } else {
        window_of(scope, target)
    }
}
/// `history.pushState` / `replaceState` called in a FRAME's realm (Chrome 150, measured): the URL
/// resolves against the FRAME's document, must be same origin with it (else SecurityError), and moves the
/// FRAME's location; the state is the frame's. They moved the PAGE's URL -- a challenge widget's widget frame
/// replaces its own URL, the page then read the challenge frame's origin as its own, a cross-origin frame
/// counted as same-origin, and the page was handed the frame's real Window as a message `source`.
/// `None` when no frame's script is running (the page's history answers); `Some(false)` when it threw.
pub fn frame_history_change(scope: &mut v8::PinScope, url: Option<&str>, state: v8::Local<v8::Value>, method: &'static str) -> Option<bool> {
    // THE HISTORY OBJECT'S OWN FRAME -- the realm its method runs in -- not the script that called it: the page's
    // `frame.contentWindow.history.pushState(…)` acts on the frame, and moved the PAGE's URL here
    // (measured against Chrome 150 on an `about:blank` frame).
    let id = current_realm_ctx(scope)?;
    // Blink's order: the state serialized first, then the URL resolved against the frame document's base URL and
    // refused where the document may not rewrite its URL to it (`CanChangeToUrlForHistoryApi`) -- an `about:blank`
    // frame's own `/path`, `#frag` and `about:blank?x` alike.
    let state = v8::Local::new(scope, state);
    let Some(cloned) = crate::modules::structured_clone::clone_for(scope, state, "History", method) else { return Some(false) };
    if let Some(u) = url.filter(|u| !u.is_empty()) {
        let doc_href = loc_href(scope, id);
        let base = url::Url::parse(&ctx_base(scope, id)).ok();
        let abs = base.as_ref().and_then(|b| b.join(u).ok()).or_else(|| url::Url::parse(u).ok());
        let origin = document_origin(scope, Some(id));
        let opaque = origin == "null" || origin.starts_with("null#");
        let allowed = match (&abs, url::Url::parse(&doc_href).ok()) {
            (Some(a), Some(d)) => crate::core::html::navigate_event::can_change_to_url_for_history_api(a, &d, opaque),
            _ => false,
        };
        if !allowed {
            let shown = match &abs {
                Some(a) => a.to_string(),
                None => crate::core::html::navigate_event::invalid_url_text(u, base.as_ref()),
            };
            let o = if opaque { "null".to_string() } else { origin };
            throw_dom(scope, &format!("Failed to execute '{method}' on 'History': A history state object with URL '{shown}' cannot be created in a document with origin '{o}' and URL '{doc_href}'."), "SecurityError");
            return Some(false);
        }
        if let Some(a) = abs {
            set_location(scope, id, a.as_str());
        }
    }
    let g = v8::Global::new(scope, cloned);
    with_ctx(scope, id, |c| c.history_state = Some(g));
    Some(true)
}

/// `history.state` of a FRAME's History: that frame's, whoever reads it; `None` for the page's.
pub fn frame_history_state<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let id = current_realm_ctx(scope)?;
    let g = with_ctx(scope, id, |c| c.history_state.clone()).flatten();
    Some(match g {
        Some(g) => v8::Local::new(scope, &g),
        None => v8::null(scope).into(),
    })
}

fn loc_read<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, k: &str) -> v8::Local<'s, v8::Value> {
    let (raw, url) = bc(scope, |b| b.ctxs.get(id as usize).map(|c| (c.raw.clone(), c.url.clone()))).unwrap_or_default();
    match url {
        None => str_val(scope, if k == "href" { &raw } else { "" }),
        Some(u) => {
            // `about:blank` / `about:srcdoc` INHERIT the page's origin. The URL parser calls them
            // opaque, which is the answer for a document that was fetched from `about:`, not for one
            // that inherited.
            // `about:srcdoc`'s `location.origin` is its URL's, opaque ("null", Chrome 150);
            // `about:blank`'s is its creator's.
            if k == "origin" && starts_with_ci(&raw, "about:srcdoc") {
                return str_val(scope, "null");
            }
            if k == "origin" && starts_with_ci(&raw, "about:") {
                let o = inherited_origin(scope, id);
                return str_val(scope, &o);
            }
            let s = url_component(&u, k);
            str_val(scope, &s)
        }
    }
}
fn define_accessor(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, get: v8::Local<v8::Function>, set: Option<v8::Local<v8::Function>>, enumerable: bool, configurable: bool) {
    let sv: v8::Local<v8::Value> = match set {
        Some(s) => s.into(),
        None => v8::undefined(scope).into(),
    };
    let mut d = v8::PropertyDescriptor::new_from_get_set(get.into(), sv);
    d.set_enumerable(enumerable);
    d.set_configurable(configurable);
    let k = name_key(scope, name);
    obj.define_property(scope, k, &d);
}
fn cb_loc_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let k = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = loc_read(scope, id, &k);
    rv.set(v);
}
fn cb_loc_href_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let v = js_string(scope, args.get(0));
    navigate_context(scope, id, &v);
}
fn cb_loc_part_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let k = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (raw, has_url) = bc(scope, |b| b.ctxs.get(id as usize).map(|c| (c.raw.clone(), c.url.is_some()))).unwrap_or_default();
    if !has_url {
        return;
    }
    let v = js_string(scope, args.get(0));
    // the `URL` setter of that name, natively (it was `set_prop` on a JS `URL`: through `URL.prototype`)
    let Ok(mut next) = url::Url::parse(&raw) else { return };
    {
        use crate::modules::url::components as c;
        let _ = match k.as_str() {
            "protocol" => c::set_protocol(&mut next, &v),
            "host" => c::set_host(&mut next, &v),
            "hostname" => c::set_hostname(&mut next, &v),
            "port" => c::set_port(&mut next, &v),
            "pathname" => c::set_pathname(&mut next, &v),
            "search" => c::set_search(&mut next, &v),
            "hash" => c::set_hash(&mut next, &v),
            _ => false,
        };
    }
    let href = url_component(&next, "href");
    navigate_context(scope, id, &href);
}
fn cb_loc_assign(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let v = js_string(scope, args.get(0));
    navigate_context(scope, id, &v);
}
fn cb_loc_reload(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let raw = bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.raw.clone())).unwrap_or_default();
    navigate_context(scope, id, &raw);
}
fn cb_loc_to_string(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let v = loc_read(scope, id, "href");
    rv.set(v);
}
fn cb_iwin_location_get(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    match loc_of(scope, id) {
        Some(l) => rv.set(l.into()),
        None => rv.set_null(),
    }
}
/// `[PutForwards=href]`: `w.location = "..."` NAVIGATES, it does not replace the object.
fn cb_iwin_location_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let v = js_string(scope, args.get(0));
    navigate_context(scope, id, &v);
}
fn install_location(scope: &mut v8::PinScope, id: u32) {
    // In its OWN realm: the Location object wears the frame's `Location.prototype`, and
    // the frame global's `location` slot -- what its own accessor reads -- holds it.
    if realm_installed(scope, id) {
        if let Some(realm) = realm_of(scope, id) {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            install_location_here(scope, id);
            if let Some(loc) = loc_of(scope, id) {
                crate::core::html::window_globals::window_slot_set(scope, "location", loc.into());
            }
            return;
        }
    }
    install_location_here(scope, id);
}

fn install_location_here(scope: &mut v8::PinScope, id: u32) {
    set_location(scope, id, "about:blank");
    let proto = crate::binding::protos::iface_object(scope, "Location")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .filter(|p| p.is_object())
        .or_else(|| crate::binding::protos::intrinsic(scope, "Object.prototype"));
    // An instance of the realm's `Location`, from the interface's template: with its brand, and with
    // Location's IMMUTABLE PROTOTYPE, which Chrome gives a frame's `location` as it gives the page's
    // (measured: `F-frame-location-immutable-proto`). It was an ordinary object handed
    // the prototype, whose prototype any script could replace.
    let loc = match proto {
        Some(p) => crate::binding::protos::mint_on(scope, p),
        None => v8::Object::new(scope),
    };
    // Each member from its template, the frame found from the realm that runs it.
    let g = template_fn(scope, cb_loc_get, "get href", 0, Some("href"));
    let s = template_fn(scope, cb_loc_href_set, "set href", 1, None);
    define_accessor(scope, loc, "href", g, Some(s), true, false);
    let g = template_fn(scope, cb_loc_get, "get origin", 0, Some("origin"));
    define_accessor(scope, loc, "origin", g, None, true, false);
    for k in LOC_PARTS {
        let g = template_fn(scope, cb_loc_get, &format!("get {k}"), 0, Some(k));
        let s = template_fn(scope, cb_loc_part_set, &format!("set {k}"), 1, Some(k));
        define_accessor(scope, loc, k, g, Some(s), true, false);
    }
    // `{ writable: false, enumerable: true, configurable: false }`: the unforgeable method shape.
    let f = template_fn(scope, cb_loc_assign, "assign", 1, None);
    let k = name_key(scope, "assign");
    loc.define_own_property(scope, k, f.into(), v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE);
    let f = template_fn(scope, cb_loc_assign, "replace", 1, None);
    let k = name_key(scope, "replace");
    loc.define_own_property(scope, k, f.into(), v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE);
    let f = template_fn(scope, cb_loc_reload, "reload", 0, None);
    let k = name_key(scope, "reload");
    loc.define_own_property(scope, k, f.into(), v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE);
    let f = template_fn(scope, cb_loc_to_string, "toString", 0, None);
    let k = name_key(scope, "toString");
    loc.define_own_property(scope, k, f.into(), v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE);
    let g = template_fn(scope, cb_loc_ancestor_origins, "get ancestorOrigins", 0, None);
    define_accessor(scope, loc, "ancestorOrigins", g, None, true, false);
    let lg = v8::Global::new(scope, loc);
    with_ctx(scope, id, |c| c.loc = Some(lg));
    if let Some(iwin) = window_of(scope, id) {
        let g = template_fn(scope, cb_iwin_location_get, "get location", 0, None);
        let s = template_fn(scope, cb_iwin_location_set, "set location", 1, None);
        // Configurable until [`furnish_window`] locks it, at the end of the window's furnishing.
        define_accessor(scope, iwin, "location", g, Some(s), true, true);
    }
}

// ── the child window ───────────────────────────────────────────────────────────────────────────────

fn cb_noop(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

/// Is this context disowned FROM WHERE THE QUESTION IS BEING ASKED? A browsing-context-group switch
/// is a property of the RELATIONSHIP: the popup is gone to the realms it left behind and perfectly
/// ordinary to its own scripts. So `popup.closed` is true when the opener reads it and false when the
/// popup's own script does, off the same object.
fn disowned_from(scope: &mut v8::PinScope, id: u32, iwin: v8::Local<v8::Value>) -> bool {
    if !bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.disowned).unwrap_or(false)) {
        return false;
    }
    let r = realm_or_global(scope);
    !r.strict_equals(iwin)
}
/// `length` counts the nested contexts INSIDE this frame -- the same question, one document down.
fn cb_iwin_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let this: v8::Local<v8::Value> = args.this().into();
    if disowned_from(scope, id, this) {
        rv.set_int32(0);
        return;
    }
    let root = doc_of(scope, id).and_then(|d| document_node_id(scope, d.into()));
    let n = match root {
        Some(r) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            frames_under(&op_state, r).len() as i32
        }
        None => 0,
    };
    rv.set_int32(n);
}
/// `closed` is FALSE for a nested context and stays false: only an auxiliary context can be closed.
fn cb_iwin_closed(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(false);
}
/// A child context's TIMERS are its own, and they re-enter that context: a popup whose script does
/// `setTimeout(() => opener.postMessage(x, "*"))` is running in the POPUP when the callback fires.
/// A frame TIMER's callback (`cb_iwin_timer`): called on the window whose `setTimeout` scheduled it -- HTML's
/// callback this value -- not on the page's window, whose own timer runs the task (a frame's
/// `setTimeout(function () { this.location.href })` read the PAGE's URL; Chrome 150 reads the frame's).
fn cb_enter_realm_timer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, _)) = unpack(scope, __body_data) else { return };
    let this: v8::Local<v8::Value> = match window_of(scope, id) {
        Some(w) => w.into(),
        None => args.this().into(),
    };
    enter_realm_call(scope, &args, rv, this);
}
/// A frame MICROTASK's callback (`cb_iwin_queue_microtask`): invoked with no callback this value (Chrome 150: a
/// strict `queueMicrotask` callback reads `this` as `undefined`).
fn cb_enter_realm(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = v8::undefined(scope).into();
    enter_realm_call(scope, &args, rv, this);
}
fn enter_realm_call(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, this: v8::Local<v8::Value>) {
    let __body_data = crate::binding::protos::body_data(scope, args);
    let Some((id, f)) = unpack(scope, __body_data) else { return };
    let Ok(f) = v8::Local::<v8::Function>::try_from(f) else { return };
    let mut argv: Vec<v8::Local<v8::Value>> = Vec::with_capacity(args.length().max(0) as usize);
    for i in 0..args.length() {
        argv.push(args.get(i));
    }
    // The CALLBACK's realm runs it, not the realm of the window whose timer or microtask queue scheduled it
    // (WebIDL "invoke a callback function"): a frame function given to an about:blank child's
    // setTimeout still runs as the frame, so a postMessage from it carries the frame as `source` --
    // an interactive-widget challenge borrows its child's timers and its parent drops any message whose source is not the
    // iframe's contentWindow. A page function clears the running frame and runs in the page's context.
    // (`id`, the scheduling window, still decides a STRING handler's realm: `cb_frame_string_timer`.)
    let _ = id;
    let own = f.get_creation_context(scope).and_then(|c| {
        let g = c.global(scope);
        ctx_of_window(scope, g)
    });
    let prev = bc(scope, |b| std::mem::replace(&mut b.current, own));
    let ctx = match own {
        Some(_) => scope.get_current_context(),
        None => main_global(scope).get_creation_context(scope).unwrap_or_else(|| scope.get_current_context()),
    };
    // A callback that throws is REPORTED IN ITS REALM: a frame's `ErrorEvent` goes to the frame's window,
    // which is what Chrome does. Rethrowing it instead let the page's scheduler report it at the PAGE's global.
    let r = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, ctx);
        v8::tc_scope!(let tc, scope);
        let r = f.call(tc, this, &argv).map(|r| v8::Global::new(tc, r));
        let thrown = tc.exception().map(|e| {
            let m = tc.message();
            (v8::Global::new(tc, e), crate::core::events::report::caught_site(tc, e, m))
        });
        tc.reset();
        if let Some((e, site)) = thrown {
            let e = v8::Local::new(tc, &e);
            crate::core::events::report::report_exception_at(tc, e, site);
        }
        r
    };
    leave_realm(scope, prev);
    if let Some(r) = r {
        let r = v8::Local::new(scope, &r);
        rv.set(r);
    }
}
fn wrap_enter<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, f: v8::Local<v8::Value>, timer: bool) -> v8::Local<'s, v8::Value> {
    if !f.is_function() {
        return rescope(scope, f);
    }
    let d = pack(scope, id, f);
    if timer {
        build_fn_data(scope, cb_enter_realm_timer, "", 0, d).into()
    } else {
        build_fn_data(scope, cb_enter_realm, "", 0, d).into()
    }
}
/// The receiver check a frame's OWN window members run first -- the one the page window's rows run
/// (`window_globals::window_receiver`), so a frame member and its page twin cannot disagree.
///
/// Measured on Chrome 150.0.7871.46 over loopback, from inside the frame and on
/// `frames[0]` from the page:
///
/// ```text
/// setTimeout / setInterval / queueMicrotask / postMessage / focus / blur .call({}, ...)
///     korvus  answered (a timer id, undefined)       chrome  TypeError: Illegal invocation
/// frames[0].setTimeout.call(Object.create(Window.prototype), f, 0)     chrome  Illegal invocation
/// frames[0].setTimeout.call(window, f, 0)                               chrome  a timer id
/// ```
///
/// A frame's `setTimeout.call({})` with no arguments is `Illegal invocation` too: the receiver comes
/// before every argument check, as it does for the page's rows.
fn iwin_receiver(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    crate::core::html::window_globals::window_receiver(scope, args.this().into())
}
fn cb_iwin_focus_blur(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    iwin_receiver(scope, &args);
}
fn cb_iwin_post_message_checked<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    // The receiver, then WebIDL's arity, as the page window's `postMessage` (a frame's
    // `postMessage()` returned undefined where Chrome throws "1 argument required"; a foreign receiver
    // is "Illegal invocation" before that).
    if !iwin_receiver(scope, &args) {
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'postMessage' on 'Window': 1 argument required, but only 0 present.");
        return;
    }
    if let Some(id) = realm_ctx(scope) {
        post_message_to(scope, id, &args);
    }
}
fn cb_iwin_timer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !iwin_receiver(scope, &args) {
        return;
    }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // WebIDL's arity, as the page window's timers (a frame's `setTimeout()` returned an id where
    // Chrome throws).
    if args.length() < 1 {
        throw_type_error(scope, &format!("Failed to execute '{name}' on 'Window': 1 argument required, but only 0 present."));
        return;
    }
    let Some(id) = realm_ctx(scope) else { return };
    let mut argv: Vec<v8::Local<v8::Value>> = Vec::new();
    let handler = args.get(0);
    // Anything but a function is source text -- a TrustedScript stringifies to its script.
    let wrapped = if !handler.is_function() {
        // Blink `DOMTimer::setTimeout(String)`, in the FRAME's realm as the page's timers do it in the page's: the
        // Trusted Types check (may throw), then CSP `'unsafe-eval'`, then an empty script -- the last two answer
        // 0 and make no timer. The frame's string timers skipped all three.
        let iface = "Window";
        let Some(text) = crate::core::trusted_types::tables::compliant_script(scope, handler, iface, &name) else { return };
        if !crate::core::trusted_types::tables::string_compilation_allowed(scope) || text.length() == 0 {
            rv.set(v8::Integer::new(scope, 0).into());
            return;
        }
        let code = text.to_rust_string_lossy(scope);
        let d = pack_str(scope, id, &code);
        build_fn_data(scope, cb_frame_string_timer, "", 0, d).into()
    } else {
        wrap_enter(scope, id, handler, true)
    };
    argv.push(wrapped);
    for i in 1..args.length().max(1) {
        argv.push(args.get(i));
    }
    // The page realm's OWN timer, never the page's `window.setTimeout`: a page that replaces it would
    // otherwise see every frame timer pass through its function.
    let g: v8::Local<v8::Value> = main_global(scope).into();
    let r = match crate::core::intersection_observer::rendering::pristine_window_fn(scope, &name, 1) {
        Some(f) => f.call(scope, g, &argv),
        None => call_method(scope, g, &name, &argv),
    };
    if let Some(r) = r {
        rv.set(r);
    }
}
fn cb_iwin_queue_microtask(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !iwin_receiver(scope, &args) {
        return;
    }
    let Some(id) = realm_ctx(scope) else { return };
    let wrapped = wrap_enter(scope, id, args.get(0), false);
    // The page realm's OWN `queueMicrotask`, as the frame's timers take the page's own timers: a page that replaced
    // `window.queueMicrotask` saw every frame microtask pass through its function.
    let g: v8::Local<v8::Value> = main_global(scope).into();
    let r = match crate::core::intersection_observer::rendering::pristine_window_fn(scope, "queueMicrotask", 1) {
        Some(f) => f.call(scope, g, &[wrapped]),
        None => call_method(scope, g, "queueMicrotask", &[wrapped]),
    };
    if let Some(r) = r {
        rv.set(r);
    }
}
/// `postMessage` on a child window: structured clone at CALL time, dispatch on a later task, and a
/// `targetOrigin` that is neither "*" nor a match drops the message. The SENDER's origin and window
/// travel with the message: the sender is whichever realm is running.
fn cb_iwin_post_message<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    post_message_to(scope, id, &args);
}

/// [`cb_iwin_post_message`]'s body for context `id`: the cross-origin view's (its data names the context)
/// and a frame's own (its realm does, [`cb_iwin_post_message_checked`]).
fn post_message_to<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, args: &v8::FunctionCallbackArguments<'s>) {
    let Some(iwin) = window_of(scope, id) else { return };
    let mut target_origin = "/".to_string();
    let mut transfer: Option<v8::Local<v8::Value>> = None;
    let mut from_options = false;
    if args.length() >= 2 {
        let a = args.get(1);
        // the `WindowPostMessageOptions` overload, converted, as the top-level `Window` body does
        if args.length() == 2 && (a.is_null_or_undefined() || a.is_object()) {
            let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, a, "WindowPostMessageOptions", "Failed to execute 'postMessage' on 'Window': ") else { return };
            if let Some(t) = get_prop(scope, o, "targetOrigin").filter(|t| t.is_string()) {
                target_origin = js_string(scope, t);
            }
            transfer = get_prop(scope, o, "transfer").filter(|t| !t.is_undefined());
            from_options = true;
        } else {
            target_origin = js_string(scope, a);
            if args.length() >= 3 && !args.get(2).is_undefined() {
                transfer = Some(args.get(2));
            }
        }
    }
    // Read and validate the whole list before anything else, exactly as the top-level `Window` body
    // does -- one algorithm, one set of sentences (`core/messaging/transfer.rs`).
    let Some(list) = crate::core::messaging::transfer::read_window_transfer(scope, transfer, from_options) else { return };
    let href = loc_href(scope, id);
    let target = url_origin(&href).unwrap_or_else(|| "null".to_string());
    if target_origin != "*" && target_origin != "/" {
        let Some(want) = url_origin(&target_origin) else {
            throw_dom(
                scope,
                &format!("Failed to execute 'postMessage' on 'Window': Invalid target origin '{target_origin}' in a call to 'postMessage'."),
                "SyntaxError",
            );
            return;
        };
        if want != target {
            return;
        }
    }
    let raw_sender = realm_or_global(scope);
    let from = realm_origin(scope, raw_sender);
    // The frame runs in a child engine -- the message crosses as structured-clone bytes. A popup open as a target of
    // its own takes them over the endpoint (`core/html/popup.rs`).
    if let Some(rid) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.remote)) {
        if remote_popup(scope, id).is_some() {
            let bytes = crate::modules::clone_state::wire_serialize(scope, args.get(0), &[]);
            crate::core::html::popup::post(rid, &bytes, &from);
        } else {
            crate::core::html::remote_frame::post_to_child(scope, rid, args.get(0), &from);
        }
        return;
    }
    let sender = sender_seen_by(scope, id);
    // StructuredSerializeWithTransfer happens at CALL time, so a DataCloneError for an unclonable
    // message is thrown to the caller here rather than swallowed on the delivery task.
    // A transferred port stays itself in the clone and becomes the port the receiver gets.
    let originals = crate::core::messaging::transfer::port_originals(&list);
    let Some(data) = clone_into_realm(scope, id, args.get(0), &originals) else { return };
    // The ports the FRAME sees are fresh ports entangled with the sender's peers, not the sender's
    // own objects: the old body sliced the caller's array and handed that over. They are the frame's
    // own MessagePorts, minted in its realm.
    let receiver = if id == MAIN_CTX || realm_installed(scope, id) { realm_of(scope, id) } else { None };
    let (ports, pairs) = crate::core::messaging::transfer::take_ports_mapped(scope, &list, receiver);
    let data = crate::modules::structured_clone::substitute(scope, data, &pairs);
    let ports: v8::Local<v8::Value> = ports.into();
    crate::core::messaging::transfer::detach_buffers(scope, &list);
    let init = crate::binding::protos::init_dict(scope);
    set_prop(scope, init, "data", data);
    // an opaque origin is "null", as in [`window.rs`]'s page-bound path (Chrome 150, measured)
    let origin = str_val(scope, &from);
    set_prop(scope, init, "origin", origin);
    set_prop(scope, init, "source", sender);
    set_prop(scope, init, "ports", ports);
    let d = pack(scope, id, init.into());
    let _ = iwin;
    // the delivery is the RECEIVER's task (its long animation frame, not the sender's): its callback is made in the
    // receiver's realm, which is the realm the timer task is begun for (`loaf::begin_task`)
    let f = match if realm_installed(scope, id) { realm_of(scope, id) } else { None } {
        Some(realm) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            let f = build_fn_data(scope, cb_deliver_message, "", 0, d);
            v8::Global::new(scope, f)
        }
        None => {
            let f = build_fn_data(scope, cb_deliver_message, "", 0, d);
            v8::Global::new(scope, f)
        }
    };
    let f = v8::Local::new(scope, &f);
    set_timeout(scope, f, 0);
}
/// StructuredSerialize at the caller, StructuredDeserialize in the RECEIVER's realm: a
/// frame's realm has its own `Object`/`Array`, and `e.data instanceof Object` holds in the frame.
fn clone_into_realm<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    id: u32,
    v: v8::Local<'s, v8::Value>,
    transferred: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Local<'s, v8::Value>> {
    let realm = if realm_installed(scope, id) { realm_of(scope, id) } else { None };
    let Some(realm) = realm else {
        return crate::modules::structured_clone::clone_for_transfer(scope, v, "Window", "postMessage", transferred);
    };
    let kept: Vec<v8::Global<v8::Value>> = transferred.iter().map(|t| v8::Global::new(scope, *t)).collect();
    let g = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let v = v8::Local::new(scope, v);
        let transferred: Vec<v8::Local<v8::Value>> = kept.iter().map(|t| v8::Local::new(scope, t)).collect();
        crate::modules::structured_clone::clone_for_transfer(scope, v, "Window", "postMessage", &transferred).map(|d| v8::Global::new(scope, d))
    }?;
    Some(v8::Local::new(scope, &g))
}

fn cb_deliver_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, init)) = unpack(scope, __body_data) else { return };
    let Some(iwin) = window_of(scope, id) else { return };
    // The receiver realm's `MessageEvent`: a frame's own interface object.
    let holder = if realm_installed(scope, id) { iwin } else { global_obj(scope) };
    let Some(ctor) = get_prop(scope, holder, "MessageEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    // The `source` goes straight into the event's slot: the page-facing constructor accepts only a
    // Window, a MessagePort or a ServiceWorker, and a cross-origin sender's WindowProxy view is a
    // Proxy here -- through the constructor the whole delivery threw and the message was lost.
    let source = v8::Local::<v8::Object>::try_from(init).ok().and_then(|o| {
        let s = get_prop(scope, o, "source");
        let k = name_key(scope, "source");
        o.delete(scope, k.into());
        s
    });
    let ty = str_val(scope, "message");
    let ev = {
        v8::tc_scope!(let tc, scope);
        let r = ctor.new_instance(tc, &[ty, init]).map(|o| v8::Global::new(tc, o));
        tc.reset();
        r
    };
    let Some(ev) = ev else { return };
    let ev = v8::Local::new(scope, &ev);
    if let Some(s) = source {
        crate::core::events::event::evt_slot_write(scope, ev, "source", s);
    }
    mark_trusted(scope, ev);
    et_dispatch(scope, iwin.into(), ev);
}

/// A nested browsing context's window and its initial empty document.
/// The context whose document holds frame `id`'s element: another frame's, or [`MAIN_CTX`] for the
/// page's. `None` for a popup, or a frame whose element is in no document.
fn frame_parent_ctx(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    if id == MAIN_CTX {
        return None;
    }
    let el = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el))?;
    let (root, page_root) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.try_borrow::<HostDom>()?.0;
        (dom.shadow_including_root(el), dom.root())
    };
    if root == page_root {
        return Some(MAIN_CTX);
    }
    bc(scope, |b| b.by_doc.get(&root).copied())
}

/// The top of frame `id`'s ancestor chain; the frame itself when it has no parent (a popup).
fn frame_top_ctx(scope: &mut v8::PinScope, id: u32) -> u32 {
    let mut cur = id;
    for _ in 0..64 {
        match frame_parent_ctx(scope, cur) {
            Some(p) => cur = p,
            None => break,
        }
    }
    cur
}

/// Window `target` as the running script sees it: the real object from the same origin, the
/// cross-origin view otherwise.
fn window_for_caller<'s>(scope: &mut v8::PinScope<'s, '_>, target: u32) -> Option<v8::Local<'s, v8::Object>> {
    if cross_origin_to_caller(scope, target) {
        xo_window(scope, target)
    } else {
        window_of(scope, target)
    }
}

/// The running script's window as the realm of `receiver` (a context, or [`MAIN_CTX`]) sees it: the
/// `source` of a message posted to `receiver`. It must be the very object that realm reaches the
/// sender by -- `iframe.contentWindow`, `parent`, `top` -- so a cross-origin sender is its
/// cross-origin view; korvus handed out the raw window, and `e.source === frame.contentWindow` was
/// false where every login and captcha frame protocol checks it (a site's login provider).
/// `KORVUS_TRACE_PM`'s line: the running realm, the entered context's realm, the sender's origin and what
/// `source` will be, and the script's own frames.
pub(crate) fn trace_post(scope: &mut v8::PinScope, to: &str, origin: &str, source: v8::Local<v8::Value>) {
    let cur = bc(scope, |b| b.current);
    let entered = {
        let ctx = scope.get_entered_or_microtask_context();
        let g = ctx.global(scope);
        ctx_of_window(scope, g)
    };
    let current = {
        let ctx = scope.get_current_context();
        let g = ctx.global(scope);
        ctx_of_window(scope, g)
    };
    let src_id = v8::Local::<v8::Object>::try_from(source).ok().and_then(|o| ctx_of_window(scope, o));
    let tag = if source.is_null() { "null".to_string() } else { source.type_of(scope).to_rust_string_lossy(scope) };
    let mut frames = String::new();
    if let Some(st) = v8::StackTrace::current_stack_trace(scope, 6) {
        for i in 0..st.get_frame_count() {
            if let Some(f) = st.get_frame(scope, i) {
                let name = f.get_script_name(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default();
                let func = f.get_function_name(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default();
                frames.push_str(&format!(" | {func}@{}:{}:{}", name.chars().rev().take(60).collect::<String>().chars().rev().collect::<String>(), f.get_line_number(), f.get_column()));
            }
        }
    }
    let run = running(scope);
    let (href, dorig, page_orig) = (
        run.map(|r| loc_href(scope, r)).unwrap_or_default(),
        document_origin(scope, run),
        document_origin(scope, None),
    );
    let is_xo = run.and_then(|r| xo_window(scope, r)).is_some_and(|x| { let xv: v8::Local<v8::Value> = x.into(); xv.strict_equals(source) });
    let is_real = run.and_then(|r| window_of(scope, r)).is_some_and(|w| { let wv: v8::Local<v8::Value> = w.into(); wv.strict_equals(source) });
    let parent = run.and_then(|r| frame_parent_ctx(scope, r));
    eprintln!("[pm] to={to} current={cur:?} entered={entered:?} curctx={current:?} running={run:?} parent={parent:?} href={} dorig={dorig} page={page_orig} origin={origin} source_ctx={src_id:?} source={tag} is_xo={is_xo} is_real={is_real}{frames}", href.chars().take(80).collect::<String>());
}

pub(crate) fn sender_seen_by<'s>(scope: &mut v8::PinScope<'s, '_>, receiver: u32) -> v8::Local<'s, v8::Value> {
    let sender = running(scope).unwrap_or(MAIN_CTX);
    let recv_origin = document_origin(scope, Some(receiver));
    let seen = if sender != receiver && document_origin(scope, Some(sender)) != recv_origin {
        xo_window(scope, sender)
    } else {
        window_of(scope, sender)
    };
    match seen {
        Some(w) => w.into(),
        None => global_obj(scope).into(),
    }
}

/// [`sender_seen_by`] for a message to the page's own window.
pub(crate) fn sender_seen_by_page<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    sender_seen_by(scope, MAIN_CTX)
}

fn cb_iwin_parent(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let target = frame_parent_ctx(scope, id).unwrap_or(id);
    if let Some(w) = window_for_caller(scope, target) {
        rv.set(w.into());
    }
}

/// A frame window's `top` or `parent` (the frame realm's own getters ask this).
pub(crate) fn frame_top_or_parent<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, parent: bool) -> Option<v8::Local<'s, v8::Object>> {
    let target = if parent { frame_parent_ctx(scope, id).unwrap_or(id) } else { frame_top_ctx(scope, id) };
    window_for_caller(scope, target)
}

fn cb_iwin_top(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let target = frame_top_ctx(scope, id);
    if let Some(w) = window_for_caller(scope, target) {
        rv.set(w.into());
    }
}

/// `frameElement`: the embedding element, or null when the embedder's document is cross-origin to
/// the caller (HTML "WindowProxy frameElement", step 4).
fn cb_iwin_frame_element(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let el = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el));
    let parent = frame_parent_ctx(scope, id);
    match (el, parent) {
        (Some(el), Some(p)) if !cross_origin_to_caller(scope, p) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let w = wrap_cached(scope, &op_state, el);
            rv.set(w.into());
        }
        _ => rv.set_null(),
    }
}

/// `document.referrer` for a frame navigated to `url` from `parent_url`, under the embedder's
/// referrer policy (`referrerpolicy` on the frame element, else Chrome's default
/// `strict-origin-when-cross-origin`). It was once always "".
fn frame_referrer(parent_url: &str, url: &str, policy: &str) -> String {
    let Ok(p) = url::Url::parse(parent_url) else { return String::new() };
    if !matches!(p.scheme(), "http" | "https") {
        return String::new();
    }
    let mut full = p.clone();
    full.set_fragment(None);
    let _ = full.set_username("");
    let _ = full.set_password(None);
    let full = full.to_string();
    let origin = format!("{}/", p.origin().ascii_serialization());
    let target = url::Url::parse(url).ok();
    // `about:srcdoc`'s URL origin is opaque, so it is CROSS-origin here: Chrome 150 / Brave 151 read the
    // embedder's origin only (`http://host/`) under the default policy (measured 2026-10-05). `about:blank`
    // never comes here: its referrer is its creator's URL ([`begin_frame_document`]).
    let same = target.as_ref().is_some_and(|t| t.origin() == p.origin());
    let downgrade = p.scheme() == "https" && target.as_ref().is_some_and(|t| t.scheme() == "http");
    match policy.trim().to_ascii_lowercase().as_str() {
        "no-referrer" => String::new(),
        "unsafe-url" => full,
        "origin" => origin,
        "same-origin" => if same { full } else { String::new() },
        "origin-when-cross-origin" => if same { full } else { origin },
        "strict-origin" => if downgrade { String::new() } else { origin },
        "no-referrer-when-downgrade" => if downgrade { String::new() } else { full },
        _ => if same { full } else if downgrade { String::new() } else { origin },
    }
}

/// A frame's new document is about to run its parser scripts: it is `loading`, and its referrer is
/// the embedder's URL under the policy (F1, F5).
fn begin_frame_document(scope: &mut v8::PinScope, id: u32) {
    // The modules the frame's previous document loaded are that document's.
    frame_modules::forget(scope, id);
    let url = loc_href(scope, id);
    let parent_url = match frame_parent_ctx(scope, id) {
        Some(p) => loc_href(scope, p),
        None => String::new(),
    };
    // The element's `referrerpolicy`, else the EMBEDDING document's policy (a
    // `Referrer-Policy: same-origin` page's cross-origin frame reads "" in Chrome; korvus named the page).
    let policy = frame_navigation_policy(scope, id).map(|p| p.token()).unwrap_or("").to_string();
    // An `about:blank` document's referrer is its CREATOR's URL, verbatim -- fragment kept, and the
    // embedder's referrer policy not applied (Chrome 150 / Brave 151, measured 2026-10-05: `src=about:blank`
    // under `referrerpolicy=no-referrer` still reads the page's full URL; a blank frame nested in a blank
    // frame reads `about:blank`). It was "".
    let referrer = if starts_with_ci(&url, "about:blank") { parent_url } else { frame_referrer(&parent_url, &url, &policy) };
    // The document's own clock: from its navigation's start, or now for one no navigation fetched, at its
    // document's precision -- 5 µs with the cross-origin isolated capability (Chrome 150: a frame its container
    // denies `cross-origin-isolated` reads a 100 µs clock in an isolated page; it read the page's grain here)
    let start = with_ctx(scope, id, |c| c.inline_nav_start.take().or_else(|| c.delivered.timing.as_ref().map(|t| t.start))).flatten();
    let mut clock = start.map(spec::PerfClock::since_process_ms).unwrap_or_default();
    let capable = crate::core::html::isolation_policy::capability(scope, Some(id));
    clock.set_cross_origin_isolated(capable);
    if let Some(realm) = with_ctx(scope, id, |c| c.realm.clone()).flatten() {
        let realm = v8::Local::new(scope, &realm);
        crate::core::html::isolation_policy::gate_frame_realm(scope, id, realm);
    }
    with_ctx(scope, id, |c| {
        c.readiness = Readiness::Loading;
        c.load_event = FrameLoadEvent::NotRun;
        c.delays.clear();
        c.load_parked = false;
        c.referrer = referrer;
        c.current_script = None;
        c.parser_aborted = false;
        c.clock = clock;
        c.milestones.clear();
        // the new document begins `loading` now: `performance.timing.domLoading` (Blink `MarkDomLoading`)
        c.milestones.push(("domLoading", clock.now_ms()));
        c.nav_entry = None;
    });
    scope.set_slot(FrameClocks);
    refresh_opaque(scope, id);
}

/// The sandboxing flags frame `id`'s document runs under, of the ones korvus models, as HTML computes its active
/// sandboxing flag set: its parent document's (flags reach every nested context), its frame element's `sandbox`
/// attribute at the time, and the `sandbox` directive of the response's enforced CSP (`Content-Security-Policy:
/// sandbox ...`, a document of its own response; `about:srcdoc` and friends hold their parent's policies anyway). A
/// flag is set unless the token that relaxes it is in EVERY list that names one: an `<iframe sandbox="">` loading a
/// `sandbox allow-scripts` response runs no script, nor does an `allow-scripts` iframe loading a bare `sandbox` one.
/// Chrome 150, three runs alike: of thirteen frames only `sandbox="allow-scripts"`
/// srcdoc, a `sandbox allow-scripts` response, the frames nested in an `allow-scripts` srcdoc and an `allow-scripts`
/// frame's `onerror` attribute ran script; korvus ran every one it loaded (an earlier fix had the origin only).
struct Sandbox {
    /// no script runs (no `allow-scripts`)
    scripts: bool,
    /// an opaque origin (no `allow-same-origin`)
    opaque: bool,
}

fn sandbox_of(scope: &mut v8::PinScope, id: u32) -> Sandbox {
    let parent = frame_parent_ctx(scope, id)
        .filter(|&p| p != MAIN_CTX)
        .and_then(|p| with_ctx(scope, p, |c| (c.scripts_sandboxed, c.opaque.is_some())))
        .unwrap_or((false, false));
    let mut s = Sandbox { scripts: parent.0, opaque: parent.1 };
    let mut lists: Vec<Vec<String>> = Vec::new();
    if let Some(el) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el)) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if let Some(tokens) = st.borrow::<HostDom>().0.get_attribute(el, "sandbox") {
            lists.push(tokens.split_ascii_whitespace().map(str::to_string).collect());
        }
    }
    for p in ctx_delivered_enforced(scope, id) {
        if let Some(t) = p.sandbox() {
            lists.push(t.iter().map(|t| t.to_string()).collect());
        }
    }
    let lacks = |list: &[String], token: &str| !list.iter().any(|t| t.eq_ignore_ascii_case(token));
    for list in &lists {
        s.scripts |= lacks(list, "allow-scripts");
        s.opaque |= lacks(list, "allow-same-origin");
    }
    s
}

/// Give frame `id`'s current document its sandbox: an opaque origin, or none, and its sandboxed scripts
/// flag ([`sandbox_of`]).
fn refresh_opaque(scope: &mut v8::PinScope, id: u32) {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let s = sandbox_of(scope, id);
    let opaque = s.opaque.then(|| NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    with_ctx(scope, id, |c| {
        c.opaque = opaque;
        c.scripts_sandboxed = s.scripts;
    });
}

/// Whether no script may run in frame context `id`'s document: HTML's "scripting is disabled" through its sandboxed
/// scripts flag ([`sandbox_of`]). The page's own document is never sandboxed here.
pub(crate) fn scripts_sandboxed(scope: &mut v8::PinScope, id: u32) -> bool {
    id != MAIN_CTX && with_ctx(scope, id, |c| c.scripts_sandboxed).unwrap_or(false)
}

/// [`scripts_sandboxed`] for the realm of the CURRENT V8 context (an attribute's handler compiles there).
pub(crate) fn realm_scripts_sandboxed(scope: &mut v8::PinScope) -> bool {
    realm_ctx(scope).is_some_and(|id| scripts_sandboxed(scope, id))
}

/// The serialisation of an origin [`document_origin`] answered: an opaque one's is "null".
pub(crate) fn serialize_origin(origin: &str) -> &str {
    if origin.starts_with("null#") { "null" } else { origin }
}

/// Whether `doc` is a frame's document whose origin a sandbox made opaque.
pub(crate) fn document_is_sandboxed(scope: &mut v8::PinScope, doc: v8::Local<v8::Value>) -> bool {
    context_of_document(scope, doc).filter(|&id| id != MAIN_CTX).is_some_and(|id| with_ctx(scope, id, |c| c.opaque.is_some()).unwrap_or(false))
}

/// Whether the realm of `win` (a frame's window) has an opaque origin through a sandbox.
pub(crate) fn realm_is_sandboxed(scope: &mut v8::PinScope, win: v8::Local<v8::Value>) -> bool {
    ctx_of_value(scope, win).filter(|&id| id != MAIN_CTX).is_some_and(|id| with_ctx(scope, id, |c| c.opaque.is_some()).unwrap_or(false))
}

/// Move frame `id`'s document to `to`, firing `readystatechange` at it when it changed.
fn set_frame_readiness(scope: &mut v8::PinScope, id: u32, to: Readiness) {
    let changed = with_ctx(scope, id, |c| std::mem::replace(&mut c.readiness, to) != to).unwrap_or(false);
    match to {
        Readiness::Interactive if changed => note_frame_milestone(scope, id, "domInteractive"),
        Readiness::Complete if changed => note_frame_milestone(scope, id, "domComplete"),
        _ => {}
    }
    if changed {
        if let Some(d) = doc_of(scope, id) {
            fire_at(scope, d.into(), "readystatechange");
        }
    }
}

/// The frame document's parser scripts have run: `interactive`, then `DOMContentLoaded` (bubbling,
/// trusted) -- which a frame never fired, and a device-check script's interstitial waits for.
fn frame_dom_content_loaded(scope: &mut v8::PinScope, id: u32) {
    set_frame_readiness(scope, id, Readiness::Interactive);
    let Some(d) = doc_of(scope, id) else { return };
    let Some(ev) = trusted_event_with(scope, "DOMContentLoaded", true) else { return };
    note_frame_milestone(scope, id, "domContentLoadedEventStart");
    et_dispatch(scope, d.into(), ev);
    note_frame_milestone(scope, id, "domContentLoadedEventEnd");
    // Blink's `Document::FinishedParsing` performs a microtask checkpoint after `DOMContentLoaded`, before it checks
    // for completion: the handlers' promise jobs run before `load` (Chrome 150, measured). Not with a script on the
    // stack -- a `close()` from script runs the document's end inside it, and performs none.
    if v8::StackTrace::current_stack_trace(scope, 1).map_or(0, |st| st.get_frame_count()) == 0 {
        scope.perform_microtask_checkpoint();
    }
}

/// The readiness of the frame document whose arena root is `root`, `None` when no frame shows it.
pub(crate) fn frame_doc_readiness(scope: &mut v8::PinScope, root: u32) -> Option<Readiness> {
    bc(scope, |b| {
        let id = *b.by_doc.get(&root)?;
        b.ctxs.get(id as usize).filter(|c| c.doc_root == Some(root)).map(|c| c.readiness)
    })
}

/// `document.currentScript` of the frame document whose root is `root`: `Some(null)` when a frame
/// shows it and no script of its is running, `None` when no frame shows it.
pub(crate) fn frame_doc_current_script<'s>(scope: &mut v8::PinScope<'s, '_>, root: u32) -> Option<v8::Local<'s, v8::Value>> {
    let g = bc(scope, |b| {
        let id = *b.by_doc.get(&root)?;
        b.ctxs.get(id as usize).filter(|c| c.doc_root == Some(root)).map(|c| c.current_script.clone())
    })?;
    Some(match g {
        Some(g) => v8::Local::new(scope, &g),
        None => v8::null(scope).into(),
    })
}

/// The cookie context of the frame document whose arena root is `root`: the URL its
/// `document.cookie` reads and writes under -- its own, or for an `about:` document its creator's --,
/// whether it is in a CROSS-SITE context: some context from it up to its top-level one (the page, or a
/// popup, which is its own top-level) is not same-site with that top-level document, and that top-level
/// document's URL (its cookies' partition). `None` for a document no frame holds.
pub(crate) fn frame_doc_cookie_context(scope: &mut v8::PinScope, root: u32) -> Option<(String, bool, String)> {
    let id = bc(scope, |b| {
        let id = *b.by_doc.get(&root)?;
        b.ctxs.get(id as usize).filter(|c| c.doc_root == Some(root)).map(|_| id)
    })?;
    Some(ctx_cookie_context(scope, id))
}

/// [`frame_doc_cookie_context`] of the RUNNING realm's document -- `cookieStore`'s -- or `None` for the page's own.
pub(crate) fn realm_cookie_context(scope: &mut v8::PinScope) -> Option<(String, bool, String)> {
    let id = running(scope)?;
    Some(ctx_cookie_context(scope, id))
}

/// [`frame_doc_cookie_context`] of context `id`.
fn ctx_cookie_context(scope: &mut v8::PinScope, id: u32) -> (String, bool, String) {
    let url = frame_request_client(scope, id).0;
    let mut chain = Vec::new();
    let mut cur = id;
    let top = loop {
        if cur == MAIN_CTX {
            break page_location_prop(scope, "href", "");
        }
        if bc(scope, |b| b.ctxs.get(cur as usize).map(|c| c.popup)).unwrap_or(true) {
            break frame_request_client(scope, cur).0;
        }
        chain.push(cur);
        match frame_parent_ctx(scope, cur) {
            Some(p) if chain.len() < 64 => cur = p,
            _ => break page_location_prop(scope, "href", ""),
        }
    };
    let top_url = url::Url::parse(&top).ok();
    let mut cross = false;
    for c in chain {
        let u = url::Url::parse(&frame_request_client(scope, c).0).ok();
        if !matches!((&top_url, &u), (Some(t), Some(u)) if transport::same_site(t, u)) {
            cross = true;
            break;
        }
    }
    (url, cross, top)
}

/// The frame context showing the document whose arena root is `root` -- `None` for a document no frame
/// holds (the page's own, or one with no browsing context: `new Document()`, a `DOMParser` result).
pub(crate) fn frame_of_doc(scope: &mut v8::PinScope, root: u32) -> Option<u32> {
    bc(scope, |b| {
        let id = *b.by_doc.get(&root)?;
        b.ctxs.get(id as usize).filter(|c| c.doc_root == Some(root)).map(|_| id)
    })
}

/// The window object of frame context `id`, from its own realm.
pub(crate) fn frame_window<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    window_of(scope, id)
}

/// `document.referrer` of the frame document whose root is `root`.
pub(crate) fn frame_doc_referrer(scope: &mut v8::PinScope, root: u32) -> Option<String> {
    bc(scope, |b| {
        let id = *b.by_doc.get(&root)?;
        b.ctxs.get(id as usize).filter(|c| c.doc_root == Some(root)).map(|c| c.referrer.clone())
    })
}

/// `location.ancestorOrigins` of frame `id`: the origins of its embedders, nearest first.
fn cb_loc_ancestor_origins(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let mut origins: Vec<String> = Vec::new();
    let mut cur = id;
    for _ in 0..64 {
        let Some(p) = frame_parent_ctx(scope, cur) else { break };
        origins.push(document_origin(scope, Some(p)));
        if p == MAIN_CTX {
            break;
        }
        cur = p;
    }
    let items: Vec<v8::Local<v8::Value>> = origins.iter().map(|o| str_val(scope, o)).collect();
    let arr = v8::Array::new_with_elements(scope, &items);
    let list = crate::core::dom::collections::make_static(scope, "DOMStringList", arr);
    rv.set(list);
}

/// The page's own V8 context. Recorded when the surface is sealed (before any page script); a
/// runtime that never sealed is running its page context whenever this module is entered from it.
pub(crate) fn main_context<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Context> {
    match bc(scope, |b| b.main.clone()) {
        Some(g) => v8::Local::new(scope, &g),
        None => scope.get_current_context(),
    }
}

/// Is the current realm the page's own, and not a frame's? Every frame has a realm of its own;
/// a runtime with no browsing contexts -- a worker's -- is its own page realm here.
/// Whether the running realm's document is the TOP-LEVEL one: the page realm, in a process that is not a
/// remote frame of another. A frame's document is not.
pub(crate) fn realm_is_top_document(scope: &mut v8::PinScope) -> bool {
    let embedded = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.has::<crate::core::html::remote_frame::FrameEmbedding>()
    };
    in_page_realm(scope) && !embedded
}

/// Whether the running realm's document has focus -- the top-level document of the page (a frame is
/// not focused until something focuses it, and nothing here does). `document.hasFocus()`'s answer.
pub(crate) fn realm_document_focused(scope: &mut v8::PinScope) -> bool {
    realm_is_top_document(scope)
}

pub(crate) fn in_page_realm(scope: &mut v8::PinScope) -> bool {
    let cur = scope.get_current_context();
    main_context(scope) == cur
}

/// The page's own global: "the window" to a Rust callback, whichever context it runs in.
pub(crate) fn main_global<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // read in place: a cloned `Global` is a global-handle create and release per call
    let main = {
        let st = op_state.borrow();
        st.try_borrow::<BrowsingContexts>().and_then(|b| b.main.as_ref()).map(|g| v8::Local::new(scope, g))
    };
    match main {
        Some(c) => c.global(scope),
        None => scope.get_current_context().global(scope),
    }
}

/// The V8 context context `id`'s current document runs in.
fn realm_of<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Context>> {
    if id == MAIN_CTX {
        return Some(main_context(scope));
    }
    let g = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.realm.clone()))?;
    Some(v8::Local::new(scope, &g))
}

/// A new V8 context for a browsing context's document, behind `proxy` when the context
/// already has a WindowProxy -- a navigation keeps the proxy's identity and replaces everything behind
/// it, which is what `iframe.contentWindow` stays across a navigation in a browser.
///
/// It shares two things with the page's context. The SECURITY TOKEN, because the page and its frames
/// read each other's windows directly and cross-origin access is decided by this module's own views
/// (`xo_window`), not by V8's access check -- which, with two tokens, refuses every global-proxy
/// access. And deno_core's two per-context slots (its context state and module map): deno_core reads
/// them off the CURRENT context -- its promise-rejection hook runs in the context of the rejected
/// promise -- and a context without them is a null pointer dereferenced. A frame is therefore the
/// same deno_core realm as the page, with its own JavaScript global; every Web API function it holds
/// is the page realm's, which is the boundary the single-context model already had.
///
/// Its global is made from `Window`'s template: an instance of the interface from birth,
/// carrying its brand, as Blink makes a frame's; [`install_lazy_frame`] gives it `Window.prototype`. Every
/// frame realm is made with the same template, which is what V8 asks of a reused `proxy`.
pub(crate) fn new_realm<'s>(scope: &mut v8::PinScope<'s, '_>, proxy: Option<v8::Local<'s, v8::Object>>) -> v8::Local<'s, v8::Context> {
    let main = main_context(scope);
    let global_template = crate::install::templates::window_global_template(scope);
    let realm = v8::Context::new(scope, v8::ContextOptions { global_template, global_object: proxy.map(Into::into), ..Default::default() });
    // every string `eval` / `Function` asks the realm's CSP and Trusted Types (the code-generation
    // callback, core/trusted_types/tables.rs), as Blink's every context does
    realm.set_allow_generation_from_strings(false);
    let token = main.get_security_token(scope);
    realm.set_security_token(token);
    for slot in [deno_core::CONTEXT_STATE_SLOT_INDEX, deno_core::MODULE_MAP_SLOT_INDEX] {
        let p = main.get_aligned_pointer_from_embedder_data(slot);
        // SAFETY: the pointers are the main realm's, which lives as long as the runtime -- longer
        // than any frame context. deno_core clones the `Rc` behind them on every read and never
        // releases a slot of a context it did not create.
        unsafe { realm.set_aligned_pointer_in_embedder_data(slot, p) };
    }
    // AND NO ANNEX. rusty_v8 gives a context an "annex" (its typed-slot store) the first time embedder
    // data is set -- the loop above -- and frees it from a guaranteed finalizer on a `Weak<Context>`.
    // An annex still alive when the isolate drops is freed AFTER the isolate handle has been nulled,
    // so its `Weak` skips `Global::Reset` and V8's teardown GC then runs the weak callback on freed
    // memory: SIGSEGV, SIGABRT or SIGTRAP at `Engine` drop, at random. deno_core clears its own realm
    // (`clear_all_slots`) before dropping; nothing cleared a frame's. Measured 2026-09-26: the WPT
    // file `permissions-policy-frame-policy-allowed-for-self.https.sub.html` (86 frames) crashed 8 of
    // 22 parallel runs, and the gate's shard 10 twice. The two pointers set above are raw embedder
    // data, not annex slots, so they stay; nothing reads a frame context's annex.
    realm.clear_all_slots();
    // (Priming the realm's own deno_core classes stood here; no class is a deno_core one since P6.2.)
    realm
}

/// A FRAME REALM FROM TEMPLATES: its global is the page global's shape, made the way Blink
/// makes one -- every interface object a LAZY data property that makes the interface from its template
/// on first read (`templates::realm_interface`), in the page global's order and with its attributes;
/// `Window`'s own members from `Window`'s template; `Window.prototype` as the global's prototype.
pub(crate) fn install_lazy_frame(scope: &mut v8::PinScope, realm: v8::Local<v8::Context>) {
    let page = main_global(scope);
    let creator = page;
    let fresh = realm.global(scope);
    crate::binding::hidden::share_ops(scope, creator, fresh);
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    crate::install::surface::enter(scope, spec::SurfaceRealm::Window);
    crate::install::templates::mark_lazy_realm(scope);
    let fresh = realm.global(scope);
    if let Some((_, wproto)) = crate::install::templates::realm_interface(scope, "Window") {
        // and `Window`'s brand: the global is the realm's one Window
        crate::binding::protos::adopt_as(scope, fresh, wproto.into());
    }
    let Some(plan) = lazy_plan(scope, fresh) else { return };
    let window_tpl = crate::install::templates::tpl_by_name(scope, "Window");
    let global_tpl = crate::install::templates::tpl_by_name(scope, crate::install::templates::GLOBAL_TPL);
    // Every name left CONFIGURABLE (the locks come last). The ORDER is made here, not by `furnish_window`'s
    // seat: reading a lazy property's descriptor makes its interface. So the prefix the frame already shares
    // with the page stays, and from the first difference on every name goes in in the page's order -- one
    // the frame has is moved (out, and in again).
    for (k, row) in plan.rows.iter() {
        let name = v8::Local::new(scope, k);
        if matches!(row, LazyRow::Fixed) {
            continue;
        }
        let own = matches!(row, LazyRow::Own).then(|| fresh.get_own_property_descriptor(scope, name)).flatten().and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()).map(|o| descriptor_of(scope, o));
        fresh.delete(scope, name.into());
        match row {
            LazyRow::Fixed => {}
            LazyRow::Lazy(data, enumerable, writable) => {
                let data = v8::Local::new(scope, data);
                let mut attr = v8::PropertyAttribute::NONE;
                if !enumerable {
                    attr = attr | v8::PropertyAttribute::DONT_ENUM;
                }
                if !writable {
                    attr = attr | v8::PropertyAttribute::READ_ONLY;
                }
                fresh.set_lazy_data_property_with_data(scope, name, crate::binding::refs::g(cb_lazy_interface), data.into(), attr, v8::SideEffectType::HasSideEffect, v8::SideEffectType::HasSideEffect);
            }
            LazyRow::Templated(global, enumerable) => {
                let n = name.to_rust_string_lossy(scope);
                let t = if *global { global_tpl.as_ref() } else { window_tpl.as_ref() };
                if let Some(t) = t {
                    crate::install::templates::attach_one(scope, t, &n, fresh);
                }
                let mut flags = v8::PropertyDescriptor::new();
                flags.set_enumerable(*enumerable);
                flags.set_configurable(true);
                fresh.define_property(scope, name, &flags);
            }
            LazyRow::Own => {
                if let Some(mut pd) = own {
                    pd.set_configurable(true);
                    fresh.define_property(scope, name, &pd);
                }
            }
            LazyRow::Copy(d) => {
                let d = v8::Local::new(scope, d);
                let mut pd = descriptor_of(scope, d);
                pd.set_configurable(true);
                fresh.define_property(scope, name, &pd);
            }
        }
    }
    // The legacy factories over THIS realm's element prototypes (their `prototype` is another interface's,
    // which no template carries).
    crate::core::html::legacy_factories::install_window_legacy_factories(scope, fresh);
    if let Some((proto, _)) = crate::install::live::parent_of(scope, "HTMLImageElement") {
        crate::install::live::html_image::install_image_factory(scope, fresh, proto);
    }
    // The realm's own named properties object between `Window.prototype` and `EventTarget.prototype`.
    crate::core::dom::window_named::splice_window_named(scope, fresh);
    for k in plan.locks.iter() {
        let key = v8::Local::new(scope, k);
        if fresh.has_own_property(scope, key).unwrap_or(false) {
            let mut pd = v8::PropertyDescriptor::new();
            pd.set_configurable(false);
            fresh.define_property(scope, key, &pd);
        }
    }
    // The frame's own intrinsics, before any of its scripts: its `Array.from` makes ITS
    // arrays, and a frame script that replaces one cannot reach the engine's algorithms.
    crate::binding::protos::seal_intrinsics(scope);
}

/// This runtime's [`LazyPlan`], worked out from the install-time surface and a fresh frame global (`fresh`,
/// untouched yet) the first time a frame realm is built from templates.
fn lazy_plan(scope: &mut v8::PinScope, fresh: v8::Local<v8::Object>) -> Option<std::rc::Rc<LazyPlan>> {
    if let Some(p) = bc(scope, |b| b.lazy_plan.clone()) {
        return Some(p);
    }
    let surface = frame_surface(scope);
    // Not an index: `window[0]` is the page's own frame, not surface.
    let is_index = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    let surface: Vec<(v8::Local<v8::Name>, v8::Local<v8::Value>)> = surface.into_iter().filter(|(k, _)| k.is_string() && !is_index(&k.to_rust_string_lossy(scope))).collect();
    let page_order: Vec<String> = surface.iter().map(|(k, _)| k.to_rust_string_lossy(scope)).collect();
    let frame_order = own_string_keys(scope, fresh);
    let shared = page_order.iter().zip(&frame_order).take_while(|(a, b)| a == b).count();
    let window_tpl = crate::install::templates::tpl_by_name(scope, "Window");
    let global_tpl = crate::install::templates::tpl_by_name(scope, crate::install::templates::GLOBAL_TPL);
    // The names [`furnish_window`] makes the frame's own and locks itself.
    const FURNISHED: &[&str] = &[
        "window", "self", "document", "frames", "length", "parent", "top", "frameElement", "opener", "setTimeout",
        "setInterval", "queueMicrotask", "closed", "name", "focus", "blur", "postMessage", "location", "console",
    ];
    let mut rows = Vec::new();
    let mut locks = Vec::new();
    for (i, (name, d)) in surface.iter().enumerate() {
        let n = &page_order[i];
        let Ok(d) = v8::Local::<v8::Object>::try_from(*d) else { continue };
        if !FURNISHED.contains(&n.as_str()) && !get_prop(scope, d, "configurable").is_some_and(|v| v.boolean_value(scope)) {
            locks.push(v8::Global::new(scope, *name));
        }
        if i < shared {
            continue;
        }
        let enumerable = get_prop(scope, d, "enumerable").is_some_and(|v| v.boolean_value(scope));
        let own = fresh.get_own_property_descriptor(scope, *name).filter(|v| !v.is_undefined()).and_then(|d| v8::Local::<v8::Object>::try_from(d).ok());
        let movable = own.is_some_and(|o| get_prop(scope, o, "configurable").is_some_and(|v| v.boolean_value(scope)));
        let lazy_page = get_prop(scope, d, LAZY_IFACE).filter(|v| v.is_string());
        let row = if own.is_some() && !movable {
            LazyRow::Fixed
        } else if let Some(iface) = lazy_page {
            // the page's own lazy interface: the frame's is lazy too, from the same template
            let writable = get_prop(scope, d, "writable").is_some_and(|v| v.boolean_value(scope));
            let data = v8::Local::<v8::String>::try_from(iface).ok()?;
            LazyRow::Lazy(v8::Global::new(scope, data), enumerable, writable)
        } else if let Some(iface) = get_prop(scope, d, "value")
            .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
            .and_then(|f| crate::install::templates::interface_name_of_value(scope, f))
        {
            let writable = get_prop(scope, d, "writable").is_some_and(|v| v.boolean_value(scope));
            let data = v8::String::new(scope, &iface)?;
            LazyRow::Lazy(v8::Global::new(scope, data), enumerable, writable)
        } else if window_tpl.as_ref().is_some_and(|t| crate::install::templates::has_global_entry(t, n)) {
            LazyRow::Templated(false, enumerable)
        } else if global_tpl.as_ref().is_some_and(|t| crate::install::templates::has_global_entry(t, n)) {
            LazyRow::Templated(true, enumerable)
        } else if own.is_some() {
            // a V8 intrinsic the page kept as V8 made it stays the frame's own
            LazyRow::Own
        } else {
            LazyRow::Copy(v8::Global::new(scope, d))
        };
        rows.push((v8::Global::new(scope, *name), row));
    }
    let plan = std::rc::Rc::new(LazyPlan { rows, locks });
    bc(scope, |b| b.lazy_plan = Some(plan.clone()));
    Some(plan)
}

/// A property descriptor from a descriptor object (`Object.getOwnPropertyDescriptor`'s answer).
fn descriptor_of<'s>(scope: &mut v8::PinScope<'s, '_>, d: v8::Local<'s, v8::Object>) -> v8::PropertyDescriptor {
    let flag = |scope: &mut v8::PinScope<'s, '_>, k: &str| get_prop(scope, d, k).is_some_and(|v| v.boolean_value(scope));
    let get = get_prop(scope, d, "get").filter(|v| !v.is_undefined());
    let set = get_prop(scope, d, "set").filter(|v| !v.is_undefined());
    let mut pd = if get.is_some() || set.is_some() {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        v8::PropertyDescriptor::new_from_get_set(get.unwrap_or(u), set.unwrap_or(u))
    } else {
        let v = get_prop(scope, d, "value").unwrap_or_else(|| v8::undefined(scope).into());
        let w = flag(scope, "writable");
        v8::PropertyDescriptor::new_from_value_writable(v, w)
    };
    pd.set_enumerable(flag(scope, "enumerable"));
    pd.set_configurable(flag(scope, "configurable"));
    pd
}

/// A lazy interface global's first read: the realm's interface object, made from its template (V8 then
/// makes the name the ordinary data property it reads as).
///
/// A DETACHED GLOBAL'S NAME TOO. When a frame navigates to a new realm behind the same proxy
/// ([`forget_previous_window`]), its old document's code can still run, and its unqualified read of a name
/// it never read before finds the OLD global's lazy property. The old context is detached first, so V8
/// hands such a read a proxy of the old global's own (`global_proxy_for_api`) -- which no context has as its
/// global -- and replaces the property on the old global through it. Such a name is `undefined`, as Chrome
/// 150 answers it (a name the old document read before the navigation keeps its interface):
/// F-detached-frame-names, B24. Without the detach, V8 replaced the name THROUGH THE FRAME'S PROXY -- on
/// the NEW global -- with an interface of the wrong realm, and a second such read found a data property
/// there and died on `Check failed: LookupIterator::ACCESSOR == it.state()`: un.org, whose YouTube embeds
/// the IFrame API re-navigates (exit 133 in 6 of 6 replays; the pre-migration binary, with no
/// per-frame realms, 0 of 4).
pub(crate) fn cb_lazy_interface<'s>(scope: &mut v8::PinScope<'s, '_>, name: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Value>) {
    // The data is the INTERFACE's name, which an alias (`webkitURL`) does not share with its key.
    let n = args.data().to_rust_string_lossy(scope);
    let holder = args.holder();
    // IN THE REALM THAT OWNS THE PROPERTY, not the caller's: a getter runs in the reader's context, and the
    // page reading a frame's global would otherwise make the frame's interface in the PAGE -- where V8's
    // template cache answers the page's own, which then stays on the frame's global. A live holder is its
    // realm's global proxy; a detached global's is not.
    let live = holder.get_creation_context(scope).filter(|c| c.global(scope) == holder);
    let Some(ctx) = live else {
        crate::install::templates::report(&format!("LAZY-DETACHED {n}"));
        rv.set_undefined();
        return;
    };
    let scope = &mut v8::ContextScope::new(scope, ctx);
    match crate::install::templates::realm_interface(scope, &n) {
        Some((c, _)) => {
            let c: v8::Local<v8::Value> = c.into();
            rv.set(c);
        }
        // Nothing to make it from: an install puts a name lazily only on a template hit, and a restore checks every lazy
        // name's source before it commits -- so this is a bug, and says so (the lock's snapshot stage
        // fails on the line)
        None => crate::install::templates::report_snap(&format!("LAZY-UNMADE {n}")),
    }
    // the page's record of its lazy names: this one is made now (an alias's key is not the interface's)
    if crate::install::templates::has_lazy_globals(scope) {
        let key = name.to_rust_string_lossy(scope);
        crate::install::templates::forget_lazy_global(scope, holder, &key);
    }
}

/// Every built nested context with its realm: `(id, realm)`, the frames' and the popups'
/// (the frames' paint timing walks them).
pub(crate) fn built_contexts(scope: &mut v8::PinScope) -> Vec<(u32, v8::Global<v8::Context>)> {
    bc(scope, |b| {
        b.ctxs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.installed)
            .filter_map(|(i, c)| c.realm.clone().map(|r| (i as u32, r)))
            .collect()
    })
}

/// Every nested context's realm that is built -- the frames' and the popups' -- for a walk over all of
/// them (`templates::report_loaded_surfaces`).
pub(crate) fn frame_realms(scope: &mut v8::PinScope) -> Vec<v8::Global<v8::Context>> {
    bc(scope, |b| b.ctxs.iter().filter(|c| c.installed).filter_map(|c| c.realm.clone()).collect())
}

fn make_frame_context(scope: &mut v8::PinScope) -> u32 {
    // the initial about:blank document's clock starts as the context is made, not after its realm's build
    let born = spec::process_ms(std::time::Instant::now());
    let realm = new_realm(scope, None);
    let iwin = realm.global(scope);
    let iwin_g = v8::Global::new(scope, iwin);
    let realm_g = v8::Global::new(scope, realm);
    let id = bc(scope, |b| {
        let mut c = Ctx::new(iwin_g);
        c.clock = spec::PerfClock::since_process_ms(born);
        c.realm = Some(realm_g);
        b.ctxs.push(c);
        (b.ctxs.len() - 1) as u32
    });
    // its clock starts now ([`Ctx::new`]): from here a realm's clock is its own (in an
    // out-of-process frame's engine, whose only nested frame was an `about:blank` no navigation committed, that
    // child read its parent's clock -- `performance.now()` seconds into the parent's life at its creation)
    scope.set_slot(FrameClocks);
    build_frame_realm(scope, id, realm, iwin);
    match parse_frame_doc(scope, id, "") {
        Some(d) => set_ctx_document(scope, id, d),
        None => {
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            set_prop(scope, iwin, "document", null);
        }
    }
    id
}

/// Whether context `id` is a nested one whose realm is built: every frame's, from the moment its context
/// is made (the only kind). The page's realm is never one.
fn realm_installed(scope: &mut v8::PinScope, id: u32) -> bool {
    id != MAIN_CTX && with_ctx(scope, id, |c| c.installed).unwrap_or(false)
}


/// GIVE CONTEXT `id` ITS OWN REALM, when the context is made -- its first document's or a
/// navigation's: the global built from this isolate's templates ([`install_lazy_frame`]), whose interfaces
/// are made on first read, then the frame's own wiring ([`furnish_window`]). EVERY frame, since templates
/// made a realm cheap: 2.69 ms and 0.70 MB per frame (`tools/templates/bench/run.py`, 2026-09-28; a
/// frame sharing the page's realm cost 2.75 ms and 0.45 MB), where the window install run in each frame
/// cost 33.8 ms and 7.4 MB and had to stay opt-in (`KORVUS_F19`, gone). So `frame.contentDocument
/// instanceof frame.contentWindow.Document`, as Chrome 150 answers, and nothing waits for the frame's
/// first script: an object handed out before then no longer keeps a shared prototype, and
/// `document.open()` no longer meets a window that changes under it (`no-new-global.window.js`).
fn build_frame_realm<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, realm: v8::Local<'s, v8::Context>, iwin: v8::Local<'s, v8::Object>) {
    with_ctx(scope, id, |c| c.installed = true);
    install_lazy_frame(scope, realm);
    furnish_window(scope, id, realm, iwin);
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    crate::install::templates::report_surface(scope, "frame");
    // the isolate's heap as the frame's realm is ready: what its `performance.memory` takes away if the frame is
    // another site's (another renderer in Chrome, its heap its own; a same-site frame reads its page's)
    crate::core::timing::performance::mark_frame_heap_baseline(scope);
}

/// Everything a browsing context's window carries beyond what its realm's templates made
/// ([`install_lazy_frame`]): the context's own members, in the page global's order.
fn furnish_window<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, realm: v8::Local<'s, v8::Context>, iwin: v8::Local<'s, v8::Object>) {
    // The realm already has the page's install-time surface, in the page's order, as lazy properties;
    // what follows overwrites the names a frame owns. The frame-surface COPY that stood
    // here -- the page's own descriptors, filled over placeholders, for frames that shared the page's
    // realm -- went with that kind of frame.
    //
    // IN THE FRAME'S REALM: every function built below is the frame's own. Built from the page's context,
    // they were the page's -- measured 2026-09-28 on Chrome 150.0.7871.46 (headless, same-origin frame):
    // `Object.getPrototypeOf(frame.setTimeout) === frame.Function.prototype` is true for the timers,
    // `queueMicrotask`, `postMessage`, `focus`/`blur`, the `parent`/`length`/`frameElement`/`closed`/`name`
    // getters and `console`'s members, where this engine answered false for all of them.
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    let surface = frame_surface(scope);
    // What V8 itself put on the fresh global, before anything below adds to it.
    let intrinsics = own_string_keys(scope, iwin);
    if let Some(pname) = v8::String::new(scope, P_CTX) {
        let key = v8::Private::for_api(scope, Some(pname));
        let idv = u32_data(scope, id);
        iwin.set_private(scope, key, idv);
    }
    // `window`, `self`, `frames` and `opener` are the Window template's accessors, which answer for any
    // window they are read off -- `self`/`frames` the receiver, `opener` null for a nested context. They
    // were written over here with data properties, which ran their `[Replaceable]` setters: a frame's
    // `self`, `frames` and `opener` were data properties where Chrome 150.0.7871.46 has the accessors
    // (measured).
    // `document` is the context's own, set by [`set_ctx_document`] once its document exists. It is
    // CLAIMED here, before the fill below, which would otherwise copy the page's `[LegacyUnforgeable]`
    // accessor over it -- and a frame whose `document` answers the page's finds none of its elements.
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    set_prop(scope, iwin, "document", null);
    // `length` counts the nested contexts INSIDE this frame.
    // Each function from its template, the frame found from the realm that runs it; the
    // `[Replaceable]` setters are the page's, as Chrome's frame has them (B21).
    let g = template_fn(scope, cb_iwin_length, "get length", 0, None);
    let s = template_fn(scope, crate::core::html::window_globals::cb_window_replace, "set length", 1, Some("length"));
    define_accessor(scope, iwin, "length", g, Some(s), true, true);
    // `parent`, `top` and `frameElement` answer per CALLER: a cross-origin frame sees
    // its embedder through the cross-origin view -- `top.location.href` and `parent.document` throw
    // SecurityError -- and its `frameElement` is null, as in Chrome; the embedder itself, and a
    // same-origin frame, get the real objects. They were plain data properties holding the page's
    // own window and the iframe element for every caller.
    let g = template_fn(scope, cb_iwin_parent, "get parent", 0, None);
    let s = template_fn(scope, crate::core::html::window_globals::cb_window_replace, "set parent", 1, Some("parent"));
    define_accessor(scope, iwin, "parent", g, Some(s), true, true);
    let g = template_fn(scope, cb_iwin_top, "get top", 0, None);
    // Configurable until [`furnish_window`] locks it, at the end of the window's furnishing.
    define_accessor(scope, iwin, "top", g, None, true, true);
    let g = template_fn(scope, cb_iwin_frame_element, "get frameElement", 0, None);
    define_accessor(scope, iwin, "frameElement", g, None, true, true);
    // A nested context has no opener: the template's `opener` answers null (an auxiliary context's is its own).
    for n in ["setTimeout", "setInterval"] {
        let f = template_fn(scope, cb_iwin_timer, n, 1, Some(n));
        set_prop(scope, iwin, n, f.into());
    }
    let f = template_fn(scope, cb_iwin_queue_microtask, "queueMicrotask", 1, None);
    set_prop(scope, iwin, "queueMicrotask", f.into());
    let g = template_fn(scope, cb_iwin_closed, "get closed", 0, None);
    define_accessor(scope, iwin, "closed", g, None, true, true);
    // `name` is the CONTEXT's name -- the frame element's `name` attribute when the context is made
    // ([`frame_ctx_for`]), writable from inside -- never the page window's. The same pair an
    // auxiliary context's `name` is.
    let g = template_fn(scope, cb_iwin_name_get, "get name", 0, None);
    let s = template_fn(scope, cb_iwin_name_set, "set name", 1, None);
    define_accessor(scope, iwin, "name", g, Some(s), true, true);
    // Focus/blur are the spec's no-ops here for the same reason the page window's are: there is no UI
    // to move focus in.
    for n in ["focus", "blur"] {
        let f = template_fn(scope, cb_iwin_focus_blur, n, 0, None);
        set_prop(scope, iwin, n, f.into());
    }
    // Enumerable, as every Window operation is (it was DONT_ENUM: B21).
    let f = template_fn(scope, cb_iwin_post_message_checked, "postMessage", 1, None);
    let k = name_key(scope, "postMessage");
    iwin.define_own_property(scope, k, f.into(), v8::PropertyAttribute::NONE);
    install_location(scope, id);
    // `console` is the frame's own, built by the SAME install the page's is: a console is per realm
    // in Chrome, and a page patches it -- a frame that wraps `console.error` must wrap ITS console.
    // It replaces the one V8 gives every new context.
    crate::modules::console_ns::install_console(scope, iwin, true);
    // V8's own `WebAssembly` has no streaming members without a host callback; the page's got
    // `compileStreaming`/`instantiateStreaming` from its install, and so does this realm's.
    {
        let scope = &mut v8::ContextScope::new(scope, realm);
        crate::modules::wasm_streaming::install_wasm_streaming_rows(scope);
    }
    let locks = ["top", "location"];
    // An intrinsic the page's surface does not have is not the frame's either: V8 gives every context
    // `SharedArrayBuffer`, and the page's install takes it away from a document that is not
    // cross-origin isolated (`isolation_policy::isolation_gate`).
    let known: HashSet<String> = surface.iter().map(|(k, _)| js_string(scope, (*k).into())).collect();
    // ...but the page's surface is its BOOT surface, taken before any document said whether it is
    // isolated. A frame of a cross-origin-isolated document is isolated too and keeps V8's
    // `SharedArrayBuffer` (`H-coi`: Chrome 150 exposes it in a COEP iframe of a COOP+COEP
    // page; korvus answered `crossOriginIsolated === true` there with no `SharedArrayBuffer`).
    let isolated = crate::core::html::isolation_policy::cross_origin_isolated(scope);
    // ...and where Chrome keeps it: LAST. V8 installs a per-context `SharedArrayBuffer`
    // (`--enable-sharedarraybuffer-per-context`) once the embedder's global is built, so in Chrome 150 it
    // follows every Web API member -- 1,223rd of an isolated frame's 1,225 own names; here it sat where V8's
    // context bootstrap leaves it, 61st, after `isNaN` (the challenge frame posts its
    // window's own names in order). Taken off with the rest and put back at the end, below.
    let mut sab: Option<v8::Local<v8::Value>> = None;
    for k in intrinsics {
        if isolated && k == "SharedArrayBuffer" {
            let key = name_key(scope, &k);
            sab = iwin.get(scope, key.into()).filter(|v| !v.is_undefined());
            iwin.delete(scope, key.into());
        } else if !known.contains(&k) {
            let key = name_key(scope, &k);
            iwin.delete(scope, key.into());
        }
    }
    if let Some(v) = sab {
        // the descriptor V8 gives it: writable, configurable, not enumerable
        let key = name_key(scope, "SharedArrayBuffer");
        iwin.define_own_property(scope, key, v, v8::PropertyAttribute::DONT_ENUM);
    }
    // The window's own keys already enumerate in the PAGE's sequence (Chrome runs one install
    // for both): a pass that re-seated them in the page's order (`seat_in_page_order`) moved nothing in 74
    // frame realms -- the lock's five bundles and every fixture -- and it was deleted.
    for name in locks {
        let key = name_key(scope, name);
        let mut pd = v8::PropertyDescriptor::new();
        pd.set_configurable(false);
        iwin.define_property(scope, key, &pd);
    }
}

fn own_string_keys(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Vec<String> {
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
        .index_filter(v8::IndexFilter::SkipIndices)
        .build();
    let Some(names) = o.get_own_property_names(scope, args) else { return Vec::new() };
    let mut out = Vec::with_capacity(names.length() as usize);
    for i in 0..names.length() {
        if let Some(k) = names.get_index(scope, i) {
            out.push(js_string(scope, k));
        }
    }
    out
}

/// The page realm's install-time global surface, taken ONCE, before any page script runs: every own
/// string key of the global, in order, with its property descriptor.
///
/// Called by the runtime right after `seal_interface_protos`, whose contract it shares -- after the
/// last install, before the first page script -- and for the same reason: a frame's window must be
/// built from what the ENGINE put on the global, never from what a page's scripts later added to it.
/// A page's `var R` or `function done(){}` is the page's, and a nested context in Chrome does not
/// see it (measured: `contentWindow.R` is `undefined` there).
pub fn seal_frame_surface(scope: &mut v8::PinScope) {
    let (names, descs) = read_global_surface(scope);
    let names = v8::Global::new(scope, names);
    let descs = v8::Global::new(scope, descs);
    let main = scope.get_current_context();
    let main = v8::Global::new(scope, main);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(b) = st.try_borrow_mut::<BrowsingContexts>() {
        b.surface = Some((names, descs));
        b.main = Some(main);
    }
}

/// The field of a sealed-surface row that names a page interface put lazily ([`read_global_surface`]).
const LAZY_IFACE: &str = "korvusLazyInterface";

fn read_global_surface<'s>(scope: &mut v8::PinScope<'s, '_>) -> (v8::Local<'s, v8::Array>, v8::Local<'s, v8::Array>) {
    let global = global_obj(scope);
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
        .index_filter(v8::IndexFilter::SkipIndices)
        .build();
    let names = global.get_own_property_names(scope, args).unwrap_or_else(|| v8::Array::new(scope, 0));
    let descs = v8::Array::new(scope, names.length() as i32);
    let any_lazy = crate::install::templates::has_lazy_globals(scope);
    for i in 0..names.length() {
        let Some(k) = names.get_index(scope, i) else { continue };
        let Ok(key) = v8::Local::<v8::Name>::try_from(k) else { continue };
        // A page interface put lazily and not made yet: its row, not its descriptor -- reading that would
        // make it, at the end of every page install. [`lazy_plan`] reads the row as the frame's own lazy name.
        if any_lazy {
            let n = key.to_rust_string_lossy(scope);
            if let Some((iface, enumerable, writable)) = crate::install::templates::lazy_global(scope, global, &n) {
                let d = v8::Object::new(scope);
                for (f, v) in [("enumerable", enumerable), ("writable", writable), ("configurable", true)] {
                    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, v).into();
                    set_prop(scope, d, f, b);
                }
                if let Some(s) = v8::String::new(scope, &iface) {
                    set_prop(scope, d, LAZY_IFACE, s.into());
                }
                descs.set_index(scope, i, d.into());
                continue;
            }
        }
        // A DESCRIPTOR, never a `get`: reading the global by name fires its accessors (and mints
        // their singletons) -- see `seal_interface_protos`.
        let d = global.get_own_property_descriptor(scope, key).unwrap_or_else(|| v8::undefined(scope).into());
        descs.set_index(scope, i, d);
    }
    (names, descs)
}

/// The surface a frame is built from: the sealed one, or -- in a realm that never sealed (a bare
/// test runtime) -- the global as it is now.
fn frame_surface<'s>(scope: &mut v8::PinScope<'s, '_>) -> Vec<(v8::Local<'s, v8::Name>, v8::Local<'s, v8::Value>)> {
    let sealed = bc(scope, |b| b.surface.clone());
    let (names, descs) = match sealed {
        Some((n, d)) => (v8::Local::new(scope, &n), v8::Local::new(scope, &d)),
        None => read_global_surface(scope),
    };
    let mut out = Vec::with_capacity(names.length() as usize);
    for i in 0..names.length() {
        let Some(k) = names.get_index(scope, i) else { continue };
        let Ok(k) = v8::Local::<v8::Name>::try_from(k) else { continue };
        let d = descs.get_index(scope, i).unwrap_or_else(|| v8::undefined(scope).into());
        out.push((k, d));
    }
    out
}

/// A frame's document is a REAL parsed document: `DOMParser.parseFromString(html, "text/html")`, parsed IN
/// THE FRAME'S REALM, so what a document is minted with -- its own `[LegacyUnforgeable]` `location`
/// accessor pair -- is the frame's. Parsed from the page's context, that pair was the page's (the surface
/// lock's `/foreign` marker found it).
fn parse_frame_doc<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, html: &str) -> Option<v8::Local<'s, v8::Object>> {
    parse_frame_doc_as(scope, id, html, false)
}

/// [`parse_frame_doc`], for an `about:srcdoc` document when `srcdoc`: never in a quirks mode.
fn parse_frame_doc_as<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, html: &str, srcdoc: bool) -> Option<v8::Local<'s, v8::Object>> {
    // Natively, not through `new DOMParser().parseFromString`: those are the page's globals.
    let Some(realm) = realm_of(scope, id) else { return Some(crate::core::xml::dom_parser::parse_html_document_as(scope, html, srcdoc)) };
    let g = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let d = crate::core::xml::dom_parser::parse_html_document_as(scope, html, srcdoc);
        v8::Global::new(scope, d)
    };
    Some(v8::Local::new(scope, &g))
}

/// Replace a frame's document with the tree `html` parses to, then run its scripts in the frame's
/// scope; the context's load event fires once every script has run.
fn load_frame_html(scope: &mut v8::PinScope, id: u32, html: &str) {
    load_frame_typed(scope, id, html, None);
}

/// [`load_frame_html`], the document a response of MIME type `mime` made ([`frame_document_source`]).
fn load_frame_typed(scope: &mut v8::PinScope, id: u32, html: &str, mime: Option<String>) {
    // `frame_load_srcdoc` sets the location first, so an `about:srcdoc` URL here is that navigation.
    let href = loc_href(scope, id);
    let srcdoc = starts_with_ci(&href, "about:srcdoc");
    // a document no response delivered (`about:srcdoc`, `about:blank`, a `blob:`) holds its parent's policies
    if is_local_scheme(&href) {
        inherit_csp(scope, id);
    }
    begin_image_document(scope, id, mime.as_deref(), &href);
    if let Some(d) = parse_frame_doc_as(scope, id, html, srcdoc) {
        set_ctx_document(scope, id, d);
        record_frame_content_type(scope, id, mime);
    }
    begin_frame_document(scope, id);
    run_doc_scripts(scope, id);
}

/// The frame `id`'s new document, about to be parsed from the response to `url` of MIME type `mime`: when that type
/// selects the image viewer, its `<img>` takes the navigation's response and never asks the network
/// (`subresource::ImageDocumentContent`). Both ways a frame gets a document from a response pass here -- an element's
/// navigation ([`frame_navigated`]) and a context's ([`load_frame_typed`]) -- so any other document clears the entry.
fn begin_image_document(scope: &mut v8::PinScope, id: u32, mime: Option<&str>, url: &str) {
    let image = mime.and_then(crate::core::html::text_viewer::viewer_for) == Some(crate::core::html::text_viewer::Viewer::Image);
    crate::core::html::subresource::begin_frame_image_document(scope, id, image.then_some(url));
}

/// What a frame's response becomes, by its MIME type -- the page's rule, through the same two functions (A/B C5):
/// the network stack's type (`transport::mime::mime_type_and_charset`, every `Content-Type` value, the last valid one
/// winning) and the viewer it selects (`text_viewer::viewer_for`). A frame parsed EVERY body as HTML: a `text/plain`
/// frame ran the markup in it, and a joined `text/html;charset=gbk,text/plain` (Chrome: the text viewer) ran its
/// scripts. Returns the markup to parse and the `contentType` its document reports when that is not `text/html`
/// (`None`: the HTML parser's own `text/html`). NOT MODELLED: an XML response is still parsed as HTML here, and a
/// frame's `characterSet` is its page's (the frame body arrives as text, not bytes).
fn frame_document_source(headers: &[(String, String)], body: &str, url: &str) -> (Option<String>, Option<String>) {
    use crate::core::html::text_viewer::{image_document, json_document, text_document, viewer_for, Viewer};
    let mime = transport::mime::mime_type_and_charset(headers).mime;
    match viewer_for(&mime) {
        Some(Viewer::Text) => (Some(text_document(body)), Some(mime)),
        Some(Viewer::Json) => (Some(json_document(body)), Some(mime)),
        Some(Viewer::Image) => (Some(image_document(url, None)), Some(mime)),
        None => (None, None),
    }
}

/// Record the `contentType` a frame's new document reports, or clear it (`None`: its parser's own).
fn record_frame_content_type(scope: &mut v8::PinScope, id: u32, mime: Option<String>) {
    let Some(root) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.doc_root)) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<crate::core::dom::document::DocContentTypes>().is_none() {
        st.put(crate::core::dom::document::DocContentTypes::default());
    }
    let map = &mut st.borrow_mut::<crate::core::dom::document::DocContentTypes>().0;
    match mime {
        Some(m) => {
            map.insert(root, m);
            // A viewer's document is in no-quirks mode, as the page's is (`navigation.rs`; Chrome 150 measured).
            if let Some(h) = st.try_borrow_mut::<HostDom>() {
                h.0.set_document_mode(root, korvus_dom::DocumentMode::NoQuirks);
            }
        }
        None => {
            map.remove(&root);
        }
    }
}

/// A child context's load event, on a later task. BOTH targets fire, because in a browser both do:
/// the `<iframe>` ELEMENT gets a `load` event and so does the frame's own WINDOW. An auxiliary context
/// has no element, so only the window fires. The frame's navigation has ENDED, so it stops delaying
/// its parent document's load event -- after the two dispatches, because a browser fires a child's
/// load before the parent's.
fn ctx_loaded(scope: &mut v8::PinScope, id: u32) {
    let d = u32_data(scope, id);
    let f = build_fn_data(scope, cb_ctx_loaded_fire, "", 0, d);
    set_timeout(scope, f, 0);
}
fn cb_ctx_loaded_fire(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    frame_loaded_now(scope, id);
}

/// Is a frame in context `id`'s document still loading -- Blink's `AllDescendantsAreComplete` false, which keeps
/// the document's load waiting for it?
fn child_frames_loading(scope: &mut v8::PinScope, id: u32) -> bool {
    let Some(root) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.doc_root)) else { return false };
    let frames: Vec<(u32, Readiness)> = bc(scope, |b| b.ctxs.iter().filter_map(|c| c.el.map(|e| (e, c.readiness))).collect());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(h) = st.try_borrow::<HostDom>() else { return false };
    frames.iter().any(|&(el, r)| r != Readiness::Complete && h.0.shadow_including_root(el) == root)
}

/// A frame document's load steps: `complete`, the load event at its window, then the iframe load event steps at
/// its element (HTML "the end" then "completely finish loading"; Chrome 150, measured on srcdoc, same-origin `src`
/// and `document.close()`: the window's `load` first). The element's came first here.
fn frame_loaded_now(scope: &mut v8::PinScope, id: u32) {
    set_frame_readiness(scope, id, Readiness::Complete);
    // Blink's `CheckCompletedInternal`: the load event only when it has not run (`LoadEventStillNeeded`), and not while
    // it is being dispatched (`ShouldComplete`). An `open()` from inside the window's or the frame element's `load`
    // handler reopens the document with its load event still in progress (`ImplicitOpen` keeps it), so that
    // document's `close()` ends `complete` and fires no second `load` -- Chrome 150, measured: an `iframe.onload` that
    // writes and closes the frame's document runs once. Here it ran again inside `close()`, and again, until WPT's
    // `query-encoding/*?include=base-href` overflowed the stack.
    if with_ctx(scope, id, |c| c.load_event != FrameLoadEvent::NotRun).unwrap_or(false) {
        return;
    }
    with_ctx(scope, id, |c| c.load_event = FrameLoadEvent::InProgress);
    let el = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el));
    if let Some(iwin) = window_of(scope, id) {
        note_frame_milestone(scope, id, "loadEventStart");
        fire_at(scope, iwin.into(), "load");
        note_frame_milestone(scope, id, "loadEventEnd");
    }
    if let Some(el) = el {
        // a navigation with no timing of its own (one CSP refused) has its entry now, before the element's `load`
        crate::core::timing::resource_timing::frame_fallback_report(scope, el);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let w = wrap_cached(scope, &op_state, el);
        fire_at(scope, w.into(), "load");
        undelay_load(scope, el);
    }
    with_ctx(scope, id, |c| c.load_event = FrameLoadEvent::Completed);
    // the frame document's `fonts.ready` first resolves on the task after its load event (F9)
    if let Some(doc) = doc_of(scope, id) {
        crate::core::fonts::font_face::load_event_finished(scope, doc.into());
    }
    schedule_frame_refresh(scope, id);
}

/// A frame's OWN declarative refresh: the first valid `<meta http-equiv=refresh>` of its document,
/// timed from its load, navigates THAT frame -- Chrome 150 (measured, `tools/realweb/seed/framerefresh`); the page's
/// scan reads only the page's document (U20). The timer is dropped if the frame shows another document by then.
fn schedule_frame_refresh(scope: &mut v8::PinScope, id: u32) {
    let Some(root) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.doc_root)) else { return };
    let url = frame_request_client(scope, id).0;
    let found = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return };
        let dom = &h.0;
        dom.get_elements_by_tag_name_in(root, "meta").into_iter().find_map(|n| {
            let refresh = dom.get_attribute(n, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("refresh"));
            let content = dom.get_attribute(n, "content").filter(|c| refresh && !c.is_empty())?;
            crate::page::parse_declarative_refresh(content, &url, &url)
        })
    };
    let Some((delay, target)) = found else { return };
    let ms = (delay * 1000.0).clamp(0.0, i32::MAX as f64) as i32;
    let data = v8::Array::new(scope, 3);
    let (i, r, t) = (v8::Integer::new_from_unsigned(scope, id), v8::Integer::new_from_unsigned(scope, root), str_val(scope, &target));
    data.set_index(scope, 0, i.into());
    data.set_index(scope, 1, r.into());
    data.set_index(scope, 2, t);
    let f = build_fn_data(scope, cb_frame_refresh, "", 0, data.into());
    set_timeout(scope, f, ms);
}

fn cb_frame_refresh(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Some(data) = v8::Local::<v8::Array>::try_from(data).ok() else { return };
    let num = |scope: &mut v8::PinScope, i: u32| data.get_index(scope, i).and_then(|v| v.uint32_value(scope));
    let (Some(id), Some(root)) = (num(scope, 0), num(scope, 1)) else { return };
    let Some(target) = data.get_index(scope, 2).map(|v| v.to_rust_string_lossy(scope)) else { return };
    // Still the document that asked: a frame navigated since has no refresh of that one's.
    if bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.doc_root)) != Some(root) {
        return;
    }
    // And still SHOWN: a frame removed from its document is discarded, and its refresh with it (WPT
    // `abort-refresh-multisecond-*`: a removed 1-second frame's refresh disturbed the next subtest).
    if !context_fully_active(scope, id) {
        return;
    }
    navigate_target(scope, Some(id), NavigationTarget::Context(id), PageNavigation::get(target, NavigationReason::MetaTagRefresh));
}

// ── "delaying the load event" ──────────────────────────────────────────────────────────────────────
//
// HTML gives every Document a SET of things that DELAY THE LOAD EVENT, and the document's `load`
// fires only once that set is empty. A parser-produced `<iframe src>` is in it from the moment the
// element is created until its navigation ends -- which is why, in a real browser, a page's `onload`
// handler can read `frames[0].document` and find the frame's content there. Measured on
// `encoding/legacy-mb-*`, whose `<body onload>` reads spans out of a subframe: without the set, load
// fired while the frame was still `about:blank` with 0 spans, and 320 files left the denominator.
//
// A FRAME'S TOKEN IS THE FRAME ELEMENT. The increment lives where the navigation is issued
// (core/html/subresource.rs) and the decrement where that navigation ends (`ctx_loaded`), so both
// name the same object, a double release is a no-op instead of a negative count, and a frame that
// returns early without navigating never took a token to begin with.
//
// EVERY OTHER SUBRESOURCE'S TOKEN IS ANONYMOUS ([`new_load_delay_token`]), because the element is
// not a unique key there: one `<video src poster>` induces TWO loads, and a `<img>` whose `src` is
// rewritten induces a second while the first is still in flight. Two loads sharing one token
// release each other early -- a silent under-delay rather than a hang, and exactly the kind of
// almost-right that is impossible to see in a scoreboard. The counter costs one `u32`.

/// The delay set emptied with the load step parked: CHECK AGAIN FROM A TASK, as Blink does
/// (`Document::DecrementLoadEventDelayCount` -> the zero-delay `load_event_delay_timer_` ->
/// `CheckCompleted`, which asks `IsDelayingLoadEvent()` anew). A load started between the release and
/// that task -- a script a running script inserts from a timer, a frame -- delays the event like any
/// other, and the step waits for it to end (a challenge provider's orchestrator inserts an interactive-widget challenge
/// `api.js` from a task just after its own fetch released the last token; korvus fired `load` with that
/// script still in flight, and the script read `readyState` `complete` where Chrome's reads
/// `interactive`).
fn load_delay_drained(scope: &mut v8::PinScope) {
    if !bc(scope, |b| b.delay.set.is_empty() && b.delay.waiter.is_some()) {
        return;
    }
    let check = build_fn(scope, cb_load_step_check, "", 0);
    set_timeout(scope, check, 0);
}

/// The task [`load_delay_drained`] queues: run the parked load step if nothing delays it any more;
/// otherwise leave it parked -- the release that empties the set queues the next check -- with the
/// starvation guard armed, as [`when_load_delays_done`] arms it for a step that parks at once.
fn cb_load_step_check(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    load_step_check(scope);
}

fn load_step_check(scope: &mut v8::PinScope) {
    let waiter = bc(scope, |b| {
        if !b.delay.set.is_empty() || b.delay.waiter.is_none() {
            return None;
        }
        b.delay.starve_limit = None;
        b.delay.realm = None;
        if let Some(w) = &b.delay.watchdog {
            w.disarm();
        }
        b.delay.waiter.take()
    });
    match waiter {
        Some(w) => {
            let f = v8::Local::new(scope, &w);
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            let _ = f.call(scope, undef, &[]);
        }
        None if bc(scope, |b| b.delay.waiter.is_some() && b.delay.starve_limit.is_none()) => {
            let cap = bc(scope, |b| b.delay.cap_ms);
            arm_load_starvation_guard(scope, cap);
        }
        None => {}
    }
}
/// HTML's "update the current document readiness": set the value, then fire `readystatechange` at
/// the document. The two halves are ONE operation here on purpose — a caller that could set the
/// string without firing the event is exactly the drift this goal exists to remove, and the
/// lifecycle's `readystatechange` used to be fired from the runtime's JS with no state behind it.
///
/// `notify` is false for the one transition the spec does not call an update: a document is CREATED
/// with readiness `loading`, so `complete -> loading` at the start of a navigation is initialisation,
/// not a change, and Chrome fires nothing there (there is no document yet that could be listening).
/// Every other transition fires.
///
/// `korvus-runtime`'s `load_page_inner` reaches this three times per navigation, through
/// `webapi::page::set_document_readiness`: `loading` before any script of the document runs,
/// `interactive` when the classic scripts are done and before the deferred module scripts +
/// `DOMContentLoaded`, and `complete` immediately before the `load` event — the spec's order, and
/// Chrome's measured order.
pub(crate) fn set_doc_readiness(scope: &mut v8::PinScope, to: Readiness, notify: bool) {
    let changed = bc(scope, |b| {
        let was = b.readiness;
        b.readiness = to;
        was != to
    });
    // (the new document's empty "delay the load event" set is [`reset_load_delays`]'s, before the document is installed)
    if !changed || !notify {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(doc) = crate::binding::protos::document_val(scope, &op_state) else { return };
    fire_at(scope, doc, "readystatechange");
}

/// A DOCUMENT IS CREATED WITH AN EMPTY "delay the load event" SET. Without the reset the set is per-ISOLATE rather
/// than per-document: a load the previous document started and never finished (a fetch that outlived its page, an
/// element removed mid-flight) is still holding a token, and the NEXT document's load step parks behind it for good.
/// One `Engine` that navigates twice is the normal shape of the CLI and of every multi-page test, so this is not a
/// corner. Called by the host BEFORE it installs the document: installing it starts the document's `<iframe srcdoc>`
/// navigations, whose tokens are the new document's (cleared with the readiness reset after them, a page
/// whose network frames finished first fired `load` while its srcdoc frame was still `about:blank`).
pub(crate) fn reset_load_delays(scope: &mut v8::PinScope) {
    bc(scope, |b| {
        b.delay.set.clear();
        b.delay.waiter = None;
        b.delay.starve_limit = None;
        b.delay.realm = None;
        if let Some(w) = &b.delay.watchdog {
            w.disarm();
        }
    });
}

/// The page document's current readiness, for `document.readyState` (`core/dom/document.rs`).
pub(crate) fn doc_readiness(scope: &mut v8::PinScope) -> Readiness {
    bc(scope, |b| b.readiness)
}

pub(crate) fn delay_load(scope: &mut v8::PinScope, token: u32) {
    // A load a FRAME document starts before its load event has run delays that document's load -- the frame's own
    // set, which the page's load waits on through the frame element's token. Every other load is the page's.
    let frame = running(scope).filter(|&id| id != MAIN_CTX && with_ctx(scope, id, |c| c.load_event == FrameLoadEvent::NotRun).unwrap_or(false));
    if let Some(id) = frame {
        with_ctx(scope, id, |c| {
            c.delays.insert(token);
        });
        return;
    }
    bc(scope, |b| {
        b.delay.set.insert(token);
    });
}
/// A delay token that names no DOM node, for a subresource load (see the block comment above). The
/// high bit is set so one of these can never collide with a frame's element-id token: arena node ids
/// are dense from 0 and a document that reached 2^31 nodes has run out of memory long before.
pub(crate) fn new_load_delay_token(scope: &mut v8::PinScope) -> u32 {
    bc(scope, |b| {
        b.next_delay_token = b.next_delay_token.wrapping_add(1);
        0x8000_0000 | (b.next_delay_token & 0x7fff_ffff)
    })
}
pub(crate) fn undelay_load(scope: &mut v8::PinScope, token: u32) {
    if bc(scope, |b| b.delay.set.remove(&token)) {
        load_delay_drained(scope);
        return;
    }
    // a frame document's: its load steps run once its last load is in, on a task of their own (Blink's
    // `CheckLoadEventSoon`)
    let Some(id) = bc(scope, |b| b.ctxs.iter().position(|c| c.delays.contains(&token))) else { return };
    let id = id as u32;
    let run = with_ctx(scope, id, |c| {
        c.delays.remove(&token);
        let run = c.delays.is_empty() && c.load_parked;
        if run {
            c.load_parked = false;
        }
        run
    })
    .unwrap_or(false);
    if run {
        ctx_loaded(scope, id);
    }
}
/// Run `f` -- the document's load-event step -- when nothing delays the load event any more. With an
/// empty set it runs at once, in the parse's end; with a non-empty set it waits for the last
/// release, HOWEVER LONG THAT TAKES.
///
/// THERE IS NO DEADLINE, BECAUSE CHROME HAS NONE. Until this unit `cap_ms` was one: 1000
/// ms after parking, the set was cleared and `load` fired with its subresources still in flight. On
/// the web that is every page whose async loader inserts a bundle that takes over a second, and whose
/// `load` handler then starts an app that is not there yet. Chrome 150, loopback server that really
/// sleeps (the table is in `tests/load_delay_chrome_live.rs`): an inserted script of 1.5 s, 3 s and 6 s
/// ran before `load` every time, and one that was never answered still had `readyState` `interactive`
/// 20 s later. A fetch that truly never settles is ended by the transport's own read timeout, which
/// releases its token like any other failure -- the network's timeout, not the document's.
///
/// What `cap_ms` bounds now is STARVATION: how long the page may hold the isolate in JavaScript
/// without the host's event loop getting a single turn while this step is parked. The release that
/// would empty the set is itself a task on that loop, so such a page can never load by itself.
/// Measured on `rakuten.co.jp`, 2026-09-11: the page read `document.readyState` 1,058,730
/// times in four seconds waiting for `complete`, and `complete` arrives only through this step. The
/// watchdog ([`CapWatchdog`]) breaks exactly that, from outside the page's queue, and nothing else.
///
/// Cost if wrong: a page whose ONE synchronous task outlasts the limit while a subresource is still in
/// flight gets `load` early -- the same early `load` every such page used to get at the old deadline.
pub(crate) fn when_load_delays_done(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, cap_ms: i32) {
    let fg = v8::Global::new(scope, f);
    let empty = bc(scope, |b| {
        b.delay.waiter = Some(fg);
        b.delay.cap_ms = cap_ms;
        b.delay.set.is_empty()
    });
    if empty {
        // Nothing delays it: the load step runs now, in the task that ended the parse -- Blink's
        // `FrameLoader::FinishedParsing` reaches `Document::ImplicitClose` there when every load is in (Chrome 150,
        // measured: `readystatechange: complete` and `load` before the `setTimeout(0)` an inline
        // script set; korvus ran that timer first). A load that ends later still queues its check
        // ([`load_delay_drained`]), as Blink's `CheckLoadEventSoon` does.
        load_step_check(scope);
        return;
    }
    arm_load_starvation_guard(scope, cap_ms);
}

/// Arm the starvation guard over a parked load step (see [`when_load_delays_done`]).
fn arm_load_starvation_guard(scope: &mut v8::PinScope, cap_ms: i32) {
    let realm = {
        let c = scope.get_current_context();
        v8::Global::new(scope, c)
    };
    let handle = scope.thread_safe_handle();
    let limit = std::time::Duration::from_millis(if cap_ms > 0 { cap_ms as u64 } else { 1000 });
    bc(scope, |b| {
        b.delay.starve_limit = Some(limit);
        b.delay.realm = Some(realm);
        let turns = b.turns.clone();
        b.delay.watchdog.get_or_insert_with(|| CapWatchdog::new(handle, turns)).arm(limit);
    });
}

/// Run the parked load step NOW, with its set cleared. The one release the two host-side paths share.
///
/// It leaves exactly the state a natural drain leaves -- set cleared, waiter taken, limit cleared,
/// watchdog disarmed -- BEFORE the step runs, so a `load` handler that starts another subresource
/// parks a fresh wait instead of re-entering this one, and so a second call (the interrupt landing a
/// moment after the pump released) is a no-op rather than a second `load` event.
///
/// It CALLS the waiter rather than scheduling it: on a starved page a scheduled hop is starved exactly
/// as the release was, and releasing the tokens without reaching the load step changes nothing.
fn release_parked_load_step(scope: &mut v8::PinScope) -> bool {
    let waiter = bc(scope, |b| {
        b.delay.waiter.as_ref()?;
        b.delay.starve_limit = None;
        b.delay.realm = None;
        b.delay.set.clear();
        if let Some(w) = &b.delay.watchdog {
            w.disarm();
        }
        b.delay.waiter.take()
    });
    let Some(w) = waiter else { return false };
    let f = v8::Local::new(scope, &w);
    let recv: v8::Local<v8::Value> = global_obj(scope).into();
    v8::tc_scope!(let tc, scope);
    f.call(tc, recv, &[]);
    tc.reset();
    true
}

/// The watchdog's release: only while the event loop is STILL starved past the limit. The interrupt
/// was requested from another thread and can land after the spin ended, inside something else.
fn release_starved_load_step(scope: &mut v8::PinScope) -> bool {
    let starved = bc(scope, |b| match (b.delay.starve_limit, b.turns.held_for()) {
        (Some(limit), Some(held)) => held >= limit,
        _ => false,
    });
    starved && release_parked_load_step(scope)
}

/// `Engine::pump`'s release, for a document whose event loop has DRAINED -- no timer, no in-flight
/// op, no unresolved promise -- with its load step still parked behind a non-empty set.
///
/// Every token is released by a completion that runs on that loop, so a drained loop proves no release
/// can ever come: the token belongs to a load that ended without releasing (a defect) or to one that
/// cannot finish. Waiting forever turns that document into one that never loads while doing nothing,
/// so the step runs now. This is not a deadline: a loop with a fetch still in flight never drains.
pub fn release_unreleasable_load_delays(scope: &mut v8::PinScope) -> bool {
    if !bc(scope, |b| b.delay.waiter.is_some() && !b.delay.set.is_empty()) {
        return false;
    }
    release_parked_load_step(scope)
}

/// The host event loop's turn record, for `Engine::pump` to stamp (see [`EventLoopTurns`]).
pub fn event_loop_turns(scope: &mut v8::PinScope) -> std::sync::Arc<EventLoopTurns> {
    bc(scope, |b| b.turns.clone())
}

// ── COOP enforcement and navigation ────────────────────────────────────────────────────────────────

/// COOP enforcement for the one navigation this engine really performs. A response whose COOP does
/// not match the navigating context's puts the new document in a NEW browsing-context group, and the
/// handle the opener is holding is left pointing at a context that is no longer in the group: it
/// reports `closed`, it has no length, and the new document's `opener` is null. A context that left
/// the group is no longer reachable BY NAME from inside it: `open(url, "x")` has to create a fresh
/// one rather than hand back the context that just walked out. COOP is a TOP-LEVEL policy: a nested
/// context (an iframe) has none and is never switched.
fn apply_coop(scope: &mut v8::PinScope, id: u32, headers: &[(String, String)], url: &str) {
    let Some((popup, policy, left)) = bc(scope, |b| b.ctxs.get(id as usize).map(|c| (c.popup, c.policy.clone(), c.left_initial_about_blank))) else { return };
    if !popup {
        return;
    }
    let response = resolve_for(scope, headers, url);
    // The context's CURRENT policy. A popup on its initial `about:blank` has no response of its own
    // and inherits the opener's, which is what makes `same-origin-allow-popups` mean anything at all.
    let active = match policy {
        Some(p) => p,
        None => doc_policy(scope),
    };
    let switched = coop_requires_switch(!left, &active, &response);
    if switched {
        bc(scope, |b| {
            let Some(c) = b.ctxs.get_mut(id as usize) else { return };
            c.disowned = true;
            if let (Some(n), Some(realm)) = (c.name.take(), c.name_realm.take()) {
                if let Some(m) = b.named.get_mut(&realm) {
                    if m.get(&n) == Some(&id) {
                        m.remove(&n);
                    }
                }
            }
        });
        if let Some(iwin) = window_of(scope, id) {
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            set_prop(scope, iwin, "opener", null);
        }
    }
    with_ctx(scope, id, |c| {
        c.policy = Some(response);
        c.left_initial_about_blank = true;
    });
}

// ── the navigation a document asks for (extended by P13c) ──────────────────────────────
//
// THE SLOT LIVES HERE because the three things that fill it live here: `location.*` (the runtime's
// `location.rs`), `window.open(url, "_self")` and a form submission whose target is the page itself.
// P12d put it in `korvus-runtime` as a bare `Option<String>`, which the two below could not reach and
// which could not carry a POST's body; one slot, in the crate both callers already link, replaces it.

/// Why a document is being replaced. The names are CDP's `Page.ClientNavigationReason`, which is the
/// vocabulary Chrome reports them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationReason {
    /// `location.href = …`, `assign`, `replace`, `reload`, `window.location = …`.
    ScriptInitiated,
    /// `<meta http-equiv=refresh>`.
    MetaTagRefresh,
    /// A `Refresh` response header.
    HttpHeaderRefresh,
    /// `window.open(url, "_self" | "_top" | "_parent")` — Chrome 150 reports `other` for it
    /// (measured, `tests/navigation.rs`'s P13c table).
    Other,
    /// A `method=get` form submission.
    FormSubmissionGet,
    /// A `method=post` form submission.
    FormSubmissionPost,
}

impl NavigationReason {
    /// The `reason` string CDP carries.
    pub fn as_cdp(self) -> &'static str {
        match self {
            NavigationReason::ScriptInitiated => "scriptInitiated",
            NavigationReason::MetaTagRefresh => "metaTagRefresh",
            NavigationReason::HttpHeaderRefresh => "httpHeaderRefresh",
            NavigationReason::Other => "other",
            NavigationReason::FormSubmissionGet => "formSubmissionGet",
            NavigationReason::FormSubmissionPost => "formSubmissionPost",
        }
    }

    /// Does Chrome announce this navigation with `Page.frameScheduledNavigation` (and clear it with
    /// `Page.frameClearedScheduledNavigation`)? Measured: every reason but `other` does.
    /// `window.open(url, "_self")` navigates immediately and is never "scheduled".
    pub fn is_scheduled(self) -> bool {
        !matches!(self, NavigationReason::Other)
    }

    /// Is this a form submission? Those are queued as a task, so the document that submits still
    /// reaches its own `load` (measured; see the P13c table in `plugins/korvus-cdp/tests/navigation.rs`).
    pub fn is_form_submission(self) -> bool {
        matches!(self, NavigationReason::FormSubmissionGet | NavigationReason::FormSubmissionPost)
    }
}

/// A navigation the current document asked for, and the request that performs it.
#[derive(Debug, Clone, PartialEq)]
pub struct PageNavigation {
    pub url: String,
    pub reason: NavigationReason,
    /// Seconds between being scheduled and being due (`0` for everything but a declarative refresh).
    pub delay_s: f64,
    /// The request method, uppercase. `POST` only for a `method=post` form submission.
    pub method: String,
    /// The submission body, already serialised in the form's encoding -- bytes, as a multipart body's files are.
    pub body: Option<Vec<u8>>,
    /// The `Content-Type` that body travels under.
    pub content_type: Option<String>,
    /// The URL of the document that asked for this navigation -- its request's initiator, which
    /// `Sec-Fetch-Site` and `Referer` are computed from. `None` for one the driver starts
    /// (`korvus fetch`'s own URL, CDP `Page.navigate`), which goes out as a typed one does.
    pub initiator: Option<String>,
    /// A RELOAD of the current document: `location.reload()`, `history.go(0)`, `navigation.reload()`
    /// and a declarative refresh to the document's own URL. Chrome requests one with
    /// `Cache-Control: max-age=0` and the new document reports the type `reload`;
    /// `location.href = location.href` is NOT one (measured: a plain `navigate`).
    pub reload: bool,
    /// When the navigation began -- the moment it was asked for (it falls due at once unless it is a
    /// declarative refresh, whose start is when it falls due): the new document's time origin, before
    /// the old one's `beforeunload` and the request's setup (`transport::FetchRequest::navigation_start`).
    pub started: Option<std::time::Instant>,
    /// The navigation began inside the transient window of a trusted user activation
    /// (`core/events/user_activation.rs`): its request carries `Sec-Fetch-User: ?1`. Chrome reads the
    /// asking frame's `HasTransientUserActivation` when the navigation is requested
    /// (`sec_header_helpers.cc:176-182`); measured, a trusted click that navigates at once sends `?1`, one
    /// 6 s later or a script navigation with no input sends nothing.
    pub user_activated: bool,
    /// Where the document enters the session history (D88 step 2): `navigate_page` decides push or replace as Blink
    /// does, a reload is in place, a traversal goes to its entry.
    pub history: crate::core::html::session_history::HistoryCommit,
    /// Chrome's name for how the entry was reached, when the driver knows better than the reason does: `typed` for
    /// `Page.navigate` (or its `transitionType`), `auto_toplevel` for a new tab's first URL.
    pub transition: Option<String>,
}

impl PageNavigation {
    /// A plain `GET` navigation to `url`.
    pub fn get(url: impl Into<String>, reason: NavigationReason) -> Self {
        PageNavigation {
            url: url.into(),
            reason,
            delay_s: 0.0,
            method: "GET".to_string(),
            body: None,
            content_type: None,
            initiator: None,
            reload: false,
            started: Some(std::time::Instant::now()),
            user_activated: false,
            history: Default::default(),
            transition: None,
        }
    }

    /// A reload of `url`, the current document's own URL.
    pub fn reload(url: impl Into<String>, reason: NavigationReason) -> Self {
        PageNavigation { reload: true, history: crate::core::html::session_history::HistoryCommit::Reload, ..PageNavigation::get(url, reason) }
    }
}

/// The TOP-LEVEL navigation a script of this realm has asked for and nobody has performed yet.
/// The drivers (the CDP server, `korvus fetch`) take it; nothing in the engine performs it, because
/// the fetch and the protocol are theirs.
#[derive(Default)]
pub struct PendingNavigation(pub Option<PageNavigation>);

/// Whether the running realm may navigate the TOP-LEVEL context. A frame sandboxed without
/// `allow-top-navigation` -- or inside one that is, sandbox flags reach every nested context -- may not:
/// measured on Chrome 150 (`c7/repro/framebust`), `top.location.href = x` throws "The current window does not
/// have permission to navigate the target frame to 'x'.", `window.open(x, "_top")` returns null and a `_top`
/// link does nothing, for `sandbox="allow-scripts"`, `"allow-scripts allow-same-origin"` and
/// `"allow-scripts allow-top-navigation-by-user-activation"` (this engine has no user activation). An unsandboxed
/// cross-origin frame MAY (all three navigate). Ad frames are sandboxed: without this, an ad's redirect took
/// the page with it (salon.com and stackoverflow.com ended on googleads' `drt/si`).
pub(crate) fn may_navigate_top(scope: &mut v8::PinScope) -> bool {
    let Some(mut id) = running(scope) else { return true };
    for _ in 0..64 {
        if id == MAIN_CTX || bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.popup)).unwrap_or(true) {
            return true;
        }
        let el = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el));
        if let Some(el) = el {
            let tokens = {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(el, "sandbox").map(str::to_string))
            };
            if tokens.is_some_and(|t| !t.split_ascii_whitespace().any(|x| x.eq_ignore_ascii_case("allow-top-navigation"))) {
                return false;
            }
        }
        match frame_parent_ctx(scope, id) {
            Some(p) => id = p,
            None => return true,
        }
    }
    true
}

/// Chrome's refusal of a top-level navigation to `href` from a sandboxed frame, after its `prefix`.
fn refuse_top_navigation(scope: &mut v8::PinScope, prefix: &str, href: &str) {
    let abs = match running(scope) {
        Some(id) => resolve_in(scope, id, href).unwrap_or_else(|| href.to_string()),
        None => href.to_string(),
    };
    xo_throw(scope, format!("{prefix}: The current window does not have permission to navigate the target frame to '{abs}'."));
}

/// Ask for a TOP-LEVEL navigation. A second ask before the first is performed replaces it, which is
/// what a browser does with two `location.href =` in one script.
pub fn request_top_navigation(scope: &mut v8::PinScope, mut nav: PageNavigation) {
    // A sandboxed frame without `allow-top-navigation` does not get one. The callers that answer
    // the page -- a `Location` setter's SecurityError, `window.open`'s null -- ask `may_navigate_top` first.
    if !may_navigate_top(scope) {
        return;
    }
    // THE DOCUMENT THAT ASKED is the request's initiator: the running frame's document for
    // a frame's `top.location = …`, else the page's. Measured on Chrome 150 / Brave 151: a script's
    // navigation goes out `Sec-Fetch-Site: same-origin` (or `cross-site`) with the asker's `Referer`,
    // where this engine sent the typed-URL shape (`none`, `Sec-Fetch-User: ?1`, no `Referer`).
    if nav.initiator.is_none() {
        let client = match running(scope) {
            Some(id) => frame_request_client(scope, id).0,
            None => page_location_prop(scope, "href", ""),
        };
        if starts_with_ci(&client, "http:") || starts_with_ci(&client, "https:") {
            nav.initiator = Some(client);
        }
    }
    // Sec-Fetch-User: a navigation the PAGE asked for is user-activated when it was asked for
    // inside the transient activation window -- read now, at the request, not when the driver performs it.
    if nav.initiator.is_some() {
        nav.user_activated = crate::core::events::user_activation::has_transient_user_activation(scope);
    }
    // `KORVUS_TRACE_NAV`: who asked -- the realm running and the script's own frames (a frame's
    // navigation that landed on the top, bloomberg.com).
    if std::env::var_os("KORVUS_TRACE_NAV").is_some() {
        let realm = running(scope);
        let mut frames = String::new();
        if let Some(st) = v8::StackTrace::current_stack_trace(scope, 8) {
            for i in 0..st.get_frame_count() {
                if let Some(f) = st.get_frame(scope, i) {
                    let name = f.get_script_name(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default();
                    let func = f.get_function_name(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default();
                    frames.push_str(&format!("\n    at {func} ({name}:{}:{})", f.get_line_number(), f.get_column()));
                }
            }
        }
        eprintln!("[nav] top <- {} ({:?}) running realm {:?}{frames}", nav.url, nav.reason, realm);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<PendingNavigation>().is_none() {
        st.put(PendingNavigation::default());
    }
    st.borrow_mut::<PendingNavigation>().0 = Some(nav);
}

/// The realm whose script is running right now: a nested or auxiliary context's id, or `None` for
/// the page's own realm. What [`choose_target`] resolves `_self` and a relative URL against.
pub fn current_realm(scope: &mut v8::PinScope) -> Option<u32> {
    running(scope)
}

/// The browsing context the element with arena id `el` belongs to -- `None` for the page's own
/// document.
///
/// This is what a form submission resolves its target against, and it is NOT [`current_realm`]: a
/// frame's `onload` handler runs from the PAGE's task queue, so `current` is the page there, and a
/// `<form>` inside that frame with no `target` would have submitted the page instead of the frame
/// (measured against Chrome, which navigates the frame). Which document an element is in is a
/// property of the element; which script happened to call `submit()` is not.
///
/// The element's ARENA ROOT, not its `ownerDocument`: measured on master, an element inside a frame
/// answers the PAGE's document for `ownerDocument` (`f.ownerDocument === top.document` is true in
/// the frame), which is its own defect and not this unit's to fix -- but a target chosen off it
/// would inherit the wrong answer silently. The arena root is what `by_doc` is keyed by.
/// Is the element's node document the one its browsing context is SHOWING -- HTML's "fully active",
/// as far as this engine models it?
///
/// `by_doc` keeps every document a context ever held (a `write()` after `close()` still has to reach
/// it), so an element left behind in a document the frame has navigated away from is still mapped to
/// that frame. Its `doc_root` is the current one, and the difference is what "fully active" means to
/// a form: `html/semantics/forms/form-submission-0/submission-checks.window.js` appends a form to a
/// frame's initial `about:blank`, navigates the frame, and asserts that submitting that form moves
/// nothing.
pub fn element_document_is_current(scope: &mut v8::PinScope, el: u32) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (root, is_document) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let root = dom.root_of(el);
        (root, dom.node_type(root) == 9)
    };
    // CONNECTED is asked of the ARENA, not of `isConnected`: measured on master, an element inside a
    // frame's document answers `isConnected === false` while `document.body.contains(it)` is true --
    // its own defect, not this unit's to fix, but a form that read it would never submit from a
    // frame. A connected element's root is a DOCUMENT node; a `createElement`ed one's is itself.
    if !is_document {
        return false;
    }
    match bc(scope, |b| b.by_doc.get(&root).copied()) {
        // Not a nested context's document: the page's own, which is always the one it is showing.
        None => true,
        Some(ctx) => bc(scope, |b| b.ctxs.get(ctx as usize).and_then(|c| c.doc_root)) == Some(root),
    }
}

pub fn context_of_element(scope: &mut v8::PinScope, el: u32) -> Option<u32> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = {
        let st = op_state.borrow();
        st.borrow::<HostDom>().0.root_of(el)
    };
    bc(scope, |b| b.by_doc.get(&root).copied())
}

/// The V8 context an element's document runs in, when that is a nested context's.
pub(crate) fn realm_of_element<'s>(scope: &mut v8::PinScope<'s, '_>, el: u32) -> Option<v8::Local<'s, v8::Context>> {
    let id = context_of_element(scope, el)?;
    realm_of(scope, id)
}

/// Blink's `HTMLFrameOwnerElement::IsURLAllowed`, the two limits a frame load must pass before it is
/// issued: a URL (fragment ignored) that already names an ancestor document TWICE is recursion, and a
/// page holds at most 1000 frames. Without either, a document that embeds itself -- WPT's served `.py`
/// sources carry `<iframe src=` their own URL in their template -- grew a binary tree of frames until
/// the process died: 884 contexts in 12 s for `embedded-enforcement/required_csp-header-crlf.html`,
/// and a V8 heap-limit abort once every frame had its own realm. Chrome leaves such a
/// frame on its initial `about:blank`.
pub(crate) fn frame_url_refused(scope: &mut v8::PinScope, el: u32, url: &str) -> bool {
    const MAX_FRAMES: usize = 1000;
    if bc(scope, |b| b.ctxs.len()) >= MAX_FRAMES {
        return true;
    }
    let want = strip_fragment(url);
    let mut cur = context_of_element(scope, el).unwrap_or(MAIN_CTX);
    let mut seen = false;
    for _ in 0..64 {
        if strip_fragment(&loc_href(scope, cur)) == want {
            if seen {
                return true;
            }
            seen = true;
        }
        match frame_parent_ctx(scope, cur) {
            Some(p) => cur = p,
            None => break,
        }
    }
    false
}
fn strip_fragment(u: &str) -> &str {
    u.split('#').next().unwrap_or(u)
}

/// The V8 context a NODE belongs to: its node document's realm. `None` is the page's.
///
/// The node document is read off the tree the way `node::node_document` reads it, minus the minting
/// (this runs inside `wrap_cached`): the document at the top of the node's shadow-including tree; for
/// `<template>` contents, the template's; else the document that created or adopted it (`OwnerMap`).
/// A browsing context's document is that context's realm. Any other document -- `createDocument`,
/// `createHTMLDocument`, a `DOMParser` result, `new Document()` -- was made by script, and belongs to
/// the realm that made it, which is where its wrapper was created. Measured 2026-09-28 on Chrome
/// 150.0.7871.46: in a same-origin frame, nodes of all four, template contents and a
/// `createProcessingInstruction` node are instances of the FRAME's interfaces; here they were the
/// page's, because only a context's own document mapped to a realm.
pub(crate) fn realm_of_node<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Context>> {
    if let Some(ctx) = realm_of_element(scope, id) {
        return Some(ctx);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (root, root_is_document, template_host) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let root = dom.shadow_including_root(id);
        if root == dom.root() {
            return None;
        }
        (root, dom.node_type(root) == 9 && !dom.is_shadow_root(root), dom.template_host_of(dom.root_of(id)))
    };
    let doc: v8::Local<v8::Value> = if root_is_document {
        if let Some(ctx) = bc(scope, |b| b.by_doc.get(&root).copied()) {
            return realm_of(scope, ctx);
        }
        // No wrapper means no script has reached the document yet, and nothing of it: the page's.
        let doc = op_state.borrow().borrow::<crate::binding::NodeCache>().local(scope, root)?;
        doc.into()
    } else if let Some(host) = template_host {
        return realm_of_node(scope, host);
    } else {
        let g = {
            let st = op_state.borrow();
            let m = st.try_borrow::<crate::binding::OwnerMap>()?;
            let map = m.0.borrow();
            map.get(&id).or_else(|| map.get(&root)).cloned()
        }?;
        v8::Local::new(scope, &g)
    };
    if let Some(ctx) = context_of_document(scope, doc) {
        return realm_of(scope, ctx);
    }
    let made_in = v8::Local::<v8::Object>::try_from(doc).ok()?.get_creation_context(scope)?;
    (made_in != main_context(scope)).then_some(made_in)
}

/// Re-point a wrapper's prototype into the PAGE's realm: a node made the page document's after its wrapper
/// was minted in a frame's (`document.importNode` of a frame's node).
pub(crate) fn rehome_to_page(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) {
    let main = main_context(scope);
    crate::binding::protos::rehome_in_realm(scope, main, obj);
}

/// Re-point a freshly minted node wrapper's prototype into the node's own realm, when that is a frame's
/// ([`realm_of_node`]).
/// [`rehome_node_wrapper`] for a wrapper not made yet: the prototype node `id`'s wrapper takes -- `proto`, the
/// same interface's in the realm of the document the node belongs to (the wrapper is then
/// minted from the interface's template in that realm).
pub(crate) fn node_proto_in_its_realm<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>, id: u32) -> v8::Local<'s, v8::Object> {
    let none = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<BrowsingContexts>().is_none_or(|b| b.ctxs.is_empty())
    };
    if none {
        return proto;
    }
    match realm_of_node(scope, id) {
        Some(ctx) => {
            let ctx = v8::Local::new(scope, ctx);
            crate::binding::protos::proto_in_realm(scope, ctx, proto)
        }
        None => proto,
    }
}

pub(crate) fn rehome_node_wrapper(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, id: u32) {
    // No nested context, no other realm: the common page pays one map read per wrapper, not a walk.
    let none = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<BrowsingContexts>().is_none_or(|b| b.ctxs.is_empty())
    };
    if none {
        return;
    }
    if let Some(ctx) = realm_of_node(scope, id) {
        crate::binding::protos::rehome_in_realm(scope, ctx, obj);
    }
}

/// The browsing context a `target` keyword or name names, from the realm that is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationTarget {
    /// The page itself: the top-level browsing context.
    Top,
    /// A nested or auxiliary context of this isolate.
    Context(u32),
    /// A context that would have to be CREATED (`_blank`, a name nothing answers to). Chrome creates
    /// one only with transient user activation, which this engine never has -- see the P13c ruling in
    /// `html_form_element.rs`.
    Fresh,
}

/// HTML's "choose a browsing context", for the two callers that choose one by name: `window.open`'s
/// `target` and a form's `target`/`formtarget`. `initiator` is the realm doing the choosing (`None`
/// is the page's own realm); the keywords are ASCII case-insensitive.
///
/// This engine has ONE level of nesting, so `_parent` and `_top` are the same answer from a frame and
/// are the frame's own context from nowhere else.
pub fn choose_target(scope: &mut v8::PinScope, initiator: Option<u32>, target: &str) -> NavigationTarget {
    let name = target.trim();
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "" | "_self" => match initiator {
            Some(id) => NavigationTarget::Context(id),
            None => NavigationTarget::Top,
        },
        "_parent" | "_top" => NavigationTarget::Top,
        "_blank" => NavigationTarget::Fresh,
        _ => match named_child_context(scope, name) {
            Some(id) => NavigationTarget::Context(id),
            None => NavigationTarget::Fresh,
        },
    }
}

/// The nested context whose frame element carries `name`, searched by `name` then by `id` -- the two
/// things a `target` matches for a nested browsing context.
///
/// Through [`frame_ctx_for`], which MINTS the context if the frame has not had one yet: an
/// `<iframe name=x>` with no `src` sits on its initial `about:blank` and has no context until
/// something navigates it, and the something is usually the form that names it
/// (`html/semantics/forms/form-submission-0/`'s `targetted-form.js` builds exactly that).
fn named_child_context(scope: &mut v8::PinScope, name: &str) -> Option<u32> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let frames: Vec<(u32, Option<String>, Option<String>)> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut out = Vec::new();
        for tag in ["iframe", "frame"] {
            for id in dom.get_elements_by_tag_name(tag) {
                out.push((id, dom.get_attribute(id, "name").map(str::to_string), dom.get_attribute(id, "id").map(str::to_string)));
            }
        }
        out
    };
    let mut by_id = None;
    for (el, n, i) in frames {
        if n.as_deref() == Some(name) {
            return Some(frame_ctx_for(scope, el));
        }
        if by_id.is_none() && i.as_deref() == Some(name) {
            by_id = Some(el);
        }
    }
    by_id.map(|el| frame_ctx_for(scope, el))
}

/// Navigate the context a [`choose_target`] answer names, resolving `href` against the INITIATING
/// document. `NavigationTarget::Fresh` navigates nothing: creating a context needs user activation
/// this engine never has.
pub fn navigate_target(scope: &mut v8::PinScope, initiator: Option<u32>, target: NavigationTarget, nav: PageNavigation) {
    match target {
        NavigationTarget::Fresh => {}
        // `window.open(url, "_self")` from the page itself: the page's navigation, `navigate` event included
        NavigationTarget::Top if initiator.is_none() && matches!(nav.reason, NavigationReason::Other) => {
            crate::core::html::navigate_event::navigate_page(
                scope,
                crate::core::html::navigate_event::PageNavigate {
                    nav,
                    history: crate::core::html::navigate_event::HistoryHandling::Auto,
                    reason: crate::core::html::navigate_event::ClientReason::None,
                    source_element: None,
                    user_initiated: false,
                    form_data: None,
                },
            );
        }
        NavigationTarget::Top => request_top_navigation(scope, nav),
        NavigationTarget::Context(id) => {
            if Some(id) == initiator || initiator.is_none() {
                // Same context, or the page navigating one of its own frames: either way the URL is
                // already absolute by the time it gets here.
            }
            // a frame's navigation fetch takes its body as text
            let body = nav.body.as_deref().map(|b| String::from_utf8_lossy(b).into_owned());
            navigate_context_with(scope, id, &nav.url, &nav.method, body.as_deref(), nav.content_type.as_deref());
        }
    }
}

/// Whether the document element `el` is in is fully active: the page's, or the CURRENT document of a frame
/// that is fully active itself. A frame's document it has navigated away from is not, nor is a document no
/// context shows (DOMParser's). An element in no document -- detached, or in a fragment -- answers `true`:
/// a disconnected `<a>` still follows its link.
fn element_document_fully_active(scope: &mut v8::PinScope, el: u32) -> bool {
    let (root, page_root, is_doc) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return true };
        let root = h.0.shadow_including_root(el);
        (root, h.0.root(), h.0.node_type(root) == 9)
    };
    if root == page_root || !is_doc {
        return true;
    }
    match frame_of_document_root(scope, root) {
        Some(id) => bc(scope, |b| b.ctxs.get(id as usize).is_some_and(|c| c.doc_root == Some(root))) && context_fully_active(scope, id),
        None => false,
    }
}

/// FOLLOW THE HYPERLINK `el` (an `<a>` or `<area>` with `href`) -- its activation behaviour (run by
/// `core/html/activation.rs`). Chrome 150, measured (`tools/realweb/seed/activation`): a `download` link does not
/// navigate; a link to its document's own URL with a fragment is a same-document navigation (`location.hash`,
/// `hashchange`); anything else navigates the context its `target` names from the link's own document -- the
/// frame, or with `_top` the page (F36: a frame's `_top` link did nothing). `_blank` would need a user activation
/// this engine does not have, and Chrome's popup blocker refuses it without one too. The navigation is reported
/// as script-initiated, the reason the drivers already handle; Chrome's own is `anchorClick`.
pub(crate) fn follow_hyperlink(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, trusted: bool) {
    let Some(el_id) = unwrap_id(scope, el.into()) else { return };
    // A link whose document is not fully active follows nothing (HTML "cannot navigate"; WPT
    // `following-hyperlinks/active-document`: a frame's first document, navigated away from).
    if !element_document_fully_active(scope, el_id) {
        return;
    }
    let (href, target, download) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return };
        let dom = &h.0;
        (
            dom.get_attribute(el_id, "href").map(str::to_string),
            dom.get_attribute(el_id, "target").unwrap_or("").to_string(),
            dom.get_attribute(el_id, "download").map(str::to_string),
        )
    };
    let Some(href) = href else { return };
    // A download link downloads rather than navigates: handed to a DevTools client's download behaviour when one is
    // set (`devtools_log::download_requested`), and otherwise -- no download manager here -- nothing happens.
    if let Some(name) = download {
        let initiator = context_of_element(scope, el_id).filter(|&c| c != MAIN_CTX);
        if let Some(abs) = resolve_for_realm(scope, initiator, &href) {
            crate::devtools_log::download_requested(abs, Some(name).filter(|n| !n.trim().is_empty()));
        }
        return;
    }
    let initiator = context_of_element(scope, el_id).filter(|&c| c != MAIN_CTX);
    let Some(abs) = resolve_for_realm(scope, initiator, &href) else { return };
    let chosen = choose_target(scope, initiator, &target);
    let own = match (&chosen, initiator) {
        (NavigationTarget::Top, None) => true,
        (NavigationTarget::Context(c), Some(i)) => *c == i,
        _ => false,
    };
    // The page's own link: HTML's "navigate" of the page, with the link as the navigation's source element and the
    // click's trust as the user's involvement (Blink `HTMLAnchorElement::NavigateToHyperlink`) -- its `navigate` event
    // included.
    if own && initiator.is_none() && !starts_with_ci(&abs, "javascript:") {
        let nav = PageNavigation::get(abs, NavigationReason::ScriptInitiated);
        crate::core::html::navigate_event::navigate_page(
            scope,
            crate::core::html::navigate_event::PageNavigate {
                nav,
                history: crate::core::html::navigate_event::HistoryHandling::Auto,
                reason: crate::core::html::navigate_event::ClientReason::AnchorClick,
                source_element: Some(v8::Global::new(scope, el)),
                user_initiated: trusted,
                form_data: None,
            },
        );
        return;
    }
    if own && !starts_with_ci(&abs, "javascript:") {
        let cur = match initiator {
            Some(id) => loc_href(scope, id),
            None => page_location_prop(scope, "href", ""),
        };
        let strip = |u: &str| u.split('#').next().unwrap_or("").to_string();
        if let Some((_, frag)) = abs.split_once('#') {
            if strip(&abs) == strip(&cur) {
                let loc = match initiator {
                    Some(id) => loc_of(scope, id),
                    None => {
                        let g = main_global(scope);
                        get_prop(scope, g, "location").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
                    }
                };
                if let Some(loc) = loc {
                    let h = str_val(scope, &format!("#{frag}"));
                    set_prop(scope, loc, "hash", h);
                }
                return;
            }
        }
    }
    if matches!(chosen, NavigationTarget::Fresh) {
        // A link to a window that does not exist yet (`_blank`, a name nothing answers to) opens one -- with the user
        // activation a person's click carries, as Chrome's popup blocker requires -- and, `_blank` or not, with no
        // opener unless `rel` asks for one (Chrome 88 and later: a `target` link is `noopener` by default). It opened
        // nothing, so Playwright's `page.on('popup')` waited for a window that never came.
        if trusted || crate::core::events::user_activation::has_transient_user_activation(scope) {
            let rel = {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(el_id, "rel").map(str::to_ascii_lowercase)).unwrap_or_default()
            };
            let tokens: Vec<&str> = rel.split_ascii_whitespace().collect();
            let noopener = !tokens.contains(&"opener") || tokens.contains(&"noopener") || tokens.contains(&"noreferrer");
            let url_v = str_val(scope, &abs);
            let target_v = str_val(scope, &target);
            let features_v = str_val(scope, if noopener { "noopener" } else { "" });
            open_window(scope, url_v, target_v, features_v);
        }
        return;
    }
    navigate_target(scope, initiator, chosen, PageNavigation::get(abs, NavigationReason::ScriptInitiated));
}

/// Queue `hashchange` at frame `id`'s window, in its own realm, with the URLs before and after.
fn queue_frame_hashchange(scope: &mut v8::PinScope, id: u32, old: &str, new: &str) {
    let Some(realm) = realm_of(scope, id) else { return };
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    let data = v8::Array::new(scope, 2);
    let o = str_val(scope, old);
    data.set_index(scope, 0, o);
    let n = str_val(scope, new);
    data.set_index(scope, 1, n);
    let f = crate::binding::protos::build_fn_data(scope, cb_frame_hashchange, "", 0, data.into());
    crate::core::intersection_observer::rendering::queue_task(scope, f, 0.0);
}
fn cb_frame_hashchange(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let init = crate::binding::protos::init_dict(scope);
    for (i, k) in ["oldURL", "newURL"].iter().enumerate() {
        if let Some(v) = data.get_index(scope, i as u32) {
            set_prop(scope, init, k, v);
        }
    }
    crate::core::events::navigation_events::fire_window_event(scope, "HashChangeEvent", "hashchange", init);
}

/// The absolute form of `href` for the realm that is asking: a frame resolves against its OWN
/// document's URL, the page against the page's base URL.
pub fn resolve_for_realm(scope: &mut v8::PinScope, initiator: Option<u32>, href: &str) -> Option<String> {
    match initiator {
        Some(id) => resolve_in(scope, id, href),
        None => {
            let base = crate::core::dom::base_url(scope);
            url::Url::parse(href).ok().or_else(|| url::Url::parse(&base).ok().and_then(|b| b.join(href).ok())).map(|u| u.to_string())
        }
    }
}

/// Is the engine's own loader reachable at all? (`window.open` answers `null` when it is not.)
fn loader_ready(scope: &mut v8::PinScope) -> bool {
    crate::core::html::subresource::loader_ready(scope)
}
/// The engine's own loader (`core/html/subresource.rs`): the same kernel service every element-induced
/// load goes through, with the same guarantees -- the CALLER names the destination because the caller
/// read the tag. `None` when the load could not be issued at all.
fn call_loader<'s>(scope: &mut v8::PinScope<'s, '_>, url: &str, dest: &str, mode: &str) -> Option<v8::Local<'s, v8::Promise>> {
    crate::core::html::subresource::start_fetch(scope, url, dest, mode)
}
/// The same loader, for a navigation that carries a METHOD and a BODY -- a `method=post` form
/// submission into a nested or auxiliary context.
fn call_loader_with<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    url: &str,
    dest: &str,
    method: &str,
    body: Option<&str>,
    content_type: Option<&str>,
) -> Option<v8::Local<'s, v8::Promise>> {
    crate::core::html::subresource::start_navigation_fetch(scope, url, dest, method, body, content_type)
}

/// The request destination of context `id`'s navigations: its container's -- `iframe`, `frame` -- or `document`
/// for a context no element holds (a popup's). Chrome 150: a form POST targeting a frame
/// goes out `Sec-Fetch-Dest: iframe`; korvus sent every navigation of a nested context as `document`.
fn navigation_destination(scope: &mut v8::PinScope, id: u32) -> &'static str {
    let Some(el) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el)) else { return "document" };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    match st.try_borrow::<HostDom>().and_then(|h| h.0.tag_name(el).map(str::to_ascii_lowercase)).as_deref() {
        Some("iframe") => "iframe",
        Some("frame") => "frame",
        _ => "document",
    }
}

/// Navigate `ctx` to `href`: fetch it, replace the document, run its scripts, fire `load`. One
/// implementation for all four ways a second context is navigated -- `window.open(url)`, an assignment
/// to `location`/`location.href`, `location.assign`/`replace`, and a component setter.
fn navigate_context(scope: &mut v8::PinScope, id: u32, href: &str) {
    navigate_context_with(scope, id, href, "GET", None, None)
}
/// [`navigate_context`], for a navigation that carries a method and a body (a `method=post` form
/// submission into a nested or auxiliary context).
fn navigate_context_with(
    scope: &mut v8::PinScope,
    id: u32,
    href: &str,
    method: &str,
    body: Option<&str>,
    content_type: Option<&str>,
) {
    if id == MAIN_CTX {
        // `top.location = url` from a cross-origin frame: a top-level navigation, resolved against
        // the frame that asked (HTML "navigate", source browsing context).
        let base = match running(scope) { Some(c) => ctx_base(scope, c), None => page_location_prop(scope, "href", "") };
        let abs = url::Url::parse(&base).ok().and_then(|b| b.join(href).ok()).map(|u| u.to_string()).unwrap_or_else(|| href.to_string());
        if !starts_with_ci(&abs, "javascript:") {
            request_top_navigation(scope, PageNavigation::get(abs, NavigationReason::ScriptInitiated));
        }
        return;
    }
    if bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.closed).unwrap_or(true)) {
        return;
    }
    let Some(abs) = resolve_in(scope, id, href) else { return };
    // A `javascript:` URL does not replace the document, and `about:` is the initial empty one.
    // Neither touches the network, so both complete now.
    if starts_with_ci(&abs, "javascript:") {
        return;
    }
    // A popup open as a target of its own navigates there (`core/html/popup.rs`).
    if let Some(rid) = remote_popup(scope, id) {
        crate::core::html::popup::navigate(rid, &abs);
        set_location(scope, id, &abs);
        return;
    }
    // HTML "navigate", step 13: a URL equal to the document's except for a fragment it HAS is a
    // NAVIGATION TO A FRAGMENT -- the URL moves now, nothing is fetched, and `hashchange` follows a task
    // later when the fragment changed. It used to fetch the frame's document again: Chrome 150 reads
    // `(f.contentWindow.location.hash = "x", f.contentWindow.location.hash)` as "#x", and this read ""
    // until a reload landed (measured).
    if method.eq_ignore_ascii_case("GET") && body.is_none() && abs.contains('#') {
        let cur = loc_href(scope, id);
        let strip = |u: &str| u.split('#').next().unwrap_or("").to_string();
        if !cur.is_empty() && strip(&cur) == strip(&abs) {
            set_location(scope, id, &abs);
            if cur != abs {
                queue_frame_hashchange(scope, id, &cur, &abs);
            }
            return;
        }
    }
    if starts_with_ci(&abs, "about:") {
        with_ctx(scope, id, |c| c.delivered = FrameDelivered::default());
        set_location(scope, id, &abs);
        load_frame_html(scope, id, "");
        return;
    }
    // HTML "navigate", step 1: abort the context's ONGOING navigation. This engine has no fetch to
    // cancel, so the generation says which response is still wanted and a superseded one is dropped
    // when it arrives.
    let gen = bc(scope, |b| {
        let Some(c) = b.ctxs.get_mut(id as usize) else { return 0 };
        c.nav_gen = c.nav_gen.wrapping_add(1);
        c.nav_pending = true;
        c.nav_gen
    });
    // Mode `navigate`, destination the context's container's (`iframe`) or `document` for a popup's own document.
    let policy = frame_navigation_policy(scope, id);
    let dest = navigation_destination(scope, id);
    crate::core::html::subresource::set_pending_referrer_policy(scope, policy);
    // A frame's document may inherit its parent's encoding; a popup has no parent frame to inherit from.
    let el = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el));
    if let Some(parent) = el.and_then(|el| frame_parent_hint(scope, el)) {
        let shape = crate::core::html::subresource::LoadShape { parent: Some(parent), ..Default::default() };
        crate::core::html::subresource::set_pending_load_shape(scope, shape);
    }
    let p = call_loader_with(scope, &abs, dest, method, body, content_type);
    let _ = crate::core::html::subresource::take_pending_referrer_policy(scope);
    let _ = crate::core::html::subresource::take_pending_load_shape(scope);
    let Some(p) = p else { return };
    let d = pack_str(scope, id, &format!("{gen}\u{1f}{abs}"));
    let ok = build_fn_data(scope, cb_nav_ok, "", 1, d);
    let err = build_fn_data(scope, cb_nav_err, "", 1, d);
    // the response is a task, not a promise job (`then_task`)
    crate::then_task(scope, p, ok, Some(err));
}
fn cb_nav_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, abs)) = current_nav(scope, __body_data) else { return };
    let resp = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let st = resp.and_then(|r| get_prop(scope, r, "status")).and_then(|s| s.int32_value(scope)).unwrap_or(0);
    // 204/205 abort the navigation: the context keeps the document it had and fires nothing.
    if st == 204 || st == 205 {
        return;
    }
    let ok = resp.is_some() && (200..300).contains(&st);
    let final_url = resp
        .and_then(|r| get_prop(scope, r, "url"))
        .filter(|u| u.boolean_value(scope))
        .map(|u| js_string(scope, u))
        .unwrap_or_else(|| abs.clone());
    // The response's OWN headers decide whether this navigation leaves the opener's browsing-context
    // group. This is the one place in the engine where a real navigation and a real response meet.
    let headers = resp.and_then(|r| get_prop(scope, r, "headers")).map(|h| headers_from_js(scope, h)).unwrap_or_default();
    apply_coop(scope, id, &headers, &final_url);
    let mut delivered = FrameDelivered::from_headers(&headers);
    delivered.timing = resp.and_then(|r| response_timing(scope, r));
    with_ctx(scope, id, |c| {
        c.delivered = delivered;
        c.inline_nav_start = None;
    });
    set_location(scope, id, &final_url);
    let body = if ok {
        resp.and_then(|r| get_prop(scope, r, "body")).map(|b| nullish_or_string(scope, b)).unwrap_or_default()
    } else {
        String::new()
    };
    if ok && try_go_remote(scope, id, &final_url, &body, &headers) {
        return;
    }
    // The document its MIME type makes (A/B C5): a remote frame's engine decides the same in its own navigation.
    let (shown, mime) = if ok { frame_document_source(&headers, &body, &final_url) } else { (None, None) };
    load_frame_typed(scope, id, shown.as_deref().unwrap_or(&body), mime);
}
/// The `(context, url)` a navigation callback is for -- `None` when the context has since started
/// ANOTHER navigation, which supersedes this one (see `Ctx::nav_gen`).
fn current_nav(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<(u32, String)> {
    let (id, packed) = unpack_str(scope, data)?;
    let (gen, abs) = packed.split_once('\u{1f}')?;
    let gen: u32 = gen.parse().ok()?;
    if bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.nav_gen)) != Some(gen) {
        return None;
    }
    Some((id, abs.to_string()))
}

/// A failed navigation still ENDS: an empty document at the requested URL, and a `load`.
fn cb_nav_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, abs)) = current_nav(scope, __body_data) else { return };
    with_ctx(scope, id, |c| c.delivered = FrameDelivered::default());
    set_location(scope, id, &abs);
    load_frame_html(scope, id, "");
}

// ── scripts in the child's scope ───────────────────────────────────────────────────────────────────

/// A `<script>`'s `type` decides whether it is a script at all. An unknown type is a DATA BLOCK and
/// must not run -- the regex scraper this replaced ran the contents of every
/// `<script type="application/json">` a page carried, which is not a shortcut but a wrong answer.
const CLASSIC_TYPES: [&str; 8] = ["", "text/javascript", "application/javascript", "text/ecmascript", "application/ecmascript", "text/jscript", "application/x-javascript", "module"];

/// A frame script's throw: the value, and where V8 says it happened.
type FrameThrow = (v8::Global<v8::Value>, Option<ErrorSite>);

/// Run one classic script in the child's realm.
///
/// A child document's script error belongs to the child, and it is REPORTED THERE: an
/// `ErrorEvent` at the frame's window and the engine's uncaught log, through the one
/// "report an exception" path. It used to be written into the PARENT's `console.error` as
/// `frame script error: …`, which Chrome never does -- measured: an `<iframe>` whose script throws
/// leaves the parent's (wrapped) `console.error` uncalled and fires `error` at the frame's window only.
fn run_script(scope: &mut v8::PinScope, id: u32, code: &str, url: &str, el: Option<v8::Local<v8::Value>>) {
    if code.trim().is_empty() {
        return;
    }
    // An inline script's lines count from the start of its document (see `webapi::page::script_source_position`).
    let pos = el.map(|e| crate::page::script_source_position(scope, e)).unwrap_or((0, 0));
    // `document.currentScript` of the frame's document for as long as the script runs (F3), saved
    // and restored so a script that inserts a script nests.
    let cs = el.map(|e| v8::Global::new(scope, e));
    let saved = with_ctx(scope, id, |c| std::mem::replace(&mut c.current_script, cs)).flatten();
    run_script_body(scope, id, code, url, pos);
    // "Clean up after running script": when no script is on the stack, the microtask
    // checkpoint runs HERE -- before currentScript is restored and before the next script -- as the page's
    // pass does (`script_exec`). tiptap.dev's Turbopack runs Next's appBootstrap in a promise job that reads
    // document.currentScript. A script run from inside another performs no checkpoint (HTML; measured).
    if v8::StackTrace::current_stack_trace(scope, 1).map_or(0, |st| st.get_frame_count()) == 0 {
        scope.perform_microtask_checkpoint();
    }
    with_ctx(scope, id, |c| c.current_script = saved);
}

/// A classic script, compiled and run IN THE CONTEXT'S OWN V8 CONTEXT.
///
/// It was a function compiled in the PAGE's context and called inside `with(blocker)with(iwin){...}`.
/// Every identifier the frame's own scope did not answer resolved against the page -- and so did
/// everything the language resolves against the realm rather than the scope: `Function('return
/// this')()` and `(0, eval)('this')` answered the PAGE's window, so a challenge provider a challenge widget's challenge
/// frame looked its elements up in the page's document and threw `null.appendChild` (seek.com.au,
/// apmex, glassdoor and four more on the C2 live run). The frame's `Array`, `Object` and `Function`
/// were the page's too. A real context gives the frame its own intrinsics and its own global scope:
/// `var` and `function` land on its window, `let`/`const`/`class` are shared by its scripts, and a
/// name the frame does not define is a ReferenceError, as it is in a browser.
fn run_script_body(scope: &mut v8::PinScope, id: u32, code: &str, url: &str, pos: (i32, i32)) {
    run_script_in(scope, id, code, url, pos, false);
}

/// [`run_script_body`], `anonymous` for source text with no script resource of its own (a string timer):
/// compiled under no name, so its frames read `<anonymous>` and its errors the document's URL.
fn run_script_in(scope: &mut v8::PinScope, id: u32, code: &str, url: &str, pos: (i32, i32), anonymous: bool) {
    // every script of a frame's realm runs through here: none in a sandboxed document ([`scripts_sandboxed`])
    if scripts_sandboxed(scope, id) {
        return;
    }
    let Some(realm) = realm_of(scope, id) else { return };
    let prev = enter_realm(scope, id);
    let thrown = {
        let scope = &mut v8::ContextScope::new(scope, realm);
        classic_script(scope, id, code, url, pos, anonymous)
    };
    leave_realm(scope, prev);
    if let Some(t) = thrown {
        report_in_frame(scope, id, t, url);
    }
}

/// Compile and run `code` as a classic script in the current context, with `url` as its origin.
fn classic_script(scope: &mut v8::PinScope, id: u32, code: &str, url: &str, pos: (i32, i32), anonymous: bool) -> Option<FrameThrow> {
    let script_url = url.split('\n').next().unwrap_or("");
    let name: v8::Local<v8::Value> = if anonymous { v8::undefined(scope).into() } else { str_val(scope, script_url) };
    // Its `import()` loads in this frame's realm, resolved against the script's URL.
    let opts = frame_modules::host_options(scope, id, script_url);
    let origin = v8::ScriptOrigin::new(scope, name, pos.0, pos.1, false, 0, None, false, false, false, opts);
    let text = v8::String::new(scope, code)?;
    v8::tc_scope!(let tc, scope);
    let _ = v8::Script::compile(tc, text, Some(&origin)).and_then(|s| s.run(tc));
    let thrown = if tc.has_caught() {
        tc.exception().map(|e| {
            let m = tc.message();
            (v8::Global::new(tc, e), crate::core::events::report::caught_site(tc, e, m))
        })
    } else {
        None
    };
    tc.reset();
    thrown
}

/// A frame's `setTimeout(code)` / `setInterval(code)` with a STRING: the code runs as a script of the
/// frame's realm when the timer fires, not the page's.
fn cb_frame_string_timer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, code)) = unpack_str(scope, __body_data) else { return };
    let url = ctx_base(scope, id);
    // Blink compiles a timer's source text as a script of no resource (`ScheduledAction`): its frames read
    // `<anonymous>:1:16` (stock Chrome 150 in a cross-site frame; this compiled it under the document's URL).
    run_script_in(scope, id, &code, &url, (0, 0), true);
}

/// "Report an exception" in context `id`: the frame's realm is entered, so the `ErrorEvent` is
/// dispatched at the frame's window.
fn report_in_frame(scope: &mut v8::PinScope, id: u32, thrown: FrameThrow, url: &str) {
    let (err, site) = thrown;
    // The positions are the script's own: the compile origin already offsets the wrapper.
    let site = site.map(|mut s| {
        if s.filename.is_empty() {
            s.filename = url.to_string();
        }
        s
    });
    let err = v8::Local::new(scope, &err);
    let prev = enter_realm(scope, id);
    {
        v8::tc_scope!(let tc, scope);
        crate::core::events::report::report_exception_at(tc, err, site);
        tc.reset();
    }
    leave_realm(scope, prev);
}

fn el_attr(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let n = str_val(scope, name);
    let v = crate::core::dom::element::attr_call(scope, el.into(), "getAttribute", &[n])?;
    if v.is_null_or_undefined() {
        None
    } else {
        Some(js_string(scope, v))
    }
}
fn cb_identity(_scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(args.get(0));
}
fn cb_to_null(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_null();
}

/// B4 (ABI 3 step 10): a page called one of the plugin bindings (`host->add_binding`). The binding's name
/// travels as the function's `data` (bound in [`install_plugin_bindings`]) and the single argument is the
/// payload; both go to `plugin_host::on_binding`, which tells every plugin's `on_binding` slot in load order,
/// on this (the calling realm's) thread. A missing or null argument is the empty string.
fn cb_plugin_binding(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = args.data().to_rust_string_lossy(scope);
    let payload = nullish_or_string(scope, args.get(0));
    // ABI 3 (WebExtension message bus): a plugin that answers via `on_binding_call` returns a response the call
    // resolves to (e.g. chrome.runtime.sendMessage); otherwise fall back to the void `on_binding` (B4), whose call
    // returns undefined as before.
    if let Some(resp) = plugin_host::on_binding_call(&name, &payload) {
        if let Some(s) = v8::String::new(scope, &resp) {
            rv.set(s.into());
        }
    } else {
        plugin_host::on_binding(&name, &payload);
    }
}

/// B4 (ABI 3 step 10): install each plugin binding (`plugin_host::binding_names`) as a global function on
/// context `id`'s realm -- the same point and realm the init scripts run in (`run_doc_scripts`), before the
/// document's own scripts, so either can call them. Each function carries its binding name as its `data` and
/// hands `(name, argument)` to [`cb_plugin_binding`]. The name list is empty until a plugin calls
/// `host->add_binding`, so with no binding registered no global is added (invariant I9/J2); a frame's realm is
/// a page realm, so frames get the bindings too.
fn install_plugin_bindings(scope: &mut v8::PinScope, id: u32) {
    let names = plugin_host::binding_names();
    if names.is_empty() {
        return;
    }
    let Some(realm) = realm_of(scope, id) else { return };
    let prev = enter_realm(scope, id);
    {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let g = scope.get_current_context().global(scope);
        for name in &names {
            // the binding's name travels as the function's data, read back in `cb_plugin_binding`
            let data = str_val(scope, name);
            let f = build_fn_data(scope, cb_plugin_binding, name, 1, data);
            set_prop(scope, g, name, f.into());
        }
    }
    leave_realm(scope, prev);
}

/// B4 (ABI 3 step 10), the TOP-page path: install the plugin bindings as globals on the CURRENT realm, the one
/// the scope is already in. `navigation.rs` calls this where the injected (`addScriptToEvaluateOnNewDocument`)
/// scripts run -- before the page's own scripts -- the top-page counterpart of [`install_plugin_bindings`], which
/// `run_doc_scripts` calls for frames (the top page never goes through `run_doc_scripts`). Empty name list -> no
/// global added, so nothing loaded means nothing changed (I9/J2).
pub fn install_plugin_bindings_current(scope: &mut v8::PinScope) {
    let names = plugin_host::binding_names();
    if names.is_empty() {
        return;
    }
    let g = scope.get_current_context().global(scope);
    for name in &names {
        let data = str_val(scope, name);
        let f = build_fn_data(scope, cb_plugin_binding, name, 1, data);
        set_prop(scope, g, name, f.into());
    }
}

/// ABI 3 (WebExtension message bus, P1b): install a GIVEN list of plugin-binding names as globals on the CURRENT
/// realm (the scope's context). korvus-runtime enters a NAMED ISOLATED world's context first and calls this with
/// that world's `host->add_world_binding` names, so an extension's content<->layer channel lives only in its
/// isolated world -- never in the page's main world. Empty list -> nothing added. Each global hands (name, arg) to
/// [`cb_plugin_binding`], exactly like the main-world path.
pub fn install_named_bindings(scope: &mut v8::PinScope, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let g = scope.get_current_context().global(scope);
    for name in names {
        let data = str_val(scope, name);
        let f = build_fn_data(scope, cb_plugin_binding, name, 1, data);
        set_prop(scope, g, name, f.into());
    }
}

/// Run a child document's scripts, IN DOCUMENT ORDER, in that context's scope -- fetching the
/// external ones. FETCH IN PARALLEL, EXECUTE IN ORDER, which is what a browser does and what the
/// whole `html/syntax/speculative-parsing` directory exists to measure: a classic external script
/// still blocks the scripts after it, it is just no longer blocking their DOWNLOAD too. The context's
/// load event fires once every script has run (or failed); a document with no external script never
/// touches a promise, so the common case is exactly as synchronous as it was.
/// A frame document's PARSED `<body on*>` attributes, compiled in its realm and forwarded to its window, as the
/// runtime's `activate_body_handlers` does for the page's: the parser writes them into the arena, through no attribute
/// seam, so nothing else would see them. Chrome 150 runs a srcdoc frame's `<body onload>`;
/// korvus never compiled it. A sandboxed document compiles none (`html_body_element::compile`).
fn activate_frame_body_handlers(scope: &mut v8::PinScope, id: u32) {
    let Some(root) = frame_document_root(scope, id) else { return };
    let found = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return };
        let d = &h.0;
        let is = |e: u32, t: &str| d.tag_name(e).is_some_and(|n| n.eq_ignore_ascii_case(t));
        d.children(root)
            .into_iter()
            .find(|&e| is(e, "html"))
            .and_then(|html| d.children(html).into_iter().find(|&e| is(e, "body")))
            .map(|body| (body, d.attribute_names(body)))
    };
    let Some((body_id, names)) = found else { return };
    let names: Vec<String> = names.into_iter().map(|n| n.to_ascii_lowercase()).filter(|n| n.starts_with("on")).collect();
    if names.is_empty() {
        return;
    }
    let Some(realm) = realm_of(scope, id) else { return };
    let prev = enter_realm(scope, id);
    {
        let scope = &mut v8::ContextScope::new(scope, realm);
        let body = crate::page::node_object(scope, body_id);
        for n in &names {
            crate::core::html::html_body_element::body_handler_attr(scope, body, n);
        }
    }
    leave_realm(scope, prev);
}

fn run_doc_scripts(scope: &mut v8::PinScope, id: u32) {
    activate_frame_body_handlers(scope, id);
    // The document's own subresources (its images, stylesheets, frames) -- everything
    // but the scripts, which this function fetches and runs below.
    // First the frame document's load priorities as its preload scanner gives them
    // -- its own first image and boosted images; its scripts below take theirs from the same walk.
    let scanned = match frame_document_root(scope, id) {
        Some(root) => {
            let scanned = crate::core::html::subresource::scan_load_priorities(scope, root);
            crate::core::html::subresource::start_frame_subresources(scope, root);
            scanned
        }
        None => Default::default(),
    };
    // the frames its parser built, as `window[i]` before its scripts run (the page's forced rescan)
    sync_child_window_frames(scope, id);
    // Diagnosis only: `KORVUS_FRAME_EARLY=<file.js>` runs that file in
    // every frame document's realm before the document's own scripts -- `--eval-early` for frames,
    // which a top-page hook cannot reach once a cross-origin frame has its own realm (F9, F19).
    if let Some(code) = std::env::var_os("KORVUS_FRAME_EARLY").and_then(|p| std::fs::read_to_string(p).ok()) {
        run_script(scope, id, &code, "korvus:frame-early", None);
    }
    // A DevTools client's bindings and new-document scripts, before the document's own, as Chrome runs them in every
    // frame (`Page.addScriptToEvaluateOnNewDocument`, `Runtime.addBinding`); the worlds they name are made over this
    // document.
    devtools_frame_document_start(scope, id);
    // B3 (ABI 3 wave 1): the loaded plugins' init scripts run here, in load order, each before any of the
    // document's own scripts -- the same point the diagnostic `--eval-early` scripts run and where
    // `Page.addScriptToEvaluateOnNewDocument` injects. A frame's realm is a page realm (its global is a `Window`),
    // so the kind is `REALM_PAGE`.
    // TODO(1021-verify): the worker realms (dedicated/shared/service) have their own script boot, driven from
    // korvus-runtime, not this document path; their `init_scripts(REALM_DEDICATED_WORKER | ...)` injection is the
    // runtime pass's to add at the worker's pre-script point.
    if plugin_host::any() {
        // B4 (ABI 3 step 10): install the plugins' page bindings (`host->add_binding`) as global functions on
        // this realm, BEFORE the init scripts and the document's own scripts -- so either can call them. Empty
        // until a plugin adds one; a no-plugin run never reaches here (invariant I9/J2).
        install_plugin_bindings(scope, id);
        for code in plugin_host::init_scripts(plugin_host::REALM_PAGE) {
            run_script(scope, id, &code, "korvus:plugin-init", None);
        }
    }
    let mut els: Vec<v8::Global<v8::Object>> = Vec::new();
    // a sandboxed document's scripts are neither fetched nor run, and fire nothing ("scripting is disabled")
    if doc_of(scope, id).is_some() && !scripts_sandboxed(scope, id) {
        // The frame document's scripts from the ARENA, not its page-visible
        // `querySelectorAll` -- a frame wrapping it saw the browser call it before any of its scripts.
        let ids = frame_document_root(scope, id)
            .and_then(|root| {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                st.try_borrow::<HostDom>().map(|h| h.0.get_elements_by_tag_name_in(root, "script"))
            })
            .unwrap_or_default();
        {
            for nid in ids {
                let el = crate::page::node_object(scope, nid);
                if let Ok(el) = v8::Local::<v8::Object>::try_from(el) {
                    els.push(v8::Global::new(scope, el));
                }
            }
        }
    }
    // A FRAME's deferred scripts wait for the end of its parse, as the page's do: the
    // scripts that run during the parse first, then -- one list, in document order -- every classic
    // `<script defer src>` and every module script. Chrome 150: `s1,inline,s2,d1,module,d2`; this runner
    // took the scripts in document order, `defer` and modules included (`d1,s1,inline,module,d2,s2`).
    // A document script rebuilt after its last write(): the load steps run, its scripts do not.
    if with_ctx(scope, id, |c| std::mem::take(&mut c.skip_doc_scripts)).unwrap_or(false) {
        els.clear();
    }
    let deferred: Vec<bool> = els
        .iter()
        .map(|g| {
            let el = v8::Local::new(scope, g);
            let ty = el_attr(scope, el, "type").unwrap_or_default().trim().to_ascii_lowercase();
            let has_src = el_attr(scope, el, "src").is_some_and(|s| !s.trim().is_empty());
            ty == "module" || (has_src && el_attr(scope, el, "defer").is_some() && el_attr(scope, el, "async").is_none())
        })
        .collect();
    let (now, later): (Vec<_>, Vec<_>) = els.into_iter().zip(deferred).partition(|(_, d)| !*d);
    let els: Vec<v8::Global<v8::Object>> = now.into_iter().chain(later).map(|(g, _)| g).collect();
    let n = els.len();
    let identity = build_fn(scope, cb_identity, "", 1);
    let to_null = build_fn(scope, cb_to_null, "", 1);
    let mut run = ScriptRun { ctx: id, els, runs: vec![false; n], abs: vec![None; n], pending: vec![None; n], i: 0 };
    for k in 0..n {
        let el = v8::Local::new(scope, &run.els[k]);
        let ty = el_attr(scope, el, "type").unwrap_or_default().trim().to_ascii_lowercase();
        if !CLASSIC_TYPES.contains(&ty.as_str()) {
            continue;
        }
        run.runs[k] = true;
        let src = el_attr(scope, el, "src").unwrap_or_default().trim().to_string();
        if src.is_empty() {
            continue;
        }
        run.abs[k] = resolve_in(scope, id, &src);
        let Some(abs) = run.abs[k].clone() else { continue };
        // A module script is fetched by the frame's module loader when its turn comes, with its
        // imports, as a module -- not ahead as a classic script.
        if ty == "module" {
            continue;
        }
        // CSP `script-src`: a blocked script is never fetched, and with no fetch pending
        // the loop below fires `error` at it in its turn.
        let el_id = crate::binding::protos::unwrap_id(scope, el.into());
        if let Some(e) = el_id.filter(|&e| !crate::core::html::csp::allows_script_url(scope, e, &abs, true)) {
            // its entry, in the refusal shape: the element's, after the scanner's (`frame_scan_refusals`)
            crate::core::timing::resource_timing::refused_load(scope, e, "script", &abs);
            continue;
        }
        let cors = el_attr(scope, el, "crossorigin");
        let mode = if cors.is_some() { "cors" } else { "no-cors" };
        // The parser's script, at the priority its document's scan gave it (`async`, `defer`, after the
        // first image, in the head), with the credentials its `crossorigin` asks for.
        let credentials = if cors.as_deref().is_some_and(|v| v.trim().eq_ignore_ascii_case("use-credentials")) { "include" } else { "" };
        let priority = el_id.and_then(|e| scanned.get(&e).cloned());
        crate::core::html::subresource::set_pending_load_shape(scope, crate::core::html::subresource::LoadShape { priority, credentials, parent: None });
        // The script is the FRAME DOCUMENT'S request: its initiator, `Referer` and `Sec-Fetch-Site` are the
        // frame's, and so is its HTTP-cache partition (top-level site, frame site). Chrome 150, measured
        // (`tests/fixtures/worker-script-loading/cache.js`, three runs alike): a cross-site frame's `<script src>` to
        // its own origin goes out `same-origin` with the frame's URL as `Referer`. Asked for here with no frame's realm
        // running, the loader took it for the page's: `cross-site`, the page's Referer, keyed (page site, page site) --
        // so the frame's worker, whose imports ride the frame's partition, fetched it again.
        let prev = enter_realm(scope, id);
        let fetched = call_loader(scope, &abs, "script", mode);
        leave_realm(scope, prev);
        // A rejection is a result, not an unhandled rejection: it becomes `null` here and an `error`
        // event when the loop reaches this script.
        if let Some(p) = fetched {
            if let Some(norm) = p.then2(scope, identity, to_null) {
                run.pending[k] = Some(v8::Global::new(scope, norm));
            }
        }
    }
    let run_id = bc(scope, |b| {
        b.next_run += 1;
        let r = b.next_run;
        b.runs.insert(r, run);
        r
    });
    step_scripts(scope, run_id);
}

/// One `<script>` of a run: its element, whether it runs, its resolved `src`, its fetch.
struct StepItem {
    el: v8::Global<v8::Object>,
    runs: bool,
    abs: Option<String>,
    pending: Option<v8::Global<v8::Promise>>,
}
/// A frame document's "the end": `DOMContentLoaded`, then -- in the same task -- its load steps when nothing holds them.
fn frame_the_end(scope: &mut v8::PinScope, ctx: u32) {
    frame_dom_content_loaded(scope, ctx);
    // Blink's `Document::FinishedParsing` checks for completion in the same task as `DOMContentLoaded`
    // (`CheckCompleted`): with no child frame still loading, `complete`, the window's `load` and the
    // element's follow at once -- Chrome 150, same- and cross-origin frames alike, a 0 ms timer set by a
    // `DOMContentLoaded` handler reads `complete` and a `loadEventEnd` (the challenge
    // frame's handler read the navigation entry's `duration` as 0 here, 1006 ms in Chrome).
    // The `DOMContentLoaded` handlers' promise jobs run first (Blink's checkpoint there): with no script on
    // the stack and no microtask running they have (`frame_dom_content_loaded`), and the load steps follow
    // here; inside a microtask job they cannot, so the load steps queue behind them as one.
    let in_close = with_ctx(scope, ctx, |c| std::mem::take(&mut c.end_in_close)).unwrap_or(false);
    if with_ctx(scope, ctx, |c| {
        c.load_parked = !c.delays.is_empty();
        c.load_parked
    })
    .unwrap_or(false)
    {
        // a load its document started is still out -- a script it inserted, an image, a nested frame: the load steps
        // wait for it ([`undelay_load`]), as Blink's `CheckCompleted` waits on the document's load-event delay count
    } else if in_close {
        frame_loaded_now(scope, ctx);
    } else if child_frames_loading(scope, ctx) {
        ctx_loaded(scope, ctx);
    } else if !scope.get_current_context().get_microtask_queue().is_some_and(|q| q.is_running_microtasks())
        && v8::StackTrace::current_stack_trace(scope, 1).map_or(0, |st| st.get_frame_count()) == 0
    {
        frame_loaded_now(scope, ctx);
    } else {
        let d = u32_data(scope, ctx);
        let f = build_fn_data(scope, cb_ctx_loaded_fire, "", 0, d);
        scope.enqueue_microtask(f);
    }
}
fn cb_frame_the_end(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(ctx) = data_u32(scope, __body_data) else { return };
    frame_the_end(scope, ctx);
}

fn step_scripts(scope: &mut v8::PinScope, run_id: u32) {
    loop {
        let next = bc(scope, |b| {
            let r = b.runs.get_mut(&run_id)?;
            if r.i >= r.els.len() {
                return Some(None);
            }
            let k = r.i;
            r.i += 1;
            Some(Some((k, StepItem { el: r.els[k].clone(), runs: r.runs[k], abs: r.abs[k].clone(), pending: r.pending[k].clone() })))
        });
        let Some(next) = next else { return };
        let Some((k, item)) = next else {
            // Every script has run: the navigation is complete and `load` may fire.
            let ctx = bc(scope, |b| b.runs.remove(&run_id).map(|r| r.ctx));
            if let Some(ctx) = ctx {
                // "The end" is a task of its own, after the one that ran the last script: Blink's
                // parser runs the scripts as the body's bytes are handed to it, and finishes -- `FinishedParsing`,
                // `DOMContentLoaded`, the load steps -- when the network says the body is complete, a later task. So
                // a message the frame's last script posted is delivered before the frame's `load` (Chrome 150,
                // measured 6/6). A document no response delivered (`about:blank`,
                // `about:srcdoc`, `blob:`) has no such notification, and a `document.close()` finishes the parser
                // inside the call: both end here.
                let local = is_local_scheme(&loc_href(scope, ctx));
                if local || with_ctx(scope, ctx, |c| c.end_in_close).unwrap_or(false) {
                    frame_the_end(scope, ctx);
                } else {
                    let d = u32_data(scope, ctx);
                    let f = build_fn_data(scope, cb_frame_the_end, "", 0, d);
                    crate::core::intersection_observer::rendering::queue_task(scope, f, 0.0);
                }
            }
            return;
        };
        if !item.runs {
            continue;
        }
        let el = v8::Local::new(scope, &item.el);
        let ctx = bc(scope, |b| b.runs.get(&run_id).map(|r| r.ctx)).unwrap_or(0);
        // `<script type=module>`: a module of the frame's realm, run in its deferred turn. It
        // had been run as a classic script, where its first `import` was a SyntaxError.
        if el_attr(scope, el, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("module")) {
            let src = el_attr(scope, el, "src").unwrap_or_default();
            let promise = if src.trim().is_empty() {
                // The CHILD text content off the arena, never the page-visible `textContent` getter.
                let code = crate::page::child_text_content(scope, el.into());
                let el_id = crate::binding::protos::unwrap_id(scope, el.into());
                if el_id.is_some_and(|e| !crate::core::html::csp::allows_inline_script(scope, e, &code, true)) {
                    continue; // CSP `script-src`
                }
                let doc_url = loc_href(scope, ctx);
                let base = ctx_base(scope, ctx);
                frame_modules::run_script(scope, ctx, None, Some((&code, &doc_url, &base)))
            } else {
                match item.abs.as_deref() {
                    Some(abs) => {
                        let el_id = crate::binding::protos::unwrap_id(scope, el.into());
                        if el_id.is_some_and(|e| !crate::core::html::csp::allows_script_url(scope, e, abs, true)) {
                            fire_at(scope, el.into(), "error");
                            continue;
                        }
                        frame_modules::run_script(scope, ctx, Some(abs), None)
                    }
                    None => {
                        fire_at(scope, el.into(), "error");
                        continue;
                    }
                }
            };
            let Some(promise) = promise else { continue };
            let kv = u32_data(scope, k as u32);
            let d = pack(scope, run_id, kv);
            let ok = build_fn_data(scope, cb_step_module_ok, "", 1, d);
            let bad = build_fn_data(scope, cb_step_module_err, "", 1, d);
            let _ = promise.then2(scope, ok, bad);
            return; // the next script runs once this module has
        }
        if item.abs.is_none() && item.pending.is_none() {
            // Inline: its source is right here.
            let inline = el_attr(scope, el, "src").unwrap_or_default();
            if inline.trim().is_empty() {
                // The CHILD text content off the arena, never the page-visible `textContent` getter.
                let code = crate::page::child_text_content(scope, el.into());
                let el_id = crate::binding::protos::unwrap_id(scope, el.into());
                if el_id.is_some_and(|e| !crate::core::html::csp::allows_inline_script(scope, e, &code, true)) {
                    continue; // CSP `script-src`
                }
                // an inline script is named by its DOCUMENT's URL, not the base URL (Blink `kInline`: a srcdoc frame's
                // scripts are `about:srcdoc` -- a CSP violation's `sourceFile` "about", an error's stack -- where they
                // were named by the parent's base; Chrome 150 measured)
                let doc_url = loc_href(scope, ctx);
                if run_parser_script(scope, ctx, &code, &doc_url, Some(el.into())) {
                    schedule_step(scope, run_id, None);
                    return;
                }
                continue;
            }
            fire_at(scope, el.into(), "error"); // an unresolvable `src`
            continue;
        }
        let Some(p) = item.pending else {
            fire_at(scope, el.into(), "error");
            continue;
        };
        let p = v8::Local::new(scope, &p);
        let kv = u32_data(scope, k as u32);
        let d = pack(scope, run_id, kv);
        let resume = build_fn_data(scope, cb_step_resume, "", 1, d);
        p.then(scope, resume);
        return; // a classic external script BLOCKS the ones after it from RUNNING
    }
}
/// A frame module script has run: an external one fires `load` (HTML's "execute the script element"), and the
/// run goes on.
fn cb_step_module_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    step_module_done(scope, &args, None);
}

/// A frame module script failed: a module that could not be FETCHED fires `error` at its element; a parse or
/// evaluation error is reported at the frame's window, and an external script still fires `load`.
fn cb_step_module_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let e = args.get(0);
    step_module_done(scope, &args, Some(e));
}

fn step_module_done(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, err: Option<v8::Local<v8::Value>>) {
    let __body_data = crate::binding::protos::body_data(scope, args);
    let Some((run_id, kv)) = unpack(scope, __body_data) else { return };
    let Some(k) = kv.uint32_value(scope) else { return };
    let Some((ctx, el)) = bc(scope, |b| b.runs.get(&run_id).and_then(|r| Some((r.ctx, r.els.get(k as usize)?.clone())))) else { return };
    let el = v8::Local::new(scope, &el);
    let external = el_attr(scope, el, "src").is_some_and(|s| !s.trim().is_empty());
    let fetch_failed = err.is_some_and(|e| e.is_native_error() && e.to_rust_string_lossy(scope).starts_with("TypeError: Failed to fetch dynamically imported module"));
    if fetch_failed {
        fire_at(scope, el.into(), "error");
    } else {
        if let Some(e) = err {
            let url = el_attr(scope, el, "src").unwrap_or_else(|| loc_href(scope, ctx));
            report_in_frame(scope, ctx, (v8::Global::new(scope, e), None), &url);
        }
        if external {
            fire_at(scope, el.into(), "load");
        }
    }
    step_scripts(scope, run_id);
}

fn cb_step_resume(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((run_id, kv)) = unpack(scope, __body_data) else { return };
    let Some(k) = kv.uint32_value(scope) else { return };
    let k = k as usize;
    let Some((ctx, el, abs)) = bc(scope, |b| b.runs.get(&run_id).and_then(|r| Some((r.ctx, r.els.get(k)?.clone(), r.abs.get(k)?.clone())))) else { return };
    let el = v8::Local::new(scope, &el);
    let resp = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let st = resp.and_then(|r| get_prop(scope, r, "status")).and_then(|s| s.int32_value(scope)).unwrap_or(0);
    let ok = resp.is_some() && (200..300).contains(&st);
    if ok {
        let body = resp.and_then(|r| get_prop(scope, r, "body")).map(|b| nullish_or_string(scope, b)).unwrap_or_default();
        let url = resp
            .and_then(|r| get_prop(scope, r, "url"))
            .filter(|u| u.boolean_value(scope))
            .map(|u| js_string(scope, u))
            .or(abs)
            .unwrap_or_default();
        if run_parser_script(scope, ctx, &body, &url, Some(el.into())) {
            schedule_step(scope, run_id, Some(el));
            return;
        }
    }
    fire_at(scope, el.into(), if ok { "load" } else { "error" });
    step_scripts(scope, run_id);
}

/// A frame's PARSER script: run with `currentScript` set, then "clean up after running script" --
/// its microtasks run while `currentScript` still names it and before the next script. A step that runs inside
/// a microtask job (a fetch's or a resume's promise reaction) cannot checkpoint there -- V8 does not nest them --
/// so it leaves `currentScript` set and answers true; the caller continues from a TASK ([`schedule_step`]),
/// which the job's remaining microtasks precede. tiptap.dev's Turbopack reads document.currentScript from one.
fn run_parser_script(scope: &mut v8::PinScope, id: u32, code: &str, url: &str, el: Option<v8::Local<v8::Value>>) -> bool {
    if code.trim().is_empty() {
        return false;
    }
    let pos = el.map(|e| crate::page::script_source_position(scope, e)).unwrap_or((0, 0));
    let cs = el.map(|e| v8::Global::new(scope, e));
    let saved = with_ctx(scope, id, |c| {
        c.parser_nesting += 1;
        std::mem::replace(&mut c.current_script, cs)
    })
    .flatten();
    run_script_body(scope, id, code, url, pos);
    let stack_empty = v8::StackTrace::current_stack_trace(scope, 1).map_or(0, |st| st.get_frame_count()) == 0;
    if stack_empty && scope.get_current_context().get_microtask_queue().is_some_and(|q| q.is_running_microtasks()) {
        // The nesting level stays up until the continuation task: the job's microtasks are this script's.
        with_ctx(scope, id, |c| c.restore_script = Some(saved));
        return true;
    }
    if stack_empty {
        scope.perform_microtask_checkpoint();
    }
    with_ctx(scope, id, |c| {
        c.current_script = saved;
        c.parser_nesting = c.parser_nesting.saturating_sub(1);
    });
    false
}

/// Continue run `run_id` from a new task: restore the `currentScript` a deferred parser script left set, fire the
/// external script's `load`, then the next step.
fn schedule_step(scope: &mut v8::PinScope, run_id: u32, loaded: Option<v8::Local<v8::Object>>) {
    let v: v8::Local<v8::Value> = match loaded {
        Some(el) => el.into(),
        None => v8::undefined(scope).into(),
    };
    let d = pack(scope, run_id, v);
    let f = build_fn_data(scope, cb_step_continue, "", 0, d);
    set_timeout(scope, f, 0);
}
fn cb_step_continue(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((run_id, loaded)) = unpack(scope, __body_data) else { return };
    if let Some(ctx) = bc(scope, |b| b.runs.get(&run_id).map(|r| r.ctx)) {
        with_ctx(scope, ctx, |c| {
            if let Some(s) = c.restore_script.take() {
                c.current_script = s;
                c.parser_nesting = c.parser_nesting.saturating_sub(1);
            }
        });
    }
    if let Ok(el) = v8::Local::<v8::Object>::try_from(loaded) {
        fire_at(scope, el.into(), "load");
    }
    step_scripts(scope, run_id);
}

// ── frames ─────────────────────────────────────────────────────────────────────────────────────────

/// The context of a frame element, created on first access. The context keeps a back-reference to
/// its element, because `document.close()` ends a navigation and has to fire load on the frame.
/// `contentWindow.frameElement` is the `<iframe>` in every browser.
fn frame_ctx_for(scope: &mut v8::PinScope, el: u32) -> u32 {
    if let Some(c) = bc(scope, |b| b.by_frame.get(&el).copied()) {
        return c;
    }
    let id = make_frame_context(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let w = wrap_cached(scope, &op_state, el);
    let nm = el_attr(scope, w, "name").unwrap_or_default();
    with_ctx(scope, id, |c| c.win_name = nm);
    bc(scope, |b| {
        if let Some(c) = b.ctxs.get_mut(id as usize) {
            c.el = Some(el);
        }
        b.by_frame.insert(el, id);
    });
    // The initial `about:blank` document's referrer: its creator's URL, as for any `about:blank`
    // ([`begin_frame_document`]). Fixed now, when the document is created.
    if let Some(p) = frame_parent_ctx(scope, id) {
        let creator = loc_href(scope, p);
        with_ctx(scope, id, |c| c.referrer = creator);
    }
    note_about_base(scope, id);
    refresh_opaque(scope, id);
    // the initial `about:blank` document holds a clone of its creator's policy container
    inherit_csp(scope, id);
    // ...and its cross-origin isolated capability, now its container is known: its clock's grain and its
    // `SharedArrayBuffer`
    let capable = crate::core::html::isolation_policy::capability(scope, Some(id));
    with_ctx(scope, id, |c| c.clock.set_cross_origin_isolated(capable));
    if let Some(realm) = with_ctx(scope, id, |c| c.realm.clone()).flatten() {
        let realm = v8::Local::new(scope, &realm);
        crate::core::html::isolation_policy::gate_frame_realm(scope, id, realm);
    }
    // A frame whose element asks for a navigation is requesting one from the moment it exists, so its initial
    // about:blank window is made without the extension bindings.
    let requesting = op_state.borrow().try_borrow::<HostDom>().is_some_and(|h| requests_navigation(&h.0, el));
    if requesting {
        defer_extension_bindings(scope, id);
    }
    // made while its initial about:blank `load` is dispatched ([`fire_initial_blank_load`])
    if op_state.borrow().try_borrow::<InitialBlankLoads>().is_some_and(|l| l.0.contains(&el)) {
        with_ctx(scope, id, |c| c.load_event = FrameLoadEvent::InProgress);
    }
    id
}

/// The frame elements whose initial about:blank `load` is being dispatched as they are inserted
/// (`subresource::blank_frame_load`): a context made during that dispatch -- a handler reading `contentDocument` --
/// begins with its load event IN PROGRESS, as the one made before it is set, so a `document.open()` in the handler keeps
/// it from running again when `close()` ends the document (HTML's "mute iframe load", [`fire_initial_blank_load`]).
#[derive(Default)]
struct InitialBlankLoads(HashSet<u32>);

/// The initial about:blank `load` at frame element `el`, inserted with no `src` to fetch. It is the
/// frame's load event like any other: while it is dispatched, an `open()` -- `write()` -- `close()` of the frame's
/// document from its handler does not fire a second one (HTML "iframe load in progress" sets the document's "mute
/// iframe load" flag). Chrome 150, measured: `iframe.onload` that opens, writes and closes the frame's document runs
/// ONCE; korvus fired it again from inside `close()`, with the frame's URL now the page's. a bot-detection script's press & hold
/// captcha builds its challenge frames exactly so, and its state broke on the second run (its hold ended in a
/// `TypeError` reported to PX as a clientError, 11 per replayed session).
pub(crate) fn fire_initial_blank_load<'s>(scope: &mut v8::PinScope<'s, '_>, el: u32, target: v8::Local<'s, v8::Value>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<InitialBlankLoads>().is_none() {
            st.put(InitialBlankLoads::default());
        }
        st.borrow_mut::<InitialBlankLoads>().0.insert(el);
    }
    if let Some(ctx) = bc(scope, |b| b.by_frame.get(&el).copied()) {
        with_ctx(scope, ctx, |c| c.load_event = FrameLoadEvent::InProgress);
    }
    fire_at(scope, target, "load");
    if let Some(l) = op_state.borrow_mut().try_borrow_mut::<InitialBlankLoads>() {
        l.0.remove(&el);
    }
    if let Some(ctx) = bc(scope, |b| b.by_frame.get(&el).copied()) {
        with_ctx(scope, ctx, |c| {
            if c.load_event == FrameLoadEvent::InProgress {
                c.load_event = FrameLoadEvent::Completed;
            }
        });
    }
}

/// Whether frame element `el` asks for a navigation when it gets its browsing context -- the browser's
/// `IsRequestingNavigation` from then until the navigation commits. An `<iframe srcdoc>` does (`about:srcdoc` goes
/// through the browser), and so does any `src` but an empty one or `about:blank` -- which Blink commits
/// synchronously in its initial empty document. Measured on Chrome 150 (`fixtures/chrome-object/frames-chrome-150.json`):
/// a frame inserted with a `srcdoc`, set before its insertion or parsed, or with a same-origin `src`, shows no
/// `chrome.app` until it commits; one with no `src` or `src="about:blank"` has it, and keeps it when a `srcdoc`
/// is set afterwards. A `javascript:` `src` set before the insertion requests one too: Chromium 152 (a reference host-OS policy,
/// 2026-10-22) shows `Object.keys(chrome)` `["loadTimes","csi"]` on such a frame at insertion and 600 ms later, for
/// `javascript:;` and for a URL whose value replaces the document alike, while a frame given the same `src` after its
/// insertion keeps `app` -- and a bot-detection endpoint's challenge frame reads exactly a `src="javascript:;"` one at insertion.
fn requests_navigation(dom: &korvus_dom::Dom, el: u32) -> bool {
    let is_iframe = dom.tag_name(el).is_some_and(|t| t.eq_ignore_ascii_case("iframe"));
    if is_iframe && dom.get_attribute(el, "srcdoc").is_some() {
        return true;
    }
    let Some(src) = dom.get_attribute(el, "src") else { return false };
    let src = src.trim_matches(|c: char| c.is_ascii_whitespace()).to_ascii_lowercase();
    let blank = src == "about:blank" || src.starts_with("about:blank#") || src.starts_with("about:blank?");
    !(src.is_empty() || blank)
}

/// Take the extension bindings off context `id`'s initial window ([`requests_navigation`];
/// `modules/chrome_object::take_app` has the Chromium sources), to go back when its navigation commits
/// ([`commit_extension_bindings`]).
fn defer_extension_bindings(scope: &mut v8::PinScope, id: u32) {
    let Some(w) = window_of(scope, id) else { return };
    let Some(app) = crate::modules::chrome_object::take_app(scope, w) else { return };
    let g = v8::Global::new(scope, app);
    with_ctx(scope, id, |c| c.deferred_bindings = Some(g));
}

/// Context `id`'s navigation is ready to commit: the bindings [`defer_extension_bindings`] took go back on the
/// window that has been showing the initial about:blank -- the same window the committed document keeps, as a
/// same-origin first navigation reuses it, so a `chrome` object the page held since gains `app` too (measured).
fn commit_extension_bindings(scope: &mut v8::PinScope, id: u32) {
    let Some(g) = with_ctx(scope, id, |c| c.deferred_bindings.take()).flatten() else { return };
    let Some(w) = window_of(scope, id) else { return };
    let app = v8::Local::new(scope, &g);
    crate::modules::chrome_object::restore_app(scope, w, app);
}

/// The text of a `blob:` URL's Blob, from the registry `URL.createObjectURL` writes (read
/// synchronously; a Blob keeps its bytes as `_bytes`, so the text is available without the async
/// FileReader/Response round-trip the page-facing API would have to take).
///
/// That registry moved from two JS `Map`s behind `KB.__blobUrlContents` into Rust
/// (`modules/url/url.rs::BlobUrlRegistry`), and this is now a direct read of it.
///
/// IT ALSO REPAIRED THIS FUNCTION. The map it used to read stored the DECODED TEXT (a JS string) while
/// this code looked for an object with `_bytes` on it, so `v8::Local::<v8::Object>::try_from` failed
/// on every lookup and the answer was ALWAYS `""` -- measured at master: an `<iframe>` whose `src` was
/// a `blob:` URL for `<p id=q>HELLO</p>` had an EMPTY body. Chrome renders that document. The two
/// halves agree now, so a `blob:` iframe gets its markup.
fn blob_text(scope: &mut v8::PinScope, href: &str) -> String {
    crate::modules::url::url::blob_url_text(scope, href).unwrap_or_default()
}

/// `iframe.src = url` navigates the frame. Only `blob:` URLs are resolved here, from the registry
/// `URL.createObjectURL` writes -- location first, then the document, because a blob: document's
/// scripts resolve their own relative URLs against the blob URL. Any other scheme is a real
/// navigation: the attribute write already put the request in flight through the subresource seam
/// (dom/09j-subresource.js), and the document is built and `load` fired from `frame_navigated` when
/// the response lands.
fn frame_src_set(scope: &mut v8::PinScope, el: u32, v: v8::Local<v8::Value>) {
    let href = nullish_or_string(scope, v);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    set_attribute_notify(scope, &op_state, el, "src", &href);
    if tag_upper(&op_state, el) != "IFRAME" {
        return;
    }
    if href.starts_with("blob:") {
        let id = frame_ctx_for(scope, el);
        let html = blob_text(scope, &href);
        let key = normal_url(&href);
        with_ctx(scope, id, |c| c.blob_nav = Some(key));
        commit_extension_bindings(scope, id);
        set_location(scope, id, &href);
        load_frame_html(scope, id, &html);
    }
}

fn normal_url(u: &str) -> String {
    url::Url::parse(u).map(|p| p.to_string()).unwrap_or_else(|_| u.to_string())
}
/// `iframe.srcdoc = markup` navigates the frame to `about:srcdoc` with that markup as its document.
/// No network is involved, so this happens now rather than on a fetch callback; `load` still fires on
/// a later task, because that is when a navigation completes and when the page's handler exists.
fn frame_srcdoc_set(scope: &mut v8::PinScope, el: u32, v: v8::Local<v8::Value>) {
    // (DOMString or TrustedHTML): a TrustedHTML read as itself, nothing on it called; anything else ToString'd --
    // `null` is "null" and `undefined` "undefined" (Chrome 150; this made both "")
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: "HTMLIFrameElement srcdoc", context: "Failed to set the 'srcdoc' property on 'HTMLIFrameElement'" };
    let Some(html) = crate::core::trusted_types::tables::compliant_text(scope, v, &sink, crate::core::trusted_types::tables::SinkAt::Node(el)) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // the write navigates a connected frame, as any write of the attribute does (`subresource::resource_attr`);
    // a frame in no document has no browsing context to navigate until it is inserted
    set_attribute_notify(scope, &op_state, el, "srcdoc", &html);
}
/// A frame's navigation to its `srcdoc`: from a write of the attribute (`subresource::resource_attr`), the frame's
/// insertion and the parser pass (which writes the attribute directly). It COMMITS a few milliseconds after it
/// begins (stock Chrome 150: the frame's inner script ran after the parent's 0-2 ms timers and
/// before its 16 ms one, the frame showing its initial about:blank document -- `complete`, an empty body -- until
/// then); this loaded it at once, so a `srcdoc` frame's scripts ran inside the `appendChild` that inserted it. On
/// beginning, the ongoing navigation is superseded, the about base URL is taken from the creator and the parent's
/// load waits for it. `location` is set BEFORE the scripts run.
pub(crate) fn frame_load_srcdoc(scope: &mut v8::PinScope, el: u32, html: &str) {
    begin_inline_navigation(scope, el, "about:srcdoc", html);
}

/// A connected frame whose `srcdoc` was removed navigates to `about:blank` (Chrome 150, its `src` absent; with one,
/// `src` is fetched again -- `subresource::resource_attr_removed`), committing as a `srcdoc` navigation does.
pub(crate) fn frame_load_blank(scope: &mut v8::PinScope, el: u32) {
    begin_inline_navigation(scope, el, "about:blank", "");
}

/// Begin a navigation of frame `el` to a document no network carries (`about:srcdoc` with its markup, or
/// `about:blank`), committed [`SRCDOC_COMMIT_MS`] later by [`cb_srcdoc_commit`].
fn begin_inline_navigation(scope: &mut v8::PinScope, el: u32, url: &str, html: &str) {
    // the navigation begins NOW -- or as the insertion that began it did, before the context made for it took the
    // milliseconds a realm costs (Chrome 150, 2026-10-11: a srcdoc frame's `performance.timeOrigin` is
    // 0.2-0.5 ms after its insertion -- it keeps the window its initial about:blank document was made in; korvus's
    // was 7-8 ms after, past its realm's build)
    let now = navigation_begins_now(scope);
    let id = frame_ctx_for(scope, el);
    let gen = bc(scope, |b| {
        let Some(c) = b.ctxs.get_mut(id as usize) else { return 0 };
        c.nav_gen = c.nav_gen.wrapping_add(1);
        c.nav_pending = true;
        c.nav_gen
    });
    note_about_base(scope, id);
    delay_load(scope, el);
    // the document this navigation commits starts its clock then (see `Ctx::inline_nav_start`)
    with_ctx(scope, id, |c| c.inline_nav_start = Some(now));
    let d = pack_str(scope, id, &format!("{gen}\u{1f}{url}\u{1f}{html}"));
    // the browser's round trip begins once the task that began the navigation ends -- a frame's own setup
    // here can take longer than the commit itself, which put the first of two srcdoc frames appended in one task
    // ahead of a 0 ms timer queued after them (Chrome 150: the timer, then both frames)
    let f = build_fn_data(scope, cb_srcdoc_queue, "", 0, d);
    scope.enqueue_microtask(f);
}

/// [`begin_inline_navigation`]'s microtask: queue the commit [`SRCDOC_COMMIT_MS`] from now.
fn cb_srcdoc_queue(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let d = crate::binding::protos::body_data(scope, &args);
    let f = build_fn_data(scope, cb_srcdoc_commit, "", 0, d);
    crate::core::intersection_observer::rendering::queue_task(scope, f, SRCDOC_COMMIT_MS);
}

/// How long a `srcdoc` navigation takes to commit: the browser process's round trip, as measured above (Chrome's
/// inner script ran between the parent's 2 and 16 ms timers, most often between its 4 and 8 ms ones).
const SRCDOC_COMMIT_MS: f64 = 4.0;

/// The commit of [`frame_load_srcdoc`]'s navigation -- unless a newer navigation superseded it (that one owns the
/// frame and its load delay) or the frame left its document meanwhile (the navigation went with its navigable).
fn cb_srcdoc_commit(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Some((id, packed)) = current_nav(scope, data) else { return };
    let (url, html) = packed.split_once('\u{1f}').unwrap_or(("about:srcdoc", packed.as_str()));
    if let Some(el) = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el)) {
        if crate::core::html::subresource::home_of(scope, el) == crate::core::html::subresource::Home::Nowhere {
            undelay_load(scope, el);
            return;
        }
    }
    commit_extension_bindings(scope, id);
    forget_previous_window(scope, id);
    set_location(scope, id, url);
    load_frame_html(scope, id, html);
}
/// The subresource loader's callback for a frame's own navigation: the response for `iframe.src`.
/// `body` is the document the frame navigated to (`null` when the fetch failed, which still ends a
/// navigation -- a browser shows an error document and fires `load`). Scripts run AFTER `location`
/// is set and the document is installed, because the frame's own `<script src>` URLs resolve against
/// THAT location; `load` fires once they have all run.
/// Whether frame `id`'s document is same-origin with the top-level document (every ancestor's origin is
/// not consulted: Chrome's notification rule is about the TOP).
pub(crate) fn frame_same_origin_with_top(scope: &mut v8::PinScope, id: u32) -> bool {
    // The page's origin from its delivered URL, never from its (page-visible) `location`.
    let top = crate::core::feature_policy::document_origin(scope);
    let mine = crate::core::feature_policy::frame_origin(scope, id);
    top != "null" && top == mine
}

/// Frame `id`'s element's attribute `name`, when the frame has an element and it carries one.
pub(crate) fn frame_element_attr(scope: &mut v8::PinScope, id: u32, name: &str) -> Option<String> {
    let el = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.el))?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(el, name).map(str::to_string))
}

/// Hand frame `id`'s committed document to a child engine when it is cross-site to the top
/// (`core/html/remote_frame.rs`). `true` when it did -- the caller then loads nothing locally.
fn try_go_remote(scope: &mut v8::PinScope, id: u32, url: &str, html: &str, headers: &[(String, String)]) -> bool {
    if !crate::core::html::remote_frame::enabled() {
        return false;
    }
    let top_url = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default()
    };
    if !crate::core::html::remote_frame::wants_remote(url, &top_url) {
        return false;
    }
    let Some(host) = crate::core::html::remote_frame::host(scope) else { return false };
    let parent = frame_parent_ctx(scope, id).filter(|&p| p != MAIN_CTX);
    let parent_origin = document_origin(scope, parent);
    let parent_url = match frame_parent_ctx(scope, id) {
        Some(p) => loc_href(scope, p),
        None => String::new(),
    };
    let policy = frame_navigation_policy(scope, id).map(|p| p.token()).unwrap_or("").to_string();
    let referrer = frame_referrer(&parent_url, url, &policy);
    let viewport = crate::core::html::window_geometry::frame_viewport_size(scope, id).unwrap_or((300.0, 150.0));
    let injected: Vec<String> = std::env::var_os("KORVUS_FRAME_EARLY").and_then(|p| std::fs::read_to_string(p).ok()).into_iter().collect();
    // The body arrives here DECODED, so it travels as UTF-8 and says so.
    let mut hs: Vec<(String, String)> = headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("content-type")).cloned().collect();
    hs.push(("content-type".to_string(), "text/html; charset=utf-8".to_string()));
    // `location.ancestorOrigins` of the frame: its parent first, the top last.
    let mut ancestor_origins: Vec<String> = Vec::new();
    let mut cur = id;
    for _ in 0..64 {
        let Some(p) = frame_parent_ctx(scope, cur) else { break };
        ancestor_origins.push(document_origin(scope, (p != MAIN_CTX).then_some(p)));
        if p == MAIN_CTX {
            break;
        }
        cur = p;
    }
    // What the frame inherits disabled is decided HERE, where the container and the parent's declared
    // policy are: the child engine only applies the answer.
    let child_origin = url::Url::parse(url).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".to_string());
    let disabled_features: Vec<String> = crate::core::feature_policy::REGISTRY
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !crate::core::feature_policy::inherited_enabled(scope, n, id, &child_origin))
        .map(str::to_string)
        .collect();
    let session_seed = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<spec::farble::SessionSeed>().map(|s| s.0)
    };
    let timing = with_ctx(scope, id, |c| c.delivered.timing.clone()).flatten();
    let cluster_isolated = crate::core::html::isolation_policy::cluster_isolated(scope);
    let scrollbars_off = {
        let el = with_ctx(scope, id, |c| c.el).flatten();
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        el.zip(st.try_borrow::<crate::binding::HostDom>()).is_some_and(|(el, h)| crate::core::html::remote_frame::scrolling_off(&h.0, el))
    };
    let rid = host.spawn(spec::RemoteFrameSpawn {
        url: url.to_string(),
        headers: hs,
        body: html.as_bytes().to_vec(),
        parent_origin,
        top_url,
        referrer,
        viewport,
        scrollbars_off,
        injected,
        ancestor_origins,
        disabled_features,
        session_seed,
        timing,
        cluster_isolated,
    });
    if std::env::var_os("KORVUS_TRACE_OOPIF").is_some() {
        eprintln!("[oopif] frame ctx {id} -> remote #{rid}: {url}");
    }
    forget_previous_window(scope, id);
    if let Some(d) = parse_frame_doc(scope, id, "") {
        set_ctx_document(scope, id, d);
    }
    set_location(scope, id, url);
    with_ctx(scope, id, |c| c.remote = Some(rid));
    crate::core::html::remote_frame::arm_child_recv(scope, id, rid);
    true
}

/// The `timing` member of a loader response object, when the transport measured one.
pub(crate) fn response_timing(scope: &mut v8::PinScope, resp: v8::Local<v8::Object>) -> Option<spec::FetchTiming> {
    let t = get_prop(scope, resp, "timing").filter(|t| t.is_object())?;
    deno_core::serde_v8::from_v8::<spec::FetchTiming>(scope, t).ok()
}

/// A remote frame's `parent.postMessage(...)`: delivered at the window of the document holding frame `id`'s
/// element, in that window's realm, with `source` = the frame's cross-origin WindowProxy.
pub(crate) fn remote_message_to_parent(scope: &mut v8::PinScope, id: u32, bytes: &[u8], origin: &str) {
    let Some(parent) = frame_parent_ctx(scope, id) else { return };
    let realm = if parent == MAIN_CTX { Some(main_context(scope)) } else { realm_of(scope, parent) };
    let Some(realm) = realm else { return };
    let source = xo_window(scope, id).map(|w| v8::Global::new(scope, w));
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    let Some(target) = window_of(scope, parent) else { return };
    let data = crate::modules::clone_state::wire_deserialize(scope, bytes, &[]);
    let source = source.map(|g| v8::Local::<v8::Value>::from(v8::Local::new(scope, &g)));
    crate::core::html::remote_frame::dispatch_message(scope, target, data, origin, source);
}

/// The current realm's CSP list as the headers that would deliver it: what a `blob:` / `data:` worker it starts
/// inherits (HTML's policy container -- a worker whose script no response delivered holds its creator's policies),
/// handed across to the worker's thread. In a worker realm, the worker's own list (a nested worker inherits it).
pub(crate) fn current_realm_csp_headers(scope: &mut v8::PinScope) -> Vec<(String, String)> {
    let id = realm_ctx(scope);
    ctx_csp(scope, id).as_headers()
}

/// The enforced policies frame context `id`'s document was DELIVERED with or inherited -- not its own metas, which a
/// parser-inserted script reads only as far as the parser has reached (`csp::script_policies`).
pub(crate) fn ctx_delivered_enforced(scope: &mut v8::PinScope, id: u32) -> Vec<crate::core::html::csp::Policy> {
    bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.delivered.csp.enforced_policies()).unwrap_or_default())
}

/// The CSP list of the document in context `id` (`None`, or the page's own id: the page document's): a
/// frame's delivered or inherited policies, then the metas in its document's `<head>`.
pub(crate) fn ctx_csp(scope: &mut v8::PinScope, id: Option<u32>) -> crate::core::html::csp::CspList {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(id) = id.filter(|&i| i != MAIN_CTX) else {
        let st = op_state.borrow();
        return crate::core::html::csp::page_csp(&st);
    };
    let (mut list, root) = {
        let st = op_state.borrow();
        let Some(b) = st.try_borrow::<BrowsingContexts>() else { return Default::default() };
        match b.ctxs.get(id as usize) {
            Some(c) => (c.delivered.csp.clone(), c.doc_root),
            None => return Default::default(),
        }
    };
    if let Some(root) = root {
        let st = op_state.borrow();
        if let Some(h) = st.try_borrow::<HostDom>() {
            list.add_metas(&h.0, root);
        }
    }
    list
}

/// The Trusted Types requirement of the document in context `id` ([`ctx_csp`]'s, read in place: every DOM sink
/// asks it).
pub(crate) fn ctx_trusted_types(scope: &mut v8::PinScope, id: Option<u32>) -> crate::core::html::csp::TrustedTypesRequirement {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(id) = id.filter(|&i| i != MAIN_CTX) else {
        return crate::core::html::csp::page_trusted_types(&st);
    };
    let Some(b) = st.try_borrow::<BrowsingContexts>() else { return Default::default() };
    let Some(c) = b.ctxs.get(id as usize) else { return Default::default() };
    let mut r = c.delivered.csp.trusted_types();
    if let (Some(root), Some(h)) = (c.doc_root, st.try_borrow::<HostDom>()) {
        r |= crate::core::html::csp::metas_trusted_types(&h.0, root);
    }
    r
}

/// The context whose realm `context` is: a frame's id, `None` for the page's (or a runtime with no frames).
pub(crate) fn ctx_of_context(scope: &mut v8::PinScope, context: v8::Local<v8::Context>) -> Option<u32> {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if st.try_borrow::<BrowsingContexts>().map_or(true, |b| b.ctxs.is_empty()) {
            return None;
        }
    }
    let g = context.global(scope);
    ctx_of_window(scope, g).filter(|&id| id != MAIN_CTX)
}

/// The realm of context `id` (`None`: the page's -- the current one in a runtime with no frames, a worker's).
pub(crate) fn ctx_realm<'s>(scope: &mut v8::PinScope<'s, '_>, id: Option<u32>) -> Option<v8::Local<'s, v8::Context>> {
    let has_contexts = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<BrowsingContexts>().is_some()
    };
    if !has_contexts {
        return Some(scope.get_current_context());
    }
    realm_of(scope, id.unwrap_or(MAIN_CTX))
}

/// The context of the document that node `node` belongs to -- the one whose policies govern a DOM sink on
/// it (Blink: the node's `GetExecutionContext()`, its node document's window): a frame's id for a frame's
/// document, `None` for the page's. A document no frame shows belongs to the realm that made it
/// ([`realm_of_node`]).
pub(crate) fn node_ctx(scope: &mut v8::PinScope, node: u32) -> Option<u32> {
    let realm = realm_of_node(scope, node)?;
    ctx_of_context(scope, realm)
}

/// HTML's policy container for a document no response delivered: frame `id`'s document takes a clone of
/// its parent document's CSP list (`about:blank` -- the initial one too --, `about:srcdoc`, `blob:`,
/// `data:`).
fn inherit_csp(scope: &mut v8::PinScope, id: u32) {
    let parent = frame_parent_ctx(scope, id);
    let list = ctx_csp(scope, parent);
    with_ctx(scope, id, |c| c.delivered.csp = list);
}

fn is_local_scheme(url: &str) -> bool {
    starts_with_ci(url, "about:") || starts_with_ci(url, "blob:") || starts_with_ci(url, "data:")
}

/// The eval policy of the realm `context` (a frame's document's, or the page's): the code-generation
/// check asks the realm whose `eval` / `Function` / timer it is (Blink `ExecutionContext::From(context)`).
pub(crate) fn realm_eval_policy(scope: &mut v8::PinScope, context: v8::Local<v8::Context>) -> crate::core::html::csp::EvalPolicy {
    // a DevTools client switched the page's policy off (`Page.setBypassCSP`): a string may become code
    if spec::devtools::bypass_csp() {
        return crate::core::html::csp::EvalPolicy::default();
    }
    let id = ctx_of_context(scope, context);
    ctx_csp(scope, id).eval_policy()
}

/// The eval policy of the current realm (`core/html/csp.rs`).
pub(crate) fn running_eval_policy(scope: &mut v8::PinScope) -> crate::core::html::csp::EvalPolicy {
    let context = scope.get_current_context();
    realm_eval_policy(scope, context)
}

/// When the DOM insertion now being reported began (a [`spec::process_ms`] reading), while it is: a frame it inserts
/// begins its navigation as it is inserted, and the milliseconds its context's realm then costs are korvus's, not
/// the navigation's. An isolate slot: every insertion reports through it.
#[derive(Default, Clone, Copy)]
struct InsertionBegan(Option<f64>);

/// A DOM insertion's report begins ([`crate::core::dom::shared`]'s childList notification): navigations it begins date
/// from now. Returns what an enclosing insertion had set, for [`insertion_ended`].
pub(crate) fn insertion_began(scope: &mut v8::PinScope) -> Option<f64> {
    let outer = scope.get_slot::<InsertionBegan>().and_then(|s| s.0);
    scope.set_slot(InsertionBegan(Some(spec::process_ms(std::time::Instant::now()))));
    outer
}

/// The insertion's report is over: put back what an enclosing one had set.
pub(crate) fn insertion_ended(scope: &mut v8::PinScope, outer: Option<f64>) {
    scope.set_slot(InsertionBegan(outer));
}

/// When a navigation beginning now began: the insertion being reported, if one is, else now (HTML: a document's time
/// origin is its navigation's start, and an inserted frame's navigation starts with its insertion).
pub(crate) fn navigation_begins_now(scope: &mut v8::PinScope) -> f64 {
    match scope.get_slot::<InsertionBegan>().and_then(|s| s.0) {
        Some(began) => began,
        None => spec::process_ms(std::time::Instant::now()),
    }
}

/// A remote frame's document fired `load`: its `<iframe>` element does, and the parent stops waiting on it.
pub(crate) fn remote_frame_loaded(scope: &mut v8::PinScope, id: u32) {
    ctx_loaded(scope, id);
}

/// The policies the response for frame element `el`'s navigation delivered (`Ctx::delivered`), recorded
/// before [`frame_navigated`] builds the document so its first script already reads them.
pub(crate) fn frame_response_delivered(scope: &mut v8::PinScope, el: u32, headers: &[(String, String)], timing: Option<spec::FetchTiming>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !is_frameish(&op_state, el) {
        return;
    }
    let id = frame_ctx_for(scope, el);
    let mut delivered = FrameDelivered::from_headers(headers);
    delivered.timing = timing;
    with_ctx(scope, id, |c| {
        c.delivered = delivered;
        c.inline_nav_start = None;
    });
}

/// The encoding frame `el`'s document was decoded with (`spec::SubresourceResponse::encoding`), kept with
/// what its response delivered -- after [`frame_response_delivered`], which replaces that record.
pub(crate) fn frame_encoding_delivered(scope: &mut v8::PinScope, el: u32, encoding: Option<String>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !is_frameish(&op_state, el) {
        return;
    }
    let id = frame_ctx_for(scope, el);
    with_ctx(scope, id, |c| c.delivered.encoding = encoding);
}

/// The encoding of node `id`'s document. A nested context's document that a response delivered has its own
/// (Chrome 150 reports a frame's own `characterSet`); every other document has the engine's `DocEncoding` -- the
/// page's, which is also what a document no response delivered (`about:blank`, `srcdoc`) inherits from its
/// creator. What `document.characterSet`, a form's submission and a link's query encoding read.
pub(crate) fn node_document_encoding(scope: &mut v8::PinScope, id: u32) -> &'static encoding_rs::Encoding {
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().map(|h| h.0.root_of(id))
    };
    if let Some(e) = root.and_then(|r| document_root_encoding(scope, r)) {
        return e;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<crate::core::dom::DocEncoding>().map(|d| d.0).unwrap_or(encoding_rs::UTF_8)
}

/// What frame element `el`'s next document may inherit from its parent -- the parent document's encoding and
/// origin -- for the decoder's "parent's encoding" step (Blink `TextResourceDecoderBuilder`: the hint is used only
/// when the parent's origin `CanAccess` the frame's, which the loader checks against the response's final URL). `None`
/// for a frame sandboxed without `allow-same-origin`: its origin is opaque and no parent can access it.
pub(crate) fn frame_parent_hint(scope: &mut v8::PinScope, el: u32) -> Option<(String, String)> {
    let sandbox = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(el, "sandbox").map(str::to_string))
    };
    if sandbox.is_some_and(|s| !s.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("allow-same-origin"))) {
        return None;
    }
    let origin = url::Url::parse(&crate::core::html::subresource::node_document_url(scope, el)).ok()?.origin();
    if !origin.is_tuple() {
        return None;
    }
    let encoding = node_document_encoding(scope, el).name().to_string();
    Some((encoding, origin.ascii_serialization()))
}

/// The delivered encoding of the nested document whose arena root is `root`, if it is one.
pub(crate) fn document_root_encoding(scope: &mut v8::PinScope, root: u32) -> Option<&'static encoding_rs::Encoding> {
    let id = frame_of_document_root(scope, root)?;
    let name = bc(scope, |b| b.ctxs.get(id as usize).and_then(|c| c.delivered.encoding.clone()))?;
    encoding_rs::Encoding::for_label(name.as_bytes())
}

pub(crate) fn frame_navigated(scope: &mut v8::PinScope, el: u32, body: v8::Local<v8::Value>, url: v8::Local<v8::Value>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !is_frameish(&op_state, el) {
        return;
    }
    let id = frame_ctx_for(scope, el);
    let u = js_string(scope, url);
    // ONE NAVIGATION, NOT TWO. `iframe.src = blobUrl` navigates the frame synchronously
    // ([`frame_src_set`]: the Blob's bytes are the engine's own) AND writes the attribute, which issues
    // the subresource loader's fetch for the same URL. That fetch fails (the transport cannot resolve
    // `blob:`) and arrived here as a SECOND navigation to an empty document with a SECOND `load`:
    //
    //   iframe.src = URL.createObjectURL(<p>hello</p>)   korvus load x2 (body "<p>hello</p>", then "")
    //                                                    chrome 150  load x1 (body "<p>hello</p>")
    //
    // WPT's html5lib_url runner reuses one iframe per test and installs the NEXT test's `onload` right
    // after the previous test's; whichever of the stale `load` and the new navigation's landed first
    // decided the verdict, so `?file=scripted_adoption01` and `?file=pending-spec-changes-plain-text-
    // unsafe` passed or failed by timing. The completion of a navigation this frame already made is
    // dropped; it still ends the loader's load-event delay, which is keyed by this element.
    let already = with_ctx(scope, id, |c| {
        if c.blob_nav.as_deref() == Some(normal_url(&u).as_str()) {
            c.blob_nav = None;
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if already {
        undelay_load(scope, el);
        return;
    }
    let mut html = nullish_or_string(scope, body);
    // A `blob:` reached through `setAttribute("src", …)` has no synchronous navigation in front of it:
    // its document is still the Blob's bytes, never the failed fetch's empty body.
    if starts_with_ci(&u, "blob:") && html.is_empty() {
        html = blob_text(scope, &u);
    }
    // a `blob:` or `data:` document's policies are its parent's, not a response's
    if is_local_scheme(&u) {
        inherit_csp(scope, id);
    }
    let headers = with_ctx(scope, id, |c| c.delivered.all_headers.clone()).unwrap_or_default();
    commit_extension_bindings(scope, id);
    if try_go_remote(scope, id, &u, &html, &headers) {
        return;
    }
    forget_previous_window(scope, id);
    // The document its MIME type makes (A/B C5); a `blob:`/`data:` document has no delivered headers here.
    let (shown, mime) = frame_document_source(&headers, &html, &u);
    begin_image_document(scope, id, mime.as_deref(), &u);
    if let Some(d) = parse_frame_doc(scope, id, shown.as_deref().unwrap_or(&html)) {
        set_ctx_document(scope, id, d);
        record_frame_content_type(scope, id, mime);
    }
    set_location(scope, id, &u);
    begin_frame_document(scope, id);
    run_doc_scripts(scope, id);
}
/// A NEW DOCUMENT GETS A NEW WINDOW, so the old document's listeners on it are gone.
/// HTML creates a Window per Document -- reusing it only when the context leaves its INITIAL
/// `about:blank` for a same-origin document -- and this engine keeps one window object per frame, so
/// the first document's listeners stayed registered on the second document's window. YouTube's embed
/// registers `addEventListener('DOMContentLoaded', () => { window.ytAtN(); delete window.ytAtN; })`;
/// the IFrame API re-navigates every embed with `enablejsapi=1`, and the second document's DCL then ran
/// BOTH documents' listeners -- the second call found `ytAtN` deleted: "window.ytAtN is not a function",
/// once per embed on un.org, where Chrome throws nothing. Only a navigation REPLACING a navigated
/// document drops them: the first one away from the initial `about:blank` keeps what the parent
/// registered, and `document.open()` is not a navigation at all. The window's own properties stay --
/// a window object per document is F9's.
fn forget_previous_window(scope: &mut v8::PinScope, id: u32) {
    let replacing = with_ctx(scope, id, |c| std::mem::replace(&mut c.navigated, true)).unwrap_or(false);
    if !replacing {
        return;
    }
    let Some(iwin) = window_of(scope, id) else { return };
    crate::core::events::event_target::et_forget(scope, iwin.into());
    // AND A NEW REALM: the new document's scripts start from a fresh global scope -- a
    // second document declaring the `let` the first one declared is not a redeclaration -- behind the
    // same WindowProxy, so a parent holding `contentWindow` still holds the frame's window. The old realm is
    // DETACHED first, as Blink does before reusing a proxy: V8 then gives the old global a proxy of its own
    // for its old document's accessor reads, so they never reach through the frame's proxy into the new
    // global ([`cb_lazy_interface`]).
    if let Some(old) = with_ctx(scope, id, |c| c.realm.clone()).flatten() {
        let old = v8::Local::new(scope, &old);
        crate::binding::v8_legacy::detach_global(old);
        // DetachGlobal also gives the old context a security token of its own, so its document's code
        // could no longer reach any other realm ("TypeError: no access" on un.org, uncaught where it
        // threw nothing before). Every realm of a page shares one token here (`new_realm`): the old one
        // keeps it.
        let token = main_context(scope).get_security_token(scope);
        old.set_security_token(token);
    }
    let realm = new_realm(scope, Some(iwin));
    let fresh = realm.global(scope);
    let (w, r) = (v8::Global::new(scope, fresh), v8::Global::new(scope, realm));
    with_ctx(scope, id, |c| {
        c.iwin = w;
        c.realm = Some(r);
        c.loc = None;
        c.installed = false;
    });
    build_frame_realm(scope, id, realm, fresh);
    if let Some(d) = doc_of(scope, id) {
        set_prop(scope, fresh, "document", d.into());
    }
}

// `navigate_frame` WAS HERE. It was a form submission's "navigation" of its target
// frame: the frame's location became the submission URL and its `load` fired, and NOTHING WAS
// FETCHED -- so `contentDocument` still held the previous document while `contentWindow.location`
// claimed a new one. That is the incoherence the file comment called a deliberate limit. A form
// targeting a frame goes through [`navigate_target`] now, which is the same `navigate_context` every
// other nested navigation takes: a real request, a real response, a real document.

/// `window[0]`, `window[1]`, ... -- the child browsing contexts, by index. Measured against Brave 151:
/// appending an iframe makes `window[0]` an OWN DATA property (enumerable, configurable) equal to that
/// frame's contentWindow, and removing the frame drops both the count and the index. Synchronous, from
/// the native childList notification, because that is when a browser creates and destroys the context.
/// `forced` is the parser's full rescan (the parser builds its nodes straight into the arena without
/// a notification), run BEFORE the page's own scripts.
///
/// A frame's NAME is not kept here any more: it was an own non-enumerable property of the window, so
/// `Object.getOwnPropertyNames(window)` and `hasOwnProperty` listed it, `delete window.name` removed it, and
/// a frame named `addEventListener` hid `EventTarget.prototype`'s. Chrome 150 answers it from the Window's
/// named properties object on the prototype chain (a bot-detection service reads it): [`named_child_window`].
/// Whether a childList change can move `window.length`/`window[i]`: some frame already exists, or an
/// added node is one. The common case -- no frames anywhere, a `<div>` appended -- costs two lookups
/// instead of an array and a scan per insertion.
pub(crate) fn frames_may_change(scope: &mut v8::PinScope, added: &[v8::Local<v8::Value>]) -> bool {
    if bc(scope, |b| b.frame_idx_count) > 0 {
        return true;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    added.iter().any(|n| unwrap_id(scope, *n).is_some_and(|id| holds_frame(&op_state, id)))
}

pub(crate) fn sync_frames(scope: &mut v8::PinScope, forced: bool, added: v8::Local<v8::Value>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let count = bc(scope, |b| b.frame_idx_count);
    if count == 0 && !forced {
        // Nothing to maintain yet, so only a frame ENTERING the tree can change that.
        let touches = array_ids(scope, added).into_iter().any(|id| holds_frame(&op_state, id));
        if !touches {
            return;
        }
    }
    // Once a page HAS frames, any childList mutation can change the set: the document is the
    // authority, not the mutation record.
    let root = op_state.borrow().borrow::<HostDom>().0.root();
    let frames = frames_now(&op_state, root, forced);
    let n = frames.len() as u32;
    let global = global_obj(scope);
    for (i, &f) in frames.iter().enumerate() {
        let id = frame_ctx_for(scope, f);
        let w: v8::Local<v8::Value> = match window_for_page(scope, id) {
            Some(w) => w.into(),
            None => v8::null(scope).into(),
        };
        let k = name_key(scope, &i.to_string());
        global.define_own_property(scope, k, w, v8::PropertyAttribute::NONE);
    }
    for i in n..count {
        let k = name_key(scope, &i.to_string());
        global.delete(scope, k.into());
    }
    bc(scope, |b| b.frame_idx_count = n);
}

/// The window's DOCUMENT-TREE CHILD NAVIGABLE called `name` (HTML "named access on the Window object"), for the
/// Window named properties object `holder` (`core/dom/window_named.rs`): Blink's
/// `WindowProperties::AnonymousNamedGetter` (`window_properties.cc:24-58`), asked before any element.
///
/// * the CHILD FRAME whose browsing context name is `name` -- the frame's `window.name`, set from its element's
///   `name` attribute when the context is made and by the frame itself after (Chrome 150: a frame that sets its
///   own `window.name` is reached by it; renaming the element afterwards reaches nothing new), among the frames
///   in this window's document tree (`FrameTree::ScopedChild`). Two of one name: the one made FIRST, Blink's
///   child-frame order, not tree order (measured: a frame inserted at the body's start after another of the same
///   name does not win);
/// * step 3: a child whose document is not same-origin with this window's is reached only when `name` is still
///   its container's `name` attribute;
/// * the child's window as this window's realm holds it -- the cross-origin view for another origin, as
///   `window[i]` holds it ([`sync_child_window_frames`]).
///
/// `None` (the common case: no context has the name) costs one walk over the contexts.
pub(crate) fn named_child_window<'s>(scope: &mut v8::PinScope<'s, '_>, holder: v8::Local<'s, v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    if name.is_empty() {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let candidates: Vec<(u32, u32)> = {
        let st = op_state.borrow();
        let b = st.try_borrow::<BrowsingContexts>()?;
        b.ctxs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.win_name == name)
            .filter_map(|(i, c)| c.el.filter(|el| b.by_frame.get(el) == Some(&(i as u32))).map(|el| (i as u32, el)))
            .collect()
    };
    if candidates.is_empty() {
        return None;
    }
    // The window this named properties object serves, and its document.
    let ctx = holder.get_creation_context(scope)?;
    let global = ctx.global(scope);
    let owner = ctx_of_window(scope, global).unwrap_or(MAIN_CTX);
    let owner_root = if owner == MAIN_CTX {
        op_state.borrow().try_borrow::<HostDom>()?.0.root()
    } else {
        frame_document_root(scope, owner)?
    };
    let (child, el) = {
        let st = op_state.borrow();
        let dom = &st.try_borrow::<HostDom>()?.0;
        candidates.into_iter().find(|&(_, el)| dom.root_of(el) == owner_root)?
    };
    let owner_origin = document_origin(scope, (owner != MAIN_CTX).then_some(owner));
    let cross = document_origin(scope, Some(child)) != owner_origin;
    if cross {
        let container_name = op_state.borrow().try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(el, "name").map(str::to_string));
        if container_name.as_deref() != Some(name) {
            return None;
        }
        return xo_window(scope, child);
    }
    window_of(scope, child)
}

/// `window[i]` on a FRAME's window: the nested contexts of the frame's OWN document, in tree order, the
/// same own data properties [`sync_frames`] keeps on the page's. Only the page's were kept: a frame's window
/// counted its frames (`length`, a walk) and held none of them -- Chrome 150, measured: an iframe whose
/// document holds an iframe (appended by the parent's script or by its own) answers `w[0] ===` that frame's
/// window, `'0' in w`, and `Object.keys(w)` lists `"0"`; here `w[0]` was undefined (the
/// challenge frame enumerates its own window and its blank frames'). The value is the child as THIS window's
/// realm holds it: its own window when same origin, else its cross-origin WindowProxy.
fn sync_child_window_frames(scope: &mut v8::PinScope, id: u32) {
    let Some(root) = with_ctx(scope, id, |c| c.doc_root).flatten() else { return };
    let Some(w) = window_of(scope, id) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let frames = frames_under(&op_state, root);
    let n = frames.len() as u32;
    let count = with_ctx(scope, id, |c| c.idx_count).unwrap_or(0);
    if n == 0 && count == 0 {
        return;
    }
    let mine = document_origin(scope, Some(id));
    for (i, &f) in frames.iter().enumerate() {
        let child = frame_ctx_for(scope, f);
        let held = if document_origin(scope, Some(child)) != mine { xo_window(scope, child) } else { window_of(scope, child) };
        let v: v8::Local<v8::Value> = match held {
            Some(c) => c.into(),
            None => v8::null(scope).into(),
        };
        let k = name_key(scope, &i.to_string());
        w.define_own_property(scope, k, v, v8::PropertyAttribute::NONE);
    }
    for i in n..count {
        let k = name_key(scope, &i.to_string());
        w.delete(scope, k.into());
    }
    with_ctx(scope, id, |c| c.idx_count = n);
}

/// A childList mutation in a FRAME's document re-keeps that frame's `window[i]` ([`sync_child_window_frames`]),
/// once it has frames or the mutation adds one. `false` when the mutated node is in the PAGE's document: the
/// page's are [`sync_frames`]'s.
pub(crate) fn sync_frames_at(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, added: &[v8::Local<v8::Value>]) -> bool {
    let Some(t) = crate::binding::protos::unwrap_id(scope, target) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (root, page_root) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        (dom.root_of(t), dom.root())
    };
    if root == page_root {
        return false;
    }
    let Some(id) = bc(scope, |b| b.by_doc.get(&root).copied().filter(|&id| b.ctxs.get(id as usize).is_some_and(|c| c.doc_root == Some(root)))) else {
        return true;
    };
    let had = with_ctx(scope, id, |c| c.idx_count).unwrap_or(0);
    if had > 0 || added.iter().any(|n| unwrap_id(scope, *n).is_some_and(|i| holds_frame(&op_state, i))) {
        sync_child_window_frames(scope, id);
    }
    true
}

// ── auxiliary browsing contexts: `window.open` ────────────────────────────────────────────────────
//
// An auxiliary context is a sibling of the page, not a child of it: its `parent` and `top` are
// ITSELF, it has no `frameElement`, it has an `opener`, and it can be CLOSED. Everything else -- a
// real parsed document over arena nodes, a location, an event target, a delivering `postMessage`,
// scripts that run (including the external ones) -- is the same machinery a nested context uses,
// because in a browser it is the same machinery.

/// A frame's (and a popup's) own `name` pair, for the context whose realm runs it ([`realm_ctx`]).
///
/// `name` is NOT one of the members a cross-origin WindowProxy exposes, so a realm holding a
/// disowned handle gets a TypeError, which is the throw WPT's `resource-popup` catches to report the
/// name as null. The popup's own scripts read it normally.
fn cb_iwin_name_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    name_get(scope, id, &args, rv);
}
fn cb_iwin_name_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    name_set(scope, id, &args);
}
fn name_get(scope: &mut v8::PinScope, id: u32, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if disowned_from(scope, id, this) {
        throw_type_error(scope, "Blocked a frame from accessing a cross-origin frame.");
        return;
    }
    let n = bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.win_name.clone())).unwrap_or_default();
    let v = str_val(scope, &n);
    rv.set(v);
}
fn name_set(scope: &mut v8::PinScope, id: u32, args: &v8::FunctionCallbackArguments) {
    let n = nullish_or_string(scope, args.get(0));
    with_ctx(scope, id, |c| c.win_name = n);
}
/// `closed` is true for a CLOSED context and also for a DISOWNED one: after a COOP browsing-context
/// group switch the handle the opener kept no longer names a context in its group.
fn cb_popup_closed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    let this: v8::Local<v8::Value> = args.this().into();
    let closed = bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.closed).unwrap_or(true));
    let d = closed || disowned_from(scope, id, this);
    rv.set_bool(d);
}
/// `close()` really closes: the flag it sets is the one `closed` reads, and a closed context stops
/// delivering. The spec's own effect on the object -- discard the context and report `closed` -- is
/// exactly this.
fn cb_popup_close(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = realm_ctx(scope) else { return };
    bc(scope, |b| {
        let Some(c) = b.ctxs.get_mut(id as usize) else { return };
        c.closed = true;
        if let (Some(n), Some(realm)) = (c.name.clone(), c.name_realm) {
            if let Some(m) = b.named.get_mut(&realm) {
                if m.get(&n) == Some(&id) {
                    m.remove(&n);
                }
            }
        }
    });
}
/// An auxiliary context starts on the initial `about:blank`, which INHERITS the opener's COOP (and
/// its origin) -- the whole reason `same-origin-allow-popups` has an effect. The opener is the realm
/// that CALLED `open`, not the page: a popup opened by a popup inherits the opening popup's policy
/// and has the INNER window as its `opener`.
fn make_popup_context(scope: &mut v8::PinScope, name: &str) -> u32 {
    let id = make_frame_context(scope);
    let opener_realm = running(scope);
    let opener_win: v8::Local<v8::Value> = match opener_realm.and_then(|c| window_of(scope, c)) {
        Some(w) => w.into(),
        None => global_obj(scope).into(),
    };
    let inherited = opener_realm.and_then(|c| bc(scope, |b| b.ctxs.get(c as usize).and_then(|x| x.policy.clone())));
    let policy = match inherited {
        Some(p) => p,
        None => doc_policy(scope),
    };
    let nm = name.to_string();
    with_ctx(scope, id, |c| {
        c.el = None;
        c.popup = true;
        c.policy = Some(policy);
        c.left_initial_about_blank = false;
        c.disowned = false;
        c.win_name = nm;
    });
    let Some(iwin) = window_of(scope, id) else { return id };
    // `parent`, `top` and `frameElement` need nothing: the frame's own getters answer the popup itself, itself
    // and null for a context with no frame element. They were written over with data properties -- a
    // popup's `parent` became one, where Chrome's is an accessor (B21). `opener` is kept by the popup's
    // realm and read by its own accessor, below.
    crate::core::dom::element::priv_set(scope, iwin, P_OPENER, opener_win);
    // The popup's own members, IN ITS OWN REALM and from their templates: Chrome
    // 150.0.7871.46 makes a popup's `name`/`closed` getters and its `close` the popup's functions
    // (`Object.getPrototypeOf(popup.close) === popup.Function.prototype`, measured). They were built here, in
    // the OPENER's realm, over the context id as data. `name` is the pair `furnish_window` gave every frame
    // (the same body); `closed` and `close` are the popup's, each finding the popup through its realm.
    let Some(realm) = realm_of(scope, id) else { return id };
    let scope = &mut v8::ContextScope::new(scope, realm);
    let g = template_fn(scope, cb_popup_closed, "get closed", 0, None);
    define_accessor(scope, iwin, "closed", g, None, true, true);
    let f = template_fn(scope, cb_popup_close, "close", 0, None);
    let k = name_key(scope, "close");
    iwin.define_own_property(scope, k, f.into(), v8::PropertyAttribute::NONE);
    let g = template_fn(scope, cb_popup_opener_get, "get opener", 0, None);
    let s = template_fn(scope, crate::core::html::window::cb_opener_set, "set opener", 1, None);
    define_accessor(scope, iwin, "opener", g, Some(s), true, true);
    id
}

/// Popup `id`'s id at the browser endpoint, when it is open as a target of its own (`core/html/popup.rs`).
fn remote_popup(scope: &mut v8::PinScope, id: u32) -> Option<u64> {
    bc(scope, |b| b.ctxs.get(id as usize).filter(|c| c.popup && c.popup_opener.is_some()).and_then(|c| c.remote))
}

/// `close()` on a cross-origin WindowProxy. A popup open as a target of its own closes: its target goes, and its
/// `closed` is true from now on (Chrome's "is closing"). Any other cross-origin window's `close()` does nothing.
fn cb_xo_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    if let Some(rid) = remote_popup(scope, id) {
        crate::core::html::popup::close(rid);
        remote_popup_closed(scope, id);
    }
}

/// A popup open as a target of its own is gone: its `closed` is true, and its name names nothing.
pub(crate) fn remote_popup_closed(scope: &mut v8::PinScope, id: u32) {
    bc(scope, |b| {
        let Some(c) = b.ctxs.get_mut(id as usize) else { return };
        c.closed = true;
        if let (Some(n), Some(realm)) = (c.name.clone(), c.name_realm) {
            if let Some(m) = b.named.get_mut(&realm) {
                if m.get(&n) == Some(&id) {
                    m.remove(&n);
                }
            }
        }
    });
}

/// A popup open as a target of its own called `opener.postMessage(...)`: delivered at the window whose script opened
/// it, in that window's realm, with `source` = the popup's cross-origin WindowProxy.
pub(crate) fn remote_popup_message(scope: &mut v8::PinScope, id: u32, bytes: &[u8], origin: &str) {
    let opener = with_ctx(scope, id, |c| c.popup_opener).flatten().flatten().unwrap_or(MAIN_CTX);
    let realm = if opener == MAIN_CTX { Some(main_context(scope)) } else { realm_of(scope, opener) };
    let Some(realm) = realm else { return };
    let source = xo_window(scope, id).map(|w| v8::Global::new(scope, w));
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    let Some(target) = window_of(scope, opener) else { return };
    let data = crate::modules::clone_state::wire_deserialize(scope, bytes, &[]);
    let source = source.map(|g| v8::Local::<v8::Value>::from(v8::Local::new(scope, &g)));
    crate::core::html::remote_frame::dispatch_message(scope, target, data, origin, source);
}

/// The private a popup's window keeps its opener's window under ([`cb_popup_opener_get`]).
const P_OPENER: &str = "korvus_popup_opener";

/// A popup's `opener`: the window that opened it, kept by the popup's realm -- an accessor, as Chrome's is
/// (the setter is the page's: an assignment replaces it with a data property).
fn cb_popup_opener_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::core::html::window_globals::window_receiver(scope, args.this().into()) {
        return;
    }
    let g = scope.get_current_context().global(scope);
    match crate::core::css::css_style_declaration::priv_get(scope, g, P_OPENER) {
        Some(w) => rv.set(w),
        None => rv.set_null(),
    }
}

/// `/(^|[,\s;])noopener([,\s;=]|$)/i` over the features string.
fn has_noopener(feat: &str) -> bool {
    let lower: Vec<char> = feat.to_lowercase().chars().collect();
    let needle: Vec<char> = "noopener".chars().collect();
    let sep = |c: char| c == ',' || c == ';' || c.is_whitespace();
    let mut i = 0;
    while i + needle.len() <= lower.len() {
        if lower[i..i + needle.len()] == needle[..] {
            let before_ok = i == 0 || sep(lower[i - 1]);
            let after_ok = match lower.get(i + needle.len()) {
                None => true,
                Some(&c) => sep(c) || c == '=',
            };
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// `window.open(url, target, features)` -- the auxiliary browsing context, really opened. Returns the
/// context's `Window`, or `null` in the two cases the spec itself returns null: the `noopener`
/// feature (the opener is deliberately given no handle) and a target that names a context this
/// engine cannot hand back. `_self`/`_parent`/`_top` all name the CURRENT top-level context here (the
/// page has no parent), and `open` on an existing context returns it without navigating it -- this
/// engine has no top-level navigation, and reporting one that did not happen would be the lie.
fn open_window<'s>(scope: &mut v8::PinScope<'s, '_>, url: v8::Local<v8::Value>, target: v8::Local<v8::Value>, features: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let feat = nullish_or_string(scope, features);
    let noopener = has_noopener(&feat);
    // "If noopener is true, then set target to `_blank`" -- the spec's own first step, so a `noopener`
    // open never reuses or returns an existing context.
    let mut name = if noopener { String::new() } else { nullish_or_string(scope, target).trim().to_string() };
    let lower = name.to_lowercase();
    // A TARGET THAT NAMES AN EXISTING CONTEXT REALLY NAVIGATES IT. `_self`, `_top` and
    // `_parent` from the page are the page itself, which is a TOP-LEVEL navigation -- the thing this
    // engine could not perform until P12d built the queue, and the reason the comment above used to
    // say "reporting one that did not happen would be the lie". Chrome 150, measured over loopback:
    // `window.open('t','_self')` lands on `t` with `Page.frameRequestedNavigation` reason `other`
    // and no `frameScheduledNavigation`; `window.open('t','frame1')` navigates the named iframe and
    // returns its window.
    if matches!(lower.as_str(), "_self" | "_parent" | "_top") || (!lower.is_empty() && lower != "_blank") {
        let initiator = current_realm(scope);
        let chosen = choose_target(scope, initiator, &name);
        // A sandboxed frame without `allow-top-navigation` opens nothing at the top: `null`.
        if matches!(chosen, NavigationTarget::Top) && initiator.is_some() && !may_navigate_top(scope) {
            return null;
        }
        if let NavigationTarget::Fresh = chosen {
            // A name nothing answers to: fall through to the auxiliary-context path below.
        } else {
            let raw = if url.is_undefined() { String::new() } else { js_string(scope, url) };
            let handle: v8::Local<v8::Value> = match chosen {
                NavigationTarget::Context(id) => match window_of(scope, id) {
                    Some(w) => w.into(),
                    None => null,
                },
                _ => global_obj(scope).into(),
            };
            // No URL, or a `javascript:` URL: nothing is navigated and the handle is the answer.
            if raw.is_empty() || starts_with_ci(&raw, "javascript:") {
                return if noopener { null } else { handle };
            }
            let Some(abs) = resolve_for_realm(scope, initiator, &raw) else {
                return if noopener { null } else { handle };
            };
            navigate_target(scope, initiator, chosen, PageNavigation::get(abs, NavigationReason::Other));
            return if noopener { null } else { handle };
        }
    }
    if lower == "_blank" {
        name.clear();
    }
    // Names resolve in the group of the realm that CALLED `open`, which is where the lookup starts.
    let realm = running(scope);
    let existing = if name.is_empty() {
        None
    } else {
        bc(scope, |b| b.named.get(&realm).and_then(|m| m.get(&name).copied()).filter(|&c| !b.ctxs.get(c as usize).map(|x| x.closed).unwrap_or(true)))
    };
    // Step 13 (Frontier M), popup / target control: a `window.open` that opens a NEW auxiliary context does
    // it HERE, in the page process -- it never reaches the korvus-cdp `Target.createTarget` gate (router.rs),
    // which only a client's `createTarget` goes through. So the same gate is asked at this seam: when no
    // existing context is reused (a fresh popup / named-but-fresh target) and a target plugin is loaded
    // (`has_target_gate`), it must allow (1) or block (0), and a block returns `null` -- the answer Chrome's
    // popup blocker gives -- without creating the context. Reuse of an existing named context, and a `_self` /
    // `_top` / `_parent` navigation of the page itself, returned above, are the page's own navigation and are
    // never gated. Off with no such plugin: the gate is never asked and the open path is byte-for-byte what it
    // was (invariant I9/J2). CDP names a new window with `newWindow`; here a features string means a popup.
    if existing.is_none() && plugin_host::has_target_gate() {
        let raw = if url.is_undefined() { String::new() } else { js_string(scope, url) };
        let disposition = if feat.trim().is_empty() { "newTab" } else { "popup" };
        if !plugin_host::on_target_open(&raw, disposition) {
            return null;
        }
    }
    // D74 step 3, POPUPS AS TARGETS (`core/html/popup.rs`): under the browser endpoint a popup whose URL is cross-site
    // to its opener -- or one opened with `noopener` -- does not load here. The endpoint opens it as a page target of
    // its own in this browser context, as Chrome's site isolation puts it in a process of its own, and a client sees
    // it and drives it as any page (a front end's new window; Playwright's `popup`). The opener keeps its context, as
    // it keeps a cross-site frame's: the cross-origin WindowProxy, `closed`, `close()`, `postMessage` and `location`
    // cross to it. A same-site popup stays here, where its opener's script can reach into it as Chrome's can.
    if existing.is_none() && crate::core::html::popup::available() {
        let raw = if url.is_undefined() { String::new() } else { js_string(scope, url) };
        let initiator = current_realm(scope);
        let opener_url = match initiator {
            Some(c) if c != MAIN_CTX => loc_href(scope, c),
            _ => page_location_prop(scope, "href", ""),
        };
        let abs = if raw.is_empty() { None } else { resolve_for_realm(scope, initiator, &raw) };
        let remote = abs.filter(|a| {
            let web = url::Url::parse(a).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
            web && (noopener || crate::core::html::remote_frame::wants_remote(a, &opener_url))
        });
        if let Some(abs) = remote {
            if noopener {
                crate::core::html::popup::open(&abs, "", true, &opener_url);
                return null;
            }
            let id = make_popup_context(scope, &name);
            if !name.is_empty() {
                let n = name.clone();
                bc(scope, |b| {
                    if let Some(c) = b.ctxs.get_mut(id as usize) {
                        c.name = Some(n.clone());
                        c.name_realm = Some(realm);
                    }
                    b.named.entry(realm).or_default().insert(n, id);
                });
            }
            let Some(rid) = crate::core::html::popup::open(&abs, &name, false, &opener_url) else { return null };
            // An empty document at the popup's URL, as a frame that went remote shows: its origin is the popup's,
            // so every reach into it is a cross-origin one.
            set_location(scope, id, &abs);
            with_ctx(scope, id, |c| {
                c.remote = Some(rid);
                c.popup_opener = Some(initiator);
            });
            crate::core::html::remote_frame::arm_popup_recv(scope, id, rid);
            return match xo_window(scope, id) {
                Some(w) => w.into(),
                None => null,
            };
        }
    }
    let id = match existing {
        Some(c) => c,
        None => {
            let id = make_popup_context(scope, &name);
            if !name.is_empty() {
                let n = name.clone();
                bc(scope, |b| {
                    if let Some(c) = b.ctxs.get_mut(id as usize) {
                        c.name = Some(n.clone());
                        c.name_realm = Some(realm);
                    }
                    b.named.entry(realm).or_default().insert(n, id);
                });
            }
            id
        }
    };
    let Some(iwin) = window_of(scope, id) else { return null };
    // `noopener` severs the link in BOTH directions: the caller gets no handle and the new context
    // gets no `opener`.
    if noopener {
        set_prop(scope, iwin, "opener", null);
    }
    if !loader_ready(scope) {
        // No loader means no second context can actually be created. `null` is the spec's answer.
        return null;
    }
    let raw = if url.is_undefined() { String::new() } else { js_string(scope, url) };
    // No URL, or a `javascript:` URL: the context keeps the initial empty document it was created
    // with and the navigation is already complete, so `load` is all that is owed.
    if raw.is_empty() || starts_with_ci(&raw, "javascript:") {
        ctx_loaded(scope, id);
    } else {
        navigate_context(scope, id, &raw);
    }
    if noopener { null } else { iwin.into() }
}

// ── what a DevTools client is told about the frames ─────────────────────────────────────────────────
//
// Chrome reports every frame of a page -- `Page.frameAttached`, `frameNavigated` with its document, an execution
// context for its document's realm and one for each isolated world in it, its lifecycle -- and a client evaluates in
// a frame, finds a frame's element and the frame it holds (`DOM.describeNode`), and clicks into it. This engine had
// the frames (one V8 context per frame document, `iframe.contentDocument` and all) and told a client none of them:
// Playwright's `page.frames` listed the main frame alone and `frameLocator` waited for a frame that never came.

/// One frame (an `<iframe>`'s or `<frame>`'s browsing context) as a DevTools client is told of it.
#[derive(Debug, Clone, PartialEq)]
pub struct DevtoolsFrame {
    /// The context's id: what [`devtools_realm_index`] and the rest name it by.
    pub id: u32,
    /// The frame it is nested in: `None` the page itself.
    pub parent: Option<u32>,
    /// Its frame element.
    pub element: u32,
    /// Its current document's arena root -- a new one with every document.
    pub document: u32,
    pub url: String,
    pub name: String,
    /// Its document has reached `interactive` (`DOMContentLoaded`) / run its load event.
    pub dom_content_loaded: bool,
    pub loaded: bool,
    /// The isolated worlds the client's new-document scripts made over this document: name and index.
    pub worlds: Vec<(String, usize)>,
    /// Its document is the initial `about:blank` while a navigation is on its way: not yet one to announce.
    pub provisional: bool,
}

/// A popup a script of this process opened and shows here (a same-site `window.open`): a page of its own to a DevTools
/// client, served from this process.
#[derive(Debug, Clone, PartialEq)]
pub struct DevtoolsPopup {
    pub id: u32,
    pub document: u32,
    pub url: String,
    pub name: String,
    pub dom_content_loaded: bool,
    pub loaded: bool,
    pub worlds: Vec<(String, usize)>,
    /// See [`DevtoolsFrame::provisional`].
    pub provisional: bool,
    /// It was closed (`close()`, its own `window.close()`).
    pub closed: bool,
}

/// Every popup this process shows for its scripts (not one shown as a target of its own elsewhere).
pub fn devtools_popups(scope: &mut v8::PinScope) -> Vec<DevtoolsPopup> {
    let rows: Vec<(u32, Option<u32>, String, bool, bool, Vec<(String, usize, u32)>, bool, bool)> = bc(scope, |b| {
        b.ctxs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.popup && c.remote.is_none() && c.el.is_none())
            .map(|(i, c)| {
                let provisional = !c.navigated && c.nav_pending;
                (i as u32, c.doc_root, c.win_name.clone(), !matches!(c.readiness, Readiness::Loading), c.load_event == FrameLoadEvent::Completed, c.devtools_worlds.clone(), provisional, c.closed)
            })
            .collect()
    });
    let mut out = Vec::new();
    for (id, doc, name, dcl, loaded, worlds, provisional, closed) in rows {
        let Some(document) = doc else { continue };
        let url = loc_href(scope, id);
        let worlds = worlds.into_iter().filter(|(_, _, r)| *r == document).map(|(n, i, _)| (n, i)).collect();
        out.push(DevtoolsPopup { id, document, url, name, dom_content_loaded: dcl, loaded, worlds, provisional, closed });
    }
    out
}

/// A DevTools client closed popup `id` (`Target.closeTarget`): closed as its `close()` closes it.
pub fn close_popup(scope: &mut v8::PinScope, id: u32) {
    remote_popup_closed(scope, id);
}

/// A DevTools client navigated popup `id` (`Page.navigate` on its target).
pub fn navigate_popup(scope: &mut v8::PinScope, id: u32, url: &str) {
    navigate_context(scope, id, url);
}

/// Every frame of the page a DevTools client should know: nested contexts in this process whose element is in the
/// document its parent shows -- not a popup, not a frame shown in another process -- parents before children.
pub fn devtools_frames(scope: &mut v8::PinScope) -> Vec<DevtoolsFrame> {
    let rows: Vec<(u32, u32, Option<u32>, String, bool, bool, bool, Vec<(String, usize, u32)>)> = bc(scope, |b| {
        b.ctxs
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.popup && !c.closed && c.remote.is_none())
            .filter_map(|(i, c)| {
                let el = c.el?;
                let doc = c.doc_root?;
                let ready = !matches!(c.readiness, Readiness::Loading);
                let loaded = c.load_event == FrameLoadEvent::Completed;
                let provisional = !c.navigated && c.nav_pending;
                Some((i as u32, el, Some(doc), c.win_name.clone(), provisional, ready, loaded, c.devtools_worlds.clone()))
            })
            .collect()
    });
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut out: Vec<DevtoolsFrame> = Vec::new();
    for (id, el, doc, name, provisional, ready, loaded, worlds) in rows {
        let Some(doc) = doc else { continue };
        // the element's document, and the frame showing it (or the page)
        let (el_root, page_root, connected) = {
            let st = op_state.borrow();
            let Some(h) = st.try_borrow::<HostDom>() else { continue };
            let r = h.0.shadow_including_root(el);
            (r, h.0.root(), h.0.node_type(r) == 9)
        };
        if !connected {
            continue;
        }
        let parent = if el_root == page_root {
            None
        } else {
            match bc(scope, |b| b.by_doc.get(&el_root).copied().filter(|p| b.ctxs.get(*p as usize).is_some_and(|c| c.doc_root == Some(el_root)))) {
                Some(p) => Some(p),
                None => continue,
            }
        };
        let url = loc_href(scope, id);
        let worlds = worlds.into_iter().filter(|(_, _, root)| *root == doc).map(|(n, i, _)| (n, i)).collect();
        out.push(DevtoolsFrame { id, parent, element: el, document: doc, url, name, dom_content_loaded: ready, loaded, worlds, provisional });
    }
    // a frame whose parent is not listed (its document gone) is not either; parents first
    let mut ordered: Vec<DevtoolsFrame> = Vec::new();
    let mut pending = out;
    loop {
        let before = pending.len();
        let mut rest = Vec::new();
        for f in pending {
            if f.parent.is_none_or(|p| ordered.iter().any(|o| o.id == p)) {
                ordered.push(f);
            } else {
                rest.push(f);
            }
        }
        if rest.is_empty() || rest.len() == before {
            break;
        }
        pending = rest;
    }
    ordered
}

/// The realm frame context `id`'s current document runs in.
pub fn frame_realm<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Context>> {
    if id == MAIN_CTX {
        return None;
    }
    realm_of(scope, id)
}

/// Frame `id`'s `Location` object.
pub(crate) fn frame_location<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    loc_of(scope, id)
}

/// The frame whose realm `ctx` is.
pub(crate) fn frame_of_realm(scope: &mut v8::PinScope, ctx: v8::Local<v8::Context>) -> Option<u32> {
    let realms: Vec<(u32, v8::Global<v8::Context>)> = bc(scope, |b| {
        b.ctxs.iter().enumerate().filter(|(_, c)| !c.closed).filter_map(|(i, c)| c.realm.clone().map(|r| (i as u32, r))).collect()
    });
    realms.into_iter().find(|(_, r)| v8::Local::new(scope, r) == ctx).map(|(i, _)| i)
}

/// The frame whose document node `node` belongs to: `Some(None)` the page's own, `Some(Some(id))` frame `id`'s, `None`
/// a node in no document a browsing context shows.
pub fn frame_of_node(scope: &mut v8::PinScope, node: u32) -> Option<Option<u32>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = {
        let st = op_state.borrow();
        let h = st.try_borrow::<HostDom>()?;
        let r = h.0.shadow_including_root(node);
        if r == h.0.root() {
            return Some(None);
        }
        r
    };
    bc(scope, |b| b.by_doc.get(&root).copied().filter(|c| b.ctxs.get(*c as usize).is_some_and(|x| x.doc_root == Some(root)))).map(Some)
}

/// The frame frame element `el` holds in this process, if it holds one.
pub fn content_frame_of(scope: &mut v8::PinScope, el: u32) -> Option<u32> {
    bc(scope, |b| b.by_frame.get(&el).copied().filter(|c| b.ctxs.get(*c as usize).is_some_and(|x| x.el == Some(el) && x.remote.is_none())))
}

/// An isolated world over frame `id`'s current document (`Page.createIsolatedWorld` with the frame's id): its index.
pub fn create_frame_world(scope: &mut v8::PinScope, id: u32, name: &str) -> Option<usize> {
    let index = crate::core::html::isolated_world::create_isolated_world_in(scope, Some(id))?;
    let root = frame_document_root(scope, id)?;
    with_ctx(scope, id, |c| c.devtools_worlds.push((name.to_string(), index, root)));
    Some(index)
}

/// A frame document starts (before its own scripts): the DevTools client's bindings on its window, its page-world
/// new-document scripts in its realm, and the worlds its other scripts name made over it, each running its scripts.
fn devtools_frame_document_start(scope: &mut v8::PinScope, id: u32) {
    let scripts = crate::devtools_log::new_document_scripts();
    let bindings = crate::devtools_log::binding_names();
    if scripts.is_empty() && bindings.is_empty() {
        return;
    }
    if let Some(realm) = realm_of(scope, id) {
        let scope = &mut v8::ContextScope::new(scope, realm);
        crate::devtools_log::install_bindings(scope, &bindings);
    }
    for (world, src) in scripts.iter().filter(|(w, _)| w.is_empty()) {
        let _ = world;
        run_script(scope, id, src, "", None);
    }
    let mut named: Vec<(String, Vec<String>)> = Vec::new();
    for (world, src) in scripts.into_iter().filter(|(w, _)| !w.is_empty()) {
        match named.iter_mut().find(|(n, _)| *n == world) {
            Some((_, list)) => list.push(src),
            None => named.push((world, vec![src])),
        }
    }
    for (name, list) in named {
        let Some(index) = create_frame_world(scope, id, &name) else { continue };
        let Some(ctx) = crate::core::html::isolated_world::isolated_world_context(scope, index) else { continue };
        let scope = &mut v8::ContextScope::new(scope, ctx);
        for src in list {
            v8::tc_scope!(let tc, scope);
            if let Some(code) = v8::String::new(tc, &src) {
                if let Some(script) = v8::Script::compile(tc, code, None) {
                    let _ = script.run(tc);
                }
            }
        }
    }
}

// ── the ops ──────────────────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_frame_src_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(el) = unwrap_id(scope, args.this().into()) else { return };
    frame_src_set(scope, el, args.get(0));
}
pub(crate) fn cb_frame_srcdoc_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(el) = unwrap_id(scope, args.this().into()) else { return };
    frame_srcdoc_set(scope, el, args.get(0));
}
pub(crate) fn cb_frame_content_window_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = unwrap_id(scope, args.this().into()) else { rv.set_null(); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // A frame element has a content navigable only while it is CONNECTED (HTML "when an iframe element
    // becomes connected, create a new child navigable"; removal destroys it): stock Chrome 150 answers
    // `null` for `document.createElement("iframe").contentWindow`, where this engine minted a context
    // for any frame element it was asked about (the Chrome oracle, 2026-10-08).
    if !is_frameish(&op_state, el) || !crate::core::html::subresource::connected_anywhere(scope, el) {
        rv.set_null();
        return;
    }
    let id = frame_ctx_for(scope, el);
    // A frame whose document is cross-origin to the caller hands out its cross-origin WindowProxy,
    // never the window itself.
    if cross_origin_to_caller(scope, id) {
        match xo_window(scope, id) {
            Some(w) => rv.set(w.into()),
            None => rv.set_null(),
        }
        return;
    }
    match window_of(scope, id) {
        Some(w) => rv.set(w.into()),
        None => rv.set_null(),
    }
}
// ── the CROSS-ORIGIN WindowProxy and Location ──────────────────────────────────────
//
// A page could read EVERYTHING through a cross-origin frame's `contentWindow`: `w.document`,
// `w.JSON`, `w.location.href`, all 1,200 names. `contentDocument` was already `null` for such a frame
// (P10b), and the window beside it was the open door. Measured on stock Chrome 150.0.7871.46, Brave
// 151.1.93.138, Edge 151.0.4129.93 and Opera 135.0.5973.133 over two loopback origins, identical on all
// four (`tests/fixtures/p11a/<browser>.xo.txt`):
//
// ```text
// readable            window self location closed frames length top opener parent blur close focus
//                     postMessage, and `then` / @@toStringTag / @@hasInstance / @@isConcatSpreadable
//                     as undefined
// anything else       SecurityError "Failed to read a named property 'X' from 'Window': Blocked a frame
//                     with origin "<caller>" from accessing a cross-origin frame."   ([N]: "an indexed
//                     property [N]")
// set                 only `location` (navigates); else "Failed to set a named property 'X' on 'Window': …"
// define / delete / in (other) / setPrototypeOf / preventExtensions
//                     SecurityError "Blocked a frame with origin "<caller>" from accessing a cross-origin frame."
// getOwnPropertyNames window self location closed frames length top opener parent blur close focus postMessage then
// descriptors         methods data w=false e=false c=true; `location` get+set; the rest getter-only; all e=false
// getPrototypeOf      null;  isExtensible true;  Object.prototype.toString "[object Object]"
// location            only `href` (set) and `replace()`; `then` undefined; keys `href replace then`
// ```
//
// THE MECHANISM IS THE PLATFORM'S OWN SHAPE. HTML defines a WindowProxy and a Location as EXOTIC
// objects whose every internal method runs a cross-origin check; a `v8::Proxy` whose handler traps are
// native Rust functions is exactly an object with those internal methods, and it is what this engine can
// mint in one V8 context (Chrome uses V8 access checks, which `rusty_v8` does not expose). The allow-list
// and every sentence above are the traps' whole body. The same proxy object answers for a context every
// time, so `f.contentWindow === f.contentWindow` and `w.window === w`.
//
// SCOPE, stated: this guards the PARENT reading a cross-origin CHILD through `contentWindow`. The
// single-context model's child-to-parent direction (`parent.document` from inside a cross-origin frame's
// own scripts) is not guarded here.

const XO_WINDOW_KEYS: [&str; 13] =
    ["window", "self", "location", "closed", "frames", "length", "top", "opener", "parent", "blur", "close", "focus", "postMessage"];
const XO_METHODS: [(&str, i32); 4] = [("blur", 0), ("close", 0), ("focus", 0), ("postMessage", 1)];

#[derive(Default)]
struct CrossOriginProxies {
    by_ctx: HashMap<u32, XoEntry>,
}

struct XoEntry {
    window: v8::Global<v8::Object>,
    location: v8::Global<v8::Object>,
    /// The functions a page can take out of the proxy, minted once PER CALLER REALM (`<realm>:<name>`)
    /// so their identity holds: `blur close focus postMessage replace`, and the descriptor getters/setter.
    fns: HashMap<String, v8::Global<v8::Function>>,
}

fn xo_caller_origin(scope: &mut v8::PinScope) -> String {
    let caller = running(scope);
    document_origin(scope, caller)
}

fn xo_blocked(scope: &mut v8::PinScope) -> String {
    let o = xo_caller_origin(scope);
    let o = serialize_origin(&o);
    format!("Blocked a frame with origin \"{o}\" from accessing a cross-origin frame.")
}

fn xo_key_label(scope: &mut v8::PinScope, key: v8::Local<v8::Value>) -> String {
    if let Ok(sym) = v8::Local::<v8::Symbol>::try_from(key) {
        let d = sym.description(scope).to_rust_string_lossy(scope);
        return format!("[{d}]");
    }
    key.to_rust_string_lossy(scope)
}

fn xo_throw(scope: &mut v8::PinScope, msg: String) {
    crate::binding::protos::throw_dom(scope, &msg, "SecurityError");
}

/// `then`, `@@toStringTag`, `@@hasInstance`, `@@isConcatSpreadable`: HTML's CrossOriginPropertyFallback
/// -- readable, and `undefined`.
fn xo_fallback_key(scope: &mut v8::PinScope, key: v8::Local<v8::Value>) -> bool {
    if key.is_string() {
        return key.to_rust_string_lossy(scope) == "then";
    }
    let Ok(sym) = v8::Local::<v8::Symbol>::try_from(key) else { return false };
    let s: v8::Local<v8::Value> = sym.into();
    let tag: v8::Local<v8::Value> = v8::Symbol::get_to_string_tag(scope).into();
    let inst: v8::Local<v8::Value> = v8::Symbol::get_has_instance(scope).into();
    let spread: v8::Local<v8::Value> = v8::Symbol::get_is_concat_spreadable(scope).into();
    s == tag || s == inst || s == spread
}

fn xo_string_key(scope: &mut v8::PinScope, key: v8::Local<v8::Value>) -> Option<String> {
    key.is_string().then(|| key.to_rust_string_lossy(scope))
}

/// Mint (once) or fetch a cached function of context `id`'s cross-origin surface, IN THE CALLER'S
/// REALM: HTML's CrossOriginGetOwnPropertyHelper creates a cross-origin method or accessor function in
/// the current realm and keeps it in the cross-origin property descriptor map, keyed by (current
/// settings object, relevant settings object, key). Chrome 150, measured on a page with a frame on
/// another origin: `frames[0].postMessage instanceof Function`, and the same for `close`, `focus` and
/// the `location` setter, are `true` (the page's own `Function.prototype`), and
/// `frames[0].postMessage === frames[0].postMessage`. korvus minted them in whatever context was
/// current the first time any realm asked, so the page saw another realm's functions (korvus-analysis
/// U-7: an interactive challenge widget's `postMessage` into its challenge frame).
fn xo_fn<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, name: &str) -> Option<v8::Local<'s, v8::Function>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let caller = running(scope);
    let key = match caller {
        Some(c) => format!("{c}:{name}"),
        None => format!("page:{name}"),
    };
    let cached = op_state
        .borrow()
        .try_borrow::<CrossOriginProxies>()
        .and_then(|p| p.by_ctx.get(&id))
        .and_then(|e| e.fns.get(&key).cloned());
    if let Some(g) = cached {
        return Some(v8::Local::new(scope, &g));
    }
    // the caller's realm, re-anchored to this HandleScope (the entered-context handle borrows `scope`)
    let realm = {
        let c = scope.get_entered_or_microtask_context();
        v8::Global::new(scope, c)
    };
    let realm = v8::Local::new(scope, &realm);
    let g = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let idd = u32_data(scope, id);
        let f = match name {
            "postMessage" => build_fn_data(scope, cb_iwin_post_message, "postMessage", 1, idd),
            "close" => build_fn_data(scope, cb_xo_close, name, 0, idd),
            "blur" | "focus" => build_fn_data(scope, cb_noop, name, 0, idd),
            "replace" => build_fn_data(scope, cb_xo_location_replace, "replace", 1, idd),
            "set location" | "set href" => build_fn_data(scope, cb_xo_set_location, name, 1, idd),
            other => {
                let member = other.strip_prefix("get ").unwrap_or(other);
                let d = pack_str(scope, id, member);
                build_fn_data(scope, cb_xo_member_get, other, 0, d)
            }
        };
        v8::Global::new(scope, f)
    };
    let f = v8::Local::new(scope, &g);
    if let Some(e) = op_state.borrow_mut().try_borrow_mut::<CrossOriginProxies>().and_then(|p| p.by_ctx.get_mut(&id)) {
        e.fns.insert(key, g);
    }
    Some(f)
}

/// The value an allowed member of context `id`'s cross-origin WindowProxy reads as.
fn xo_member_value<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, name: &str) -> v8::Local<'s, v8::Value> {
    match name {
        "window" | "self" | "frames" => xo_window(scope, id).map(|w| w.into()).unwrap_or_else(|| v8::null(scope).into()),
        "location" => xo_location(scope, id).map(|l| l.into()).unwrap_or_else(|| v8::null(scope).into()),
        "top" | "parent" => {
            // Computed, not read off the window: the page's own `top` is its raw global, which a
            // cross-origin caller must never be handed.
            let target = if name == "top" { frame_top_ctx(scope, id) } else { frame_parent_ctx(scope, id).unwrap_or(id) };
            window_for_caller(scope, target).map(|w| w.into()).unwrap_or_else(|| v8::null(scope).into())
        }
        "opener" => v8::null(scope).into(),
        "closed" => {
            let closed = id != MAIN_CTX && bc(scope, |b| b.ctxs.get(id as usize).map(|c| c.closed).unwrap_or(true));
            v8::Boolean::new(scope, closed).into()
        }
        "length" => window_of(scope, id).and_then(|w| get_prop(scope, w, "length")).unwrap_or_else(|| v8::Integer::new(scope, 0).into()),
        method => xo_fn(scope, id, method).map(|f| f.into()).unwrap_or_else(|| v8::undefined(scope).into()),
    }
}

fn cb_xo_member_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, name)) = unpack_str(scope, __body_data) else { return };
    let v = xo_member_value(scope, id, &name);
    rv.set(v);
}

fn cb_xo_set_location(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    let v = js_string(scope, args.get(0));
    if id == MAIN_CTX && !may_navigate_top(scope) {
        return refuse_top_navigation(scope, "Failed to set a named property 'location' on 'Window'", &v);
    }
    navigate_context(scope, id, &v);
}

fn cb_xo_location_replace(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'replace' on 'Location': 1 argument required, but only 0 present.");
        return;
    }
    let v = js_string(scope, args.get(0));
    if id == MAIN_CTX && !may_navigate_top(scope) {
        return refuse_top_navigation(scope, "Failed to execute 'replace' on 'Location'", &v);
    }
    navigate_context(scope, id, &v);
}

// The WindowProxy's traps. `args.get(0)` is the target, `args.get(1)` the key.

fn cb_xo_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    let key = args.get(1);
    if let Some(name) = xo_string_key(scope, key) {
        if XO_WINDOW_KEYS.contains(&name.as_str()) {
            let v = xo_member_value(scope, id, &name);
            rv.set(v);
            return;
        }
        if let Ok(n) = name.parse::<u32>() {
            if n.to_string() == name {
                let msg = xo_blocked(scope);
                xo_throw(scope, format!("Failed to read an indexed property [{n}] from 'Window': {msg}"));
                return;
            }
        }
    }
    if xo_fallback_key(scope, key) {
        rv.set_undefined();
        return;
    }
    let label = xo_key_label(scope, key);
    let msg = xo_blocked(scope);
    xo_throw(scope, format!("Failed to read a named property '{label}' from 'Window': {msg}"));
}

fn cb_xo_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    let key = args.get(1);
    if xo_string_key(scope, key).as_deref() == Some("location") {
        let v = js_string(scope, args.get(2));
        navigate_context(scope, id, &v);
        rv.set_bool(true);
        return;
    }
    let label = xo_key_label(scope, key);
    let msg = xo_blocked(scope);
    xo_throw(scope, format!("Failed to set a named property '{label}' on 'Window': {msg}"));
}

fn cb_xo_has(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = args.get(1);
    let allowed = xo_string_key(scope, key).map(|n| XO_WINDOW_KEYS.contains(&n.as_str())).unwrap_or(false) || xo_fallback_key(scope, key);
    if allowed {
        rv.set_bool(true);
        return;
    }
    let msg = xo_blocked(scope);
    xo_throw(scope, msg);
}

fn cb_xo_blocked_trap(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let msg = xo_blocked(scope);
    xo_throw(scope, msg);
}

fn cb_xo_get_prototype_of(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_null();
}

fn cb_xo_is_extensible(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(true);
}

fn xo_accessor_descriptor<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, name: &str, with_setter: bool) -> Option<v8::Local<'s, v8::Object>> {
    let d = v8::Object::new(scope);
    let get = xo_fn(scope, id, &format!("get {name}"))?;
    set_prop(scope, d, "get", get.into());
    let set: v8::Local<v8::Value> = if with_setter { xo_fn(scope, id, &format!("set {name}"))?.into() } else { v8::undefined(scope).into() };
    set_prop(scope, d, "set", set);
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    set_prop(scope, d, "enumerable", f);
    set_prop(scope, d, "configurable", t);
    Some(d)
}

fn xo_value_descriptor<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Object> {
    let d = v8::Object::new(scope);
    set_prop(scope, d, "value", value);
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    set_prop(scope, d, "writable", f);
    set_prop(scope, d, "enumerable", f);
    set_prop(scope, d, "configurable", t);
    d
}

fn cb_xo_get_own_property_descriptor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    let key = args.get(1);
    if let Some(name) = xo_string_key(scope, key) {
        if XO_METHODS.iter().any(|(m, _)| *m == name) {
            let Some(f) = xo_fn(scope, id, &name) else { return };
            let d = xo_value_descriptor(scope, f.into());
            rv.set(d.into());
            return;
        }
        if XO_WINDOW_KEYS.contains(&name.as_str()) {
            if let Some(d) = xo_accessor_descriptor(scope, id, &name, name == "location") {
                rv.set(d.into());
            }
            return;
        }
    }
    if xo_fallback_key(scope, key) {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        let d = xo_value_descriptor(scope, u);
        rv.set(d.into());
        return;
    }
    let label = xo_key_label(scope, key);
    let msg = xo_blocked(scope);
    xo_throw(scope, format!("Failed to read a named property '{label}' from 'Window': {msg}"));
}

fn cb_xo_own_keys(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut keys: Vec<v8::Local<v8::Value>> = XO_WINDOW_KEYS.iter().map(|k| str_val(scope, k)).collect();
    keys.push(str_val(scope, "then"));
    keys.push(v8::Symbol::get_to_string_tag(scope).into());
    keys.push(v8::Symbol::get_has_instance(scope).into());
    keys.push(v8::Symbol::get_is_concat_spreadable(scope).into());
    let arr = v8::Array::new_with_elements(scope, &keys);
    rv.set(arr.into());
}

// The Location's traps: only `href` (set) and `replace` cross.

fn cb_xo_loc_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    let key = args.get(1);
    if xo_string_key(scope, key).as_deref() == Some("replace") {
        if let Some(f) = xo_fn(scope, id, "replace") {
            rv.set(f.into());
        }
        return;
    }
    if xo_fallback_key(scope, key) {
        rv.set_undefined();
        return;
    }
    let label = xo_key_label(scope, key);
    let msg = xo_blocked(scope);
    xo_throw(scope, format!("Failed to read a named property '{label}' from 'Location': {msg}"));
}

fn cb_xo_loc_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    let key = args.get(1);
    if xo_string_key(scope, key).as_deref() == Some("href") {
        let v = js_string(scope, args.get(2));
        if id == MAIN_CTX && !may_navigate_top(scope) {
            return refuse_top_navigation(scope, "Failed to set a named property 'href' on 'Location'", &v);
        }
        navigate_context(scope, id, &v);
        rv.set_bool(true);
        return;
    }
    let label = xo_key_label(scope, key);
    let msg = xo_blocked(scope);
    xo_throw(scope, format!("Failed to set a named property '{label}' on 'Location': {msg}"));
}

fn cb_xo_loc_has(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = args.get(1);
    let allowed = matches!(xo_string_key(scope, key).as_deref(), Some("href") | Some("replace")) || xo_fallback_key(scope, key);
    if allowed {
        rv.set_bool(true);
        return;
    }
    let msg = xo_blocked(scope);
    xo_throw(scope, msg);
}

fn cb_xo_loc_get_own_property_descriptor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(id) = data_u32(scope, __body_data) else { return };
    let key = args.get(1);
    match xo_string_key(scope, key).as_deref() {
        Some("replace") => {
            let Some(f) = xo_fn(scope, id, "replace") else { return };
            let d = xo_value_descriptor(scope, f.into());
            rv.set(d.into());
            return;
        }
        Some("href") => {
            let d = v8::Object::new(scope);
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            set_prop(scope, d, "get", u);
            let Some(s) = xo_fn(scope, id, "set href") else { return };
            set_prop(scope, d, "set", s.into());
            let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            set_prop(scope, d, "enumerable", f);
            set_prop(scope, d, "configurable", t);
            rv.set(d.into());
            return;
        }
        _ => {}
    }
    if xo_fallback_key(scope, key) {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        let d = xo_value_descriptor(scope, u);
        rv.set(d.into());
        return;
    }
    let label = xo_key_label(scope, key);
    let msg = xo_blocked(scope);
    xo_throw(scope, format!("Failed to read a named property '{label}' from 'Location': {msg}"));
}

fn cb_xo_loc_own_keys(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let keys: Vec<v8::Local<v8::Value>> = vec![
        str_val(scope, "href"),
        str_val(scope, "replace"),
        str_val(scope, "then"),
        v8::Symbol::get_to_string_tag(scope).into(),
        v8::Symbol::get_has_instance(scope).into(),
        v8::Symbol::get_is_concat_spreadable(scope).into(),
    ];
    let arr = v8::Array::new_with_elements(scope, &keys);
    rv.set(arr.into());
}

fn xo_handler<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let h = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    h.set_prototype(scope, null);
    h
}

/// One trap of a handler: a native function carrying context `id`.
fn xo_trap(scope: &mut v8::PinScope, h: v8::Local<v8::Object>, id: u32, name: &str, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let idd = u32_data(scope, id);
    let f = build_fn_data(scope, cb, name, 0, idd);
    set_prop(scope, h, name, f.into());
}

fn xo_proxy<'s>(scope: &mut v8::PinScope<'s, '_>, handler: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let target = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    target.set_prototype(scope, null);
    v8::Proxy::new(scope, target, handler).map(|p| p.into())
}

fn xo_ensure(scope: &mut v8::PinScope, id: u32) -> Option<()> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<CrossOriginProxies>().is_none() {
            st.put(CrossOriginProxies::default());
        }
        if st.borrow::<CrossOriginProxies>().by_ctx.contains_key(&id) {
            return Some(());
        }
    }
    let wh = xo_handler(scope);
    xo_trap(scope, wh, id, "get", cb_xo_get);
    xo_trap(scope, wh, id, "set", cb_xo_set);
    xo_trap(scope, wh, id, "has", cb_xo_has);
    xo_trap(scope, wh, id, "deleteProperty", cb_xo_blocked_trap);
    xo_trap(scope, wh, id, "defineProperty", cb_xo_blocked_trap);
    xo_trap(scope, wh, id, "setPrototypeOf", cb_xo_blocked_trap);
    xo_trap(scope, wh, id, "preventExtensions", cb_xo_blocked_trap);
    xo_trap(scope, wh, id, "getPrototypeOf", cb_xo_get_prototype_of);
    xo_trap(scope, wh, id, "isExtensible", cb_xo_is_extensible);
    xo_trap(scope, wh, id, "getOwnPropertyDescriptor", cb_xo_get_own_property_descriptor);
    xo_trap(scope, wh, id, "ownKeys", cb_xo_own_keys);
    let lh = xo_handler(scope);
    xo_trap(scope, lh, id, "get", cb_xo_loc_get);
    xo_trap(scope, lh, id, "set", cb_xo_loc_set);
    xo_trap(scope, lh, id, "has", cb_xo_loc_has);
    xo_trap(scope, lh, id, "deleteProperty", cb_xo_blocked_trap);
    xo_trap(scope, lh, id, "defineProperty", cb_xo_blocked_trap);
    xo_trap(scope, lh, id, "setPrototypeOf", cb_xo_blocked_trap);
    xo_trap(scope, lh, id, "preventExtensions", cb_xo_blocked_trap);
    xo_trap(scope, lh, id, "getPrototypeOf", cb_xo_get_prototype_of);
    xo_trap(scope, lh, id, "isExtensible", cb_xo_is_extensible);
    xo_trap(scope, lh, id, "getOwnPropertyDescriptor", cb_xo_loc_get_own_property_descriptor);
    xo_trap(scope, lh, id, "ownKeys", cb_xo_loc_own_keys);
    let w = xo_proxy(scope, wh)?;
    let l = xo_proxy(scope, lh)?;
    // korvus-analysis: the recorder digests these as the platform objects they stand for, and never
    // records their handlers' traps (engine machinery)
    #[cfg(feature = "analysis")]
    {
        crate::analysis_hook::note_engine_object(w, "Window");
        crate::analysis_hook::note_engine_object(l, "Location");
        crate::analysis_hook::note_engine_object(wh, "");
        crate::analysis_hook::note_engine_object(lh, "");
    }
    let entry = XoEntry { window: v8::Global::new(scope, w), location: v8::Global::new(scope, l), fns: HashMap::new() };
    op_state.borrow_mut().borrow_mut::<CrossOriginProxies>().by_ctx.insert(id, entry);
    Some(())
}

fn xo_window<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    xo_ensure(scope, id)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let g = op_state.borrow().borrow::<CrossOriginProxies>().by_ctx.get(&id).map(|e| e.window.clone())?;
    Some(v8::Local::new(scope, &g))
}

fn xo_location<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    xo_ensure(scope, id)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let g = op_state.borrow().borrow::<CrossOriginProxies>().by_ctx.get(&id).map(|e| e.location.clone())?;
    Some(v8::Local::new(scope, &g))
}

/// Is context `id`'s document cross-origin to the realm asking -- the test `contentDocument` makes.
fn cross_origin_to_caller(scope: &mut v8::PinScope, id: u32) -> bool {
    let caller = running(scope);
    document_origin(scope, Some(id)) != document_origin(scope, caller)
}

pub(crate) fn cb_frame_content_document_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(el) = unwrap_id(scope, args.this().into()) else { rv.set_null(); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // No content navigable while the element is not connected (see `cb_frame_content_window_get`).
    if !is_frameish(&op_state, el) || !crate::core::html::subresource::connected_anywhere(scope, el) {
        rv.set_null();
        return;
    }
    let id = frame_ctx_for(scope, el);
    // "The contentDocument getter steps are to return this's content document", which is null
    // unless that document's origin is same origin-domain with the CURRENT settings object's --
    // Chrome 150, measured: a frame on another port of the same host answers `null`.
    let caller = running(scope);
    if document_origin(scope, Some(id)) != document_origin(scope, caller) {
        rv.set_null();
        return;
    }
    match doc_of(scope, id) {
        Some(d) => rv.set(d.into()),
        None => rv.set_null(),
    }
}
pub(crate) fn cb_window_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let r = open_window(scope, args.get(0), args.get(1), args.get(2));
    rv.set(r);
}

// THIS FILE DECLARES NO NATIVE OPS. Eleven of them were deleted from it over four units, and the
// way that happened is worth keeping because it was the same shape every time: each op was a seam
// onto one JS file; when that file became Rust, its Rust caller started calling the plain function
// and the op was left with nothing.
//
//   DELETED, and every one of these names is gone from the tree:
//   W5.1 L13  `op_navigate_frame`                              <- dom/08w-form-element.js
//   W5.0e     `op_delay_load` / `op_undelay_load` / `op_frame_navigated`
//                                                              <- dom/09j-subresource.js
//   W11.5     the last seven, listed below, each with a live Rust caller for the function it wrapped:
//
//     op_frame_load_srcdoc      -> frame_load_srcdoc     (webapi::page, from runtime navigation.rs)
//     op_when_load_delays_done  -> when_load_delays_done (webapi::page, from runtime navigation.rs)
//     op_doc_readiness          -> set_doc_readiness     (webapi::page::set_document_readiness)
//     op_sync_frames            -> sync_frames           (webapi::page, and mutation_observer.rs)
//     op_window_open_method     -> cb_window_open        (the Window table row, core/html/window.rs)
//     op_current_realm          -> current_realm_window  (core/html/window.rs, for event.source)
//     op_realm_origin           -> realm_origin          (core/html/window.rs, for event.origin)
//
// `op_window_open_method`'s comment claimed `dom/09f-window.js` mounted it and that the Window table
// would take it "in W5.1". The table did take it -- `window.rs` builds the member from
// `cb_window_open` -- and the JS file is gone; only the op outlived both, which is the same failure
// mode `op_cross_origin_isolated_get` showed W11.1. `op_sync_frames`'s claimed `host.js` as the
// source of its `added` argument; that is `mutation_observer.rs`'s childList batch now.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noopener_feature_is_a_token_of_the_list() {
        assert!(has_noopener("noopener"));
        assert!(has_noopener("width=1,NoOpener,height=2"));
        assert!(has_noopener("noreferrer; noopener=1"));
        assert!(!has_noopener("noopenerx"));
        assert!(!has_noopener("xnoopener"));
        assert!(!has_noopener(""));
    }

    #[test]
    fn case_insensitive_prefixes() {
        assert!(starts_with_ci("ABOUT:blank", "about:"));
        assert!(!starts_with_ci("abou", "about:"));
    }
}
