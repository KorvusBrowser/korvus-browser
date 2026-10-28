//! The navigation pipeline — **in Rust**.
//!
//! A navigation runs a fixed sequence between the parse and the page being loaded, and every step of
//! it used to be a raw JS string in `lib.rs` that `execute_script`'d itself into the page's own realm:
//! `INSTALL_DOCUMENT`, `START_PARSED_SUBRESOURCES`, `ACTIVATE_BODY_HANDLERS`, `LIFECYCLE_EVENTS`, plus
//! two one-line `format!` evals for the readiness transitions. Together they were ~200 lines of
//! JavaScript defining what the engine does, living inside the kernel — which is exactly what
//! invariant I9 says the kernel must not contain.
//!
//! # What was actually wrong with the strings
//!
//! Not style. Four concrete defects, each of which this file removes by construction:
//!
//! 1. **Every call was a name lookup that could not fail loudly.** `KB.__ops && typeof
//!    KB.__ops.op_sync_frames === "function"` inside a `try {} catch (e) {}` means a renamed or
//!    un-registered op is a silent no-op forever. That is not hypothetical: a migration found
//!    `__finalizeSurface` throwing `ReferenceError` into exactly such a `catch`, and the surface
//!    passes had silently not re-run for weeks. A Rust call to `webapi::page::sync_frames` does not
//!    compile if the function moves.
//! 2. **The pipeline ran through the PAGE's global.** A page that redefines `Event`, freezes
//!    `Object.prototype`, or replaces `Array.from` changed what the engine's own navigation did. The
//!    remaining reach into the realm here is deliberate and named in each function.
//! 3. **It required the ops to be reachable from JS**, which is the whole reason `Deno.core.ops` is
//!    stashed on the hidden bridge after `Deno` is deleted. That stash is a page-visible-if-found
//!    object holding the engine's entire op table.
//! 4. **The driver's own throw cost the document.** Each string was one IIFE; a bug in the driver
//!    unwound the whole thing and nothing reported it.
//!
//! # What did NOT change
//!
//! The ORDER, and the two numbers in it. Every position below was measured rather than chosen (the
//! per-function comments say by which unit), and this unit moved none of them:
//!
//! ```text
//! parse → install_document → [injected scripts] → start_parsed_subresources →
//! activate_body_handlers → run_page_scripts → readiness `interactive` →
//! deferred modules → lifecycle_events (DOMContentLoaded, then load behind the delay set)
//! ```
//!
//! The 1000 ms load-event cap is still written in exactly one place ([`LOAD_EVENT_CAP_MS`]), and it
//! is still a HOST deadline rather than a page timer — the finding, which no rewrite may
//! quietly undo.

use deno_core::v8;
use webapi::binding::protos::{build_fn, call_method, get_prop, str_val};

use crate::script_exec::{self, script_elements, script_src, script_type_runs};
use crate::{HostLocation, HostPersona, PendingModule, PendingModules};

/// How long the document's `load` event may be delayed before the host fires it anyway
/// (W9.16). **The only place this number is written.**
///
/// It exists so that delaying can only ever DELAY the load event and never withhold it: a frame whose
/// fetch never settles releases nothing, and a document that waits forever turns a file that would
/// have failed into a file with no verdict at all. 1000 ms sits below the conformance runner's 2500 ms
/// pump budget (`core/korvus-runtime/tests/wpt.rs` `PUMP_BUDGET_MS`) with enough left for the page's
/// handler to run and the harness to publish, and far below the 8 s watchdog; on the loopback server
/// the real releases land in single-digit milliseconds.
///
/// It is enforced by `Engine::pump` and a V8 interrupt, NOT by `setTimeout`. It used to be a page
/// timer, and a page that monopolises the event loop starved the cap along with everything else:
/// rakuten.co.jp reads `document.readyState` over a million times in four seconds waiting for
/// `complete`, and a 3000 ms timer registered before its scripts ran never fired at all, so no cap
/// value (1000 / 500 / 300 / 150 were tried) ever reached the queue and the document never loaded.
pub(crate) const LOAD_EVENT_CAP_MS: i32 = 1000;

/// `globalThis`, as an object.
pub(crate) fn global<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    scope.get_current_context().global(scope)
}

/// `globalThis.document`, or `None` before one is installed.
pub(crate) fn document<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let g = global(scope);
    get_prop(scope, g, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// Run `f`, swallowing anything it throws — the Rust spelling of the `try {} catch (e) {}` every one
/// of these drivers wrapped each of its steps in.
///
/// Swallowing is correct HERE and nowhere else in the pipeline: these are best-effort wiring steps
/// over a page whose own script may already have broken `Event` or frozen a prototype, and a throw
/// from one of them must not cost the document the rest of its navigation. A step whose failure a
/// caller can act on returns a value instead.
pub(crate) fn quietly(scope: &mut v8::PinScope, f: impl FnOnce(&mut v8::PinScope)) {
    v8::tc_scope!(let tc, scope);
    f(tc);
    tc.reset();
}

/// `new Event(type)` dispatched at `target` — the drivers' `fire()`.
///
/// It goes through the page's own `Event` constructor and `dispatchEvent` deliberately: those are the
/// members a listener's `event.constructor` and `event.target` are compared against, and minting the
/// event from a second path would make the engine's own lifecycle events distinguishable from a
/// page's. Both are native table rows.
fn fire(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, ty: &str) {
    fire_with(scope, target, ty, false);
}

/// [`fire`], with the event's `bubbles` flag. `DOMContentLoaded` and `visibilitychange` bubble -- HTML
/// fires both "with its bubbles attribute initialized to true" -- and a window listener for either is
/// only reached because they do (`window.addEventListener("DOMContentLoaded", …)` never
/// ran here; Chrome 150 runs it).
fn fire_with(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, ty: &str, bubbles: bool) {
    quietly(scope, |scope| {
        // TRUSTED, and at the Window with the Document as `target` for the four HTML fires with the
        // "legacy target override" flag (load, pageshow, pagehide, unload). These were untrusted
        // `new Event`s, so `addEventListener("load", e => e.isTrusted)` read false, and
        // `e.target` was the window where Chrome reports the document.
        let g: v8::Local<v8::Value> = global(scope).into();
        let legacy = target.strict_equals(g) && matches!(ty, "load" | "pageshow" | "pagehide" | "unload");
        let doc = if legacy { document(scope).map(|d| -> v8::Local<v8::Value> { d.into() }) } else { None };
        webapi::page::fire_lifecycle_event(scope, target, ty, bubbles, doc);
    });
}

/// Install `globalThis.document` for a freshly parsed tree, and finish the two things that can only
/// be done once a document exists.
///
/// Was `INSTALL_DOCUMENT`. Called from `Engine::install_document`, which then runs the surface
/// finalize passes and `crate::singletons::wire_document_identity` — that order is W5.1 L15's and L18's and
/// is unchanged.
pub(crate) fn install_document(scope: &mut v8::PinScope) {
    // The parser prepares its own scripts (the runtime runs them in document order), so they are
    // "already started" before any page script can move or clone them.
    webapi::page::mark_parser_scripts_started(&deno_core::JsRuntime::op_state_from(scope));
    let doc = webapi::page::page_document(scope);
    // Behind the window's `[LegacyUnforgeable]` `document` accessor -- an assignment
    // to `globalThis.document` cannot reach an accessor with no setter.
    webapi::install::live::set_window_document(scope, doc.into());

    // THE `__wireMQL` FORCE IS GONE. It called back into `host.js` to REDEFINE
    // `MediaQueryList.prototype`'s five members here, while only the engine was running -- because
    // the JS could not do it at its own module time (the interface object does not exist until the
    // registry's create pass) and doing it on the first `matchMedia()` call let a page watch the
    // prototype's accessors change identity under it. `MediaQueryList` is an `interface!` table
    // installed once at context creation now (`core/css/media_query_list.rs`), so there is no second
    // definition to force and no window in which a page could see one.
    //
    // Two other entries left this list before it: `__wireDocumentIdentity` is
    // `crate::singletons::wire_document_identity`, run from the caller right after this returns, and
    // `__finalizeSurface` was the scar W5.1 L18 closed. The list is empty.

    // Every parser-produced `<iframe srcdoc>` gets its document NOW. The attribute IS the document, so
    // there is nothing to fetch and no reason to defer — and unlike `src`, this is correct on every
    // path that installs a document, not only the one with a network behind it. Without it an
    // `<iframe srcdoc="...">` in the markup left the markup on the element and the frame empty,
    // because `srcdoc` was a plain string reflection and nothing more.
    // From the ARENA, not the page-visible `document.querySelectorAll`.
    let ids = webapi::page::with_host_dom(scope, |d| {
        d.get_elements_by_tag_name_in(d.root(), "iframe").into_iter().filter(|&e| d.get_attribute(e, "srcdoc").is_some()).collect::<Vec<u32>>()
    })
    .unwrap_or_default();
    for nid in ids {
        quietly(scope, |scope| {
            let el = webapi::page::node_object(scope, nid);
            let Ok(el_obj) = v8::Local::<v8::Object>::try_from(el) else { return };
            let a = str_val(scope, "srcdoc");
            let html = webapi::page::attr_call(scope, el_obj.into(), "getAttribute", &[a])
                .filter(|v| !v.is_null_or_undefined())
                .map(|v| v.to_rust_string_lossy(scope))
                .unwrap_or_default();
            webapi::page::frame_load_srcdoc(scope, el, &html);
        });
    }
}

/// Start every load the PARSER'S OWN elements induce.
///
/// Was `START_PARSED_SUBRESOURCES`. Runs after the document is installed and BEFORE the page's own
/// scripts, which is when a real browser has already started them — and why a script can install
/// `onload` afterwards and still be called.
///
/// `srcdoc` is not here: it needs no fetch, so [`install_document`] does it on every path that
/// installs a document rather than only on the one with a network behind it.
pub(crate) fn start_parsed_subresources(scope: &mut v8::PinScope) {
    // A nested browsing context is a WindowProxy on its parent -- `frames[0]`, `window[0]`, and a
    // named frame's name -- from the moment the parser creates it. The incremental sync maintained
    // that only from the native childList notification, which the parser does not go through, so a
    // page whose frames are written in the markup had `window.length === 1 && window[0] === undefined`:
    // a state no browser is in, and one a test reads in the same turn it reads `length`. `forced` is
    // the full rescan, and it runs BEFORE the page's scripts because that is when a real browser has
    // already created these contexts.
    quietly(scope, |scope| webapi::page::sync_frames(scope, true));
    // The loads themselves: which parsed element induces one, with what destination and mode, is
    // `core/html/subresource.rs`'s, and it walks the parsed tree in document order
    // through the same `maybe_load` a script-inserted element goes through -- ONE seam for every
    // subresource load the engine issues.
    quietly(scope, |scope| webapi::page::start_parsed_subresources(scope));
}

/// Compile the `<body>`'s event handler CONTENT attributes — `<body onload="init()">` — onto the
/// Window.
///
/// Was `ACTIVATE_BODY_HANDLERS`. These are not the body element's own handlers: HTML maps
/// WindowEventHandlers plus onblur/onerror/onfocus/onload/onresize/onscroll on a `body`/`frameset`
/// element onto the WINDOW, so `<body onload="init()">` is a `window.onload` assignment written in
/// markup. That is why the fix is this and not adding `body` to the load event's dispatch path:
/// forwarding puts the handler in the same slot `window.onload` uses, keeps `event.target` the Window,
/// and lets a later `window.onload = f` replace it.
///
/// It has to run HERE — after the parse, before the page's own scripts — for two reasons: that is when
/// a real browser reaches the body start tag, and it is the only order in which a script's own later
/// assignment wins. The parser writes these attributes straight into the arena, through none of the
/// JS-visible attribute seams, so nothing else would ever see them; the `setAttribute` path is covered
/// separately by the attribute seam (`core/html/subresource.rs::resource_attr`).
pub(crate) fn activate_body_handlers(scope: &mut v8::PinScope) {
    if spec::devtools::scripts_disabled() {
        return;
    }
    // The body and its attribute names from the ARENA, not through the page-visible
    // `document.body` and `getAttributeNames()` -- a page wrapping either saw the parser call it.
    let found = webapi::page::with_host_dom(scope, |d| {
        let is = |e: u32, t: &str| d.tag_name(e).is_some_and(|n| n.eq_ignore_ascii_case(t));
        let html = d.children(d.root()).into_iter().find(|&e| is(e, "html"))?;
        let body = d.children(html).into_iter().find(|&e| is(e, "body"))?;
        Some((body, d.attribute_names(body)))
    })
    .flatten();
    let Some((body_id, names)) = found else { return };
    let body = webapi::page::node_object(scope, body_id);
    for n in names {
        let n = n.to_ascii_lowercase();
        // Only `on*`. The filter is here rather than inside `body_handler_attr` because it also
        // decides how many times the engine crosses into a Web API call per parsed body.
        if !n.starts_with("on") {
            continue;
        }
        quietly(scope, |scope| webapi::page::body_handler_attr(scope, body, &n));
    }
}

/// HTML's "update the current document readiness", as the pipeline's three calls.
///
/// `loading` is the document being CREATED with that readiness, not an update, so it does not fire
/// `readystatechange` — nothing of this document can be listening yet. `interactive` and `complete`
/// are real transitions and do fire. The ORDER is the spec's and Chrome 150 headless's, measured
/// 2026-09-11:
///
/// ```text
/// head-inline:loading  body-inline:loading  ext-body:loading
/// readystatechange:interactive  defer:interactive  module:interactive
/// DOMContentLoaded:interactive  readystatechange:complete  load:complete
/// ```
pub(crate) fn set_readiness(scope: &mut v8::PinScope, state: &str, notify: bool) {
    // HTML: `domInteractive` / `domComplete` are taken as the readiness changes, before its event -- and
    // `performance.timing.domLoading` as a new document begins `loading` (Blink `DocumentTiming::MarkDomLoading`).
    match state {
        "loading" => webapi::page::note_milestone(scope, "domLoading"),
        "interactive" => webapi::page::note_milestone(scope, "domInteractive"),
        "complete" => webapi::page::note_milestone(scope, "domComplete"),
        _ => {}
    }
    quietly(scope, |scope| webapi::page::doc_readiness(scope, state, notify));
}

/// The lifecycle events a real navigation fires after parsing + scripts run: `DOMContentLoaded`
/// synchronously, then readiness `complete` + `load` + `pageshow` once nothing delays the load event.
///
/// Was `LIFECYCLE_EVENTS`. `readystatechange` is NOT fired here; it is fired by the readiness
/// transition itself, which is the only thing that can move the state this driver used to announce
/// without owning — a bare `fire(document, "readystatechange")` next to a `document.readyState` that
/// was the constant `"complete"` told every listener the state had changed to the value it had always
/// had.
///
/// Nor is there a manual `document.onreadystatechange(...)` / `window.onload(...)` call. Both existed
/// once, from when neither had a real event-handler IDL attribute and dispatching could not reach
/// them; both are real handler rows now, so their setters register listeners and the dispatch above
/// already runs them. The manual calls made each handler run TWICE per event.
pub(crate) fn fire_dom_content_loaded(scope: &mut v8::PinScope) {
    webapi::page::note_milestone(scope, "domContentLoadedEventStart");
    if let Some(doc) = document(scope) {
        fire_with(scope, doc.into(), "DOMContentLoaded", true);
    }
    webapi::page::note_milestone(scope, "domContentLoadedEventEnd");
    // WHEN it happened, for the driver that reports it. Chrome's
    // `Page.domContentEventFired.timestamp` is the moment the event fired, not the moment the
    // navigation finished; `finish_navigate` reads this rather than stamping every lifecycle event
    // with one `now()` taken after the whole load.
    with_fate(scope, |f| f.dcl_at = Some(std::time::Instant::now()));
}

/// The second half of the lifecycle: `load` behind the delay set. Split from `DOMContentLoaded` so an
/// `async` script that arrived after the parse runs between the two.
pub(crate) fn schedule_load(scope: &mut v8::PinScope) {
    // NOT `setTimeout(do_load, 0)`. A document's load event waits for its "delay the load event" set
    // (`core/html/browsing_context.rs`), which a parser-produced `<iframe src>` joins for the whole of
    // its navigation -- and so does every other subresource Chrome waits for
    // (scripts including `async`/`defer`, images, stylesheets, objects, embeds). Firing on the first
    // macrotask fired BEFORE any network-fetched frame could have a document -- structurally, not as a
    // race -- so `document.getElementById("f").contentDocument` was still the initial empty document
    // inside every onload handler on the page.
    let f = build_fn(scope, cb_do_load, "", 0);
    webapi::page::when_load_delays_done(scope, f, LOAD_EVENT_CAP_MS);
}

/// HTML "the end", last step: set the readiness to `complete` — which fires `readystatechange` — and
/// THEN fire `load`. Chrome 150 headless, measured 2026-09-11: `readystatechange:complete` immediately
/// precedes `load:complete`.
///
/// A native function rather than a closure because it is handed to the load-delay set and called back
/// from there, possibly from the host deadline rather than from the page's queue.
fn cb_do_load(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    set_readiness(scope, "complete", true);
    let g = global(scope);
    // Only at the Window: HTML fires the page's `load` at the Window with the Document as its
    // target, and never at the Document itself -- a capturing `load` listener on the document heard
    // a second, spurious one here.
    webapi::page::note_milestone(scope, "loadEventStart");
    fire(scope, g.into(), "load");
    webapi::page::note_milestone(scope, "loadEventEnd");
    // A remote frame's `load` is its `<iframe>`'s too, in the parent engine.
    crate::oopif::note_child_loaded(&deno_core::JsRuntime::op_state_from(scope).borrow());
    fire(scope, g.into(), "pageshow");
    set_load_fired(scope, true);
    // Under KORVUS_TPL_REPORT only: the surface walk once the documents exist.
    webapi::report_loaded_surfaces(scope);
    // The moment a declarative refresh's timeout counts from, and whether a `load`
    // handler already started a navigation -- Chrome then never reports this document's
    // `Page.loadEventFired` (measured: `onload = () => location.href = ...`).
    let during = navigation_pending(scope);
    let now = std::time::Instant::now();
    with_fate(scope, |f| {
        f.loaded_at = Some(now);
        f.navigated_during_load = during;
    });
    if let Some(m) = deno_core::JsRuntime::op_state_from(scope).borrow().try_borrow::<crate::LoadMarkSlot>() {
        m.0.set(Some(now));
    }
    schedule_declarative_refresh(scope);
    // Lazy images near the viewport, the favicon, the manifest's icon.
    webapi::page::load_event_finished(scope);
}

/// The page reached its `load` event for the CURRENT navigation.
///
/// The signal the navigate pump stops early on, instead of always burning the full budget on a page
/// whose perpetual timers keep the loop non-idle.
///
/// IT WAS THE `__loadFired` BRIDGE SLOT, AND THE REASON RECORDED FOR KEEPING IT THERE WAS WRONG.
/// That reason said the flag could not become a Rust field because "its READERS are
/// outside this process's Rust: `tools/corpus/run.py` and two runtime tests read it from page JS".
/// Being outside the process is not the same as needing a page-visible slot, and the disproof was in
/// `run.py` itself: the page's uncaught errors are read from the ENGINE's own stores through
/// `korvus fetch --errors` precisely because the JS array they used to live in was deleted.
/// `load_fired` now rides that same line, the two tests call [`crate::Engine::load_fired`],
/// and nothing asks the page a question the host already knows the answer to.
///
/// Three things followed from the move in the same commit. `pump_until_loaded` reads a `bool` instead
/// of evaluating `(globalThis[Symbol.for('korvus.bridge')]||{}).__loadFired === true` -- which was one
/// of the ten declared rows of `js_strings.manifest`, so the kernel's JavaScript inventory falls to
/// nine. `Engine::eval_bool` had no other caller and is gone. And the page can no longer read, spoof
/// or freeze the engine's own idea of whether it has finished loading.
pub(crate) fn set_load_fired(scope: &mut v8::PinScope, v: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(crate::LoadFired(v));
}

/// Clear the `load`-fired signal at the start of a navigation.
pub(crate) fn reset_load_fired(scope: &mut v8::PinScope) {
    set_load_fired(scope, false);
}

/// Does a navigation to `base_url`, answered with the MIME type `mime`, produce an XML document?
///
/// `mime` is the network stack's MIME type of the response (`transport::mime::mime_type_and_charset`:
/// every `Content-Type` value, the last valid one winning -- A/B C5), `""` when none named a type.
/// The header wins when there is one, exactly as a browser decides: any `*/*+xml` essence (plus the
/// two bare `text/xml`, `application/xml` spellings) is XML, and anything else — `text/html` above
/// all — is not. With no header (a harness that kept only the body) the URL's extension is the
/// fallback, which is the same call `wpt serve` itself makes from the file name.
pub(crate) fn xml_document(mime: &str, base_url: &str) -> bool {
    if !mime.is_empty() {
        return mime == "text/xml" || mime == "application/xml" || mime.ends_with("+xml");
    }
    // Extension of the URL's PATH -- a query string ("?include=file") must not be read as one.
    let path = base_url.split(['?', '#']).next().unwrap_or(base_url).to_ascii_lowercase();
    let ext = path.rsplit('/').next().unwrap_or("").rsplit('.').next().unwrap_or("");
    matches!(ext, "xhtml" | "xht" | "xml" | "svg")
}
/// The page document's `contentType`: the response's MIME type (an XML page read
/// `text/html`), Blink's `DocumentLoader::MimeType()`, which is the network stack's (A/B C5: a
/// `text/plain,text/html` page reads `text/html`). With no header, the type the URL's extension implies, as
/// [`xml_document`] reads it.
fn page_content_type(mime: &str, base_url: &str, is_xml: bool) -> String {
    if !mime.is_empty() {
        return mime.to_string();
    }
    if !is_xml {
        return "text/html".to_string();
    }
    let path = base_url.split(['?', '#']).next().unwrap_or(base_url).to_ascii_lowercase();
    match path.rsplit('.').next().unwrap_or("") {
        "xhtml" | "xht" => "application/xhtml+xml",
        "svg" => "image/svg+xml",
        _ => "application/xml",
    }
    .to_string()
}

/// The encoding a parsed document DECLARES, from `<meta charset>` or `<meta http-equiv="content-type">`.
///
/// This is not about decoding: html5ever already produced UTF-8 text before this runs. It is about what
/// `document.characterSet` reports and -- the consequential part -- which encoding the URL parser uses
/// as its override when percent-encoding a query. An `<a href="?한">` in an EUC-KR document yields
/// `?%C7%D1`, and 19 WPT files across EUC-KR, Shift_JIS, EUC-JP, ISO-2022-JP and Big5 assert it byte
/// for byte.
///
/// Deliberately NOT the full HTML encoding sniffing algorithm: no BOM (the bytes are gone by now), no
/// prescan of the first 1024 bytes (we have a parsed tree, which is strictly better), no HTTP header
/// (load_page has no response). It is the meta declaration or nothing, and "nothing" is UTF-8 -- the
/// spec's own default.
fn meta_encoding(dom: &webapi::dom::Dom) -> &'static encoding_rs::Encoding {
    // The document's own <meta>s: not one in a template's content.
    for &n in &dom.get_elements_by_tag_name_in(dom.root(), "meta") {
        // `<meta charset="euc-kr">` -- the modern form, and the one every legacy-encoding WPT file uses.
        if let Some(label) = dom.get_attribute(n, "charset") {
            if let Some(e) = encoding_rs::Encoding::for_label(label.trim().as_bytes()) {
                return e;
            }
        }
        // `<meta http-equiv="content-type" content="text/html; charset=euc-kr">` -- the legacy form.
        // Only http-equiv content-type carries an encoding; any other http-equiv is a different header.
        let is_ct = dom
            .get_attribute(n, "http-equiv")
            .map(|v| v.trim().eq_ignore_ascii_case("content-type"))
            .unwrap_or(false);
        if !is_ct {
            continue;
        }
        let Some(content) = dom.get_attribute(n, "content") else { continue };
        let lower = content.to_ascii_lowercase();
        let Some(i) = lower.find("charset") else { continue };
        let after = content[i + 7..].trim_start();
        let Some(after) = after.strip_prefix('=') else { continue };
        // The value may be quoted; it ends at the first `;` or whitespace otherwise.
        let after = after.trim_start();
        let label = if let Some(rest) = after.strip_prefix('"').or_else(|| after.strip_prefix('\'')) {
            let q = after.as_bytes()[0] as char;
            rest.split(q).next().unwrap_or("")
        } else {
            after.split([';', ' ', '\t', '\n', '\r']).next().unwrap_or("")
        };
        if let Some(e) = encoding_rs::Encoding::for_label(label.trim().as_bytes()) {
            return e;
        }
    }
    encoding_rs::UTF_8
}

