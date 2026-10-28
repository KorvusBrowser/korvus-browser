//! The PAGE PIPELINE's Rust seam.
//!
//! `korvus-runtime` drives a navigation: install the document, start the loads the parser induced,
//! compile the `<body>`'s handler attributes, run the page's scripts, fire the lifecycle events. Every
//! step of that sequence has to reach a Web API behaviour that lives HERE, and until W4.g the reach
//! was a JS string in `lib.rs` calling an op off the hidden bridge's `__ops` holder:
//!
//! ```js
//! try { if (KB.__ops && typeof KB.__ops.op_sync_frames === "function") KB.__ops.op_sync_frames(true, undefined); } catch (e) {}
//! ```
//!
//! Three things were wrong with that shape, and this module is what removes all three at once. The
//! call was a STRING, so nothing type-checked it and a renamed op failed silently inside its own
//! `try`. It went through the PAGE's global — a page that reassigns `Symbol.for("korvus.bridge")`'s
//! holder, or freezes it, changes what the engine's own pipeline does. And it needed the op to be
//! REACHABLE from JS at all, which is the only reason `Deno.core.ops` is stashed on the bridge after
//! `Deno` is deleted (`binding/hidden.rs`).
//!
//! So each function below is the same body the op already had, exposed to the kernel as a plain Rust
//! call over a `v8::PinScope`. W4.g left the op wrappers in place beside them, on the grounds that
//! some were still called from `korvus-webapi`'s own JS and the rest were registered surface. Both
//! halves of that expired: the JS is gone, and a measurement of the wrappers these functions
//! shadowed found NONE of them had a caller of any kind, so they were deleted. This module is
//! now the only way in, which is what it was always meant to be.
//!
//! Nothing here is a Web API member. These are the pipeline's own entry points, and the members they
//! move (`document.readyState`, `window.length`, `<img src>`) are table rows in their own files.

use deno_core::v8;

/// The `Document` over the PAGE's tree, with its unforgeable `location`.
///
/// NOT `new Document()`: that constructor mints a fresh empty XML document (which is what the recorded
/// browser answers for `new Document()` describe — no doctype, no document element, no browsing
/// context), and the page's document is the one over the arena's own root.
pub fn page_document<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    crate::core::dom::document::mint_document(scope, crate::core::dom::document::PAGE_DOC_ROOT, "HTMLDocument")
}

/// This realm's `performance.now()`, for a pipeline step that has to say when it asked for something.
pub fn realm_now_ms(scope: &mut v8::PinScope) -> f64 {
    crate::core::timing::performance::clock_now_ms(scope)
}

/// A parser-inserted `<script src>` the pipeline fetched itself: its resource-timing entry (`initiatorType`
/// `script`; `render_blocking` the pipeline's reading of HTML's rule -- a classic script without `async` or `defer`,
/// or `blocking="render"`, in `<head>`, measured on Chrome 150). `timing` is the transport's measurement, `None`
/// when the request failed. `cors`: it was fetched in CORS mode (`crossorigin`, a module), so its response is the
/// document's to read -- `responseStatus` and `contentType` -- across origins too (Chrome 150: a `<script
/// crossorigin>` from another origin reads 200 and its type, its sizes 0 without Timing-Allow-Origin; korvus read 0
/// and "", as for an opaque one).
/// Does the CSP of `node`'s document refuse the parser script's `url` (asked quietly: the parser's own check reports it
/// when it reaches the element)? Such a script is never requested.
pub fn parser_script_refused(scope: &mut v8::PinScope, node: u32, url: &str) -> bool {
    crate::core::html::csp::parser_script_url_refused_quietly(scope, node, url)
}

/// A refused parser script's resource-timing entry, made now in the refusal shape: the preload scanner's
/// when the fetch pass refuses it, the element's when the parser reaches it.
pub fn refused_parser_script(scope: &mut v8::PinScope, node: u32, url: &str, render_blocking: bool) {
    crate::core::timing::resource_timing::refused_load_blocking(scope, node, "script", url, render_blocking);
}

pub fn parser_script_loaded(scope: &mut v8::PinScope, url: &str, timing: Option<spec::FetchTiming>, asked_ms: f64, render_blocking: bool, cors: bool) {
    let heard = crate::core::timing::performance::clock_now_ms(scope);
    crate::core::timing::resource_timing::add(
        scope,
        crate::core::timing::resource_timing::ResourceLoad {
            name: timing.as_ref().and_then(|t| t.url_list.first().cloned()).unwrap_or_else(|| url.to_string()),
            initiator_type: "script".to_string(),
            render_blocking,
            timing,
            asked_ms,
            heard_ms: heard,
            readable: cors.then_some(true),
        },
    );
}

/// Give one parser-produced `<iframe srcdoc>` its document. The attribute IS the document, so there is
/// nothing to fetch and no reason to defer.
pub fn frame_load_srcdoc(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, html: &str) {
    let Some(id) = crate::binding::protos::unwrap_id(scope, el) else { return };
    crate::core::html::browsing_context::frame_load_srcdoc(scope, id, html);
}

/// `window[i]`, `window.length` and the named frames. `forced` is the parser's full rescan, which is
/// what a document install needs: the parser writes its frames straight into the arena, through none
/// of the childList notifications the incremental path hangs off.
pub fn sync_frames(scope: &mut v8::PinScope, forced: bool) {
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    crate::core::html::browsing_context::sync_frames(scope, forced, undef);
}

/// Start every load the PARSER'S OWN elements induce: `<img src>`, `<link rel=stylesheet>`,
/// `<object data>`, SVG `<image href>`, and a frame's navigation — in document order, through the same
/// one seam a script-inserted element goes through.
pub use crate::core::html::subresource::ParserFetch;

/// Hand the parser-time fetches' responses to the next [`start_parsed_subresources`] sweep, so a
/// `<link rel=preload>` of the same resource is not fetched again; clear them after it.
pub fn stage_parser_fetches(scope: &mut v8::PinScope, fetches: Vec<ParserFetch>) {
    for f in &fetches {
        stream_parser_script(scope, f);
    }
    crate::core::html::subresource::stage_parser_fetches(scope, fetches);
}

/// A parser `<script src>`'s body, compiled off the page's thread from its arrival until the parser runs it
/// (`core::html::script_stream`).
fn stream_parser_script(scope: &mut v8::PinScope, f: &ParserFetch) {
    if f.destination == "script" && (200..300).contains(&f.status) {
        crate::core::html::script_stream::start(scope, &f.body);
    }
}

