//! Running a page's `<script>`s — **in Rust**.
//!
//! This was two raw JS strings in `lib.rs`, `RUN_PAGE_SCRIPTS` (inline only, ~35 lines) and
//! `RUN_PAGE_SCRIPTS_EXT` (inline + external + modules, ~140 lines). They are one driver now, and the
//! duplication they carried is gone with them: the second's header said *"Mirrors `script_elements()`
//! in lib.rs element-for-element; the two must agree, because the external bodies staged in `KB.__kx`
//! are consumed positionally by this scan"*, and the same warning was repeated three more times for
//! the type list, the `src` attribute order and the raw-vs-lowercased attribute read. Every one of
//! those agreements is now a single Rust function called from both sides.
//!
//! # The semantics this driver has, and why each one is load-bearing
//!
//! **A classic script, not an `eval`.** An indirect `eval` of a body beginning `"use strict"` is a
//! STRICT EVAL, and a strict eval gets its own variable environment — its top-level `function` and
//! `var` declarations never reach the global object. A strict SCRIPT has no such rule. Running page
//! scripts through `eval` therefore meant every strict script defined NOTHING, and the failure
//! surfaced far from its cause: the helper fetches, evaluates without throwing, and the next script
//! says `doTests is not defined`. 383 files under `dom/` and `html/` in the WPT checkout start
//! `"use strict"`, and so does most modern page code. [`run_classic`] compiles a `v8::Script` in the
//! current realm with the script's own URL as its origin — which is what `Deno.core.evalContext` did
//! for the JS driver, one indirection further away.
//!
//! **A module is NOT a classic script.** Running one through the classic path raises "Cannot use
//! import statement outside a module" and used to take the whole file down — 117 core files died that
//! way, most of them not testing modules at all. It goes to the deferred module queue instead, which
//! is browser semantics anyway: module scripts evaluate after the document parses.
//!
//! **A driver bug costs ONE script, never the document.** Every per-script step is inside its own
//! guard. That is not decoration: a throw from the driver itself (as opposed to from the page script
//! it is running) used to unwind the whole IIFE, and the IIFE was what ran the rest of the page.
//! Reading `src` one line above its own `const src` was a temporal-dead-zone `ReferenceError` that
//! aborted the loop at the first `<script type=module>` in document order. Nothing saw it — the throw
//! is the driver's, so `report_exception`/`window.onerror` never fire — and the file simply registered
//! no tests. 184 files in a reference engine corpus reached NO VERDICT that way, of which 170 left not one
//! byte of evidence behind. The Rust spelling of that guarantee is that this file's own failures are
//! `Option`s and `continue`s rather than panics, and that a page script's throw is reported and
//! stepped over.
//!
//! **The scan and the fetch pass must agree element-for-element.** The external bodies are staged in
//! DOCUMENT ORDER by [`crate::Engine::load_page`]'s fetch pass and consumed POSITIONALLY here, so a
//! scan that accepts one more or one fewer script than the fetch pass hands every later script the
//! wrong source. Both sides now call [`crate::script_type_runs`] and [`script_src_of`].

use deno_core::v8;
use webapi::binding::protos::{get_prop, str_val};

use crate::navigation::{global, quietly};

/// What the page's scripts were staged with: the external bodies, in document order, and whether the
/// document is XML.
///
/// Both were bridge slots (`KB.__kx`, `KB.__xmlDoc`) written by a `format!`ed eval and read by the JS
/// driver — page-visible state the engine depended on. They are arguments now.
pub(crate) struct Staged<'a> {
    /// The fetched responses of the `<script src>` elements, in the order [`crate::script_elements`]
    /// found them. Consumed positionally. A fetch that failed outright stages `None` to keep alignment.
    /// A caller that fetched nothing passes an empty slice, and a `<script src>` then does nothing at
    /// all -- no body, no event -- which is `Engine::load_html_injected`'s documented behaviour.
    pub external: &'a [Slot],
    /// An XML document's `<script>` may be namespace-PREFIXED (`<h:script src="…">` is how every SVG
    /// test in WPT loads `testharness.js`), and `getElementsByTagName` matches the QUALIFIED name, so
    /// it finds none of them. Set by the caller from the response's parser choice, never sniffed here,
    /// so this scan and the fetch pass cannot disagree about which elements are scripts.
    pub is_xml: bool,
    /// The document's URL. Inline scripts report it in stacks, like a real browser.
    pub page_url: &'a str,
    /// When each staged `<script src>` runs, aligned with [`Staged::external`]. Decided from
    /// the element's attributes AS PARSED -- HTML's "prepare the script element" reads `async`/`defer`
    /// then, so a later `removeAttribute("async")` does not move it -- and, for `async`, from when its
    /// body arrived. Shorter than `external` (an empty slice) means every script runs in place.
    pub postpone: &'a [Postpone],
    /// The parser's script-blocking stylesheets, fetched beside the scripts, in tree
    /// order: each is applied before the first parser script that follows its link.
    pub sheets: &'a [PrefetchedSheet],
    /// Each staged `<script src>`'s resource-timing entry, aligned with `external` (empty: none), and when the
    /// fetch pass asked for them.
    pub entries: &'a [ScriptEntry],
    pub asked_ms: f64,
}

/// One parser `<script src>`'s response as the STREAMING parse sees it: the pass runs a script as soon as
/// it and everything before it is in -- Chrome runs an inline script while a later `<script src>` is still on the wire
/// (measured: korvus ran it 1.5 s late, behind a slow later script) -- and waits, with the event loop running, only
/// where Chrome's parser waits.
pub(crate) enum Slot {
    /// not in yet
    Pending,
    /// the request failed outright
    Failed,
    Got(Fetched),
}

/// The marker the driver puts while the parser waits for a script or a sheet: the event loop runs meanwhile, the
/// parser's own modules wait for the parse's end (`Engine::evaluate_pending_modules`).
pub(crate) struct ParserWaiting;

/// One parser `<script src>`'s resource-timing entry, put on the timeline when Chrome's fetch of it would be
/// complete: a script that runs in place, as the parser reaches it -- before it runs, and
/// before a refused one's `error`; a deferred, late `async` or module script, when the parse is over. Chrome 150, a
/// `PerformanceObserver` an inline script registers ahead of a `<script type=module src>` sees that module's entry;
/// korvus, which fetches every parser script before the first runs, put them all on the timeline before any
/// script could observe them.
pub(crate) struct ScriptEntry {
    pub url: String,
    pub timing: Option<spec::FetchTiming>,
    /// Its `renderBlockingStatus` is "blocking".
    pub blocking: bool,
    /// It was fetched in CORS mode (`crossorigin`, a module): its status and type are the document's to read.
    pub cors: bool,
    /// Its document's CSP refused it: never requested, its entry in the refusal shape.
    pub refused: bool,
    /// The `<script>` element.
    pub node: u32,
}

/// Put staged script `i`'s resource-timing entry on the timeline, once.
fn enter_entry(scope: &mut v8::PinScope, staged: &Staged, entered: &mut [bool], i: usize) {
    // a response not in yet has no entry yet ([`finish_late`] puts it on the timeline when it is)
    if matches!(staged.external.get(i), Some(Slot::Pending)) {
        return;
    }
    let (Some(e), Some(done)) = (staged.entries.get(i), entered.get_mut(i)) else { return };
    if std::mem::replace(done, true) {
        return;
    }
    if e.refused {
        webapi::page::refused_parser_script(scope, e.node, &e.url, e.blocking);
        return;
    }
    webapi::page::parser_script_loaded(scope, &e.url, e.timing.clone(), staged.asked_ms, e.blocking, e.cors);
}