impl crate::Engine {
    /// Parse `html` into the arena DOM, install it in `OpState`, and install a native `document`
    /// (a `Document` whose getters read the arena live).
    pub fn load_html(&mut self, html: &str) -> anyhow::Result<()> {
        let dom = webapi::dom::Dom::parse(html);
        // Collect inline <script> sources + whether each is a module, before the arena moves into
        // OpState. A `type="module"` script is deferred (browser semantics) and evaluated later in
        // run(); every other js-type script runs classically in document order. Non-js (JSON/template)
        // scripts are data, not code.
        // The document tree's scripts, not a template content's.
        let scripts: Vec<(bool, String)> = dom
            .get_elements_by_tag_name_in(dom.root(), "script")
            .iter()
            .filter_map(|&n| {
                let ty = dom.get_attribute(n, "type").unwrap_or("").to_ascii_lowercase();
                let is_js = script_type_runs(&ty);
                if !is_js {
                    return None;
                }
                // A CLASSIC `<script nomodule>` IS NOT RUN -- the same test
                // `load_page`'s driver applies, asked here so the URL-less path does not run a script
                // the URL-bearing one skips.
                if webapi::page::nomodule_skips(dom.get_attribute(n, "nomodule").is_some(), &ty) {
                    return None;
                }
                let text = dom.text_content(n);
                if text.trim().is_empty() {
                    return None;
                }
                Some((ty == "module", text))
            })
            .collect();
        self.runtime.op_state().borrow_mut().put(webapi::HostDom(dom));
        webapi::page::forget_node_tables(&self.runtime.op_state().borrow());
        self.install_document()?;
        // Classic scripts run now in document order (a throwing script must not abort the load); module
        // scripts are deferred to run() for async ES-module evaluation.
        for (is_module, src) in scripts {
            if is_module {
                // No document URL on this path (the HTML is handed over as a string), so no url: the
                // module is named `about:blank`, which is what a document with no URL is called, and
                // cannot resolve a relative import. `load_page` is the path that has a real URL, and
                // it passes one. No `import_base` either -- there is no base to be different from.
                self.runtime
                    .op_state()
                    .borrow_mut()
                    .borrow_mut::<PendingModules>()
                    .0
                    .push(PendingModule { source: src, url: String::new(), import_base: None, element: None, refused: false, inserted: false, delay_token: None, staged_slot: None, render_blocking: false });
            } else {
                let _ = self.eval("korvus:inline-script", src);
            }
        }
        Ok(())
    }

    /// Like [`Engine::load_html`], but evaluates each script in `injected` after `document` is
    /// installed and BEFORE any page `<script>` runs. Backs CDP
    /// `Page.addScriptToEvaluateOnNewDocument`: the injected scripts see a fresh document and set up
    /// globals the page then observes.
    pub fn load_html_injected(&mut self, html: &str, injected: &[String]) -> anyhow::Result<()> {
        let dom = webapi::dom::Dom::parse_page(html);
        self.runtime.op_state().borrow_mut().put(webapi::HostDom(dom));
        webapi::page::forget_node_tables(&self.runtime.op_state().borrow());
        self.install_document()?;
        // Fresh uncaught-error tally for this navigation.
        webapi::reset_uncaught_errors(&self.runtime.op_state());
        // A DevTools client's bindings (`Runtime.addBinding`), before its scripts, which may call them.
        self.with_scope(webapi::devtools_log::install_bindings_current);
        self.with_scope(webapi::devtools_log::install_touch_handlers);
        for src in injected {
            let _ = self.eval("korvus:injected-on-new-document", src.clone());
        }
        // B3/B4 (ABI 3): a loaded plugin's bound globals (`add_binding`) and init scripts, installed on the top
        // page's realm right where the injected `addScriptToEvaluateOnNewDocument` scripts run -- before the page's
        // own scripts, so either can be used by them. The frame path does the same in webapi's `run_doc_scripts`;
        // the top page never goes through it, so it is done here. Bindings first (an init script may call one).
        // Nothing loaded / nothing registered -> nothing added, so the no-plugin page is byte-for-byte unchanged
        // (invariant I9/J2).
        if plugin_host::any() {
            self.with_scope(|scope| webapi::install_plugin_bindings_current(scope));
            for code in plugin_host::init_scripts(plugin_host::REALM_PAGE) {
                let _ = self.eval("korvus:plugin-init", code);
            }
            // The plugins' own NAMED isolated worlds (ABI 3, WebExtension B3), APPENDED after any a CDP client
            // staged (push, not set -- see `Engine::push_document_worlds`), so a CDP client's named worlds survive;
            // each runs its scripts before the page's own. Built by `make_new_document_worlds` below.
            self.push_document_worlds(plugin_host::init_world_scripts(plugin_host::REALM_PAGE));
        }
        // And the isolated worlds the document gets as it starts, with their scripts (`cdp_eval.rs`).
        self.make_new_document_worlds();
        // Run the page's inline <script>s in document order with `document.currentScript` set to
        // each element (a classic script, so top-level var/function declarations stay global as on a
        // real page), so id-keyed harnesses (testing.js) record their observations. External
        // `<script src>` are not fetched here -- the empty stage below is what skips them; injected
        // new-document scripts supply that environment.
        //
        // THE SAME DRIVER as `load_page_inner`'s. It used to be a SECOND JS string,
        // `RUN_PAGE_SCRIPTS`, which differed from the real one in two ways that were bugs rather than
        // decisions: it applied NO `type` filter at all (so it executed `<script type="application/json">`,
        // which no browser does) and it ran an inline `type=module` through the classic path, which
        // raises "Cannot use import statement outside a module". Both are fixed by there being one
        // driver. Its only production caller is CDP's `about:blank` path, whose document is a fixed
        // empty shell with no `<script>` in it at all.
        let url = self.current_url();
        self.with_scope(|scope| {
            script_exec::run_page_scripts(scope, &script_exec::Staged { external: &[], is_xml: false, page_url: &url, postpone: &[], sheets: &[], entries: &[], asked_ms: 0.0 });
        });
        Ok(())
    }