/// The parser's requests still in flight when the parsed document's other loads start (the streaming parse): a
/// `<link rel=preload>` of one joins it.
pub fn stage_parser_in_flight(scope: &mut v8::PinScope, keys: Vec<(String, &'static str, String)>) {
    crate::core::html::subresource::stage_parser_in_flight(scope, keys);
}

/// One of them is in (`None`: it failed): the preloads that joined it get its response.
pub fn settle_parser_fetch(scope: &mut v8::PinScope, url: &str, destination: &'static str, mode: &str, response: Option<ParserFetch>) {
    if let Some(f) = &response {
        stream_parser_script(scope, f);
    }
    crate::core::html::subresource::settle_parser_fetch(scope, url, destination, mode, response);
}

pub fn clear_parser_fetches(scope: &mut v8::PinScope) {
    crate::core::html::subresource::clear_parser_fetches(scope);
}

pub fn start_parsed_subresources(scope: &mut v8::PinScope) {
    crate::core::html::subresource::start_parsed_subresources(scope);
}

/// The parser's scripts have run: the FIRST STYLE RECALC, and the loads the document's styles induce
/// -- backgrounds, `@font-face` fonts, `@import`s. Not at the parse's start, where it
/// was: Chrome resolves a background at style recalc, and an inline `background-image` a parser
/// script replaces before then is never requested -- measured in Chrome 150 on
/// `tools/realweb/seed/pl` (master.dev's `hero-moss.webp`). Still before `load`, so a
/// document waiting on nothing else waits for a stylesheet's images and fonts.
pub fn style_after_parse(scope: &mut v8::PinScope) {
    crate::core::css::sheet_loads::document_parsed(scope);
}

/// The document being installed is a browser VIEWER's with its own user-agent rules -- `Some(rules)`
/// for Chrome's JSON viewer -- or, `None`, an ordinary one. Every navigation says which.
pub fn set_viewer_rules(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, rules: Option<&str>) {
    let st = op_state.borrow();
    if let Some(h) = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<page::PageHandle>()) {
        h.0.set_viewer_rules(rules);
    }
}

pub use crate::core::html::subresource::ParserSheet;
pub use crate::core::html::subresource::{EarlyFetches, ScannedLoad};

/// The preload scanner over the parsed document (`core/html/subresource.rs` [`preload_scan`]): the image
/// and `<link>` loads to request at once, in tree order, skipping the stylesheets in `skip` (the fetch
/// pass's own). Call AFTER [`claim_parser_sheets`].
pub fn preload_scan(scope: &mut v8::PinScope, skip: &[u32]) -> Vec<ScannedLoad> {
    crate::core::html::subresource::preload_scan(scope, skip)
}

/// The priority field of every load the preload scanner makes for the page's document, by element
/// (`core/html/load_priority.rs`): the fetch pass states its scripts', its blocking
/// sheets' and its scanned loads' from it, and the element loader takes the rest. Call after the document
/// is installed and before its loads start.
pub fn scan_load_priorities(scope: &mut v8::PinScope) -> std::collections::HashMap<u32, String> {
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        match st.try_borrow::<crate::binding::HostDom>() {
            Some(h) => h.0.root(),
            None => return Default::default(),
        }
    };
    crate::core::html::subresource::scan_load_priorities(scope, root)
}

/// The parser's SCRIPT-BLOCKING stylesheets: every parsed `<link rel=stylesheet>` before
/// a parser script, in tree order, claimed from the loader so the runtime fetches them itself, beside
/// the scripts. Call BEFORE [`start_parsed_subresources`].
pub fn claim_parser_sheets(scope: &mut v8::PinScope) -> Vec<ParserSheet> {
    crate::core::html::subresource::claim_parser_sheets(scope)
}

/// A claimed parser sheet has been fetched (`None`: the request failed): apply it and fire the link's
/// `load` or `error`. `status, headers, body, final_url`. `timing` is the transport's measurement. `cors`: it was
/// fetched in CORS mode (`crossorigin`), so its response is the document's to read, as [`parser_script_loaded`]'s
/// (Chrome 150: a `<link rel=stylesheet crossorigin>` from another origin reads 200 and "text/css"; korvus read 0 and
/// "").
pub fn parser_sheet_arrived(
    scope: &mut v8::PinScope,
    id: u32,
    url: &str,
    fetched: Option<(u16, Vec<(String, String)>, String, String)>,
    timing: Option<spec::FetchTiming>,
    asked_ms: f64,
    cors: bool,
) {
    let heard = crate::core::timing::performance::clock_now_ms(scope);
    crate::core::timing::resource_timing::add(
        scope,
        crate::core::timing::resource_timing::ResourceLoad {
            name: timing.as_ref().and_then(|t| t.url_list.first().cloned()).unwrap_or_else(|| url.to_string()),
            initiator_type: "link".to_string(),
            render_blocking: true,
            timing,
            asked_ms,
            heard_ms: heard,
            readable: cors.then_some(true),
        },
    );
    crate::core::html::subresource::parser_sheet_arrived(scope, id, fetched);
}

/// A parser-inserted element that blocks rendering until it has loaded or run: `blocking=render`, met while the document
/// had no body -- one in `<head>` (HTML "potentially render-blocking", added while the document "allows adding
/// render-blocking elements").
pub fn parser_render_blocking(scope: &mut v8::PinScope, el: v8::Local<v8::Value>) -> bool {
    let Some(n) = crate::binding::protos::unwrap_id(scope, el) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return false };
    let dom = &h.0;
    if !dom.get_attribute(n, "blocking").is_some_and(|b| b.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("render"))) {
        return false;
    }
    let mut at = dom.parent(n);
    while let Some(a) = at {
        if dom.tag_name(a).is_some_and(|t| t.eq_ignore_ascii_case("head")) {
            return true;
        }
        at = dom.parent(a);
    }
    false
}

/// Is the node `a` before the element `el` in tree order?
pub fn precedes_element(scope: &mut v8::PinScope, a: u32, el: v8::Local<v8::Value>) -> bool {
    let Some(b) = crate::binding::protos::unwrap_id(scope, el) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return false };
    crate::core::dom::shared::cmp_points(&h.0, a, 0, b, 0) < 0
}

/// A `document.write` left a PARSER-BLOCKING `<script src>`: its element and resolved URL,
/// for the driver to fetch -- with the parser's own request shape -- and run before the parse goes on.
/// `None` when nothing is waiting.
pub fn take_written_blocking<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<(v8::Local<'s, v8::Value>, String)> {
    crate::core::dom::document::take_written_blocking(scope)
}

/// After the script [`take_written_blocking`] handed over has run: insert the written input it held
/// back, which may leave the next blocking script for the driver to take.
pub fn flush_written_input(scope: &mut v8::PinScope) {
    crate::core::dom::document::flush_written_input(scope)
}

/// A new document: forget the previous one's pending written input.
pub fn reset_written_input(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) {
    crate::core::dom::document::reset_written_input(op_state)
}