/// One parser `<link rel=stylesheet>` the fetch pass loaded: the element, its URL, the
/// response (`None`: the request failed) and when the pass asked for it.
pub(crate) struct PrefetchedSheet {
    pub id: u32,
    pub url: String,
    pub response: Option<spec::SubresourceResponse>,
    pub asked_ms: f64,
    /// not in yet (the streaming parse)
    pub pending: bool,
    /// fetched in CORS mode (`crossorigin`): its response is the document's to read
    pub cors: bool,
}

/// Apply the prefetched sheets whose link comes before `el` in tree order (all of them for `None`),
/// each once, in tree order -- HTML's "a style sheet that is blocking scripts" held the script until
/// they were in.
/// `Some(j)`: sheet `j` holds the script and is not in yet -- the pass waits for it.
fn apply_sheets_before(scope: &mut v8::PinScope, staged: &Staged, next: &mut usize, el: Option<v8::Local<v8::Value>>) -> Option<usize> {
    while let Some(sh) = staged.sheets.get(*next) {
        if let Some(el) = el {
            if !webapi::page::precedes_element(scope, sh.id, el) {
                return None;
            }
        }
        if sh.pending {
            return Some(*next);
        }
        *next += 1;
        let fetched = sh.response.as_ref().map(|r| (r.status, r.headers.clone(), r.body.clone(), r.url.clone()));
        let timing = sh.response.as_ref().and_then(|r| r.timing.clone());
        v8::tc_scope!(let tc, scope);
        webapi::page::parser_sheet_arrived(tc, sh.id, &sh.url, fetched, timing, sh.asked_ms, sh.cors);
        checkpoint(tc);
        tc.reset();
    }
    None
}

/// When a parser-inserted classic `<script src>` runs. Chrome 150, loopback, three runs
/// each (`document.readyState` logged by each script):
///
/// ```text
/// async a.js + defer d.js + sync s.js, all fast   async:loading sync:loading inline:loading defer:interactive DCL
/// async slow-a.js (800 ms) + defer d.js           inline:loading defer:interactive DCL:interactive async-slow:interactive load
/// korvus before                                   every one of them at `loading`, in document order
/// ```
///
/// So `defer` runs after the parse, at `interactive`, before `DOMContentLoaded`, in document order; and
/// `async` runs when its body is there -- during the parse if it arrived while the parser was still busy,
/// after `DOMContentLoaded` if the parse had finished first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Postpone {
    /// Runs where it stands in document order.
    InPlace,
    /// `defer` (and not `async`): after the parse, before `DOMContentLoaded`.
    Defer,
    /// `async` whose body arrived after the parse would have finished: after `DOMContentLoaded`.
    AfterParse,
    /// `async`, decided where the parser reaches it (the streaming parse): in place when its body is in, after the
    /// parse when it is not.
    Async,
}

/// A postponed script, taken off the staged list when its element was reached.
pub(crate) struct Postponed {
    el: v8::Global<v8::Value>,
    body: Option<String>,
    /// its staged response, not in when the parser reached it ([`resolve_postponed`] takes it)
    slot: Option<usize>,
    url: String,
    pub when: Postpone,
    /// an `async` script not in when the parser reached it: it runs the moment it is ([`run_arrived_async`])
    from_async: bool,
    /// How many PARSER module scripts the parse had queued before this one: deferred
    /// classic scripts and module scripts are ONE list, run in document order after the parse, so a
    /// `defer` script runs after every module above it and before every module below it.
    pub modules_before: usize,
}

/// The parser modules queued so far -- the position a deferred classic script takes among them.
fn parser_modules_queued(scope: &mut v8::PinScope) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<webapi::page::PendingModules>().map(|q| q.0.iter().filter(|m| !m.inserted).count()).unwrap_or(0)
}

/// One `<script src>` response, as the fetch pass received it. The status and headers are kept because
/// whether the body RUNS is decided by them (`webapi::page::script_response_ok`), and that decision
/// needs the script's flavour, which only the driver knows.
pub(crate) struct Fetched {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// What became of one `<script src>`'s fetch, from the element's point of view.
enum External {
    /// This pass fetched nothing (the empty stage): do nothing and fire nothing.
    NotStaged,
    /// A network error, a status of 400 or more, or a Content-Type the flavour refuses. The body is
    /// NOT run, and the element fires `error`.
    Refused,
    /// A response the flavour executes. The body may be empty (a 204, a 304), which runs nothing and
    /// still fires `load`.
    Usable(String),
    /// Not in yet: staged slot `.0` (the streaming parse).
    Pending(usize),
}

fn take_external(staged: &Staged, ei: &mut usize, flavor: webapi::page::ScriptFlavor) -> External {
    let slot = staged.external.get(*ei);
    *ei += 1;
    match slot {
        None => External::NotStaged,
        Some(Slot::Pending) => External::Pending(*ei - 1),
        Some(Slot::Failed) => External::Refused,
        Some(Slot::Got(f)) if webapi::page::script_response_ok(flavor, f.status, &f.headers) => External::Usable(f.body.clone()),
        Some(Slot::Got(_)) => External::Refused,
    }
}

/// Does a `<script type>` name a script the browser will execute?
///
/// HTML's rule is a FIXED LIST -- the "JavaScript MIME type essence match" -- not a substring test.
/// The substring test this replaces was wrong in both directions at once, which is why 464 subtests in
/// html/semantics/scripting-1 failed:
///
///   `type="javascript"`        contains "javascript", so we RAN it. It is not a MIME type at all and
///                              a browser skips it. Same for `javascript1.5`, `jscript`, `livescript`
///                              -- the bare, unprefixed spellings are all invalid.
///   `type="application/ecmascript"`  does NOT contain "javascript", so we SKIPPED it. It is valid.
///
/// The `text/javascript1.N` spellings are genuinely on the list (a legacy the spec still carries); the
/// same names without `text/` are not.
///
/// An absent or empty attribute is a classic script, as is one whose value is only whitespace: HTML
/// strips ASCII whitespace before matching. `module` is the module type. `text/babel` is not a browser
/// type at all -- it is kept because our own fixtures use it, and it is called out here rather than
/// hidden in a substring match.
pub(crate) fn script_type_runs(ty: &str) -> bool {
    // a DevTools client switched scripting off (`Emulation.setScriptExecutionDisabled`)
    if spec::devtools::scripts_disabled() {
        return false;
    }
    // The empty check is on the RAW value, BEFORE stripping. That is the whole difference between
    // `type=""` (runs) and `type=" "` (does not): both strip to "", so a rule that strips first cannot
    // tell them apart, and the suite tests exactly that pair.
    if ty.is_empty() {
        return true;
    }
    let t = ty.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\u{0C}' | '\r' | ' '));
    // MODULE IS MATCHED CASE-INSENSITIVELY, through `webapi`'s one test, which the
    // inserted-script path asks too. `t == "module"` skipped `<script type="MODULE" src=…>` entirely:
    // korvus fetched the body, ran nothing and fired no event, where Chrome 150 (measured over
    // loopback, 2026-09-18) runs it as a module and fires `load`.
    if webapi::page::is_module_type(t) || t == "text/babel" {
        return true;
    }
    // The list is `webapi`'s, the same one the inserted-script path and the response MIME checks use.
    webapi::page::is_javascript_mime_essence(&t.to_ascii_lowercase())
}
/// The `<script>` elements of a document in tree order.
///
/// HTML documents keep the fast exact-name lookup. An XML document's scripts may carry a namespace
/// PREFIX -- WPT's SVG tests all load testharness.js through `<h:script src="…"/>` -- and a
/// qualified-name match finds none of those, so there the scan is by LOCAL name. Mirrored by the
/// scan in RUN_PAGE_SCRIPTS_EXT: the two must agree element-for-element, because this pass fetches
/// the external bodies that pass consumes positionally.
///
/// A page document whose `<template for>` patches wait for the pass (`Dom::parse_page`) adds
/// the scripts inside them, in parse order (node ids): the pass streams each patch to its script first.
pub(crate) fn script_elements(dom: &webapi::dom::Dom, is_xml: bool) -> Vec<u32> {
    // The DOCUMENT TREE's scripts: the arena-wide lookup also returned every template content's,
    // which the pass (the live document's `getElementsByTagName`) never meets -- and the external bodies are
    // consumed by position, so a `<template>` holding a `<script src>` ran each later script with the body before
    // its own (Chrome 150: `t|b|c`, korvus `t|a|b`).
    let mut out: Vec<u32> = if !is_xml {
        dom.get_elements_by_tag_name_in(dom.root(), "script")
    } else {
        dom.descendant_elements(dom.root())
            .into_iter()
            .filter(|&n| {
                dom.tag_name(n)
                    .map(|t| local_name(t).eq_ignore_ascii_case("script"))
                    .unwrap_or(false)
            })
            .collect()
    };
    if dom.has_pending_patches() {
        // And the patches' (node ids: parse order).
        out.extend(dom.pending_patch_scripts());
        out.sort_unstable();
    }
    out
}
/// The URL a `<script>` loads, if it is an external script.
///
/// HTML says `src` and only `src`. SVG's own `<script>` says `href` (SVG 2) or `xlink:href`
/// (SVG 1.1), and both spellings appear in the corpus, so an XML document is asked for all three
/// in that order. HTML documents are asked only for `src`, so this changes nothing for them.
pub(crate) fn script_src<'a>(dom: &'a webapi::dom::Dom, n: u32, is_xml: bool) -> Option<&'a str> {
    if !is_xml {
        return dom.get_attribute(n, "src");
    }
    dom.get_attribute(n, "src")
        .or_else(|| dom.get_attribute(n, "href"))
        .or_else(|| dom.get_attribute(n, "xlink:href"))
}
/// The local part of a possibly-prefixed XML qualified name (`h:script` -> `script`).
fn local_name(qname: &str) -> &str {
    match qname.rfind(':') {
        Some(i) => &qname[i + 1..],
        None => qname,
    }
}