    /// Persona-coherent request headers (UA + Client-Hints + `Sec-GPC`) for subresource fetches, so a
    /// site's sensor sees the same UA on the wire as `navigator.userAgent` (mirrors `op_fetch`).
    ///
    /// It emits the IDENTITY's headers and nothing else. `Referer`, `Accept` and every `Sec-Fetch-*`
    /// used to be built here too, which is precisely how a `<script src>` inherited a navigation's
    /// header set: they are properties of the REQUEST, so transport now computes them from the
    /// request's destination/mode/initiator instead.
    fn persona_fetch_identity(&mut self) -> Option<transport::PersonaIdentity> {
        let st = self.runtime.op_state();
        let st = st.borrow();
        st.try_borrow::<HostPersona>().map(|hp| transport::persona_identity(&*hp.0))
    }

    /// Load a page END TO END (the subresource-execution pipeline): parse the HTML, install
    /// `document`, run injected scripts, then execute EVERY `<script>` in document order — including
    /// external `<script src>`, fetched over the stealth transport (persona-coherent headers) and
    /// executed. This is what lets a real site's JS actually run (e.g. its instrumentation). Classic
    /// in-order execution for the first cut; async/defer + dynamic injection are later S-steps.
    pub async fn load_page(&mut self, html: &str, base_url: &str, injected: &[String]) -> anyhow::Result<()> {
        self.load_page_typed(html, base_url, injected, None).await
    }

    /// The response headers a document is DELIVERED with that decide its cross-origin isolation, in
    /// the order the server sent them (repeats kept: a repeated field is a *list*, and a list is not
    /// a valid structured-field item, which is precisely what `header-parsing-repeated` asserts).
    ///
    /// Kept to an ALLOWLIST rather than the whole header block. These values cross into JS as a
    /// literal in the per-navigation reset eval, so the set has to be bounded and its membership has
    /// to be a statement of intent: these are the four fields whose value changes what a browsing
    /// context IS, and nothing else about a response may reach script this way.
    /// The response headers that carry a POLICY the document is delivered with, and which the
    /// webapi layer therefore has to see. COOP/COEP/DIP resolve into the isolation-policy handle
    /// (`core/html/isolation_policy.rs`); `content-security-policy` is read by the icon loader
    /// (`core/html/csp.rs`) -- measured, `default-src 'none'` makes Chrome request no
    /// favicon at all. `-report-only` is carried for the isolation parser, which reports it. CSP's
    /// report-only form blocks nothing, and it is here all the same: its `require-trusted-types-for
    /// 'script'` makes every Trusted Types sink run the document's default policy and use its answer
    /// (measured on Chrome 150, 2026-10-10 -- `core/html/csp.rs`).
    const DELIVERED_POLICY_HEADERS: [&'static str; 12] = [
        "content-security-policy",
        "content-security-policy-report-only",
        // The document's referrer policy, and whether it asked for an origin-keyed agent
        // cluster (`window.originAgentCluster`).
        "referrer-policy",
        "origin-agent-cluster",
        // `document.featurePolicy` reports what the RESPONSE declared, so the two
        // permissions-policy headers travel with the response like COOP/COEP and the CSP.
        "permissions-policy",
        "feature-policy",
        "cross-origin-opener-policy",
        "cross-origin-opener-policy-report-only",
        "cross-origin-embedder-policy",
        "cross-origin-embedder-policy-report-only",
        "document-isolation-policy",
        "document-isolation-policy-report-only",
    ];

    /// [`Engine::load_page`] handed the WHOLE response it is loading, not just the body.
    ///
    /// `load_page_typed` already proved the point in one field: a browser picks its parser from
    /// `Content-Type`, so a caller that kept only the body hands the engine a document it cannot
    /// even tokenise correctly. COOP/COEP are the same shape of fact -- a browsing context's policy
    /// is a property of the RESPONSE, not of the markup -- and until this entry point existed there
    /// was nowhere for them to arrive. Callers that genuinely have no headers (a synthesised
    /// document, `about:blank`) pass an empty slice and get the unsafe-none default, which is what a
    /// document delivered with no policy headers has.
    pub async fn load_page_delivered(
        &mut self,
        html: &str,
        base_url: &str,
        injected: &[String],
        response_headers: &[(String, String)],
    ) -> anyhow::Result<()> {
        // EVERY `Content-Type` line, read as the network stack reads them (A/B C5): it was the first line,
        // cut at its first `;`.
        let mime = transport::mime::mime_type_and_charset(response_headers).mime;
        let policy_headers: Vec<(String, String)> = response_headers
            .iter()
            .filter(|(k, _)| Self::DELIVERED_POLICY_HEADERS.iter().any(|w| k.eq_ignore_ascii_case(w)))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        // A response's cookies are stored when it is RECEIVED, before its document exists, so the
        // first inline script's `document.cookie` already reads them. The transport does that for
        // the responses it fetches; a document handed in from outside (the WPT runner's own client)
        // brings its `Set-Cookie`s here. Dropped, the page read an EMPTY `document.cookie` once the
        // read-model honoured `Path` -- before that, other pages' cookies had leaked
        // into it and hidden the gap (`content-security-policy/support/checkReport.sub.js` threw on
        // `"".split("=")[1].trim()`).
        for (k, v) in response_headers {
            if k.eq_ignore_ascii_case("set-cookie") {
                transport::store_response_cookie(base_url, v);
            }
        }
        self.load_page_inner(html, base_url, injected, &mime, &policy_headers, None, None).await
    }

    /// [`Engine::load_page_delivered`] handed the response's BYTES: the document is
    /// decoded here, by the HTML encoding sniff (`decode.rs`), instead of arriving as a `String` some
    /// earlier layer already decoded as UTF-8. This is the entry point every navigation that has a real
    /// response takes -- `korvus fetch` and the CDP server's document load.
    ///
    /// The response's `Refresh` header travels with it: it is a declarative refresh like
    /// `<meta http-equiv=refresh>`, processed when the document is created, so it wins over any
    /// `<meta>` (measured: Chrome 150 reports `httpHeaderRefresh`).
    pub async fn load_page_response(
        &mut self,
        bytes: &[u8],
        base_url: &str,
        injected: &[String],
        response_headers: &[(String, String)],
    ) -> anyhow::Result<()> {
        let header = |name: &str| {
            response_headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
        };
        // THE DOCUMENT'S TYPE AND CHARSET, as Chrome's network stack reads them (A/B C5;
        // `HttpResponseHeaders::GetMimeTypeAndCharset`, `transport/src/mime.rs`): every `Content-Type` line, each
        // split on its commas, the LAST valid value winning with its own charset. It was the FIRST line cut at
        // its first `;`, so allegro.pl's `text/html,text/html;charset=utf-8` was the type `text/html,text/html` --
        // shown as text, its device check never run (Chrome runs it).
        let ct = transport::mime::mime_type_and_charset(response_headers);
        let refresh = header("refresh");
        let policy_headers: Vec<(String, String)> = response_headers
            .iter()
            .filter(|(k, _)| Self::DELIVERED_POLICY_HEADERS.iter().any(|w| k.eq_ignore_ascii_case(w)))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        let is_xml = xml_document(&ct.mime, base_url);
        // AN IMAGE IS SHOWN, NOT PARSED: Chrome builds its image document around the
        // response, titled with the image's name and natural size, which only the BYTES carry -- so it is
        // built here, before the body is decoded as text (a PNG's bytes became body text).
        if !is_xml && crate::text_viewer::viewer_for(&ct.mime) == Some(crate::text_viewer::Viewer::Image) {
            let doc = crate::text_viewer::image_document(base_url, crate::text_viewer::image_dims(bytes));
            // and its `<img>` takes these bytes, as Blink's image document's does: it asked the network again
            webapi::page::arm_page_image_document(&self.runtime.op_state(), base_url, response_headers, bytes);
            return self.load_page_inner(&doc, base_url, injected, &ct.mime, &policy_headers, None, refresh).await;
        }
        // The device's locale is the persona's first language -- the same one `navigator.language` and
        // ICU's default locale already come from (`Engine::assemble`).
        let locale = self.persona().navigator().languages.first().cloned().unwrap_or_default();
        let decoded = crate::decode::decode(bytes, Some(&ct.charset), is_xml, &locale);
        self.load_page_inner(&decoded.text, base_url, injected, &ct.mime, &policy_headers, Some(decoded.encoding), refresh)
            .await
    }

    /// [`Engine::load_page`] told what the server said the document IS.
    ///
    /// A browser picks its parser from the response's `Content-Type`, and the choice is not
    /// cosmetic: an `application/xhtml+xml` / `text/xml` / `image/svg+xml` body run through the
    /// HTML parser keeps its `<![CDATA[ … ]]>` wrappers *inside* the inline script text, so every
    /// inline script in the document is a syntax error before it declares anything. Pass the
    /// response header when there is one; with `None` the URL's extension decides, which is what a
    /// caller that only kept the body can offer.
    pub async fn load_page_typed(
        &mut self,
        html: &str,
        base_url: &str,
        injected: &[String],
        content_type: Option<&str>,
    ) -> anyhow::Result<()> {
        let mime = content_type.map(|v| transport::mime::mime_type_and_charset_of(v).mime).unwrap_or_default();
        self.load_page_inner(html, base_url, injected, &mime, &[], None, None).await
    }