/// Forget which (element, URL) loads this realm has already started, because a new document's arena
/// reuses the node ids the old one's elements had. See
/// [`crate::core::html::subresource::reset_started_loads`] for the measurement that found it.
pub fn reset_started_loads(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) {
    crate::core::html::subresource::reset_started_loads(op_state);
    crate::core::css::sheet_loads::reset(op_state);
    // The deferred `loading=lazy` set and the favicon handler are per document too.
    crate::core::html::lazy_load::reset(op_state);
    crate::core::html::link_icons::reset(op_state);
}

/// The page's next document is the image document of `url`, built from the navigation's `bytes`: its `<img>` takes
/// them and asks the network for nothing. Called before the document is loaded, and only for one.
pub fn arm_page_image_document(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, url: &str, headers: &[(String, String)], bytes: &[u8]) {
    crate::core::html::subresource::arm_page_image_document(op_state, url, headers, bytes)
}

/// The page document's `load` event has just been dispatched (HTML "the end"). What a browser does
/// only once that is true starts here: the first look at the viewport for `loading=lazy`
/// elements, and the favicon handler's candidate list (Blink `LocalFrame::UpdateFaviconURL` returns
/// early until `LoadEventFinished`).
pub fn load_event_finished(scope: &mut v8::PinScope) {
    crate::core::html::lazy_load::document_loaded(scope);
    // a navigation the page starts from now on is no longer made replacing for coming before `load` finished
    crate::core::html::navigate_event::document_loaded(scope);
    // `document.fonts.ready` first resolves on the task after the load event (`FontFaceSetDocument::DidLayout`)
    crate::core::fonts::font_face::page_load_finished(scope);
    // Only the page's own document has icons (`link_icons`' rulings: `LocalFrame::UpdateFaviconURL` ignores a
    // non-main frame, and the installability check is the main frame's) -- and a remote frame's document is not
    // the page's, though it is this engine's window when the engine is a child (`FrameEmbedding`). Stock Chrome 150
    // asked for no icon of a bot-challenge frame (NetLog), where the out-of-process frame here fetched
    // `a challenge host/favicon.ico`. The whole handover is skipped: the
    // checkpoint it arms and the manifest's icon serve the icons alone.
    let nested = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<FrameEmbedding>().is_some()
    };
    if !nested {
        crate::core::html::link_icons::document_loaded(scope);
    }
}

/// The Web Storage areas a realm holds, taken out of it so the document that REPLACES it can hold
/// them. A cross-document navigation is a new realm -- a new isolate here -- on the same
/// browsing session, and `localStorage` / `sessionStorage` belong to the session, not to the realm.
pub struct CarriedStorage(crate::modules::storage::storage::HostStorage);

/// HOW THE PAGE'S DOCUMENT WAS REACHED: what `document.referrer`, the navigation timing
/// entry's `type`, `performance.navigation.type` and `navigation.activation.navigationType` answer.
/// The driver hands it to the realm before the document loads; a realm it was never handed (the
/// driver's own first URL) is a typed navigation -- no referrer, `navigate`.
///
/// Measured on Chrome 150 (2026-09-30, `tools/realweb/seed/navhdr`): `document.referrer` is the
/// `Referer` the request carried, and a reload reports `reload` / `1` / `reload` where every other
/// navigation reports `navigate` / `0`.
///
/// `history` and `transition` are where the document enters the session history and how Chrome names the way it
/// was reached (D88 step 2): `Traverse` reports `back_forward` / `2` / `traverse`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Arrival {
    pub referrer: String,
    pub reload: bool,
    pub history: crate::core::html::session_history::HistoryCommit,
    pub transition: String,
}

/// Hand the realm the way its document was reached; see [`Arrival`].
pub fn set_arrival(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, a: Arrival) {
    op_state.borrow_mut().put(a);
}

/// The way this realm's page document was reached; see [`Arrival`].
pub fn arrival(scope: &mut v8::PinScope) -> Arrival {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<Arrival>().cloned().unwrap_or_default()
}

/// Take this realm's storage areas (leaving it empty); see [`CarriedStorage`].
pub fn take_storage(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) -> CarriedStorage {
    let mut st = op_state.borrow_mut();
    let taken = st.try_take::<crate::modules::storage::storage::HostStorage>().unwrap_or_default();
    st.put(crate::modules::storage::storage::HostStorage::default());
    CarriedStorage(taken)
}

/// Hand a successor realm the storage areas its predecessor held.
pub fn restore_storage(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, carried: CarriedStorage) {
    op_state.borrow_mut().put(carried.0);
}

/// Mark the page's document hidden: the visibility step of HTML's "unload a document", which the
/// runtime runs just before it fires `visibilitychange` on a document it is leaving.
pub fn set_page_hidden(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) {
    op_state.borrow_mut().put(crate::core::dom::document::PageHidden(true));
}

/// HTML's "update the current document readiness". `notify` fires `readystatechange`; a document is
/// CREATED with readiness `loading`, which is not an update and must not fire.
pub fn doc_readiness(scope: &mut v8::PinScope, state: &str, notify: bool) {
    use crate::core::html::browsing_context::{set_doc_readiness, Readiness};
    let Some(to) = Readiness::parse(state) else { return };
    set_doc_readiness(scope, to, notify);
}

/// Park `f` until nothing delays the document's load event, or until `cap_ms` elapses — whichever
/// comes first. The cap is a HOST deadline, enforced from outside the page's task queue.
pub fn when_load_delays_done(scope: &mut v8::PinScope, f: v8::Local<v8::Function>, cap_ms: i32) {
    crate::core::html::browsing_context::when_load_delays_done(scope, f, cap_ms);
}

/// One `on<type>` CONTENT attribute of a `<body>`, compiled and forwarded to the Window.
pub fn body_handler_attr(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, name: &str) {
    crate::core::html::html_body_element::body_handler_attr(scope, el, name);
}

/// The policy-relevant response headers a document was DELIVERED with, in wire order (repeats
/// kept). Resolved into the kernel's isolation-policy handle at every navigation.
pub fn set_document_policy(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    crate::core::html::isolation_policy::set_document_policy(scope, headers);
}

/// A NEW DOCUMENT is going into this runtime's arena: forget every table keyed by the OLD document's
/// node ids. A freshly parsed `Dom` numbers its nodes from zero again, so a wrapper, an owner document
/// or a qualified name recorded for id N in the previous document answered for whatever node is id N
/// now -- measured: after the quality harness reloaded its page, `createElement("input")` came back
/// wearing `HTMLOptionElement.prototype` (its id had been a `createElementNS` `<option>`'s), and 589
/// members graded wrong. `LayoutCache` and `NaturalImages` were already reset per document for the
/// same reason; these three were not.
pub fn forget_node_tables(state: &deno_core::OpState) {
    if let Some(c) = state.try_borrow::<crate::binding::NodeCache>() {
        c.clear();
    }
    if let Some(o) = state.try_borrow::<crate::binding::OwnerMap>() {
        o.0.borrow_mut().clear();
    }
    if let Some(q) = state.try_borrow::<crate::binding::QNameMap>() {
        q.0.borrow_mut().clear();
    }
    // And every isolated world's -- the ids are the new document's from here on.
    crate::core::html::isolated_world::forget_nodes(state);
}