/// Every `<script>` of the LIVE document, in document order — the driver's own scan, as against
/// [`script_elements`]'s over the parsed arena. The two must agree element-for-element.
///
/// In an HTML document `getElementsByTagName("script")` is exactly right and is the fast path. In an
/// XML one it is not: it matches the QUALIFIED name, so a prefixed `<h:script>` matches nothing and the
/// harness never ran — the file produced no verdict at all. There, scan every element and match the
/// LOCAL name instead.
fn live_scripts<'s>(scope: &mut v8::PinScope<'s, '_>, is_xml: bool) -> Vec<v8::Local<'s, v8::Value>> {
    // NATIVELY, never through the page-visible `document.getElementsByTagName`: a page
    // that wraps it saw the browser call it once per document, before any of its own scripts -- Chrome's
    // parser never calls into script to find its scripts. [`script_elements`] is the same scan over the
    // live arena (with `<template for>` patches still to stream, their scripts too).
    let ids = webapi::page::with_host_dom(scope, |d| script_elements(d, is_xml)).unwrap_or_default();
    ids.into_iter().map(|id| webapi::page::node_object(scope, id)).collect()
}

/// One attribute of a script element, or `None` when it is absent.
fn attr(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, name: &str) -> Option<String> {
    let a = str_val(scope, name);
    webapi::page::attr_call(scope, el, "getAttribute", &[a])
        .filter(|v| !v.is_null_or_undefined())
        .map(|v| v.to_rust_string_lossy(scope))
}

/// A script's URL attribute: HTML says `src`, SVG says `href` (SVG 2) or `xlink:href` (SVG 1.1).
///
/// The same order as [`crate::script_src`], which the fetch pass uses over the arena — the two must
/// agree or a staged body and the element it belongs to come apart.
pub(crate) fn script_src_of(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, is_xml: bool) -> Option<String> {
    let s = attr(scope, el, "src");
    if !is_xml {
        return s;
    }
    s.or_else(|| attr(scope, el, "href")).or_else(|| attr(scope, el, "xlink:href"))
}

/// An inline script's body.
///
/// An XML inline script's body normally lives in a CDATA SECTION (`<script><![CDATA[ … ]]></script>` is
/// how essentially every XHTML test in WPT writes one), and this arena's `textContent` collects Text
/// nodes only — so the body read back empty and the script was skipped as blank. Fall back to the
/// element's own CDATA children. Guarded on `textContent` being blank, so an HTML script (which can
/// never have a CDATA child) takes the identical path it always did.
fn script_body(scope: &mut v8::PinScope, el: v8::Local<v8::Value>) -> String {
    // Read from the arena, never through the page-visible `textContent`/`childNodes`/`nodeType`:
    // the element's descendant text, else its CDATA children's data.
    let Some(id) = webapi::page::node_id(scope, el) else { return String::new() };
    webapi::page::with_host_dom(scope, |d| {
        let t = d.text_content(id);
        if !t.trim().is_empty() {
            return t;
        }
        let mut out = String::new();
        let mut c = d.first_child(id);
        while let Some(k) = c {
            if d.node_type(k) == 4 {
                out.push_str(&d.node_value(k).unwrap_or_default());
            }
            c = d.next_sibling(k);
        }
        out
    })
    .unwrap_or_default()
}

/// Run one body as a CLASSIC SCRIPT in the current realm, with `url` as its script origin.
///
/// This is `Deno.core.evalContext` without the round trip through JS: compile a `v8::Script` (not a
/// module) against the current context and run it. Giving it the script's real source URL is what lets
/// a page's own `new Error().stack` read as a normal page URL rather than a `korvus:` internal name —
/// which a sensor that captures stacks does look at.
///
/// Returns the thrown value if the script threw, with the SITE to report it at when the value cannot say
/// (see [`webapi::caught_site`]), so the caller can report it exactly as a browser does. A COMPILE error
/// and a runtime throw come back the same way, which is what the page sees too.
fn run_classic<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str, url: &str) -> Option<Thrown> {
    run_classic_at(scope, code, url, (0, 0))
}

/// [`run_classic`] for an inline script whose text starts at `pos` (line, column offsets) in its document.
fn run_classic_at<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str, url: &str, pos: (i32, i32)) -> Option<Thrown> {
    run_classic_full(scope, code, url, pos).0
}