    /// The one body both public entry points run. `policy_headers` is the already-filtered
    /// delivered-policy allowlist ([`Engine::DELIVERED_POLICY_HEADERS`]); a caller that had no
    /// response to read passes an empty slice, which is the unsafe-none default and no CSP. `mime` is the
    /// response's MIME type as the network stack reads it (`transport::mime`), `""` for none.
    async fn load_page_inner(
        &mut self,
        html: &str,
        base_url: &str,
        injected: &[String],
        mime: &str,
        policy_headers: &[(String, String)],
        decoded_as: Option<&'static encoding_rs::Encoding>,
        refresh_header: Option<String>,
    ) -> anyhow::Result<()> {
        let is_xml = xml_document(mime, base_url);
        // A TEXT OR JSON RESPONSE IS SHOWN, NOT PARSED: Chrome wraps the text in its viewer's
        // document (`text_viewer.rs`), so markup inside a text file stays text and its lines stay lines. An
        // image's document was built from its bytes by `load_page_response`.
        let viewer_doc = if is_xml {
            None
        } else {
            match crate::text_viewer::viewer_for(mime) {
                Some(crate::text_viewer::Viewer::Text) => Some(crate::text_viewer::text_document(html)),
                Some(crate::text_viewer::Viewer::Json) => Some(crate::text_viewer::json_document(html)),
                _ => None,
            }
        };
        // The JSON viewer's `<pre>` is styled by the viewer's own UA rules; any other document switches
        // them off again (the Page, and its sheet rows, outlive the document).
        let json_viewer = !is_xml && crate::text_viewer::viewer_for(mime) == Some(crate::text_viewer::Viewer::Json);
        webapi::page::set_viewer_rules(&self.runtime.op_state(), json_viewer.then_some(crate::text_viewer::JSON_UA_RULES));
        let html: &str = viewer_doc.as_deref().unwrap_or(html);
        // `parse_page`: the `<template for>` patches wait for the script pass.
        let mut dom = if is_xml { webapi::dom::Dom::parse_xml(html) } else { webapi::dom::Dom::parse_page(html) };
        // A viewer's document is in no-quirks mode (Chrome 150, measured: a `text/plain` page's `compatMode` is
        // `CSS1Compat`, A/B C5); its markup has no doctype, which the HTML parser reads as quirks.
        if viewer_doc.is_some() {
            let root = dom.root();
            dom.set_document_mode(root, webapi::dom::DocumentMode::NoQuirks);
        }
        // A top-level XML document with no style information is shown through Chrome's tree viewer,
        // which rewrites it before any script or reader sees it.
        if is_xml && crate::xml_viewer::applies(&dom) {
            crate::xml_viewer::transform(&mut dom);
        }
        // A document decoded from its bytes IS in the encoding the sniff chose; one handed over as a
        // string can only say what its `<meta>` declares.
        let doc_encoding = decoded_as.unwrap_or_else(|| meta_encoding(&dom));
        // The document's BASE url, not its location: a page carrying `<base href="/sub/">` loads
        // `<script src="x.js">` from `/sub/x.js` in Chrome, and did so from `/x.js` here.
        // `webapi::document_base_url` is the one place that knows the rule; this pass, the JS
        // driver below it (through `document.baseURI`) and the element loader all read it.
        let base = url::Url::parse(&webapi::document_base_url(&dom, base_url)).ok();
        // External script URLs in document order (js-type, non-empty src). The same is_js + src test is
        // mirrored in RUN_PAGE_SCRIPTS_EXT so the staged bodies stay aligned with the JS driver's scan.
        let mut ext_urls: Vec<String> = Vec::new();
        // Each one's element, for the order the requests leave in (the preload scanner, below).
        let mut ext_nodes: Vec<u32> = Vec::new();
        // `(async, defer)` of each classic `<script src>` AS PARSED, aligned with `ext_urls`
        // (a module is never postponed here -- the module queue is its own).
        let mut ext_kind: Vec<Option<(bool, bool)>> = Vec::new();
        // `<script crossorigin src>` is a CORS request (Chrome: `sec-fetch-mode: cors`,
        // an `Origin`), and only a request of the same mode may take a `<link rel=preload>`'s response.
        let mut ext_cors: Vec<bool> = Vec::new();
        // The referrer policy each one goes out under -- its `referrerpolicy`, else the
        // document's (`<meta name=referrer>`, else the response's `Referrer-Policy`).
        let mut ext_policy: Vec<Option<transport::ReferrerPolicy>> = Vec::new();
        // `crossorigin=use-credentials`, which includes credentials on the CORS request.
        let mut ext_cred: Vec<bool> = Vec::new();
        // Whether each one is RENDER-BLOCKING, its resource entry's `renderBlockingStatus`
        // (HTML; Chrome 150 measured): a classic script with neither `async` nor `defer`, or one marked
        // `blocking="render"`, and only one the parser met while the document had no body -- one in `<head>`. A body
        // script, a deferred, async or module one is "non-blocking"; korvus said "blocking" for every one.
        let mut ext_blocking: Vec<bool> = Vec::new();
        for &n in &script_elements(&dom, is_xml) {
            let ty = dom.get_attribute(n, "type").unwrap_or("").to_ascii_lowercase();
            let is_js = script_type_runs(&ty);
            if !is_js {
                continue; // JSON / template scripts are data, not code.
            }
            // A CLASSIC `<script nomodule>` IS NOT FETCHED. This pass is the wire: an
            // element skipped here costs no request, which is the whole point of the attribute, and
            // `run_page_scripts` applies the identical test so the staged bodies stay aligned with
            // the driver's scan element for element.
            if webapi::page::nomodule_skips(dom.get_attribute(n, "nomodule").is_some(), &ty) {
                continue;
            }
            if let Some(s) = script_src(&dom, n, is_xml) {
                if !s.trim().is_empty() {
                    let resolved = base
                        .as_ref()
                        .and_then(|b| b.join(s).ok())
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| s.to_string());
                    ext_urls.push(resolved);
                    ext_nodes.push(n);
                    let classic = !ty.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ')).eq("module");
                    // A MODULE SCRIPT IS ALWAYS A CORS REQUEST, the parser's own as much as a script-inserted
                    // one or an import: Chrome 150 sends `<script type=module src>` -- same- and
                    // cross-origin, `async` or not -- `Sec-Fetch-Mode: cors` with an `Origin`, credentials
                    // `same-origin`. This pass sent them `no-cors`, and a cross-origin one with its cookies
                    // (`tests/request_shape_live.rs`).
                    let cors = dom.get_attribute(n, "crossorigin");
                    ext_cors.push(cors.is_some() || !classic);
                    ext_cred.push(cors.is_some_and(|v| v.trim().eq_ignore_ascii_case("use-credentials")));
                    ext_policy.push(webapi::page::parsed_referrer_policy(&dom, policy_headers, Some(n)));
                    ext_kind.push(classic.then(|| (dom.get_attribute(n, "async").is_some(), dom.get_attribute(n, "defer").is_some())));
                    let in_head = in_head(&dom, n);
                    let explicit = dom.get_attribute(n, "blocking").is_some_and(|b| b.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("render")));
                    let implicit = classic && dom.get_attribute(n, "async").is_none() && dom.get_attribute(n, "defer").is_none();
                    ext_blocking.push(in_head && (explicit || implicit));
                }
            }
        }
        {
            let st = self.runtime.op_state();
            let mut st = st.borrow_mut();
            st.put(webapi::DocEncoding(doc_encoding));
            st.put(webapi::PageContentType(page_content_type(mime, base_url, is_xml)));
            st.put(webapi::HostDom(dom));
            // Every table keyed by the previous document's node ids (wrappers, owners, qualified names).
            webapi::page::forget_node_tables(&st);
            // A fresh document invalidates any cached layout. The cache key is the DOM's own
            // revision counter, and a freshly parsed document starts at zero -- so without this a
            // navigation could hand back the *previous* page's geometry.
            st.put(webapi::LayoutCache::default());
            // And the image sizes the layout was given: an element id means nothing in a new document.
            st.put(webapi::NaturalImages::default());
            st.put(webapi::CookieUrl(base_url.to_string()));
            // The page's location, which is what `fetch`/XHR resolve a relative URL against. Without
            // it `HostLocation` stays at its "about:blank" default, `about:blank.join("/a")` fails,
            // and every same-origin subresource request rejects with "relative URL without a base" --
            // so `fetch("/x")` and `xhr.open("GET","/x")` never complete on a page loaded this way.
            st.borrow::<HostLocation>().set(base_url);
            // This document's fate starts empty -- no refresh decided, nothing aborted.
            st.put(DocumentFate { refresh_header, ..Default::default() });
        }
        // The new document's empty "delay the load event" set, BEFORE the document is installed: installing it starts
        // its `<iframe srcdoc>` navigations, whose tokens are this document's (`reset_load_delays`).
        self.with_scope(webapi::page::reset_load_delays);
        self.install_document()?;
        // Fresh uncaught tally + empty external-body stage for this navigation.
        // `__xmlDoc` tells RUN_PAGE_SCRIPTS_EXT which scan to use for `<script>`: an XML document's
        // script may be namespace-PREFIXED (`<h:script>` is how every SVG test in WPT loads
        // testharness.js), and `getElementsByTagName("script")` matches the qualified name, so it
        // finds none of them. Set from Rust rather than sniffed in JS so the driver's scan and the
        // external-body fetch above cannot disagree about which elements are scripts.
        // The isolation-relevant response headers, as an array of `[name, value]` pairs in wire
        // order. The VALUES cross the boundary untouched and the isolation-policy service
        // (core/html/isolation_policy.rs, set_document_policy) resolves them into the kernel's
        // `IsolationPolicyHandle` at every navigation, so the top document and a child navigation are
        // decided by one parser rather than two that can disagree. Serialised with serde_json so a
        // header value containing a quote or a backslash cannot break out of the literal.
        //
        // `set_document_readiness(Loading, notify = false)` is HTML's "a document is created with
        // readiness loading", not an update -- so it does NOT fire readystatechange (nothing of this
        // document can be listening yet), and it must run BEFORE the injected `--eval-early` scripts,
        // which stand in for Chrome's document-start injection and see `loading` there. It reached
        // that through an `op_doc_readiness` wrapper until the op was deleted; the Rust call
        // below is and was the live path.
        webapi::reset_uncaught_errors(&self.runtime.op_state());
        // The rejection tally is an `OpState` store now, so it is cleared here rather than
        // by the `KB.__korvusRejections=[]` clause this eval used to carry.
        webapi::reset_unhandled_rejections(&self.runtime.op_state());
        // The element-load de-duplication set, cleared for the same reason as the two tallies above:
        // it is per-DOCUMENT state living in an ISOLATE-lifetime store. It is keyed by arena node id,
        // and a new document's arena hands out the same ids, so without this the second navigation on
        // one engine refused every element-induced load as a duplicate of the first's -- stylesheet,
        // images, iframe and preload all silently missing while the DOM still looked right. Found by
        // the request ledger; the measurement is in `webapi::page::reset_started_loads`.
        webapi::page::reset_started_loads(&self.runtime.op_state());
        // And any written input the last document's parse was holding back.
        webapi::page::reset_written_input(&self.runtime.op_state());
        // THE PREVIOUS DOCUMENT'S IMPORT MAP GOES WITH THE PREVIOUS DOCUMENT. HTML has
        // one import map per document; this store outlives the document because the LOADER does, so
        // a navigation has to say so. Cleared HERE, at the document reset, and not beside
        // `inline_base` below -- that line runs after the page's scripts have already installed this
        // document's maps, and clearing there would throw them away.
        self.module_fetch.borrow().import_maps.borrow_mut().clear();
        webapi::page::set_document_referrer_policy(&mut self.runtime.op_state().borrow_mut(), policy_headers);
        self.with_scope(|scope| {
            self::reset_load_fired(scope);
            script_exec::reset_current_script(scope);
            // The document's clock starts at its navigation's start, and its navigation entry reads the
            // response's phases -- before the isolation policy sets the clock's clamp.
            webapi::page::begin_document_timing(scope, base_url);
            // the document's permissions policy before its isolation: its cross-origin isolated capability needs the
            // `cross-origin-isolated` feature enabled in it
            webapi::page::set_document_permissions_policy(scope, policy_headers);
            webapi::page::set_document_policy(scope, policy_headers);
            // And the document's CSP, which the icon loader consults before it asks for
            // a favicon. An empty slice clears the last document's, which is what a navigation with
            // no policy header really delivered.
            webapi::page::set_document_csp(scope, policy_headers);
            webapi::page::set_document_origin_agent_cluster(scope, policy_headers);
            // A remote frame's document hears its parent from the start.
            webapi::page::install_frame_embedding(scope);
            self::set_readiness(scope, "loading", false);
        });
        // The load priority of every request the preload scanner makes for this document, read off the markup
        // as the scanner reads it (`webapi`'s `load_priority`): this pass states its
        // scripts', its blocking sheets' and its scanned loads' from it.
        let load_priorities = self.with_scope(webapi::page::scan_load_priorities);
        // A DevTools client's bindings (`Runtime.addBinding`), before its scripts, which may call them.
        self.with_scope(webapi::devtools_log::install_bindings_current);
        self.with_scope(webapi::devtools_log::install_touch_handlers);
        for src in injected {
            let _ = self.eval("korvus:injected-on-new-document", src.clone());
        }
        // B3/B4 (ABI 3): a loaded plugin's bound globals (`add_binding`) and init scripts, installed on the top
        // page's realm right where the injected `addScriptToEvaluateOnNewDocument` scripts run -- before the page's
        // own scripts, so either can be used by them. The frame path does the same in webapi's `run_doc_scripts`;
        // the top page never goes through it, so it is done here. Bindings first (an init script may call one).
        // Nothing loaded / nothing registered -> nothing added, so the no-plugin page is byte-for-byte unchanged
        // (invariant I9/J2).
        if plugin_host::any() {
            self.with_scope(|scope| webapi::install_plugin_bindings_current(scope));
            for code in plugin_host::init_scripts(plugin_host::REALM_PAGE) {
                let _ = self.eval("korvus:plugin-init", code);
            }
            // The plugins' own NAMED isolated worlds (ABI 3, WebExtension B3), APPENDED after any a CDP client
            // staged (push, not set -- see `Engine::push_document_worlds`), so a CDP client's named worlds survive;
            // each runs its scripts before the page's own. Built by `make_new_document_worlds` below.
            self.push_document_worlds(plugin_host::init_world_scripts(plugin_host::REALM_PAGE));
        }
        // And the isolated worlds the document gets as it starts, with their scripts (`cdp_eval.rs`).
        self.make_new_document_worlds();
        // Fetch external scripts over the stealth transport (persona headers). A real browser fetches
        // subresources CONCURRENTLY (they share a connection pool) while preserving document-order
        // EXECUTION; the old serial `for ... fetch().await` paid one network round-trip per script,
        // which dominates heavy pages (bbc.com/news: 47 scripts, 3.3 MB -> ~3.6 s). We fire all
        // fetches at once, then stage the bodies into KB.__kx in the original order (join_all
        // preserves input order), so execution order is unchanged. A failed fetch stages `None` to
        // keep alignment. The fetch futures don't touch the V8 isolate, so running
        // them concurrently is safe on the engine's single thread.
        let identity = self.persona_fetch_identity();
        let trace = std::env::var_os("KORVUS_TRACE_FETCH").is_some();
        let page_url = base_url.to_string();
        // When the page asked, for the resource-timing entry of a script whose request fails.
        let asked_ms = self.with_scope(webapi::page::realm_now_ms);
        let page_origin = url::Url::parse(&page_url).ok().filter(|u| u.scheme() == "http" || u.scheme() == "https").map(|u| u.origin().ascii_serialization());
        let fetched_as: Vec<(String, &'static str)> =
            ext_urls.iter().zip(&ext_cors).map(|(u, &cors)| (u.clone(), if cors { "cors" } else { "no-cors" })).collect();
        let ext_priority: Vec<Option<String>> = ext_nodes.iter().map(|n| load_priorities.get(n).cloned()).collect();
        // A PARSER SCRIPT ITS DOCUMENT'S CSP REFUSES IS NEVER REQUESTED. This pass fetched every one and the
        // policy was asked only when the parser reached it: a refused script went on the wire and its entry read as
        // loaded. Chrome 150 x3: no GET, and two entries in the refusal shape -- the preload scanner's,
        // made here, and the element's when the parser reaches it (`script_exec::enter_entry`). Asked quietly: the
        // parser's own check reports the violation.
        let ext_refused: Vec<bool> = self.with_scope(|scope| {
            ext_nodes
                .iter()
                .zip(&ext_urls)
                .zip(&ext_blocking)
                .map(|((&n, u), &blocking)| {
                    let refused = webapi::page::parser_script_refused(scope, n, u);
                    if refused {
                        webapi::page::refused_parser_script(scope, n, u, blocking);
                    }
                    refused
                })
                .collect()
        });
        let fetches = ext_urls.into_iter().zip(ext_cors).zip(ext_policy).zip(ext_priority).zip(ext_cred).zip(ext_refused.clone()).map(|(((((u, cors), policy), priority), cred), refused)| {
            let identity = identity.clone();
            let initiator = page_url.clone();
            let page_origin = page_origin.clone();
            async move {
                if refused {
                    return (None, (u, None));
                }
                if trace {
                    eprintln!("[subresource] GET {u}");
                }
                // A classic `<script src>`: destination `script`, mode `no-cors`, initiated by the
                // page. That triple is what makes the request's Sec-Fetch-* set differ from the
                // navigation's — it is the same seam the old shared per-page header list (this
                // function's predecessor, renamed to `persona_fetch_identity` when the
                // wire joined the headers) erased.
                let name = u.clone();
                // A `data:` script is its own body: decoded here, never handed to the network
                // client, whose request builder refused the URL -- so every `<script src="data:...">` failed
                // and instagram.com's inlined `data:application/x-javascript; charset=utf-8;base64,...`
                // modules never ran (the page kept its 202-element shell, text 0). Chrome runs them, and
                // records no resource-timing entry for one.
                if u.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
                    return match webapi::decode_data_url(&u) {
                        Some((mime, bytes)) => (
                            Some(script_exec::Fetched {
                                status: 200,
                                headers: vec![("content-type".to_string(), mime)],
                                body: String::from_utf8_lossy(&bytes).into_owned(),
                            }),
                            (name, None),
                        ),
                        None => (None, (name, None)),
                    };
                }
                let mut req = transport::FetchRequest {
                    url: u,
                    method: "GET".to_string(),
                    headers: Vec::new(),
                    body: None,
                    destination: transport::Destination::from("script"),
                    mode: if cors { transport::Mode::Cors } else { transport::Mode::NoCors },
                    initiator: Some(initiator),
                    user_activated: false,
                    wire: None,
                    page: if cors {
                        page_origin.map(|origin| transport::PageRequest { origin, author_headers: Vec::new(), cors_preconfigured: true })
                    } else {
                        None
                    },
                    referrer_policy: policy,
                    // Blink's load priority for it: `async`/`defer` none, after the first image `u=2`.
                    priority,
                    credentials: if cred { transport::CredentialsMode::Include } else { transport::CredentialsMode::SameOrigin },
                    ..Default::default()
                };
                // The page's own parser script: its site for cookies is the page's.
                req.cookie_cross_site = req.initiator.as_deref().is_some_and(|i| transport::cookie_cross_site("", i, &req.url));
                // The persona's identity headers AND its wire, written in one call from one persona:
                // a `<script src>` this page loads goes out on the same ClientHello the navigation
                // did, because both read the same `PersonaIdentity`.
                if let Some(id) = identity.as_ref() {
                    transport::apply_persona_identity(id, &mut req);
                }
                match transport::fetch(req)
                .await
                {
                    Ok(resp) => {
                        if trace {
                            eprintln!("[subresource] <- {} ({} bytes)", resp.status, resp.body.len());
                        }
                        // The STATUS AND HEADERS TRAVEL WITH THE BODY. Whether the body
                        // may run depends on them and on the script's flavour, which the driver knows
                        // and this pass does not -- so the decision is the driver's, not a filter here.
                        let timing = Some(resp.timing.clone());
                        (Some(script_exec::Fetched { status: resp.status, headers: resp.headers, body: resp.body }), (name, timing))
                    }
                    Err(e) => {
                        if trace {
                            eprintln!("[subresource] <- ERR {e}");
                        }
                        (None, (name, None))
                    }
                }
            }
        });
        // The bodies STAY IN RUST. They used to be pushed one `execute_script` at a time
        // onto `KB.__kx`, a page-reachable array holding every external script of the document as a
        // string -- and a JSON literal per body, so a script containing a lone surrogate or a 2 MB
        // minified bundle was re-encoded and re-parsed to cross a boundary the driver no longer has.
        // THE PARSER'S SCRIPT-BLOCKING STYLESHEETS, fetched beside its scripts through
        // the element loader's own service -- the request shape a `<link rel=stylesheet>` takes -- and
        // claimed from the loader so `start_parsed_subresources` does not ask for them again. Each is
        // applied before the first parser script after its link (`script_exec::apply_sheets_before`).
        let claimed = self.with_scope(webapi::page::claim_parser_sheets);
        let sheet_svc = {
            let st = self.runtime.op_state();
            let st = st.borrow();
            st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::SubresourceFetch>()).map(|h| h.0.clone())
        };
        // The document's referrer policy, header or `<meta>`, for the parser's own stylesheet requests.
        let sheet_policy = {
            let st = self.runtime.op_state();
            let st = st.borrow();
            webapi::page::page_referrer_policy(&st).map(|p| p.token()).unwrap_or("")
        };
        let sheet_fetches = claimed.iter().map(|sh| {
            let svc = sheet_svc.clone();
            let req = spec::SubresourceRequest {
                url: sh.url.clone(),
                destination: "style".to_string(),
                mode: sh.mode.to_string(),
                method: "GET".to_string(),
                body: None,
                content_type: None,
                // The page's own document.
                site_for_cookies: String::new(),
                client_url: String::new(),
                referrer_policy: sheet_policy.to_string(),
                priority: load_priorities.get(&sh.id).cloned(),
                credentials: sh.credentials.to_string(),
                navigation_start: None,
                parent_encoding: None,
                parent_origin: None,
            };
            async move {
                match svc {
                    Some(s) => s.fetch(req).await.ok(),
                    None => None,
                }
            }
        });
        // THE PRELOAD SCANNER: the document's images and `<link>` loads, requested NOW and in tree order
        // with the scripts and blocking stylesheets above, the way Chrome's preload scanner puts the whole
        // parsed document on the wire at once. They used to wait for every script and stylesheet of this
        // pass to ARRIVE (the element sweep runs after it), so the page's resource timeline began with its
        // scripts. The element that asks later takes the in-flight request (`webapi::page::EarlyFetches`);
        // nothing here waits for one -- only the scripts and stylesheets hold the parse, as before.
        let claimed_ids: Vec<u32> = claimed.iter().map(|sh| sh.id).collect();
        let scanned = self.with_scope(|scope| webapi::page::preload_scan(scope, &claimed_ids));
        let position: std::collections::HashMap<u32, usize> = {
            let st = self.runtime.op_state();
            let st = st.borrow();
            match st.try_borrow::<webapi::HostDom>() {
                Some(h) => h.0.descendant_elements(h.0.root()).into_iter().enumerate().map(|(i, n)| (n, i)).collect(),
                None => Default::default(),
            }
        };
        use deno_core::futures::FutureExt;
        use std::future::Future;
        type Waited<'a, T> = std::pin::Pin<Box<deno_core::futures::future::MaybeDone<deno_core::futures::future::LocalBoxFuture<'a, T>>>>;
        let mut script_futs: Vec<Waited<'_, _>> =
            fetches.map(|f| Box::pin(deno_core::futures::future::maybe_done(f.boxed_local()))).collect();
        let mut sheet_futs: Vec<Waited<'_, _>> =
            sheet_fetches.map(|f| Box::pin(deno_core::futures::future::maybe_done(f.boxed_local()))).collect();
        let mut early = webapi::page::EarlyFetches::default();
        let mut early_futs = Vec::new();
        // A preload of what this pass fetches itself -- `<link rel=preload as=style>` of a blocking sheet,
        // `as=script` of a parser script -- is answered from the pass's response (U16), never asked twice.
        let pass_keys: std::collections::HashSet<(String, String, String)> = claimed
            .iter()
            .map(|sh| (sh.url.clone(), "style".to_string(), sh.mode.to_string()))
            .chain(fetched_as.iter().map(|(u, m)| (u.clone(), "script".to_string(), m.to_string())))
            .collect();
        for item in &scanned {
            if pass_keys.contains(&(item.url.clone(), item.destination.to_string(), item.mode.to_string())) {
                continue;
            }
            let Some(svc) = sheet_svc.clone() else { break };
            let req = spec::SubresourceRequest {
                url: item.url.clone(),
                destination: item.destination.to_string(),
                mode: item.mode.to_string(),
                method: "GET".to_string(),
                body: None,
                content_type: None,
                site_for_cookies: item.site_for_cookies.clone(),
                client_url: String::new(),
                referrer_policy: item.referrer_policy.to_string(),
                priority: load_priorities.get(&item.id).cloned(),
                credentials: item.credentials.to_string(),
                navigation_start: None,
                parent_encoding: None,
                parent_origin: None,
            };
            let key = (item.url.clone(), item.destination.to_string(), item.mode.to_string());
            if early.0.contains_key(&key) {
                continue;
            }
            let f = svc.fetch(req).shared();
            early.0.insert(key, f.clone());
            early_futs.push((item.id, f));
        }
        // FIRST POLLS, IN TREE ORDER: polling a load once is what puts it on the wire (the transport
        // spawns the request on its first poll).
        {
            enum Kind {
                Script(usize),
                Sheet(usize),
                Early(usize),
            }
            let pos = |id: u32| position.get(&id).copied().unwrap_or(usize::MAX);
            let mut order: Vec<(usize, usize, Kind)> = Vec::new();
            for (i, &n) in ext_nodes.iter().enumerate() {
                order.push((pos(n), order.len(), Kind::Script(i)));
            }
            for (i, &id) in claimed_ids.iter().enumerate() {
                order.push((pos(id), order.len(), Kind::Sheet(i)));
            }
            for (i, (id, _)) in early_futs.iter().enumerate() {
                order.push((pos(*id), order.len(), Kind::Early(i)));
            }
            order.sort_by_key(|(p, seq, _)| (*p, *seq));
            let waker = deno_core::futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            for (_, _, k) in order {
                match k {
                    Kind::Script(i) => {
                        let _ = script_futs[i].as_mut().poll(&mut cx);
                    }
                    Kind::Sheet(i) => {
                        let _ = sheet_futs[i].as_mut().poll(&mut cx);
                    }
                    Kind::Early(i) => {
                        let mut f = early_futs[i].1.clone();
                        let _ = std::pin::Pin::new(&mut f).poll(&mut cx);
                    }
                }
            }
        }
        self.runtime.op_state().borrow_mut().put(early);
        drop(early_futs);
        // THE STREAMING PARSE. The parser's scripts run as their responses come in, not once every one
        // of them is: Chrome runs an inline script while a later `<script src>` is still on the wire, and its parser
        // waits -- with the event loop running, timers, history traversals and fetch completions included -- only at a
        // blocking script or a script-blocking sheet that is not in yet. korvus fetched every parser script first, so a
        // page's first inline script ran behind its slowest external one (WPT history/the-history-interface/007: the
        // inline `history.go(-1)` waited 1.5 s for a later slow script, and its `popstate` came after `load`).
        use script_exec::Slot;
        let mut slots: Vec<Slot> = script_futs.iter().map(|_| Slot::Pending).collect();
        let sheet_modes: Vec<String> = claimed.iter().map(|sh| sh.mode.to_string()).collect();
        // a parser stylesheet in `<head>` is render-blocking until it is in (HTML "implicitly potentially render-blocking")
        let sheet_in_head: Vec<bool> = {
            let st = self.runtime.op_state();
            let st = st.borrow();
            match st.try_borrow::<webapi::HostDom>() {
                Some(h) => claimed.iter().map(|sh| in_head(&h.0, sh.id)).collect(),
                None => vec![false; claimed.len()],
            }
        };
        let mut sheets: Vec<script_exec::PrefetchedSheet> = claimed
            .into_iter()
            .map(|sh| script_exec::PrefetchedSheet { cors: sh.mode == "cors", id: sh.id, url: sh.url, response: None, asked_ms, pending: true })
            .collect();
        // Each parser-inserted script's resource-timing entry -- put on the timeline in the script's own
        // turn of the pass (`script_exec::ScriptEntry`), not all of them before the first script runs
        let mut entries: Vec<script_exec::ScriptEntry> = fetched_as
            .iter()
            .zip(ext_blocking.iter().copied().chain(std::iter::repeat(true)))
            .zip(ext_refused.iter().zip(&ext_nodes))
            .map(|(((url, mode), blocking), (&refused, &node))| script_exec::ScriptEntry { url: url.clone(), timing: None, blocking, cors: *mode == "cors", refused, node })
            .collect();
        // WHEN EACH PARSER-INSERTED CLASSIC SCRIPT RUNS (the measurement is on `script_exec::Postpone`):
        // `defer` after the parse; `async` as soon as its body is in -- where the parser reaches it when it is in by
        // then, else the moment it arrives (`script_exec::run_arrived_async`), after `DOMContentLoaded` when that is
        // after the parse (WPT execution-timing/112 is this case, and passes in Chrome).
        let postpone: Vec<script_exec::Postpone> = ext_kind
            .iter()
            .map(|k| match k {
                Some((true, _)) => script_exec::Postpone::Async,
                Some((false, true)) => script_exec::Postpone::Defer,
                _ => script_exec::Postpone::InPlace,
            })
            .collect();
        let page_url = base_url.to_string();
        // Every response that is in, taken without waiting -- each of the parser's own requests settles the preloads
        // that joined it (`webapi::page::settle_parser_fetch`).
        macro_rules! harvest {
            () => {{
                let waker = deno_core::futures::task::noop_waker();
                let mut cx = std::task::Context::from_waker(&waker);
                let mut arrived: Vec<(String, &'static str, String, Option<webapi::page::ParserFetch>)> = Vec::new();
                for (i, f) in script_futs.iter_mut().enumerate() {
                    if !matches!(slots[i], Slot::Pending) || f.as_mut().poll(&mut cx).is_pending() {
                        continue;
                    }
                    let (body, (_, timing)) = f.as_mut().take_output().expect("polled to completion");
                    entries[i].timing = timing;
                    let (url, mode) = &fetched_as[i];
                    let staged = body.as_ref().map(|b| webapi::page::ParserFetch {
                        url: url.clone(),
                        destination: "script",
                        mode: mode.to_string(),
                        status: b.status,
                        headers: b.headers.clone(),
                        body: b.body.clone(),
                    });
                    arrived.push((url.clone(), "script", mode.to_string(), staged));
                    slots[i] = match body {
                        Some(b) => Slot::Got(b),
                        None => Slot::Failed,
                    };
                }
                for (j, f) in sheet_futs.iter_mut().enumerate() {
                    if !sheets[j].pending || f.as_mut().poll(&mut cx).is_pending() {
                        continue;
                    }
                    let response = f.as_mut().take_output().expect("polled to completion");
                    let staged = response.as_ref().map(|r| webapi::page::ParserFetch {
                        url: sheets[j].url.clone(),
                        destination: "style",
                        mode: sheet_modes[j].clone(),
                        status: r.status,
                        headers: r.headers.clone(),
                        body: r.body.clone(),
                    });
                    arrived.push((sheets[j].url.clone(), "style", sheet_modes[j].clone(), staged));
                    sheets[j].response = response;
                    sheets[j].pending = false;
                }
                arrived
            }};
        }
        macro_rules! staged {
            () => {
                script_exec::Staged { external: &slots, is_xml, page_url: &page_url, postpone: &postpone, sheets: &sheets, entries: &entries, asked_ms }
            };
        }
        // The document is render-blocked while a render-blocking script or sheet is not in (`webapi::page::render_blocked`).
        macro_rules! render_blocked {
            () => {
                slots.iter().zip(&ext_blocking).any(|(s, &b)| b && matches!(s, Slot::Pending))
                    || sheets.iter().zip(&sheet_in_head).any(|(sh, &h)| h && sh.pending)
                    || crate::modules::render_blocking_modules(&self.runtime.op_state().borrow())
            };
        }
        // What came in while the parser waited: the preloads that joined it, then the `async` scripts among it.
        macro_rules! take_in {
            ($pass:expr) => {{
                let arrived = harvest!();
                let staged = staged!();
                self.with_scope(|scope| {
                    for (u, d, m, f) in arrived {
                        webapi::page::settle_parser_fetch(scope, &u, d, &m, f);
                    }
                    script_exec::run_arrived_async(scope, $pass, &staged);
                });
            }};
        }
        // The module map and its fetch identity are this document's before any of its scripts runs: the event loop
        // runs while the parser waits, and a module a script inserts runs in it.
        self.module_fetch.borrow_mut().identity = identity.clone();
        self.module_fetch.borrow_mut().initiator = Some(base_url.to_string());
        self.module_fetch.borrow_mut().referrer_policy = webapi::page::page_referrer_policy(&self.runtime.op_state().borrow());
        // The previous document's inline-module base goes with the previous document. It is
        // navigation state because `import.meta.resolve` can be called at any later moment;
        // leaving a stale pair here would be inert rather than wrong -- its name cannot match
        // a referrer in the new document unless the new document is the same URL -- but "inert" is not
        // a property worth relying on when clearing it is one line.
        self.module_fetch.borrow_mut().inline_base = None;
        // AND SO DOES THE MODULE MAP. HTML gives every document its own; an entry in it is never
        // evicted while that document lives, and every entry is gone when the next one starts.
        self.module_fetch.borrow_mut().fetched.clear();
        self.module_fetch.borrow_mut().dyn_roots.clear();
        // the previous document's module fetches are no part of the next one's timeline
        self.module_fetch.borrow().timings.0.borrow_mut().clear();
        self.module_fetch.borrow_mut().dyn_loading.clear();
        let first = harvest!();
        // Start every load the PARSER produced -- a frame's navigation and every
        // other subresource written in the markup -- before the page's own scripts run, which is when
        // a browser has already started them and why a script can install `onload` afterwards and
        // still be called.
        //
        // The element loader's two seams (core/html/subresource.rs) only see what a SCRIPT creates or
        // points somewhere: they hang off the attribute-write and insertion choke points, and the
        // parser builds its nodes straight into the arena through neither. So until W9.17 a page
        // carrying a stylesheet and an image requested exactly one thing, itself.
        let in_flight: Vec<(String, &'static str, String)> = slots
            .iter()
            .zip(&fetched_as)
            .filter(|(s, _)| matches!(s, Slot::Pending))
            .map(|(_, (u, m))| (u.clone(), "script", m.to_string()))
            .chain(sheets.iter().zip(&sheet_modes).filter(|(sh, _)| sh.pending).map(|(sh, m)| (sh.url.clone(), "style", m.clone())))
            .collect();
        let mut pass = {
            let staged = staged!();
            self.with_scope(|scope| {
                // What the parser already fetched answers a `<link rel=preload>` of it, and a preload of one
                // still in flight joins it.
                webapi::page::stage_parser_fetches(scope, first.into_iter().filter_map(|(_, _, _, f)| f).collect());
                webapi::page::stage_parser_in_flight(scope, in_flight);
                self::start_parsed_subresources(scope);
                webapi::page::clear_parser_fetches(scope);
                // `<body onload="...">` -- a Window handler written in markup. Before the page's own
                // scripts, because that is when the parser reaches the body start tag and the only order in
                // which a script's later `window.onload = f` correctly replaces it.
                self::activate_body_handlers(scope);
                script_exec::begin_page_scripts(scope, &staged)
            })
        };
        // The pass pauses at every parser-blocking script a `document.write` produced and
        // is continued with its response, fetched HERE -- asynchronously, with the parser's request
        // shape -- so the event loop keeps running while the parser waits. It pauses too at a parser script or a
        // script-blocking sheet not in yet, and waits for it with the event loop running.
        let mut written_response = None;
        // whether the parser waited for anything: its end is then a task of its own (`webapi::page::queue_parse_end`)
        let mut waited = false;
        loop {
            let step = {
                let staged = staged!();
                self.with_scope(|scope| script_exec::continue_page_scripts(scope, &mut pass, &staged, written_response.take()))
            };
            if !matches!(step, script_exec::Step::Done) {
                waited = true;
            }
            match step {
                script_exec::Step::Done => break,
                script_exec::Step::Fetch(url) => {
                    let local = written_local_response(&self.runtime.op_state().borrow(), &url);
                    written_response = Some(match local {
                        Some(r) => r,
                        None => {
                            let policy = webapi::page::page_referrer_policy(&self.runtime.op_state().borrow());
                            fetch_written_script(identity.as_ref(), &page_url, &url, policy).await
                        }
                    });
                }
                script_exec::Step::WaitScript(i) => self.parser_wait(script_futs[i].as_mut(), render_blocked!()).await?,
                script_exec::Step::WaitSheet(j) => self.parser_wait(sheet_futs[j].as_mut(), render_blocked!()).await?,
            }
            take_in!(&mut pass);
            if written_response.is_none() && self.navigation_pending() {
                break;
            }
        }
        if waited && !self.navigation_pending() {
            self.with_scope(webapi::page::queue_parse_end);
            let op_state = self.runtime.op_state();
            let mut ended = std::pin::pin!(std::future::poll_fn(|_| {
                if webapi::page::take_parse_end(&mut op_state.borrow_mut()) { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
            }));
            self.parser_wait(ended.as_mut(), render_blocked!()).await?;
        }
        // The first style recalc, now that the parser's scripts have run.
        self.with_scope(|scope| quietly(scope, |scope| webapi::page::style_after_parse(scope)));
        // A SCRIPT OF THIS DOCUMENT STARTED A NAVIGATION WHILE IT WAS BEING PARSED. The
        // document is abandoned before "the end": Chrome 150 fires neither `DOMContentLoaded` nor `load`
        // on it and reports neither event over CDP (measured with `location.href=`, `replace()` and
        // `window.location=` in an inline script). The scripts after the navigating one did not run
        // either -- `script_exec::run_page_scripts` stops there.
        if self.navigation_pending() {
            self.with_scope(|scope| with_fate(scope, |f| f.aborted = true));
            return Ok(());
        }
        // The parse is over: HTML "the end" step 2, `update the current document readiness` to
        // `interactive`, which fires `readystatechange`. HERE, not inside LIFECYCLE_EVENTS, because
        // the spec (and Chrome 150 headless, measured 2026-09-11) puts it BEFORE the deferred module
        // scripts below and before `DOMContentLoaded`:
        //     head-inline:loading  body-inline:loading  ext-body:loading
        //     readystatechange:interactive  defer:interactive  module:interactive
        //     DOMContentLoaded:interactive  readystatechange:complete  load:complete
        self.with_scope(|scope| self::set_readiness(scope, "interactive", true));
        // HTML "the end" step 3: the deferred scripts -- `defer` and the parser's modules -- wait for their responses,
        // and for every script-blocking sheet, with the event loop running; an `async` one that arrives meanwhile runs.
        let sheets_hold = {
            let staged = staged!();
            let pass = &pass;
            self.with_scope(|scope| script_exec::deferred_scripts(scope, pass, &staged))
        };
        loop {
            let script = (0..slots.len()).find(|&i| matches!(slots[i], Slot::Pending) && postpone[i] != script_exec::Postpone::Async);
            let sheet = if sheets_hold { sheets.iter().position(|sh| sh.pending) } else { None };
            match (script, sheet) {
                (Some(i), _) => self.parser_wait(script_futs[i].as_mut(), render_blocked!()).await?,
                (None, Some(j)) => self.parser_wait(sheet_futs[j].as_mut(), render_blocked!()).await?,
                (None, None) => break,
            }
            take_in!(&mut pass);
            if self.navigation_pending() {
                self.with_scope(|scope| with_fate(scope, |f| f.aborted = true));
                return Ok(());
            }
        }
        {
            let staged = staged!();
            let pass = &mut pass;
            self.with_scope(|scope| script_exec::finish_late(scope, pass, &staged));
        }
        // Deferred module scripts, HERE, before the lifecycle events -- which is both the spec's order
        // (a module script is deferred: evaluated after the document parses and BEFORE
        // DOMContentLoaded) and the only order that works.
        //
        // The alternative -- leaving them to `run()`, which the conformance runner reaches through
        // `pump(50ms)` -- loses them. `pump` is `timeout(budget, run())`: the drain takes the queue
        // out of OpState and then awaits a network fetch for each import, and when the 50ms slice
        // expires mid-await the half-polled future is DROPPED with the queue already emptied. The
        // modules are gone and nothing reports it. Here the await is inside `load_page`, whose caller
        // gives it seconds, not a 50ms slice.
        //
        // `defer` scripts and deferred MODULE scripts are ONE list, run in document order after the
        // parse, at `interactive`, before `DOMContentLoaded`. Chrome 150 runs
        // `<script type=module src=m1>` `<script defer src=d1>` `<script type=module>`... as m1, d1, ...;
        // korvus ran every `defer` first, and coveryourtracks.eff.org's `defer` front.js ran before its
        // jQuery module ("$ is not defined").

        // ONLY THE PARSER'S: a module the page INSERTED while the parse was running is
        // a task of its own and runs after `DOMContentLoaded`, as Chrome's does -- it stays queued for
        // the event loop's next turn rather than being evaluated here.
        let mut drained = 0usize;
        let deferred: Vec<usize> = (0..pass.postponed.len()).filter(|&k| pass.postponed[k].when == script_exec::Postpone::Defer).collect();
        for k in deferred {
            let p = &pass.postponed[k];
            if p.modules_before > drained {
                let n = p.modules_before - drained;
                let _ = self.evaluate_parser_modules_first(n).await;
                drained = pass.postponed[k].modules_before;
            }
            let p = &pass.postponed[k];
            self.with_scope(|scope| script_exec::run_postponed_one(scope, p));
            if self.navigation_pending() {
                self.with_scope(|scope| with_fate(scope, |f| f.aborted = true));
                return Ok(());
            }
        }
        let _ = self.evaluate_parser_modules().await;
        // Fire the document lifecycle events a real navigation dispatches after parsing + scripts:
        // DOMContentLoaded now, then readiness `complete` + load/pageshow on the next tick (after
        // subresources). Scripts that only run from a `load`/DOMContentLoaded listener (some site
        // scripts do) would otherwise wait forever.
        self.with_scope(self::fire_dom_content_loaded);
        // An `async` script whose body arrives after `DOMContentLoaded` runs when it does, before `load`
        // (Chrome: `DCL:interactive async-slow:interactive load:complete`); a sheet no script waited for is applied
        // when it is in. The load event waits for both.
        loop {
            let script = slots.iter().position(|s| matches!(s, Slot::Pending));
            let sheet = sheets.iter().position(|sh| sh.pending);
            match (script, sheet) {
                (Some(i), _) => self.parser_wait(script_futs[i].as_mut(), render_blocked!()).await?,
                (None, Some(j)) => self.parser_wait(sheet_futs[j].as_mut(), render_blocked!()).await?,
                (None, None) => break,
            }
            take_in!(&mut pass);
            if self.navigation_pending() {
                return Ok(());
            }
        }
        {
            let staged = staged!();
            let pass = &mut pass;
            self.with_scope(|scope| script_exec::finish_late(scope, pass, &staged));
        }
        self.with_scope(|scope| script_exec::run_postponed(scope, &pass.postponed, script_exec::Postpone::AfterParse));
        self.with_scope(self::schedule_load);
        Ok(())
    }

    /// The parser waits for one of its responses (the streaming parse) with the event loop running, as
    /// Chrome's does: timers fire, fetches complete, an inserted module runs -- the parser's own modules wait for the
    /// parse's end (`script_exec::ParserWaiting`). Returns when the response is in, or when a script asked for a
    /// navigation.
    async fn parser_wait<F: std::future::Future<Output = ()> + ?Sized>(&mut self, mut fut: std::pin::Pin<&mut F>, render_blocked: bool) -> anyhow::Result<()> {
        self.runtime.op_state().borrow_mut().put(script_exec::ParserWaiting);
        webapi::page::set_render_blocked(&mut self.runtime.op_state().borrow_mut(), render_blocked);
        let end = self.pump_until(std::time::Duration::from_secs(3600), std::future::poll_fn(|cx| fut.as_mut().poll(cx))).await;
        // a drained loop has nothing left that could run before the response: wait for it alone
        if matches!(end, Ok(crate::PumpEnd::Drained)) {
            fut.as_mut().await;
        }
        self.runtime.op_state().borrow_mut().try_take::<script_exec::ParserWaiting>();
        webapi::page::set_render_blocked(&mut self.runtime.op_state().borrow_mut(), false);
        end.map(|_| ())
    }

    /// Number of uncaught JS errors (top-level script throws + throwing timer callbacks) recorded
    /// since the last navigation. Backs CDP `Page.js_error_count`. Call after [`Engine::run`] so
    /// timer callbacks have fired.
    pub fn uncaught_error_count(&mut self) -> i64 {
        webapi::uncaught_error_count(&self.runtime.op_state()) as i64
    }

    /// V8's capture of a stack for uncaught exceptions, on while a DevTools client has the `Runtime` domain enabled
    /// (as Chrome's inspector turns it on for a front-end, 200 frames: `V8Debugger::setMaxCallStackSizeToCapture`)
    /// and off otherwise -- every isolate starts with it off (deno_core's KORVUS PATCH in `runtime/setup.rs`).
    pub fn set_runtime_domain_stacks(&mut self, on: bool) {
        self.runtime.v8_isolate().set_capture_stack_trace_for_uncaught_exceptions(on, 200);
    }

    /// The uncaught errors recorded since the last navigation, as V8's message text and site each --
    /// the Rust store `report_exception` fills (W5.0b). The corpus runner and the `korvus` binary's
    /// probes read THIS, never a page-visible array.
    pub fn uncaught_errors(&mut self) -> Vec<String> {
        webapi::uncaught_errors(&self.runtime.op_state())
    }

    /// The unhandled promise rejections recorded since the last navigation, as
    /// `String((reason && reason.stack) || reason)` each -- the Rust store the native
    /// `unhandledrejection` handler fills.
    ///
    /// It used to `eval` the page-visible JS array `KB.__korvusRejections`, which is exactly the
    /// shape `KB.__korvusUncaught` had before W5.0b: a page could read it, write it, or empty it, and
    /// a harness reading it back could not tell a clean run from a cleared one.
    pub fn unhandled_rejections(&mut self) -> Vec<String> {
        webapi::unhandled_rejections(&self.runtime.op_state())
    }

    /// Parse `html` and install `document` WITHOUT executing inline `<script>`s. Used by harnesses
    /// (e.g. the conformance runner) that inject their own environment and drive the scripts
    /// themselves, so page scripts must not run once here and again under the driver.
    pub fn load_document(&mut self, html: &str) -> anyhow::Result<()> {
        let dom = webapi::dom::Dom::parse(html);
        self.runtime.op_state().borrow_mut().put(webapi::HostDom(dom));
        webapi::page::forget_node_tables(&self.runtime.op_state().borrow());
        self.install_document()?;
        Ok(())
    }

    /// Set the current page URL (backs `window.location`).
    pub fn set_url(&mut self, url: &str) {
        let st = self.runtime.op_state();
        let mut st = st.borrow_mut();
        st.borrow::<HostLocation>().set(url);
        // document.cookie scope follows the URL (backs the shared network cookie jar).
        st.put(webapi::CookieUrl(url.to_string()));
    }

    /// The current page URL (`window.location.href`).
    pub fn current_url(&self) -> String {
        self.runtime.op_state().borrow().borrow::<HostLocation>().get()
    }

    /// Take a pending top-level navigation the page requested (`location.reload`/`assign`/`replace`/
    /// `href=`), clearing it. The driver uses this to run a challenge token reload loop.
    pub fn take_nav_request(&mut self) -> Option<PageNavigation> {
        self.runtime.op_state().borrow_mut().borrow_mut::<webapi::PendingNavigation>().0.take()
    }

    /// The navigation that is queued and not yet taken, WITHOUT taking it. The CDP server reads it
    /// while it is synthesising the LEAVING document's lifecycle events: which of them Chrome
    /// reports at all, and where it announces the navigation, depend on the navigation's reason.
    pub fn peek_due_navigation(&mut self) -> Option<PageNavigation> {
        let st = self.runtime.op_state();
        let st = st.borrow();
        st.try_borrow::<webapi::PendingNavigation>()
            .and_then(|n| n.0.as_ref())
            .filter(|n| !is_javascript_url(&n.url))
            .cloned()
    }

    /// Install `globalThis.document` ([`self::install_document`]) and then give the new
    /// document its IDENTITY.
    ///
    /// The identity half was `KB.__wireDocumentIdentity`, registered by `dom/09i-singletons.js` and
    /// called from inside that script; it is `crate::singletons::wire_document_identity` now, so the bridge
    /// slot is gone and the body is Rust. It runs per NAVIGATION for the reason the JS comment gave:
    /// it needs an interface object (`HTMLDocument`) that only exists after `generated_shells.js`,
    /// AND a document, which only exists once the script above has run.
    ///
    /// AFTER the install rather than inside it -- the one ordering difference from the JS this
    /// replaced, and a safe one: what it does is re-link two [[Prototype]]s and move `location` from `Document.prototype`
    /// onto the instance, and the `__finalizeSurface` pass that used to follow it only sorts and seals
    /// OWN members of prototypes. Deleting one key after that sort leaves the rest in order, and
    /// nothing it adds lands on a prototype.
    fn install_document(&mut self) -> anyhow::Result<()> {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        self::install_document(scope);
        // The SECOND run of the three surface-finishing passes, in the slot the
        // `KB.__finalizeSurface` bridge held: after the install above, which is where `__wireMQL`
        // assigns MediaQueryList's legacy listener aliases, and after BOOTSTRAP's CSSOM block
        // assigned `CSSStyleSheet.prototype.replace`/`replaceSync`. Both REPLACE a function the first
        // run guarded and APPEND a name the first run's sort had not seen, so the passes have to run
        // once more behind them; they are idempotent, so the cost is a lookup per member.
        //
        // BEFORE `wire_document_identity`, not after -- the order the JS had, kept deliberately: that
        // call moves `location` off `Document.prototype` onto the instance, and a sort that runs
        // before the delete leaves the remaining members in the recorded order.
        webapi::install::live::finalize_surface(scope);
        crate::singletons::wire_document_identity(scope);
        // The `[SecureContext]` globals, now that this document's origin is known.
        //
        // THIS IS THE ONLY MOMENT IT CAN RUN. The window surface is installed once, in `Engine::new`,
        // where `HostLocation` is still `about:blank` (which IS potentially trustworthy, so everything
        // is correctly exposed there); the document's URL arrives in `load_page_inner` immediately
        // before this call. A browser answers the question once, when it creates the realm per
        // document; this engine's realm outlives the document, so the answer is applied here instead.
        //
        // AFTER the surface passes and BEFORE this document's first script -- the injected
        // `--eval-early` scripts run after `install_document` returns, which is where Chrome's
        // document-start injection sees the surface, so nothing can observe a name that is about to
        // go. Deleting an own global does not renumber the ones around it, so the order ratchet is
        // untouched (`core/html/exposure.rs`).
        let _removed = webapi::install::live::apply_secure_context_exposure(scope);
        Ok(())
    }
}

// ── the document a navigation ends on ─────────────────────────────────────────────────
//
// L19 E10: nothing performed the navigations a document asks for. `location.href = …` and its
// siblings (P11a) filled the request slot, and only a since-deleted dev panel ever read it; `<meta
// http-equiv=refresh>` was scheduled nowhere. Chrome 150 over loopback, each form against a target
// page titled "landed":
//
// ```text
// fixture                                          chrome 150                         korvus fd6473b8
// <meta http-equiv=refresh content="0; url=t">     /t "landed" (after the first load) stays, "mr0"
// <meta ... content="1; url=t">                    /t, one second after load          stays
// <meta ... content="0;URL='t'"> in <body>         /t                                 stays
// <meta ... content="x; url=t">                    stays (not a refresh)              stays
// Refresh: 0; url=t  (response header)             /t, reason httpHeaderRefresh       stays
// <script>location.href='t'</script> (in parse)    /t; the first document fires no    stays
//                                                  DOMContentLoaded and no load
// location.replace('t') / window.location='t'      /t, same                           stays
// setTimeout(() => location.assign('t'), 50)       /t after the first load            stays
// onload = () => location.href = 't'               /t; load fired, loadEventFired not stays
// ```
//
// The ENGINE's half is here: which navigation is due and when, the abandoned parse, the unload events
// and a new realm for the new document. The DRIVERS -- the CDP server and `korvus fetch` -- decide when
// to ask, and perform the fetch, because the fetch and the protocol are theirs.

// `NavigationReason` AND `PageNavigation` ARE `webapi`'s, and re-exported here so that
// `engine::PageNavigation` still names the same type for the drivers. They moved because the request
// slot moved: `window.open(url, "_self")` and a form submission fill it from `korvus-webapi`, which
// cannot see this crate's privates, and a form's POST body has to travel ON the request.
pub use webapi::{NavigationReason, PageNavigation};

fn without_fragment(u: &str) -> &str {
    u.split_once('#').map(|(a, _)| a).unwrap_or(u)
}

/// The request a driver sends for `nav`, worn by `persona`. ONE builder for both drivers
/// (`korvus fetch`, the CDP server), which each built their own and both sent every navigation as a
/// typed one.
///
/// Measured on Chrome 150 and Brave 151 over loopback (2026-09-30, `tools/realweb/seed/navhdr`): a
/// navigation a DOCUMENT asked for carries that document as its initiator -- `Sec-Fetch-Site` from it,
/// its `Referer` under the default policy, and `Sec-Fetch-User: ?1` only when it was asked for inside a
/// trusted user activation's transient window (`PageNavigation::user_activated`);
/// a reload adds `Cache-Control: max-age=0`, third on the wire after `connection`. One the driver
/// started has no initiator and keeps the typed shape. The request URL has no fragment.
pub fn navigation_request(nav: &PageNavigation, persona: &dyn spec::Persona) -> transport::FetchRequest {
    let mut req = transport::FetchRequest::navigation(without_fragment(&nav.url)).worn_by(persona);
    req.navigation_start = nav.started;
    if let Some(init) = &nav.initiator {
        req.initiator = Some(init.clone());
        req.user_activated = nav.user_activated;
    }
    if !nav.method.eq_ignore_ascii_case("get") {
        req.method = nav.method.to_ascii_uppercase();
    }
    if let Some(body) = &nav.body {
        req.body = Some(body.clone());
    }
    if let Some(ct) = &nav.content_type {
        req.headers.push(("content-type".into(), ct.clone()));
    }
    if nav.reload {
        req.headers.push(("cache-control".into(), "max-age=0".into()));
    }
    req
}

pub use webapi::page::Arrival;

/// How the document `nav` fetched from `final_url` was reached: its `document.referrer`
/// is the `Referer` the final hop carried -- transport's own per-hop rule, over the initiator and the
/// URL the response came from.
pub fn arrival_for(nav: &PageNavigation, final_url: &str) -> Arrival {
    let referrer = nav
        .initiator
        .as_deref()
        .and_then(|i| url::Url::parse(i).ok())
        .zip(url::Url::parse(final_url).ok())
        .and_then(|(i, t)| transport::referer_for(&i, &t))
        .unwrap_or_default();
    // Chrome's transition type for the entry: what the driver said (`typed`, `auto_toplevel`), else the reason's
    let transition = nav.transition.clone().unwrap_or_else(|| {
        match nav.reason {
            _ if nav.reload => "reload",
            NavigationReason::FormSubmissionGet | NavigationReason::FormSubmissionPost => "form_submit",
            _ => "link",
        }
        .to_string()
    });
    Arrival { referrer, reload: nav.reload, history: nav.history.clone(), transition }
}

impl crate::Engine {
    /// Hand this realm the way its next document was reached (see [`arrival_for`]), before it loads.
    pub fn arrive(&mut self, a: Arrival) {
        webapi::page::set_arrival(&self.runtime.op_state(), a);
    }

    /// The tab's session history as `Page.getNavigationHistory` reports it (D88 step 2): the current index and every
    /// entry, the current one titled by its document as it is now.
    pub fn navigation_history(&mut self) -> (usize, Vec<NavEntry>) {
        let title = webapi::page::page_title(&self.runtime.op_state());
        self.with_scope(|scope| {
            let href = crate::location::current_href(scope);
            webapi::page::with_history(scope, href, |h| {
                let (g, i) = (h.generation, h.index);
                if let Some(e) = h.entries.get_mut(i).filter(|e| e.generation == g) {
                    e.title = title;
                }
                let entries = h
                    .entries
                    .iter()
                    .map(|e| NavEntry { serial: e.serial, url: e.url.clone(), title: e.title.clone(), transition: e.transition.clone() })
                    .collect();
                (h.index, entries)
            })
        })
    }

    /// `Page.navigateToHistoryEntry`: traverse to the entry `serial` names, as `navigation.traverseTo` does -- within the
    /// document with `popstate`, or to an earlier document's entry by loading it there. False when there is none.
    pub fn traverse_to_entry(&mut self, serial: u64) -> bool {
        self.with_scope(|scope| {
            let href = crate::location::current_href(scope);
            match webapi::page::with_history(scope, href, |h| h.entries.iter().find(|e| e.serial == serial).map(|e| e.key.clone())) {
                Some(key) => {
                    crate::singletons::browser_traverse_to_key(scope, key);
                    true
                }
                None => false,
            }
        })
    }

    /// A new tab's own `about:blank` as its history's first entry, before its first document commits (see
    /// `webapi::page::seed_blank_history`).
    pub fn seed_blank_history(&mut self) {
        webapi::page::seed_blank_history(&self.runtime.op_state());
    }

    /// The page's same-document navigations since the last call (`pushState`, a fragment, a traversal within the
    /// document): each URL and what `Page.navigatedWithinDocument` calls it.
    pub fn take_same_document_navigations(&mut self) -> Vec<(String, &'static str)> {
        webapi::page::take_same_document(&self.runtime.op_state())
    }

    /// The page document's `document.title`, and the DOM revision it was read at (a title is re-read only when the
    /// revision moved).
    pub fn page_title(&self) -> (String, u64) {
        let st = self.runtime.op_state();
        (webapi::page::page_title(&st), webapi::page::page_dom_revision(&st))
    }

    /// The page DOM's revision now.
    pub fn page_dom_revision(&self) -> u64 {
        webapi::page::page_dom_revision(&self.runtime.op_state())
    }
}

/// One session-history entry, as `Page.getNavigationHistory` lists it.
pub struct NavEntry {
    pub serial: u64,
    pub url: String,
    pub title: String,
    pub transition: String,
}

/// What is to become of the current document: a declarative refresh decided or due, a parse abandoned
/// for a navigation. One per realm, replaced at every document install.
#[derive(Default)]
pub(crate) struct DocumentFate {
    /// The response's `Refresh` header, not yet processed.
    refresh_header: Option<String>,
    /// HTML's "will declaratively refresh": the first valid refresh wins, and a later one is ignored.
    refresh_decided: bool,
    /// A decided refresh, and when it falls due.
    refresh: Option<(std::time::Instant, PageNavigation)>,
    /// A refresh decided but not yet reported by [`crate::Engine::take_scheduled_navigation`].
    refresh_unreported: bool,
    /// The DOM revision the `<meta>` scan last saw, so an unchanged tree is not walked again.
    scanned_revision: Option<u64>,
    loaded_at: Option<std::time::Instant>,
    /// When `DOMContentLoaded` fired, for the same reason `loaded_at` exists.
    dcl_at: Option<std::time::Instant>,
    /// A script navigated while the document was being parsed; it will never reach `DOMContentLoaded`.
    aborted: bool,
    /// A `load` handler navigated.
    navigated_during_load: bool,
}

fn with_fate<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut DocumentFate) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<DocumentFate>().is_none() {
        st.put(DocumentFate::default());
    }
    f(st.borrow_mut::<DocumentFate>())
}