/// The same delivered headers, read for `Content-Security-Policy`. A separate call
/// from [`set_document_policy`] because the isolation resolution has a lazy re-entry of its own
/// (`isolation_policy::doc_policy`) that must not clear a document's CSP; this one runs exactly once
/// per navigation, from the runtime, next to it.
pub fn set_document_csp(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    crate::core::html::csp::set_document_csp(scope, headers);
}

/// Where an inline `<script>`'s text sits in the markup it was parsed from, as V8's `ScriptOrigin`
/// line and column offsets (`korvus_dom::Dom::script_position`): `(0, 0)` for a script no parser made.
/// An inline script compiled without it numbered its lines from 1, where Chrome numbers them from the
/// start of the document (measured 2026-10-05: an error on the script's 3rd line, its tag on line 4,
/// is line 6 in Chrome).
pub fn script_source_position(scope: &mut v8::PinScope, el: v8::Local<v8::Value>) -> (i32, i32) {
    let Some(id) = crate::binding::protos::unwrap_id(scope, el) else { return (0, 0) };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    // An EXTERNAL script's lines are its file's own, wherever its element sits.
    st.try_borrow::<crate::binding::HostDom>()
        .filter(|d| d.0.get_attribute(id, "src").is_none())
        .and_then(|d| d.0.script_position(id))
        .map(|(l, c)| (l as i32, c as i32))
        .unwrap_or((0, 0))
}

/// CSP `script-src` for a PARSER-INSERTED script element of the page: `inline` is its
/// source text, or `url` the resolved `src` of an external one.
pub fn csp_allows_parser_script(scope: &mut v8::PinScope, el: v8::Local<v8::Value>, inline: Option<&str>, url: Option<&str>) -> bool {
    let Some(id) = crate::binding::protos::unwrap_id(scope, el) else { return true };
    match (inline, url) {
        (Some(text), _) => crate::core::html::csp::allows_inline_script(scope, id, text, true),
        (None, Some(u)) => crate::core::html::csp::allows_script_url(scope, id, u, true),
        (None, None) => true,
    }
}

/// A new document's empty "delay the load event" set ([`crate::core::html::browsing_context::reset_load_delays`]), before
/// the host installs the document.
pub fn reset_load_delays(scope: &mut v8::PinScope) {
    crate::core::html::browsing_context::reset_load_delays(scope);
}

pub use crate::core::html::remote_frame::{ancestor_origins_list, set_pending_navigation_timing, FrameEmbedding};
/// A remote frame's document install -- start receiving its parent's messages.
pub fn install_frame_embedding(scope: &mut v8::PinScope) {
    crate::core::html::remote_frame::install_frame_embedding(scope);
}
/// A new document's clock (its navigation's start) and navigation timing, at install.
pub fn begin_document_timing(scope: &mut v8::PinScope, url: &str) {
    crate::core::timing::performance::begin_document_timing(scope, url);
}
/// The document reached a lifecycle milestone (`domInteractive`, `domContentLoadedEventStart`, ...).
/// The current realm's CSP list as delivering headers, for a worker that inherits it (`core/html/browsing_context.rs`).
pub fn worker_csp_headers(scope: &mut v8::PinScope) -> Vec<(String, String)> {
    crate::core::html::browsing_context::current_realm_csp_headers(scope)
}
pub fn note_milestone(scope: &mut v8::PinScope, name: &'static str) {
    crate::core::timing::performance::note_milestone(scope, name);
}
/// The page document's `Origin-Agent-Cluster` header (`window.originAgentCluster`).
pub fn set_document_origin_agent_cluster(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    crate::core::html::window::set_document_origin_agent_cluster(scope, headers);
}
/// The page document's delivered referrer policy (`Referrer-Policy`), replaced per navigation like the
/// CSP and the permissions policy (`core/html/referrer_policy.rs`).
pub fn set_document_referrer_policy(state: &mut deno_core::OpState, headers: &[(String, String)]) {
    crate::core::html::referrer_policy::set_document_referrer_policy(state, headers);
}
/// A freshly parsed document's referrer policy, before it is the realm's: its `<meta name=referrer>`,
/// else the delivered header -- for the parser's own `<script src>` pass. `el`'s own `referrerpolicy`
/// wins when given.
pub fn parsed_referrer_policy(dom: &korvus_dom::Dom, headers: &[(String, String)], el: Option<u32>) -> Option<transport::ReferrerPolicy> {
    el.and_then(|e| crate::core::html::referrer_policy::element_policy(dom, e))
        .or_else(|| crate::core::html::referrer_policy::meta_policy(dom, dom.root()))
        .or_else(|| transport::ReferrerPolicy::from_headers(headers))
}
/// The page document's referrer policy -- delivered, or a `<meta name=referrer>` -- for the runtime's
/// own request builders.
pub fn page_referrer_policy(state: &deno_core::OpState) -> Option<transport::ReferrerPolicy> {
    crate::core::html::referrer_policy::page_document_policy(state)
}

/// The same delivered headers, read for `Permissions-Policy` and the legacy `Feature-Policy`.
/// A document's permissions policy is a property of its RESPONSE, so it
/// arrives here with the response or not at all; a navigation with no such header leaves the
/// registry's defaults in place, which is what a document delivered with none has.
pub fn set_document_permissions_policy(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    crate::core::feature_policy::set_document_permissions_policy(scope, headers);
}

/// `document.currentScript` while the page driver runs a script; `undefined` clears it.
///
/// THESE TWO FUNCTIONS USED TO BE `bridge_get`/`bridge_set` -- one untyped pair over a name, which is
/// how the page realm ended up holding four pieces of engine state under `KB.__*` names. A later change
/// deleted the pair and named what the pipeline actually needs: this, and nothing else. The store is a
/// V8 Private on the global (`core/html/html_script_element.rs`), so a page can neither read the
/// engine's idea of the running script nor overwrite it.
pub fn set_current_script(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) {
    crate::core::html::html_script_element::set_current_script(scope, v);
}

/// While the page load waits for a render-blocking resource, no frame runs ([`crate::core::intersection_observer::rendering::RenderBlocked`]).
pub fn set_render_blocked(st: &mut deno_core::OpState, on: bool) {
    use crate::core::intersection_observer::rendering::RenderBlocked;
    if on {
        st.put(RenderBlocked);
    } else {
        st.try_take::<RenderBlocked>();
    }
}

pub use crate::core::intersection_observer::rendering::render_blocked;