/// [`run_classic`] for a WORKER's script, which must tell a run `terminate()` interrupted from one that
/// finished or threw: the `bool` is true when the run was TERMINATED, and then nothing is to be reported
/// and nothing more is to run. This file's `TryCatch` ends the termination where deno_core's
/// `execute_script` kept the isolate terminating, so a terminated worker would otherwise go on to its
/// event loop.
pub(crate) fn run_worker_script<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str, url: &str) -> (Option<Thrown>, bool) {
    run_classic_full(scope, code, url, (0, 0))
}

fn run_classic_full<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str, url: &str, pos: (i32, i32)) -> (Option<Thrown>, bool) {
    // The script's source, to any observer plugin before it compiles (plugin ABI 3, F3): read-only, for taint and
    // deobfuscation tooling. This is the classic-script compile point every page, frame and worker script reaches.
    // Costs nothing when no plugin observes.
    if plugin_host::has_observer() {
        let payload = serde_json::json!({ "url": url.split('\n').next().unwrap_or(""), "source": code });
        plugin_host::event("script", &payload.to_string());
    }
    // D106: a CDP client debugging this page is told of every classic script it compiles here
    // (`Debugger.scriptParsed`) and may read its text back (`Debugger.getScriptSource`). This is the one point every
    // page, frame and worker classic script reaches -- the same place the observer `script` event taps -- so one hook
    // is complete. Off, and free, unless a `Debugger` client is attached.
    if webapi::devtools_log::debugger_capturing() {
        webapi::devtools_log::record_script(url.split('\n').next().unwrap_or(""), code, pos.0);
    }
    let Some(source) = v8::String::new(scope, code) else { return (None, false) };
    // The specifier is normalised the way `Deno.core.evalContext` normalised it, so a stack frame's
    // URL does not change shape now that the compile happens one layer down.
    let spec = url::Url::parse(url).map(|u| u.to_string()).unwrap_or_else(|_| url.to_string());
    let Some(name) = v8::String::new(scope, &spec) else { return (None, false) };
    let origin = v8::ScriptOrigin::new(scope, name.into(), pos.0, pos.1, false, 0, None, false, false, false, None);
    v8::tc_scope!(let tc, scope);
    // a fetched script's background compile, when it had one (`webapi::compile_streamed_script`)
    let compiled = match webapi::compile_streamed_script(tc, code, source, &origin) {
        Some(streamed) => streamed,
        None => v8::Script::compile(tc, source, Some(&origin)),
    };
    let ran = match compiled {
        Some(script) => script.run(tc).is_some(),
        None => false,
    };
    let terminated = tc.has_terminated();
    let thrown = if ran {
        None
    } else {
        tc.exception().map(|e| {
            // Reported where the `throw` ran, read off the TryCatch's message (`webapi::caught_site`): stock
            // Chrome 150 without DevTools names the throw for an Error too (2026-10-10: `var e1 = new
            // Error(); \n  throw e1;` reports the second line, column 3). An earlier measurement here saw the
            // construction site -- through a CDP client, whose inspector makes V8 capture stacks for
            // uncaught exceptions, which Blink then prefers. The message is also the only record of a syntax
            // error's position.
            let m = tc.message();
            let site = webapi::caught_site(tc, e, m);
            (v8::Global::new(tc, e), site)
        })
    };
    tc.reset();
    (thrown, terminated)
}

/// A classic script's throw: the value, and where V8 says it happened.
type Thrown = (v8::Global<v8::Value>, Option<webapi::ErrorSite>);

/// Report a page script's throw the way a browser does: HTML "report an exception" -- an `ErrorEvent`
/// at the global and the engine's uncaught tally, which is this engine's DevTools log.
///
/// NOT the page's `console.error`. This function used to call it after every report,
/// and `console` belongs to the page: Chrome 150 calls neither a wrapped `console.error` nor a
/// replaced `console` object for an uncaught error -- measured for a classic throw, a thrown string,
/// a syntax error, an external script's throw, a timer callback's throw, a module's throw and a
/// `preventDefault()`ed one -- and the parity collector counts the page's `console.error` calls as the
/// scored `console_errors`.
pub(crate) fn report(scope: &mut v8::PinScope, err: v8::Local<v8::Value>, site: Option<webapi::ErrorSite>) {
    quietly(scope, |scope| webapi::report_exception_at(scope, err, site));
}

/// `document.currentScript`, which the `Document` table reads. Set around each script the driver
/// runs, and cleared when the pass is over.
///
/// It was a `bridge_set` of the `__curScript` slot; the store is a V8 Private on the
/// global now, so the page cannot see or change it. Nothing about WHEN it is written moved.
fn set_current_script(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) {
    quietly(scope, |scope| webapi::page::set_current_script(scope, v));
}

/// HTML "clean up after running script", at the one moment in a classic script's life it is defined
/// to happen: after the body ran and after its throw was reported, and BEFORE `currentScript` is put
/// back. The rule, and the Chrome table it was measured from, are in `webapi`'s
/// `clean_up_after_running_script` -- one definition, because this crate runs the scripts the PARSER
/// found and `webapi` runs the ones a page INSERTS.
///
/// EVERY CALLER HERE HAS AN EMPTY JS STACK, which is the condition HTML attaches to the checkpoint:
/// this driver is Rust, so the script it just ran was the only thing on the stack. The nested case
/// -- a script inserted and run from inside another -- is `webapi`'s, and performs no checkpoint of
/// its own for exactly that reason.
///
/// A module is not given one: `currentScript` is null throughout a module's evaluation, and the
/// module queue's evaluator already drains behind it.
fn checkpoint(scope: &mut v8::PinScope) {
    quietly(scope, webapi::page::clean_up_after_running_script);
}

/// Resolve a script URL against the document's BASE url (`<base href>` applied), which is what the
/// Rust fetch pass joins against too — the two must agree or a module's key and its body come apart.
fn base_url(scope: &mut v8::PinScope, page_url: &str) -> String {
    // The rule itself (`webapi::document_base_url`), not the page-visible `document.baseURI` getter.
    webapi::page::with_host_dom(scope, |d| webapi::document_base_url(d, page_url))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| page_url.to_string())
}

/// Run every `<script>` of the loaded document in document order, fetching nothing a `document.write`
/// asks for (`Engine::load_html_injected`'s stage-nothing contract: a written parser-blocking script
/// is refused -- it fires `error` -- and the parse goes on).
pub(crate) fn run_page_scripts(scope: &mut v8::PinScope, staged: &Staged) -> Vec<Postponed> {
    let mut pass = begin_page_scripts(scope, staged);
    let mut fetched = None;
    while let Step::Fetch(_) = continue_page_scripts(scope, &mut pass, staged, fetched.take()) {
        fetched = Some((None, None));
    }
    pass.postponed
}