/// Has a script of this realm asked for a cross-document navigation that nobody has performed yet?
///
/// A `javascript:` URL is not one: navigating to it runs its script IN the current document, which is
/// neither unloaded nor abandoned -- `html/semantics/scripting-1/the-script-element/execution-timing/028.html`
/// calls `location.replace('javascript:…')` in the middle of the parse and expects the next inline script
/// to run.
pub(crate) fn navigation_pending(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    navigation_requested(&st)
}

/// [`navigation_pending`], read off the realm's state with no scope -- for `Engine::pump_until`, which asks
/// between two turns of the loop while the loop's future holds the runtime (FIXER C11).
pub(crate) fn navigation_requested(st: &deno_core::OpState) -> bool {
    st.try_borrow::<webapi::PendingNavigation>().and_then(|n| n.0.as_ref()).is_some_and(|n| !is_javascript_url(&n.url))
}

/// Is `url` a `javascript:` URL (the scheme is ASCII case-insensitive)?
fn is_javascript_url(url: &str) -> bool {
    url.trim_start().get(..11).is_some_and(|s| s.eq_ignore_ascii_case("javascript:"))
}

/// HTML's "shared declarative refresh steps" parser -- `webapi`'s, where a FRAME's refresh reads it too;
/// the page's scan below and its tests use it from here.
pub use webapi::page::parse_declarative_refresh;