/// The parse's end as a task of its own: Blink's parser, once it has waited for a script, ends in a
/// task it posts then (`HTMLDocumentParser::ScheduleEndIfDelayed`), so the tasks already queued -- a timer, a message,
/// the notification of a rejected promise -- run before `readystatechange: interactive`. Chrome 150, measured:
/// `unhandledrejection` before `interactive` when the parser waited for two `<script src>`; a
/// parse that never waited ends in the task that parsed it.
struct ParseEndTurn;

pub fn queue_parse_end(scope: &mut v8::PinScope) {
    crate::core::html::task_queue::queue_host_step(scope, |scope| {
        deno_core::JsRuntime::op_state_from(scope).borrow_mut().put(ParseEndTurn);
    });
}

/// The task [`queue_parse_end`] queued has run (and is forgotten).
pub fn take_parse_end(st: &mut deno_core::OpState) -> bool {
    st.try_take::<ParseEndTurn>().is_some()
}

/// HTML "clean up after running script": the microtask checkpoint a classic script performs when it
/// ends, WHILE IT IS STILL `document.currentScript`.
///
/// One definition, two callers, for the same reason `set_current_script` has two: this crate runs the
/// scripts a page INSERTS and `korvus-runtime` runs the ones the parser found, and a checkpoint that
/// happened in one path and not the other would make `document.currentScript` depend on how the
/// script reached the document. The rule and the Chrome table are in
/// `core/html/html_script_element.rs`.
pub fn clean_up_after_running_script(scope: &mut v8::PinScope) {
    crate::core::html::html_script_element::clean_up_after_running_script(scope);
}

/// Which responses a `<script src>` executes. The rule, and the Chrome table it was
/// measured from, live in `core/html/html_script_element.rs`; the runtime's parser-inserted path and
/// its module loader ask it here, and the element loader asks it in-crate, so there is one rule.
pub use crate::core::html::html_script_element::{script_response_ok, ScriptFlavor};

/// The deferred module queue, and HTML's test for the module type.
///
/// The queue is declared in this crate because it has TWO producers in two crates -- the runtime's
/// page-script driver queues the `<script type=module>`s the parser found, and this crate's
/// inserted-script path queues the ones the page inserted -- and ONE consumer,
/// `Engine::evaluate_pending_modules`. The dependency edge points runtime -> webapi, so the shared
/// vocabulary lives at this end of it; the alternative was a second queue or a second evaluator.
pub use crate::core::html::html_script_element::{is_module_type, PendingModule, PendingModules};

/// Import maps. Declared in this crate for the same reason the module queue is: it has
/// TWO producers in two crates -- the runtime's page-script driver installs the maps the PARSER
/// found, this crate's inserted-script path installs the ones the PAGE inserted -- and one consumer,
/// the runtime's module loader, which has no `OpState` and no scope and so reads it through a shared
/// `Rc`.
pub use crate::core::html::import_map::{
    ImportMap, ImportMapParseError, ImportMapStore, ImportMaps, ResolveError as ImportMapResolveError,
};
pub use crate::core::html::html_script_element::{install_import_map, is_import_map_type};

/// HTML's `nomodule` step of "prepare the script element": a classic script carrying the
/// attribute is neither FETCHED nor run by a browser that supports module scripts. The rule, and the
/// Chrome request log it was measured from, live in `core/html/html_script_element.rs`; the runtime's
/// parser fetch pass and its page-script driver ask it here, and the element loader and the
/// inserted-script seam ask it in-crate, so all four paths skip exactly the same scripts.
pub use crate::core::html::html_script_element::{is_classic_type, mark_parser_scripts_started, nomodule_skips};
pub use crate::core::dom::element::attr_call;
/// The session history, owned here so `History` (in the kernel) and `navigation` (here) read one list.
pub use crate::core::html::session_history::{
    carry_history, note_same_document, restore_history, seed_blank_history, take_same_document, with_history, CarriedHistory, HistoryCommit,
    HistoryEntry, HistoryState,
};
/// The page document's `document.title`: a session-history entry's title, a tab's.
pub use crate::core::dom::document::page_title;

/// The page DOM's revision, which moves with every mutation: whether its title may have changed.
pub fn page_dom_revision(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) -> u64 {
    op_state.borrow().try_borrow::<crate::binding::HostDom>().map(|h| h.0.revision()).unwrap_or(0)
}

/// The task source a settled `<script src>` executes from.
///
/// HTML: *queue an element task on the networking task source given el to run these steps: execute
/// the script element.* The element loader parks each settled script and the HOST runs it -- from
/// Rust, between turns of the event loop, which is the one place in this engine where the JavaScript
/// execution context stack is empty and a microtask checkpoint is therefore a real one. The queue and
/// the Chrome table are in `core/html/subresource.rs`; `Engine::drain_load_tasks` is the consumer, and
/// it asks [`settled_scripts_pending`] so a `run()` never returns with a script still parked.
pub use crate::core::html::subresource::{run_settled_scripts, settled_scripts_pending, SettledScripts};

/// Release a "delay the load event" token. A module script's fetch takes one, and the
/// module is not finished when its bytes arrive -- it is finished when the drain has evaluated it, and
/// the drain is the runtime's. Chrome 150, measured: a 600 ms module inserted from script produces
/// `ran:module`, the element's `load`, and then the document's `load`.
pub fn undelay_load(scope: &mut v8::PinScope, token: u32) {
    crate::core::html::browsing_context::undelay_load(scope, token);
}

/// Is `essence` (lowercased, no parameters) a JavaScript MIME type? The one list behind both a
/// `<script type>` test and [`script_response_ok`].
pub fn is_javascript_mime_essence(essence: &str) -> bool {
    crate::core::html::html_script_element::is_javascript_mime_essence(essence)
}

/// Fire a trusted `load` or `error` at a `<script>` element the page driver fetched. Not bubbling, not
/// cancelable -- the event a browser fires when an external script finishes or fails.
/// Fire the page lifecycle's `ty` at `target`: a trusted event, through the native dispatch.
/// `legacy_target` is the Document for HTML's "legacy target override" fires at the Window.
pub fn fire_lifecycle_event(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, ty: &str, bubbles: bool, legacy_target: Option<v8::Local<v8::Value>>) {
    crate::core::html::browsing_context::fire_lifecycle(scope, target, ty, bubbles, legacy_target);
}

pub fn fire_script_event<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Value>, ty: &str) {
    crate::core::html::browsing_context::fire_at(scope, el, ty);
}