/// The page's script pass, RESUMABLE: the parser pauses at a parser-blocking script a
/// `document.write` produced until that script has been fetched, and the fetch is the async driver's
/// (`navigation.rs`) -- so the pass returns [`Step::Fetch`] there and is continued with the response.
/// A blocking fetch on this thread would stall every in-process server the event loop runs.
pub(crate) struct PagePass {
    /// The parser's scripts, snapshotted when the pass began (the document was parsed up front).
    scripts: Vec<v8::Global<v8::Value>>,
    next: usize,
    /// The external-body cursor -- positional, see the module header.
    ei: usize,
    /// The prefetched-sheet cursor, in tree order.
    si: usize,
    /// Which staged scripts' resource entries are on the timeline ([`ScriptEntry`]).
    entered: Vec<bool>,
    base: String,
    page_url: String,
    /// The written script whose response the next [`continue_page_scripts`] brings.
    written: Option<(v8::Global<v8::Value>, String, f64)>,
    /// The script the parser waits at, PREPARED ([`Prepared`]): its index in `scripts`, and what was read then.
    prepared: Option<(usize, Prepared)>,
    pub postponed: Vec<Postponed>,
}

/// What HTML's "prepare the script element" read of a classic script the parser then waited at (the streaming parse):
/// an inline script's source text and the CSP verdict on it (or on an external one's URL). Chrome reads both when the
/// parser reaches the end tag, before it waits for a script-blocking sheet or the script's own response -- an `async`
/// script that runs meanwhile and rewrites the text or the nonce changes neither (WPT
/// content-security-policy/script-src/script{hash,nonce}-changed-*).
pub(crate) struct Prepared {
    body: Option<String>,
    allowed: bool,
}