/// Decide the document's declarative refresh, if it has one and has not decided yet: the `Refresh`
/// header first (it is processed when the document is created), then the first valid `<meta
/// http-equiv=refresh>` in tree order. A refresh is only SCHEDULED once the document has completely
/// loaded, and it falls due `delay` seconds after that -- or after the moment a later-inserted `<meta>`
/// is first seen, whichever is later.
fn schedule_declarative_refresh(scope: &mut v8::PinScope) {
    let (decided, loaded, header, scanned) =
        with_fate(scope, |f| (f.refresh_decided, f.loaded_at, f.refresh_header.clone(), f.scanned_revision));
    if decided {
        return;
    }
    let Some(loaded_at) = loaded else { return };
    let url = crate::location::current_href(scope);
    let mut found: Option<(f64, String, NavigationReason)> = None;
    if let Some(h) = header {
        with_fate(scope, |f| f.refresh_header = None);
        if let Some((d, u)) = parse_declarative_refresh(&h, &url, &url) {
            found = Some((d, u, NavigationReason::HttpHeaderRefresh));
        }
    }
    if found.is_none() {
        let revision = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let Some(dom) = st.try_borrow::<webapi::HostDom>().map(|d| &d.0) else { return };
            let revision = dom.revision();
            if scanned == Some(revision) {
                return;
            }
            let base = webapi::document_base_url(dom, &url);
            // THE PAGE DOCUMENT'S <meta>s: the arena also holds template contents, DOMParser
            // documents and every FRAME's document, and a frame's refresh took the page away -- Google's DRT
            // cookie-sync frame (`pagead/drt/s`: `<meta http-equiv="refresh" content="0;url=…/drt/ui">`) ended
            // stackoverflow.com on `drt/si?st=NO_DATA` (F35b). A frame refreshes itself (F37).
            for n in dom.get_elements_by_tag_name_in(dom.root(), "meta") {
                let refresh = dom.get_attribute(n, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("refresh"));
                if !refresh {
                    continue;
                }
                let Some(content) = dom.get_attribute(n, "content").filter(|c| !c.is_empty()) else { continue };
                // A `<meta>` that is not a refresh (`content="x; url=…"`) is skipped, and the next one in
                // tree order still gets its turn: only a VALID refresh sets "will declaratively refresh".
                if let Some((d, u)) = parse_declarative_refresh(content, &base, &url) {
                    found = Some((d, u, NavigationReason::MetaTagRefresh));
                    break;
                }
            }
            revision
        };
        with_fate(scope, |f| f.scanned_revision = Some(revision));
    }
    let Some((delay, target, reason)) = found else { return };
    let from = std::time::Instant::now().max(loaded_at);
    // A delay no timer can express is a refresh that never comes; it still decides.
    let due = std::time::Duration::try_from_secs_f64(delay).ok().and_then(|d| from.checked_add(d));
    // A refresh to the document's own URL (fragment aside) is a RELOAD in Chrome (measured: `max-age=0`
    // on the request, type `reload` in the new document); to any other URL it is a plain navigation.
    // Either way the document is its initiator. And it REPLACES the document's entry (HTML's shared declarative
    // refresh steps navigate with "replace"; Chrome 150's `navigation.activation.navigationType` reads `replace`, and
    // `reload` for one to its own URL) -- it went in as a push.
    let reload = without_fragment(&target) == without_fragment(&url);
    let initiator = (url.starts_with("http:") || url.starts_with("https:")).then(|| url.clone());
    let history = if reload { webapi::page::HistoryCommit::Reload } else { webapi::page::HistoryCommit::Replace };
    with_fate(scope, |f| {
        f.refresh_decided = true;
        if let Some(at) = due {
            f.refresh = Some((at, PageNavigation { delay_s: delay, reload, initiator, history, ..PageNavigation::get(target, reason) }));
            f.refresh_unreported = true;
        }
    });
}