/// Read the page's arena.
/// The arena id behind a node wrapper (`None` for anything that is not one) -- for engine code that must
/// read a node natively rather than through its page-visible properties.
pub fn node_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    crate::binding::protos::unwrap_id(scope, v)
}
/// A node's CHILD TEXT CONTENT -- its child Text nodes' data, concatenated -- read off the arena.
/// Engine code that needs it (a script's source, an `<output>`'s value) must not walk `childNodes` /
/// `nodeType` / `data`: those are page-visible getters a page can wrap and count.
pub fn child_text_content(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> String {
    let Some(id) = node_id(scope, v) else { return String::new() };
    with_host_dom(scope, |d| d.children(id).into_iter().filter(|&c| d.is_text_node(c)).filter_map(|c| d.text_content_value(c)).collect::<String>())
        .unwrap_or_default()
}
/// An element's `tagName` exactly as the getter answers it, read off the arena (`None` for anything
/// that is not an element) -- for engine checks that must not call the page-visible getter.
pub fn tag_name_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let id = node_id(scope, v)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    if st.try_borrow::<crate::binding::host_dom::HostDom>()?.0.node_type(id) != 1 {
        return None;
    }
    crate::core::dom::element::element_display_name(&st, id)
}
/// An element's `localName`, read off the arena (`None` for anything that is not an element).
pub fn local_name(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let id = node_id(scope, v)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    crate::core::html::html_element::local_name_of(&op_state, id)
}
/// `Node.isConnected` for a node, read off the arena: its shadow-including root is a document.
pub fn is_connected(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Some(id) = node_id(scope, v) else { return false };
    with_host_dom(scope, |d| d.node_type(d.shadow_including_root(id)) == 9).unwrap_or(false)
}
/// A node's `nodeType`, read off the arena (`None` for anything that is not a node).
pub fn node_type_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let id = node_id(scope, v)?;
    with_host_dom(scope, |d| d.node_type(id))
}
pub fn with_host_dom<R>(scope: &mut v8::PinScope, f: impl FnOnce(&korvus_dom::Dom) -> R) -> Option<R> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<crate::binding::host_dom::HostDom>().map(|h| f(&h.0))
}

/// The element (or node) object of arena node `id`. The page's document root is the context's own `document` (the
/// same object a script there reads, an isolated world's included): an element wrapper minted over a root would be a
/// second, wrong-typed document.
pub fn node_object<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> v8::Local<'s, v8::Value> {
    if with_host_dom(scope, |dom| dom.root() == id).unwrap_or(false) {
        // the slot `window.document`'s getter answers first (`window_globals::window_singleton`), read in place: a
        // read through the accessor per call was 3.7% of a `dispatchEvent` loop
        if let Some(doc) = crate::core::html::window_globals::window_slot_get(scope, "document").filter(|d| d.is_object()) {
            return doc;
        }
        let global = scope.get_current_context().global(scope);
        if let Some(doc) = crate::binding::protos::key_str(scope, "document").and_then(|k| global.get(scope, k.into())).filter(|d| d.is_object()) {
            return doc;
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    crate::binding::protos::wrap_cached(scope, &op_state, id).into()
}

/// Bring the page document's `<template for>` patches up to the parser script `next` runs, or all the way
/// when the script pass is over (`None`) -- `korvus_dom::Dom::stream_template_patches`.
/// `false`: `next` belongs to a patch that could not open, and is not run.
pub fn stream_template_patches(scope: &mut v8::PinScope, next: Option<v8::Local<v8::Value>>) -> bool {
    let next_id = next.and_then(|el| crate::binding::protos::unwrap_id(scope, el));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    match st.try_borrow_mut::<crate::binding::host_dom::HostDom>() {
        Some(h) => h.0.stream_template_patches(next_id),
        None => true,
    }
}

/// HTML's "shared declarative refresh steps", up to the point of scheduling: `(delay seconds, url)`,
/// or `None` when `input` is not a refresh at all. Relative URLs resolve against `base`; a bare delay
/// refreshes `document_url`.
pub fn parse_declarative_refresh(input: &str, base: &str, document_url: &str) -> Option<(f64, String)> {
    let ws = |c: char| matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ');
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut p = 0usize;
    while p < n && ws(chars[p]) {
        p += 1;
    }
    let start = p;
    while p < n && chars[p].is_ascii_digit() {
        p += 1;
    }
    let digits: String = chars[start..p].iter().collect();
    if digits.is_empty() && chars.get(p) != Some(&'.') {
        return None;
    }
    // "0.5" is 0: the fraction is collected and ignored (Chrome reloads a `content="0.5"` page as fast
    // as it can, measured).
    let time: f64 = if digits.is_empty() { 0.0 } else { digits.parse::<f64>().unwrap_or(f64::MAX) };
    while p < n && (chars[p].is_ascii_digit() || chars[p] == '.') {
        p += 1;
    }
    if p < n {
        if !(chars[p] == ';' || chars[p] == ',' || ws(chars[p])) {
            return None;
        }
        while p < n && ws(chars[p]) {
            p += 1;
        }
        if p < n && (chars[p] == ';' || chars[p] == ',') {
            p += 1;
        }
        while p < n && ws(chars[p]) {
            p += 1;
        }
    }
    if p >= n {
        return Some((time, document_url.to_string()));
    }
    let is = |q: usize, c: char| chars.get(q).is_some_and(|x| x.eq_ignore_ascii_case(&c));
    let skip_quotes = |q: usize| -> String {
        let mut q = q;
        let quote = match chars.get(q) {
            Some(&c) if c == '"' || c == '\'' => {
                q += 1;
                Some(c)
            }
            _ => None,
        };
        let mut s: String = chars[q.min(n)..].iter().collect();
        if let Some(qc) = quote {
            if let Some(i) = s.find(qc) {
                s.truncate(i);
            }
        }
        s
    };
    // "Let urlString be the substring of input from position": each failed check below jumps to
    // "parse" with it unchanged, except a missing `U`, which goes to "skip quotes".
    let mut url_string: String = chars[p..].iter().collect();
    'parse: {
        let mut q = p;
        if !is(q, 'u') {
            url_string = skip_quotes(q);
            break 'parse;
        }
        q += 1;
        if !is(q, 'r') {
            break 'parse;
        }
        q += 1;
        if !is(q, 'l') {
            break 'parse;
        }
        q += 1;
        while q < n && ws(chars[q]) {
            q += 1;
        }
        if chars.get(q) != Some(&'=') {
            break 'parse;
        }
        q += 1;
        while q < n && ws(chars[q]) {
            q += 1;
        }
        url_string = skip_quotes(q);
    }
    let parsed = match url::Url::parse(base) {
        Ok(b) => b.join(url_string.trim()),
        Err(_) => url::Url::parse(url_string.trim()),
    };
    Some((time, parsed.ok()?.to_string()))
}

/// The arena node an element object wraps, if it wraps one (CDP `DOM.describeNode {objectId}`,
/// `DOM.getContentQuads {objectId}`).
pub fn node_id_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    if let Some(id) = crate::binding::protos::unwrap_id(scope, v) {
        return Some(id);
    }
    // A Document is a node too -- its tree's root (Chrome 150 names `document` `subtype: "node"`, `#document`, and
    // Puppeteer makes its document handle an ELEMENT handle only from that). The page's document names its tree by
    // `PAGE_DOC_ROOT`.
    let slot = deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, v).map(|p| unsafe { p.as_ref() }.root)?;
    with_host_dom(scope, |dom| crate::core::dom::document::resolve_doc_root(dom, slot))
}