/// Prepare the classic script `el` the parser is about to wait at; `None` for anything [`run_one`] does not run as a
/// classic script.
fn prepare(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, staged: &Staged, base: &str) -> Option<Prepared> {
    let raw_ty = attr(scope, el, "type").unwrap_or_default();
    if webapi::page::is_import_map_type(&raw_ty) || !script_type_runs(&raw_ty) || webapi::page::nomodule_skips(attr(scope, el, "nomodule").is_some(), &raw_ty) {
        return None;
    }
    let ty = raw_ty.trim_matches(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '));
    if webapi::page::is_module_type(ty) {
        return None;
    }
    match script_src_of(scope, el, staged.is_xml).filter(|s| !s.trim().is_empty()) {
        Some(src) => {
            let url = url::Url::parse(base).and_then(|b| b.join(&src)).map(|u| u.to_string()).unwrap_or(src);
            Some(Prepared { body: None, allowed: webapi::page::csp_allows_parser_script(scope, el, None, Some(&url)) })
        }
        None => {
            let body = script_body(scope, el);
            if body.trim().is_empty() {
                return None;
            }
            let allowed = webapi::page::csp_allows_parser_script(scope, el, Some(&body), None);
            Some(Prepared { body: Some(body), allowed })
        }
    }
}

/// Where the pass stands: finished, or waiting for the response to this written script's URL.
pub(crate) enum Step {
    Done,
    Fetch(String),
    /// The streaming parse waits for parser script `.0`'s response -- a blocking script, the next to
    /// run -- with the event loop running.
    WaitScript(usize),
    /// ... or for script-blocking sheet `.0`.
    WaitSheet(usize),
}

pub(crate) fn begin_page_scripts(scope: &mut v8::PinScope, staged: &Staged) -> PagePass {
    let scripts = live_scripts(scope, staged.is_xml).into_iter().map(|el| v8::Global::new(scope, el)).collect();
    let page_url = staged.page_url.to_string();
    let base = base_url(scope, &page_url);
    PagePass { scripts, next: 0, ei: 0, si: 0, entered: vec![false; staged.entries.len()], base, page_url, written: None, prepared: None, postponed: Vec::new() }
}

/// Run the pass until it ends or needs a written script's response. `fetched` is the response to the
/// URL the previous call returned in [`Step::Fetch`] (`(None, None)`: the request failed).
pub(crate) fn continue_page_scripts(
    scope: &mut v8::PinScope,
    pass: &mut PagePass,
    staged: &Staged,
    fetched: Option<(Option<Fetched>, Option<spec::FetchTiming>)>,
) -> Step {
    if let Some((el, url, asked_ms)) = pass.written.take() {
        let (response, timing) = fetched.unwrap_or((None, None));
        run_written(scope, &el, &url, response, timing, asked_ms);
        webapi::page::flush_written_input(scope);
        if crate::navigation::navigation_pending(scope) {
            return finish(scope, pass, staged);
        }
    }
    loop {
        if let Some((el, url)) = webapi::page::take_written_blocking(scope) {
            let asked_ms = webapi::page::realm_now_ms(scope);
            pass.written = Some((v8::Global::new(scope, el), url.clone(), asked_ms));
            return Step::Fetch(url);
        }
        let Some(el) = pass.scripts.get(pass.next).map(|g| v8::Local::new(scope, g)) else {
            return finish(scope, pass, staged);
        };
        pass.next += 1;
        // The document's `<template for>` patches, up to this script; a script of a patch
        // that could not open stays in its template, unrun.
        if !webapi::page::stream_template_patches(scope, Some(el)) {
            continue;
        }
        // a sheet not in yet holds only a script that waits for sheets: an `async`, `defer` or module script is prepared
        // and the parse goes on (HTML: only a parser-blocking or inline classic script waits for "a style sheet that is
        // blocking scripts")
        let sheet = apply_sheets_before(scope, staged, &mut pass.si, Some(el)).filter(|_| waits_for_sheets(scope, el, staged, pass.ei));
        let wait = match sheet {
            Some(j) => Some(Step::WaitSheet(j)),
            None => blocking_pending(scope, el, staged, pass.ei).map(Step::WaitScript),
        };
        if let Some(wait) = wait {
            pass.next -= 1;
            if pass.prepared.as_ref().is_none_or(|(k, _)| *k != pass.next) {
                pass.prepared = prepare(scope, el, staged, &pass.base).map(|p| (pass.next, p));
            }
            return wait;
        }
        let prepared = pass.prepared.take().filter(|(k, _)| *k == pass.next - 1).map(|(_, p)| p);
        // EVERY per-script step is inside this guard. It is not decoration -- see the module header:
        // a throw from the DRIVER (as opposed to from the page script it is running) used to unwind
        // the whole pass, and the pass is what runs the rest of the page. A driver bug must cost ONE
        // script, never the document, and it must be REPORTED rather than swallowed.
        let thrown = {
            v8::tc_scope!(let tc, scope);
            let el = v8::Local::new(tc, &pass.scripts[pass.next - 1]);
            run_one(tc, el, staged, &pass.base, &pass.page_url, &mut pass.ei, &mut pass.entered, &mut pass.postponed, prepared);
            let t = tc.exception().map(|e| v8::Global::new(tc, e));
            tc.reset();
            t
        };
        if let Some(t) = thrown {
            let t = v8::Local::new(scope, &t);
            // The DRIVER's own failure is the engine's diagnostic: the engine's log gets it, and so
            // does the uncaught report (a page load must still surface it) -- never the page's
            // `console`, which it used to reach with a `korvus page-script driver:` prefix.
            let shown = t.to_rust_string_lossy(scope);
            eprintln!("[korvus] page-script driver: {shown}");
            report(scope, t, None);
        }
        // A script that started a cross-document navigation ends this document's parse:
        // Chrome runs no later script of it and never reaches "the end" (measured,
        // `navigation.rs`'s P12d table). The navigation itself is the driver's to perform.
        if crate::navigation::navigation_pending(scope) {
            return finish(scope, pass, staged);
        }
    }
}

/// The script the parser is about to run waits for the script-blocking sheets before it: an inline classic script, or a
/// parser-blocking classic `<script src>`.
fn waits_for_sheets(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, staged: &Staged, ei: usize) -> bool {
    let raw_ty = attr(scope, el, "type").unwrap_or_default();
    if webapi::page::is_import_map_type(&raw_ty) || !script_type_runs(&raw_ty) || webapi::page::nomodule_skips(attr(scope, el, "nomodule").is_some(), &raw_ty) {
        return false;
    }
    let ty = raw_ty.trim_matches(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '));
    if webapi::page::is_module_type(ty) {
        return false;
    }
    if !script_src_of(scope, el, staged.is_xml).is_some_and(|s| !s.trim().is_empty()) {
        return true;
    }
    // with an empty `postpone` every staged script runs in place (`Staged::postpone`)
    matches!(staged.postpone.get(ei).copied().unwrap_or(Postpone::InPlace), Postpone::InPlace)
}

/// The script the parser is about to run is a classic, blocking `<script src>` whose response is not in: its staged
/// slot. The same classification [`run_one`] makes, read without consuming anything.
fn blocking_pending(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, staged: &Staged, ei: usize) -> Option<usize> {
    if !matches!(staged.external.get(ei), Some(Slot::Pending)) {
        return None;
    }
    let raw_ty = attr(scope, el, "type").unwrap_or_default();
    if webapi::page::is_import_map_type(&raw_ty) || !script_type_runs(&raw_ty) {
        return None;
    }
    if webapi::page::nomodule_skips(attr(scope, el, "nomodule").is_some(), &raw_ty) {
        return None;
    }
    let ty = raw_ty.trim_matches(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '));
    if webapi::page::is_module_type(ty) {
        return None;
    }
    let has_src = script_src_of(scope, el, staged.is_xml).is_some_and(|s| !s.trim().is_empty());
    let blocking = matches!(staged.postpone.get(ei).copied().unwrap_or(Postpone::InPlace), Postpone::InPlace);
    (has_src && blocking).then_some(ei)
}

/// The streaming parse's tail, once every response is in: the sheets no script waited for, the
/// resource entries of the scripts that were not in when the parse ended, and the postponed scripts' and parser
/// modules' bodies.
pub(crate) fn finish_late(scope: &mut v8::PinScope, pass: &mut PagePass, staged: &Staged) {
    apply_sheets_before(scope, staged, &mut pass.si, None);
    for i in 0..pass.entered.len() {
        enter_entry(scope, staged, &mut pass.entered, i);
    }
    for p in pass.postponed.iter_mut() {
        // a response still not in keeps its slot: the driver calls this again once it is
        if let Some(i) = p.slot.filter(|&i| !matches!(staged.external.get(i), Some(Slot::Pending))) {
            p.slot = None;
            p.body = classic_body(staged, i);
        }
    }
    crate::modules::fill_staged_modules(scope, &|i| match staged.external.get(i) {
        Some(Slot::Pending) => None,
        Some(Slot::Got(f)) if webapi::page::script_response_ok(webapi::page::ScriptFlavor::Module, f.status, &f.headers) => Some(Some(f.body.clone())),
        _ => Some(None),
    });
}

/// The parse's end has scripts to run before `DOMContentLoaded` -- `defer` ones, or the parser's modules -- and those
/// wait for every script-blocking sheet as well (HTML "the end", step 3).
pub(crate) fn deferred_scripts(scope: &mut v8::PinScope, pass: &PagePass, _staged: &Staged) -> bool {
    pass.postponed.iter().any(|p| p.when == Postpone::Defer) || parser_modules_queued(scope) > 0
}

fn classic_body(staged: &Staged, i: usize) -> Option<String> {
    match staged.external.get(i) {
        Some(Slot::Got(f)) if webapi::page::script_response_ok(webapi::page::ScriptFlavor::Classic, f.status, &f.headers) => Some(f.body.clone()),
        _ => None,
    }
}

/// The `async` scripts whose response came in while the parser waited, or while the parse's end waited for a
/// deferred one (the streaming parse): each runs now -- HTML executes an `async` script as soon as it is ready, and
/// Chrome does so between the parser's waits -- and leaves the postponed list.
pub(crate) fn run_arrived_async(scope: &mut v8::PinScope, pass: &mut PagePass, staged: &Staged) {
    let mut k = 0;
    while k < pass.postponed.len() {
        let p = &pass.postponed[k];
        let Some(i) = p.slot.filter(|&i| p.from_async && !matches!(staged.external.get(i), Some(Slot::Pending))) else {
            k += 1;
            continue;
        };
        let mut p = pass.postponed.remove(k);
        p.slot = None;
        p.body = classic_body(staged, i);
        enter_entry(scope, staged, &mut pass.entered, i);
        run_postponed_one(scope, &p);
        if crate::navigation::navigation_pending(scope) {
            return;
        }
    }
}

fn finish(scope: &mut v8::PinScope, pass: &mut PagePass, staged: &Staged) -> Step {
    // The parse's end: the entry of every script that did not run in place -- deferred, late `async`, module, or
    // never reached -- is on the timeline ([`ScriptEntry`]).
    for i in 0..pass.entered.len() {
        enter_entry(scope, staged, &mut pass.entered, i);
    }
    // Every `<template for>` patch the pass did not reach arrives.
    webapi::page::stream_template_patches(scope, None);
    // Any sheet no parser script came after -- a script the page removed -- still arrives (one not in yet, in
    // [`finish_late`]).
    let _ = apply_sheets_before(scope, staged, &mut pass.si, None);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    set_current_script(scope, null);
    Step::Done
}

/// Run one parser-blocking script `document.write` produced, now that its response is in.
///
/// Measured in Chrome 150 on `tools/realweb/seed/dw`: a script that writes `<script src="x.js">` finishes
/// first (`writer1-end:undefined`), then x.js runs with `document.currentScript` its element, then
/// x2.js -- which x.js itself wrote -- and only then the next script of the page (`b:number,number`).
/// The markup written behind a blocking script waits for it (`writer2-end:undefined`, `y.js`,
/// `inline-after-y:number`). Korvus ran every written `<script src>` as a script-inserted one, after
/// `DOMContentLoaded`, so `window.jQuery || document.write(<script src=jquery>)` -- books.toscrape.com,
/// semantic-ui.com -- left the next inline script calling an undefined `jQuery`.
fn run_written(
    scope: &mut v8::PinScope,
    el: &v8::Global<v8::Value>,
    url: &str,
    fetched: Option<Fetched>,
    timing: Option<spec::FetchTiming>,
    asked_ms: f64,
) {
    // a script `document.write` produced blocks the parser; its render-blocking status is the parser's
    webapi::page::parser_script_loaded(scope, url, timing, asked_ms, true, false);
    v8::tc_scope!(let tc, scope);
    let el = v8::Local::new(tc, el);
    let null: v8::Local<v8::Value> = v8::null(tc).into();
    set_current_script(tc, null);
    match fetched {
        Some(f) if webapi::page::script_response_ok(webapi::page::ScriptFlavor::Classic, f.status, &f.headers) => {
            if !f.body.trim().is_empty() {
                set_current_script(tc, el);
                if let Some((err, site)) = run_classic(tc, &f.body, url) {
                    let err = v8::Local::new(tc, &err);
                    report(tc, err, site);
                }
                checkpoint(tc);
                let null: v8::Local<v8::Value> = v8::null(tc).into();
                set_current_script(tc, null);
            }
            fire_at_element(tc, el, "load");
        }
        _ => fire_at_element(tc, el, "error"),
    }
    tc.reset();
}

/// Run the postponed scripts whose moment is `when`, in document order, as `run_one` would have run
/// them in place: a refused body fires `error`, a usable one runs and fires `load`.
pub(crate) fn run_postponed(scope: &mut v8::PinScope, postponed: &[Postponed], when: Postpone) {
    for p in postponed.iter().filter(|p| p.when == when) {
        run_postponed_one(scope, p);
        if crate::navigation::navigation_pending(scope) {
            break;
        }
    }
}

/// One postponed classic script: its body, or `error` when it had none.
pub(crate) fn run_postponed_one(scope: &mut v8::PinScope, p: &Postponed) {
    {
        {
            v8::tc_scope!(let tc, scope);
            let el = v8::Local::new(tc, &p.el);
            match &p.body {
                None => fire_at_element(tc, el, "error"),
                Some(body) => {
                    if !body.trim().is_empty() {
                        set_current_script(tc, el);
                        if let Some((err, site)) = run_classic(tc, body, &p.url) {
                            let err = v8::Local::new(tc, &err);
                            report(tc, err, site);
                        }
                        checkpoint(tc);
                        let null: v8::Local<v8::Value> = v8::null(tc).into();
                        set_current_script(tc, null);
                    }
                    fire_at_element(tc, el, "load");
                }
            }
            tc.reset();
        }
    }
}

/// One `<script>`: classify it, find its body, and run it (or queue it, if it is a module).
///
/// `ei` is the POSITIONAL cursor into the staged external bodies. It is `&mut` rather than returned
/// because a script that is skipped must not advance it, and that rule is easier to read where the
/// skip is. (There was a second cursor here, `mi`, numbering the inline modules for the unique
/// fragment each one was named with. The fragment and the counter are gone.)
fn run_one(
    scope: &mut v8::PinScope,
    el: v8::Local<v8::Value>,
    staged: &Staged,
    base: &str,
    page_url: &str,
    ei: &mut usize,
    entered: &mut [bool],
    postponed: &mut Vec<Postponed>,
    prepared: Option<Prepared>,
) {
    {
        let raw_ty = attr(scope, el, "type").unwrap_or_default();
        // The RAW attribute, exactly as the fetch pass reads it -- not a pre-lowercased copy. The
        // lowercased copy the JS driver used to pass disagreed with the Rust scan on `type="MODULE"`
        // and `type="Text/Babel"` (Rust skipped both; the driver ran both), and a scan that accepts one
        // more script than the fetch pass hands every later external script the wrong body.
        // AN IMPORT MAP THE PARSER FOUND, and it is read BEFORE the "does this type
        // run?" gate, because that gate is right -- an import map is not code -- and used to be the
        // whole of this engine's import-map support. Six live-corpus rows died on a bare specifier
        // whose mapping was sitting unread in the document -- five pages, each shipping one inline
        // `<script type="importmap">`, named in a demand map and not here.
        //
        // NOTHING WAS FETCHED FOR IT AND NOTHING IS CONSUMED HERE. The fetch pass asks
        // `script_type_runs` too, so no external body was staged for this element and the positional
        // cursor `ei` must not move -- the same rule the `nomodule` skip below is written to obey.
        if webapi::page::is_import_map_type(&raw_ty) {
            let has_src = script_src_of(scope, el, staged.is_xml).map(|s| !s.trim().is_empty()).unwrap_or(false);
            if has_src {
                // An EXTERNAL import map is refused outright: Chrome 150 fires `error` at the
                // element and maps nothing, even when the element also carries a well-formed body.
                fire_at_element(scope, el, "error");
            } else {
                let text = script_body(scope, el);
                // EMPTY, NOT BLANK. HTML's "prepare the script element" returns early for a
                // src-less element whose child text content is the EMPTY STRING, so nothing is
                // parsed and nothing is reported; three spaces is NOT empty and IS reported as
                // invalid JSON. Measured on Chrome 150, probe `p31` -- four empty maps on one page,
                // one error, and it is the whitespace one.
                if !text.is_empty() {
                    webapi::page::install_import_map(scope, &text);
                }
            }
            return;
        }
        if !script_type_runs(&raw_ty) {
            return; // JSON / template scripts are data, not code.
        }
        // A CLASSIC `<script nomodule>` IS NOT RUN, and this returns BEFORE `ei` moves:
        // the fetch pass (`navigation.rs`) skipped the same element, so consuming a staged body here
        // would hand every later external script the wrong one. The rule -- and the fact that the
        // fetch never happened at all -- is `webapi`'s `nomodule_skips`. The RAW `type` value, exactly
        // as the fetch pass reads it, for the same reason `script_type_runs` gets the raw one.
        if webapi::page::nomodule_skips(attr(scope, el, "nomodule").is_some(), &raw_ty) {
            return;
        }
        let ty = raw_ty.trim_matches(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '));
        let src = script_src_of(scope, el, staged.is_xml);
        let has_src = src.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        let resolve = |raw: &str| -> String {
            url::Url::parse(base).and_then(|b| b.join(raw)).map(|u| u.to_string()).unwrap_or_else(|_| raw.to_string())
        };

        if webapi::page::is_module_type(ty) {
            // THE URL A MODULE IS NAMED BY IS THE URL A PAGE READS OUT OF `Error.stack`.
            // V8 puts the `ScriptOrigin` resource name in every stack frame, and deno_core builds that
            // origin from the specifier handed over here -- so this string is a page-visible surface,
            // not an internal key, and it has to be the one Chrome reports.
            //
            // Chrome 150, measured over loopback on 2026-09-12 across eight probe pages and all three
            // surfaces (`Error.stack`, `ErrorEvent.filename`/`window.onerror`, CDP
            // `Runtime.exceptionThrown.url`): an INLINE module is named by the DOCUMENT URL, bare --
            // no fragment, no query, nothing. An EXTERNAL one is named by its own resolved `src`.
            //
            // THE FRAGMENT THAT USED TO BE HERE (`{base}#korvus-inline-module-{i}`) PUT THE ENGINE'S
            // NAME IN EVERY SUCH FRAME. It was there because a module map is keyed by URL and two
            // inline modules in one document need to stay two modules. That constraint is real in the
            // spec and FALSE in both implementations, which is why the fragment could go:
            //   * Chrome does not put an inline module in the module map at all (HTML only ever
            //     registers *fetched* modules). Two byte-identical inline modules in one document both
            //     ran, at one URL, as two `scriptId`s -- measured.
            //   * deno_core 0.410 has no uniqueness check either: `ModuleMap::create_module_info`
            //     assigns `handles.len()` as the id and `by_name.insert` merely overwrites, so a
            //     second load at a taken name compiles a second `v8::Module` and evaluates it. Only
            //     the name->id lookup is last-wins, and nothing looks an inline module up by name.
            // `two_inline_modules_are_two_modules` in `tests/inline_module_url.rs` is that proof, and
            // it was written -- and passing -- before this line changed.
            //
            // THE BASE IS STILL THE BASE. What a relative `import` resolves against is the document's
            // BASE url, which `<base href>` moves away from the document url; Chrome keeps the two
            // apart (stack says the document, `import.meta.url` says the base) and one deno_core
            // string cannot. So the base travels separately, and the loader applies it. (With
            // `<base href="/sub/">` Chrome fetches an inline module's `import "./x.js"` from
            // `/sub/x.js`; korvus fetched `/x.js`.)
            if has_src {
                // An EXTERNAL module fires `load` or `error` at its element, and does so when the
                // module queue reaches it -- after the parse, in document order, before
                // DOMContentLoaded, which is where Chrome fires both. So the element travels with the
                // queued module, and a refused one is queued too, as a failure, rather than dropped.
                let murl = resolve(src.as_deref().unwrap_or(""));
                let (outcome, slot) = match take_external(staged, ei, webapi::page::ScriptFlavor::Module) {
                    External::NotStaged => return,
                    External::Refused => (None, None),
                    External::Usable(b) => (Some(b), None),
                    // not in yet: it takes its response before the parse's end drains it
                    External::Pending(i) => (None, Some(i)),
                };
                // CSP `script-src`: a blocked module is queued as a failure, so its
                // `error` fires in the module queue's order.
                let allowed = webapi::page::csp_allows_parser_script(scope, el, None, Some(&murl));
                let (outcome, slot) = if allowed { (outcome, slot) } else { (None, None) };
                let render_blocking = webapi::page::parser_render_blocking(scope, el);
                let el = v8::Global::new(scope, el);
                crate::modules::queue_external_module(scope, outcome, murl, el, slot, render_blocking);
            } else {
                let body = script_body(scope, el);
                if !body.trim().is_empty() && webapi::page::csp_allows_parser_script(scope, el, Some(&body), None) {
                    let render_blocking = webapi::page::parser_render_blocking(scope, el);
                    crate::modules::queue_module(scope, body, page_url.to_string(), Some(base.to_string()), render_blocking);
                }
            }
            return;
        }

        if has_src {
            // A classic `<script src>`. A refused response is NOT run -- a 404's HTML
            // error page handed to V8 is a SyntaxError the page never wrote -- and the element fires
            // `error`, here, before the next script in document order, which is where Chrome fires it.
            // A usable one runs, and the element fires `load` after it, whether or not it threw: a
            // script that threw still LOADED (Chrome: uncaught error, then `load`).
            // Nothing is executing while an element's `error` or `load` handler runs, so
            // `document.currentScript` is null there -- as it is in Chrome.
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            set_current_script(scope, null);
            let pending = matches!(staged.external.get(*ei), Some(Slot::Pending));
            let from_async = pending && matches!(staged.postpone.get(*ei), Some(Postpone::Async));
            let when = match staged.postpone.get(*ei).copied().unwrap_or(Postpone::InPlace) {
                // `async`: in place when its body is in, after the parse when not (HTML "execute the script element"
                // as soon as it is ready; Chrome 150, the table)
                Postpone::Async if pending => Postpone::AfterParse,
                Postpone::Async => Postpone::InPlace,
                // a blocking script is only reached once it is in (`blocking_pending`); never wait here
                Postpone::InPlace if pending => Postpone::AfterParse,
                w => w,
            };
            if when != Postpone::InPlace {
                let (body, slot) = match take_external(staged, ei, webapi::page::ScriptFlavor::Classic) {
                    External::NotStaged => return,
                    External::Refused => (None, None),
                    External::Usable(b) => (Some(b), None),
                    External::Pending(i) => (None, Some(i)),
                };
                let url = resolve(src.as_deref().unwrap_or(""));
                // CSP `script-src`: a blocked script fires `error` in its turn.
                let allowed = webapi::page::csp_allows_parser_script(scope, el, None, Some(&url));
                let (body, slot) = if allowed { (body, slot) } else { (None, None) };
                let modules_before = parser_modules_queued(scope);
                postponed.push(Postponed { el: v8::Global::new(scope, el), body, slot, url, when, from_async, modules_before });
                return;
            }
            let taken = take_external(staged, ei, webapi::page::ScriptFlavor::Classic);
            if matches!(taken, External::NotStaged | External::Pending(_)) {
                return;
            }
            // its fetch is complete when the parser reaches it: its entry is on the timeline before it runs
            enter_entry(scope, staged, entered, *ei - 1);
            // CSP `script-src`: a blocked script never runs and its element gets `error`. It was never
            // requested either (the fetch pass asks the same policies quietly), so it is asked -- and its
            // violation reported -- before a failed response is.
            let allowed = match &prepared {
                Some(p) => p.allowed,
                None => webapi::page::csp_allows_parser_script(scope, el, None, Some(&resolve(src.as_deref().unwrap_or("")))),
            };
            if !allowed {
                fire_at_element(scope, el, "error");
                return;
            }
            let body = match taken {
                External::NotStaged | External::Pending(_) => return,
                External::Refused => {
                    fire_at_element(scope, el, "error");
                    return;
                }
                External::Usable(b) => b,
            };
            if !body.trim().is_empty() {
                set_current_script(scope, el);
                if let Some((err, site)) = run_classic(scope, &body, &resolve(src.as_deref().unwrap_or(""))) {
                    let err = v8::Local::new(scope, &err);
                    report(scope, err, site);
                }
                checkpoint(scope);
                let null: v8::Local<v8::Value> = v8::null(scope).into();
                set_current_script(scope, null);
            }
            fire_at_element(scope, el, "load");
            return;
        }
        // the source text and CSP verdict prepared where the parser waited, else read now
        let (code, allowed) = match prepared {
            Some(Prepared { body: Some(body), allowed }) => (body, allowed),
            _ => {
                let body = script_body(scope, el);
                if body.trim().is_empty() {
                    return;
                }
                let allowed = webapi::page::csp_allows_parser_script(scope, el, Some(&body), None);
                (body, allowed)
            }
        };
        let surl = page_url.to_string();
        if !allowed {
            return; // CSP `script-src`
        }
        set_current_script(scope, el);
        let pos = webapi::page::script_source_position(scope, el);
        if let Some((err, site)) = run_classic_at(scope, &code, &surl, pos) {
            let err = v8::Local::new(scope, &err);
            report(scope, err, site);
        }
        checkpoint(scope);
    }
}

/// `load` / `error` at a script element. Through a `Global` because the element handle and the
/// driver's scope carry unrelated lifetimes.
fn fire_at_element(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, ty: &str) {
    let g = v8::Global::new(scope, el);
    let el = v8::Local::new(scope, &g);
    webapi::page::fire_script_event(scope, el, ty);
}

/// Clear `document.currentScript` and stage nothing — the state a navigation starts from.
pub(crate) fn reset_current_script(scope: &mut v8::PinScope) {
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    set_current_script(scope, null);
}

/// Report a failed module script TO THE PAGE, through the same uncaught path a throwing classic script
/// takes: an `ErrorEvent` at the global and the engine's uncaught tally.
///
/// A module that 404s or throws at evaluation used to be invisible — `korvus fetch --errors` reported a
/// clean page for a document whose entire behaviour was in a module that never loaded.
pub(crate) fn report_module_failure(scope: &mut v8::PinScope, message: &str) {
    quietly(scope, |scope| {
        let g = global(scope);
        let Some(ctor) = get_prop(scope, g, "Error").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
            return;
        };
        let m = str_val(scope, message);
        let Some(err) = ctor.new_instance(scope, &[m]) else { return };
        webapi::report_exception(scope, err.into());
    });
}