impl crate::Engine {
    /// Is a script's navigation queued and not yet taken?
    pub fn navigation_pending(&mut self) -> bool {
        self.with_scope(navigation_pending)
    }

    /// A declarative refresh that has just been SCHEDULED (not yet due), reported once. Chrome announces
    /// one as `Page.frameScheduledNavigation` right after the document's load; a driver that says the
    /// same asks here.
    pub fn take_scheduled_navigation(&mut self) -> Option<PageNavigation> {
        self.with_scope(|scope| {
            schedule_declarative_refresh(scope);
            with_fate(scope, |f| {
                if !f.refresh_unreported {
                    return None;
                }
                f.refresh_unreported = false;
                f.refresh.as_ref().map(|(_, n)| n.clone())
            })
        })
    }

    /// When the scheduled declarative refresh falls due, if one is scheduled.
    pub fn navigation_due_at(&mut self) -> Option<std::time::Instant> {
        self.with_scope(|scope| {
            schedule_declarative_refresh(scope);
            with_fate(scope, |f| f.refresh.as_ref().map(|(at, _)| *at))
        })
    }

    /// The navigation the current document asks for NOW, taken: a script's queued navigation first, then
    /// a declarative refresh whose time has come. `None` leaves both where they are.
    ///
    /// A script's navigation cancels a scheduled refresh: the refresh belongs to a document that is going
    /// away.
    pub fn take_due_navigation(&mut self) -> Option<PageNavigation> {
        if let Some(nav) = self.take_nav_request() {
            // HTML "evaluate a javascript: URL": the script runs in THIS document, and nothing is fetched
            // or unloaded. (A string result would replace the document; this engine does not do that
            // part, and says so here rather than fetching the URL as if it were a page.)
            if is_javascript_url(&nav.url) {
                let body = nav.url.trim_start()[11..].to_string();
                let src = percent_encoding::percent_decode_str(&body).decode_utf8_lossy().into_owned();
                let _ = self.eval("", src);
                return self.take_due_navigation();
            }
            self.with_scope(|scope| with_fate(scope, |f| f.refresh = None));
            return Some(nav);
        }
        self.with_scope(|scope| {
            schedule_declarative_refresh(scope);
            with_fate(scope, |f| {
                let due = f.refresh.as_ref().is_some_and(|(at, _)| std::time::Instant::now() >= *at);
                if due {
                    f.refresh_unreported = false;
                    // A declarative refresh's navigation begins when it falls due, not when it was parsed.
                    f.refresh.take().map(|(_, n)| PageNavigation { started: Some(std::time::Instant::now()), ..n })
                } else {
                    None
                }
            })
        })
    }