/// Element `id`'s border box in the viewport, CSS px `(x, y, width, height)` -- `getBoundingClientRect`'s
/// numbers, measured by the engine (CDP `DOM.getContentQuads`, `DOM.getBoxModel`).
pub fn node_viewport_rect(scope: &mut v8::PinScope, id: u32) -> (f64, f64, f64, f64) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    crate::core::dom::geometry_queries::element_viewport_rect(&mut st, id)
}

/// Element `id`'s border box in the TOP-LEVEL viewport, CSS px: [`node_viewport_rect`] in its own document, moved by
/// the content-box origin of every frame element on the way up -- what Chrome's `DOM.getContentQuads` answers for a
/// node in a same-process frame (a client clicks there through the page).
pub fn node_page_rect(scope: &mut v8::PinScope, id: u32) -> (f64, f64, f64, f64) {
    let (mut x, mut y, w, h) = node_viewport_rect(scope, id);
    let mut node = id;
    for _ in 0..16 {
        let el = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let Some(h) = st.try_borrow::<crate::binding::host_dom::HostDom>() else { break };
            let root = h.0.shadow_including_root(node);
            if root == h.0.root() {
                break;
            }
            crate::core::html::browsing_context::frame_element_showing(&st, root)
        };
        let Some(el) = el else { break };
        let Some((ox, oy)) = crate::core::events::pointer_input::frame_element_origin(scope, el) else { break };
        x += ox;
        y += oy;
        node = el;
    }
    (x, y, w, h)
}

/// CDP's `DOM.scrollIntoViewIfNeeded`: scroll node `id` -- or the part `rect` of it, `(x, y, width, height)` in its
/// border box -- into view, centred on each axis where it is not already wholly visible, as Chrome's
/// `ScrollAlignment::CenterIfNeeded`. Playwright's click scrolls its target this way before it hit-tests. A node in a
/// frame is scrolled into its frame's view, and each frame element on the way up into its own document's.
pub fn scroll_node_into_view_if_needed(scope: &mut v8::PinScope, id: u32, rect: Option<(f64, f64, f64, f64)>) {
    use crate::core::dom::element::Align;
    crate::core::dom::element::scroll_into_view(scope, id, Align::CenterIfNeeded, Align::CenterIfNeeded, false, rect);
    let mut node = id;
    for _ in 0..16 {
        let el = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let Some(h) = st.try_borrow::<crate::binding::host_dom::HostDom>() else { break };
            let root = h.0.shadow_including_root(node);
            if root == h.0.root() {
                break;
            }
            crate::core::html::browsing_context::frame_element_showing(&st, root)
        };
        let Some(el) = el else { break };
        crate::core::dom::element::scroll_into_view(scope, el, Align::CenterIfNeeded, Align::CenterIfNeeded, false, None);
        node = el;
    }
}

/// An element as CDP's `DOM.Node` describes it.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// `Node.nodeType`: 1 for an element, 9 for a document.
    pub node_type: u32,
    /// `Element.tagName` (`BUTTON`; case kept for foreign elements); `#document` for a document.
    pub node_name: String,
    pub local_name: String,
    pub child_count: usize,
    /// Name/value pairs in document order.
    pub attributes: Vec<(String, String)>,
    /// Chrome's `RemoteObject.description` of the node: `localName`, then `#id`, then `.class` for each class
    /// (`input#q`, `div.a.b`).
    pub description: String,
}

/// The children DevTools shows of node `id`: all but the text nodes that are only white space, which Chrome's DOM agent
/// leaves out of `children` and of `childNodeCount` alike (`InspectorDOMAgent::ShouldSkipNode`; Chrome 150's `<head>`
/// with a `<title>`, a line break and a `<style>` counts 2).
pub fn devtools_children(dom: &korvus_dom::Dom, id: u32) -> Vec<u32> {
    dom.children(id).into_iter().filter(|&c| !(dom.node_type(c) == 3 && dom.node_value(c).is_some_and(|t| t.chars().all(|ch| ch.is_ascii_whitespace())))).collect()
}

/// Element `id`, described from the arena (`None` for a node that is not an element).
pub fn node_info(scope: &mut v8::PinScope, id: u32) -> Option<NodeInfo> {
    with_host_dom(scope, |dom| {
        if (id as usize) < dom.len() && dom.node_type(id) == 9 {
            let child_count = devtools_children(dom, id).len();
            return Some(NodeInfo {
                node_type: 9,
                node_name: "#document".into(),
                local_name: String::new(),
                child_count,
                attributes: Vec::new(),
                description: "#document".into(),
            });
        }
        let local_name = dom.tag_name(id)?.to_string();
        let node_name = dom.element_tag_name(id)?;
        let attributes = dom.attribute_pairs(id).to_vec();
        let mut description = local_name.clone();
        if let Some(i) = attributes.iter().find(|(k, _)| k == "id").map(|(_, v)| v).filter(|v| !v.is_empty()) {
            description.push('#');
            description.push_str(i);
        }
        if let Some(c) = attributes.iter().find(|(k, _)| k == "class").map(|(_, v)| v) {
            for class in c.split_ascii_whitespace() {
                description.push('.');
                description.push_str(class);
            }
        }
        Some(NodeInfo { node_type: 1, node_name, local_name, child_count: devtools_children(dom, id).len(), attributes, description })
    })
    .flatten()
}

/// A node and what is under it, as CDP's `DOM.Node` lists them for `DOM.getDocument` / `DOM.describeNode` with a
/// `depth`: Stagehand reads the whole tree this way (`depth: -1, pierce: true`) to find each element's XPath.
#[derive(Debug, Clone)]
pub struct DomTreeNode {
    pub id: u32,
    pub node_type: u32,
    pub node_name: String,
    pub local_name: String,
    pub node_value: String,
    /// `childNodeCount`, for a node that can have children.
    pub child_count: Option<usize>,
    /// Name/value pairs in document order, for an element.
    pub attributes: Option<Vec<(String, String)>>,
    /// `None` past the depth asked for.
    pub children: Option<Vec<DomTreeNode>>,
    /// An element's shadow root; `shadow_root_type` on the root itself (`open` / `closed`).
    pub shadow_roots: Vec<DomTreeNode>,
    pub shadow_root_type: Option<&'static str>,
    /// A frame element's frame (its browsing context id) and the document it shows.
    pub content_frame: Option<u32>,
    pub content_document: Option<Box<DomTreeNode>>,
    /// A `<template>`'s content.
    pub template_content: Option<Box<DomTreeNode>>,
    /// A document element: the frame its document is the document of (`Some(None)` the page's) -- Chrome puts the
    /// frame's id on it.
    pub document_of: Option<Option<u32>>,
    /// A document's url, and its `compatibilityMode` (`NoQuirksMode`, `QuirksMode`, `LimitedQuirksMode`).
    pub document_url: Option<String>,
    pub compatibility_mode: Option<&'static str>,
    /// A doctype's `publicId` and `systemId`.
    pub doctype_ids: Option<(String, String)>,
}

/// Node `id` with its children to `depth` levels (`-1`: all of them), as Chrome's `InspectorDOMAgent` builds them: a
/// node within the depth lists its children (`[]` for none), an element's shadow root and a frame element's document
/// are listed always -- their contents only with `pierce` -- and a template's content always, with nothing under it.
/// `document_push` is `DOM.getDocument`'s rule on top: a node at the last level still lists its children when its only
/// child is text (`describeNode` has no such rule: Chrome 150's `#go` button, one text child, lists none). `page_url`
/// is the page document's url (a frame's comes with the frame). `None` for an id the arena never issued.
pub fn dom_tree(scope: &mut v8::PinScope, id: u32, depth: i64, pierce: bool, document_push: bool, page_url: &str) -> Option<DomTreeNode> {
    let frames = crate::core::html::browsing_context::devtools_frames(scope);
    with_host_dom(scope, |dom| {
        if (id as usize) >= dom.len() {
            return None;
        }
        let depth = if depth < 0 { i64::MAX } else { depth };
        Some(dom_tree_node(dom, &frames, id, depth, pierce, document_push, page_url))
    })
    .flatten()
}

/// The element at client point `(x, y)` of the page's document: `document.elementFromPoint`'s hit test (CDP
/// `DOM.getNodeForLocation`). `None` outside the viewport, or over nothing.
pub fn element_at_point(scope: &mut v8::PinScope, x: f64, y: f64) -> Option<u32> {
    let root = with_host_dom(scope, |dom| dom.root())?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = crate::core::dom::geometry_queries::element_from_point(&mut op_state.borrow_mut(), root, x as f32, y as f32);
    u32::try_from(id).ok()
}

/// The elements under node `root` that `selector` matches, in tree order (only the first unless `all`), by the arena's
/// own selector engine -- never the page's `querySelector`. `Some(Err(()))` for a selector that does not parse.
pub fn query_selector(scope: &mut v8::PinScope, root: u32, selector: &str, all: bool) -> Option<Result<Vec<u32>, ()>> {
    with_host_dom(scope, |dom| {
        if (root as usize) >= dom.len() {
            return None;
        }
        Some(if all {
            dom.query_selector_all_in(root, selector).map_err(|_| ())
        } else {
            dom.query_selector_in(root, selector).map(|n| n.into_iter().collect()).map_err(|_| ())
        })
    })
    .flatten()
}

#[allow(clippy::too_many_arguments)]
fn dom_tree_node(dom: &korvus_dom::Dom, frames: &[crate::DevtoolsFrame], id: u32, depth: i64, pierce: bool, push: bool, page_url: &str) -> DomTreeNode {
    // the arena keeps a shadow root as a document-kind node; DevTools sees the `DocumentFragment` it is
    let node_type = if dom.is_shadow_root(id) { 11 } else { dom.node_type(id) };
    let mut n = DomTreeNode {
        id,
        node_type,
        node_name: String::new(),
        local_name: String::new(),
        node_value: String::new(),
        child_count: None,
        attributes: None,
        children: None,
        shadow_roots: Vec::new(),
        shadow_root_type: None,
        content_frame: None,
        content_document: None,
        template_content: None,
        document_of: None,
        document_url: None,
        compatibility_mode: None,
        doctype_ids: None,
    };
    match node_type {
        1 => {
            n.node_name = dom.element_tag_name(id).unwrap_or_default();
            n.local_name = dom.tag_name(id).unwrap_or("").to_string();
            n.attributes = Some(dom.attribute_pairs(id).to_vec());
            if let Some(sr) = dom.shadow_root_of(id) {
                n.shadow_roots.push(dom_tree_node(dom, frames, sr, if pierce { depth } else { 0 }, pierce, push, page_url));
            }
            if let Some(f) = frames.iter().find(|f| f.element == id) {
                n.content_frame = Some(f.id);
                if f.document != u32::MAX && (f.document as usize) < dom.len() {
                    let mut doc = dom_tree_node(dom, frames, f.document, if pierce { depth } else { 0 }, pierce, push, &f.url);
                    doc.document_url = Some(f.url.clone());
                    n.content_document = Some(Box::new(doc));
                }
            }
            if let Some(content) = dom.template_content_of(id) {
                n.template_content = Some(Box::new(dom_tree_node(dom, frames, content, 0, pierce, push, page_url)));
            }
            if dom.parent(id).is_some_and(|p| dom.node_type(p) == 9) {
                let doc = dom.parent(id).unwrap_or(id);
                n.document_of = Some(if doc == dom.root() { None } else { frames.iter().find(|f| f.document == doc).map(|f| f.id) });
            }
        }
        3 | 4 | 8 | 7 => {
            n.node_name = match node_type {
                3 => "#text".to_string(),
                4 => "#cdata-section".to_string(),
                8 => "#comment".to_string(),
                _ => dom.pi_fields(id).0,
            };
            n.node_value = dom.node_value(id).unwrap_or_default();
        }
        9 => {
            n.node_name = "#document".to_string();
            n.document_url = Some(page_url.to_string());
            n.compatibility_mode = Some(match dom.document_mode(id) {
                korvus_dom::DocumentMode::Quirks => "QuirksMode",
                korvus_dom::DocumentMode::LimitedQuirks => "LimitedQuirksMode",
                korvus_dom::DocumentMode::NoQuirks => "NoQuirksMode",
            });
        }
        10 => {
            let (name, public_id, system_id) = dom.doctype_fields(id);
            n.node_name = name;
            n.doctype_ids = Some((public_id, system_id));
        }
        11 => {
            n.node_name = "#document-fragment".to_string();
            n.shadow_root_type = dom.shadow_mode(id).map(|m| match m {
                korvus_dom::ShadowRootMode::Open => "open",
                korvus_dom::ShadowRootMode::Closed => "closed",
            });
        }
        _ => {}
    }
    if matches!(node_type, 1 | 9 | 11) {
        let kids = devtools_children(dom, id);
        n.child_count = Some(kids.len());
        let only_text = push && kids.len() == 1 && dom.node_type(kids[0]) == 3;
        if depth > 0 || only_text {
            let below = if depth > 0 { depth - 1 } else { 0 };
            n.children = Some(kids.into_iter().map(|c| dom_tree_node(dom, frames, c, below, pierce, push, page_url)).collect());
        }
    }
    n
}