    /// Did a script abandon this document's parse by navigating? Then it never fired
    /// `DOMContentLoaded` or `load`, and a driver must not report either.
    pub fn document_abandoned(&mut self) -> bool {
        self.with_scope(|scope| with_fate(scope, |f| f.aborted))
    }

    /// Did a `load` handler start a navigation? Chrome then reports `DOMContentLoaded` but not
    /// `Page.loadEventFired` for this document.
    pub fn navigated_during_load(&mut self) -> bool {
        self.with_scope(|scope| with_fate(scope, |f| f.navigated_during_load))
    }

    /// WHEN this document's `DOMContentLoaded` and `load` fired, for the driver that
    /// has to stamp `Page.domContentEventFired` and `Page.loadEventFired` with the moment the event
    /// happened -- which is what Chrome stamps them with, and what a client subtracting a request's
    /// timestamp from `load`'s is measuring. Either is `None` when that event has not fired.
    ///
    /// THE DEFECT THIS EXISTS TO CLOSE. `plugins/korvus-cdp` took ONE `now()` after the whole
    /// navigation returned and wrote it into all four lifecycle events, so every one of them was
    /// stamped with the end of the navigation's pump rather than with itself. The pump returns a
    /// 25 ms slice late and then settles for a further 50 ms after `load`, so `load_ms` -- §6's
    /// `nav_to_load_ms`, the metric the efficiency box is failing worst on -- carried ~75 ms of the
    /// driver's own tail on every page. Chrome's own is the event's time; so is this.
    pub fn lifecycle_marks(&mut self) -> (Option<std::time::Instant>, Option<std::time::Instant>) {
        self.with_scope(|scope| with_fate(scope, |f| (f.dcl_at, f.loaded_at)))
    }

    /// Set `mark` to the instant this document's `load` event fires (the moment [`Engine::lifecycle_marks`] reports
    /// later): a host reporting the load while it runs -- korvus-cdp's network flush, with the engine busy loading --
    /// learns it there.
    pub fn mark_load_in(&mut self, mark: crate::LoadMark) {
        self.runtime.op_state().borrow_mut().put(crate::LoadMarkSlot(mark));
    }

    /// `beforeunload` at the window: the first step of leaving a document, before the next one is
    /// requested. A `BeforeUnloadEvent` (only `document.createEvent` mints one); cancelling it prompts
    /// nobody, since there is no user here to ask.
    pub fn fire_beforeunload(&mut self) {
        self.with_scope(|scope| {
            quietly(scope, |scope| {
                let Some(doc) = document(scope) else { return };
                let kind = str_val(scope, "BeforeUnloadEvent");
                let Some(ev) = call_method(scope, doc.into(), "createEvent", &[kind]) else { return };
                let ty = str_val(scope, "beforeunload");
                let no = v8::Boolean::new(scope, false);
                let yes = v8::Boolean::new(scope, true);
                call_method(scope, ev, "initEvent", &[ty, no.into(), yes.into()]);
                let g = global(scope);
                webapi::binding::protos::engine_call(scope, g.into(), "dispatchEvent", &[ev]);
            });
        });
    }

    /// HTML "unload a document", once the new document's response has arrived: `pagehide` (a
    /// `PageTransitionEvent`, `persisted` false -- this engine keeps no back/forward cache), the
    /// visibility turning `hidden` with its `visibilitychange`, then `unload`. Chrome 150's order,
    /// measured: `beforeunload pagehide:false vis:hidden unload`.
    pub fn fire_unload(&mut self) {
        self.with_scope(|scope| {
            quietly(scope, |scope| {
                let g = global(scope);
                let ty = str_val(scope, "pagehide");
                let init = webapi::binding::protos::init_dict(scope);
                let k = str_val(scope, "persisted");
                let f = v8::Boolean::new(scope, false);
                init.set(scope, k, f.into());
                let ctor = get_prop(scope, g, "PageTransitionEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
                match ctor.and_then(|c| c.new_instance(scope, &[ty, init.into()])) {
                    Some(ev) => {
                        webapi::binding::protos::engine_call(scope, g.into(), "dispatchEvent", &[ev.into()]);
                    }
                    None => fire(scope, g.into(), "pagehide"),
                }
            });
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            webapi::page::set_page_hidden(&op_state);
            if let Some(doc) = document(scope) {
                fire_with(scope, doc.into(), "visibilitychange", true);
            }
            let g = global(scope);
            fire(scope, g.into(), "unload");
        });
    }

    /// The engine the NEXT document loads in: a fresh isolate and realm wearing the same persona, holding
    /// the session's storage areas. The current engine is dropped first -- V8 requires isolates on one
    /// thread to be dropped in the reverse order of their creation, and this one was created first.
    ///
    /// A new realm, not the old one re-used, because that is what a cross-document navigation IS: the
    /// old document's globals, listeners and timers do not survive it. Re-using the realm would run a
    /// first page's `window.addEventListener("load", …)` on the second page's load.
    pub fn successor(mut self) -> crate::Engine {
        let persona = self.persona();
        let storage = webapi::page::take_storage(&self.runtime.op_state());
        // ONE session history for the tab (D88 step 2): the realms' entries together, as Chrome's browser keeps them
        // -- `history.length` was 1 on every document, and `back()` to an earlier one did nothing.
        let title = webapi::page::page_title(&self.runtime.op_state());
        let history = self.with_scope(|scope| webapi::page::carry_history(scope, title));
        // ONE browsing session, one farbling seed: the next document's engine drew a
        // fresh one, so a Brave persona's farbled `hardwareConcurrency`/`deviceMemory` changed across a
        // navigation on the same site -- a bot challenge reload among them. Brave keeps them.
        let seed = self.runtime.op_state().borrow().try_borrow::<spec::farble::SessionSeed>().copied();
        // What the leaving document logged after its last drain -- its `pagehide` and `unload` handlers', which Chrome
        // 150 does not report -- and any other handle this thread keeps into its realm, released while the isolate
        // they belong to is alive: drained after this, in the next document's isolate, they panicked.
        webapi::release_realm_handles();
        drop(self);
        let mut next = crate::Engine::assemble_shared(persona).expect("the persona the previous document wore assembles again");
        webapi::page::restore_storage(&next.runtime.op_state(), storage);
        if let Some(h) = history {
            next.with_scope(|scope| webapi::page::restore_history(scope, h));
        }
        if let Some(seed) = seed {
            next.runtime.op_state().borrow_mut().put(seed);
        }
        next
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::parse_declarative_refresh;

    fn p(input: &str) -> Option<(f64, String)> {
        let doc = "http://h/dir/page.html";
        parse_declarative_refresh(input, doc, doc)
    }

    /// The shared declarative refresh steps over the forms Chrome was asked about, plus the spec's own.
    #[test]
    fn declarative_refresh_parses_like_the_spec() {
        let doc = "http://h/dir/page.html".to_string();
        assert_eq!(p("0; url=target.html"), Some((0.0, "http://h/dir/target.html".into())));
        assert_eq!(p("1; url=target.html"), Some((1.0, "http://h/dir/target.html".into())));
        assert_eq!(p("0;URL='target.html'"), Some((0.0, "http://h/dir/target.html".into())));
        assert_eq!(p("x; url=target.html"), None);
        assert_eq!(p("0.5"), Some((0.0, doc.clone())));
        assert_eq!(p("5"), Some((5.0, doc.clone())));
        assert_eq!(p("3, url = \"a b.html\" junk"), Some((3.0, "http://h/dir/a%20b.html".into())));
        assert_eq!(p("0; /abs"), Some((0.0, "http://h/abs".into())));
        assert_eq!(p("0 url=x"), Some((0.0, "http://h/dir/x".into())));
        assert_eq!(p("0x"), None);
        assert_eq!(p(".5; url=x"), Some((0.0, "http://h/dir/x".into())));
    }
}

/// A written script at a `blob:` or `data:` URL is answered here, as `op_fetch` answers them: the
/// HTTP client has no such scheme ("builder error: invalid authority", lowes.com). A `blob:` the page never
/// minted, or revoked, is `None` -> the `error` event.
/// `n` is inside the document's `<head>` -- where the parser meets it before the body exists, so a load that blocks it
/// is render-blocking (the streaming parse's render-blocked waits).
fn in_head(dom: &webapi::dom::Dom, n: u32) -> bool {
    let mut at = dom.parent(n);
    while let Some(a) = at {
        if dom.tag_name(a).is_some_and(|t| t.eq_ignore_ascii_case("head")) {
            return true;
        }
        at = dom.parent(a);
    }
    false
}

fn written_local_response(state: &deno_core::OpState, url: &str) -> Option<(Option<script_exec::Fetched>, Option<spec::FetchTiming>)> {
    let scheme = url.get(..5)?;
    let found = if scheme.eq_ignore_ascii_case("blob:") {
        webapi::blob_url_bytes(state, url).map(|(ty, bytes)| (ty, bytes.as_ref().clone()))
    } else if scheme.eq_ignore_ascii_case("data:") {
        webapi::decode_data_url(url)
    } else {
        return None;
    };
    Some((
        found.map(|(ty, bytes)| script_exec::Fetched {
            status: 200,
            headers: if ty.is_empty() { Vec::new() } else { vec![("content-type".to_string(), ty)] },
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }),
        None,
    ))
}

/// A parser-blocking `<script src>` that `document.write` produced, fetched with the
/// parser pass's own request shape -- destination `script`, mode `no-cors`, the page as initiator, the
/// persona's identity and wire -- while the parse waits for it.
async fn fetch_written_script(
    identity: Option<&transport::PersonaIdentity>,
    initiator: &str,
    url: &str,
    referrer_policy: Option<transport::ReferrerPolicy>,
) -> (Option<script_exec::Fetched>, Option<spec::FetchTiming>) {
    let mut req = transport::FetchRequest {
        referrer_policy,
        url: url.to_string(),
        method: "GET".to_string(),
        headers: Vec::new(),
        body: None,
        destination: transport::Destination::from("script"),
        mode: transport::Mode::NoCors,
        initiator: Some(initiator.to_string()),
        user_activated: false,
        wire: None,
        ..Default::default()
    };
    if let Some(id) = identity {
        transport::apply_persona_identity(id, &mut req);
    }
    let trace = std::env::var_os("KORVUS_TRACE_FETCH").is_some();
    if trace {
        eprintln!("[subresource] GET {url} (written, parser-blocking)");
    }
    match transport::fetch(req).await {
        Ok(resp) => {
            if trace {
                eprintln!("[subresource] <- {} ({} bytes)", resp.status, resp.body.len());
            }
            let timing = Some(resp.timing.clone());
            (Some(script_exec::Fetched { status: resp.status, headers: resp.headers, body: resp.body }), timing)
        }
        Err(_) => (None, None),
    }
}
