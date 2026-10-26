//! `core/html/html_script_element.rs` — `HTMLScriptElement` as one `interface!` table, and the
//! "inserting a `<script>` runs it" algorithm behind it.
//!
//! `js/dom/08t-script-element.js` (156 lines, fifteen `Object.defineProperty` sites, two
//! `op_helper_*` native masks, three `KB.__` bridge slots) is deleted. Seventeen rows replace its
//! sixteen: the JS never installed `blocking`, and the registry's BACKFILL pass supplied it and
//! `innerText` afterwards — which is a load order deciding a member list, and is why the table
//! carries the union the capture records rather than the union of two passes.
//!
//! Most of the interface is reflection, and every one of those rows is a shared kit naming exactly
//! the builder the deleted line called. Four are not, and each is a different kind of
//! not-a-reflection:
//!
//!   * `async` is `hasAttribute("async") || force-async` — see below.
//!   * `crossOrigin` is a nullable enumerated reflection: `null` while the attribute is absent, and
//!     "anonymous" for any value that is not a keyword. No missing-default string can express the
//!     null, hence the `reflect_nullable_enum` kit. Chrome 151, measured: absent is `null`, a bogus
//!     value is "anonymous", and `crossorigin=""` is "anonymous" too.
//!   * `text` is the element's CHILD TEXT CONTENT, not an attribute — reading it concatenates the
//!     child Text nodes, writing it replaces every child with one. It was the file's two native
//!     masks: `op_helper_get`/`op_helper_set` over the `KB.__scriptTextGet`/`__scriptTextSet` bridge
//!     slots, i.e. a JS body presented to the page as `[native code]`. It is a Rust body now and the
//!     masks and slots are gone (invariant I3).
//!   * `innerText` is an honest D2 `stub` row. Chrome owns it on THIS prototype (Trusted Types
//!     intercepts writes to a script's text there) and this engine has no innerText implementation
//!     anywhere to delegate to. The registry's backfill mounted the same stub before this commit.
//!
//! `async` is the interesting one. It is NOT `hasAttribute("async")`: Blink's
//! `HTMLScriptElement::async()` is `FastHasAttribute(kAsyncAttr) || loader_->IsForceAsync()`, and
//! the force-async flag starts TRUE (`script_loader.h`: `bool force_async_ = true;`). It is set to
//! false in exactly three places: when the HTML or XML parser inserts the element
//! (`script_loader.cc:123`), when an `async` content attribute is added (`:220`), and when the IDL
//! setter runs. So a `<script>` the page creates with `document.createElement` reports
//! `async === true` before anything is set on it, while a `<script>` the parser inserted reports
//! whatever its attribute says. Both of those fall out of one rule here: the flag is stamped on the
//! element at `createElement` time, and nothing stamps it during parsing.

use deno_core::v8;

use crate::binding::protos::{get_prop, global_get, str_val};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::priv_set;
use crate::core::dom::text::{cb_text_content_get, cb_text_content_set};
use crate::install::live::{method_row, pair_row};
use crate::install::MemberDef;

/// The force-async flag, as a per-element private slot. Its PRESENCE is the flag: an element the
/// parser built never gets one.
const FORCE_ASYNC: &str = "korvus_script_force_async";

/// Stamp a script element created by `document.createElement`. Called from the createElement path in
/// `core/dom/document.rs` — the parser does not go through there, which is exactly the distinction
/// the spec draws.
pub(crate) fn mark_force_async(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) {
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, obj, FORCE_ASYNC, t);
}

/// Is the force-async flag still set on this element? (the element loader reads it when
/// a script-inserted `<script src>` is prepared, to decide whether it joins the in-order list.)
pub(crate) fn force_async(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> bool {
    priv_get(scope, obj, FORCE_ASYNC).is_some()
}

fn has_attr(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> bool {
    let a = str_val(scope, name);
    crate::core::dom::element::attr_call(scope, this.into(), "hasAttribute", &[a])
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false)
}

fn cb_script_async_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if has_attr(scope, this, "async") { rv.set_bool(true); return; }
    rv.set_bool(priv_get(scope, this, FORCE_ASYNC).is_some());
}

/// The IDL setter clears the force-async flag whichever way it is called — "Set this's force async
/// to false" is step 1 of the setter, before the attribute is touched — and then reflects normally.
fn cb_script_async_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    priv_set(scope, this, FORCE_ASYNC, undef);
    let on = args.get(0).boolean_value(scope);
    let a = str_val(scope, "async");
    if on {
        let empty = str_val(scope, "");
        crate::core::dom::element::attr_call(scope, this.into(), "setAttribute", &[a, empty]);
    } else {
        crate::core::dom::element::attr_call(scope, this.into(), "removeAttribute", &[a]);
    }
}

/// `HTMLScriptElement.supports(type)` — a static, and a closed set. Blink's list verbatim
/// (`html_script_element.cc`): classic, module, importmap, speculationrules, webbundle.
const SUPPORTED: [&str; 5] = ["classic", "module", "importmap", "speculationrules", "webbundle"];

fn cb_script_supports(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = args.get(0).to_rust_string_lossy(scope);
    rv.set_bool(SUPPORTED.contains(&t.as_str()));
}

// ── `text`, `textContent` and the table ─────────────────────────────────────────────────────────

/// The element's CHILD TEXT CONTENT -- the concatenation of its child `Text` nodes' `data`. Not
/// `textContent`, which descends; `<script>`'s `text` is defined over the direct children only.
///
/// Read off the arena, never through `childNodes` / `nodeType` / `data`: those are page-visible
/// getters, and a page wrapping them watched this engine read its scripts (a bot challenge
/// frame counts `Node.nodeType` calls). Blink's `ScriptElementBase::ChildTextContent` reads the tree.
fn child_text(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> String {
    crate::page::child_text_content(scope, el.into())
}

fn cb_script_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = child_text(scope, args.this());
    let v = str_val(scope, &s);
    rv.set(v);
}

/// "Set this's child text content to the given value" -- one Text node replacing every child, which
/// is exactly what assigning `textContent` does. Chrome 151, measured: after appending a text node
/// and a comment and then assigning `s.text = "zz"`, `childNodes.length` is 1 and `textContent` is
/// "zz".
///
/// `(TrustedScript or DOMString)`: a TrustedScript is read as itself, nothing on it called; and the text is set by
/// the realm's own steps -- this assigned through the page's `textContent` setter.
fn cb_script_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let s = match crate::core::trusted_types::tables::html_script_id(scope, this) {
        Some(id) => crate::core::trusted_types::tables::script_text_sink(scope, id, Some(args.get(0)), "text"),
        None => crate::core::trusted_types::tables::sink_text(scope, args.get(0), "TrustedScript"),
    };
    let Some(s) = s else { return };
    crate::core::dom::text::set_text_content_of(scope, this, &s);
}

pub fn install_html_script_element(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "async" => pair_row(scope, target, member, cb_script_async_get, cb_script_async_set),
        "text" => pair_row(scope, target, member, cb_script_text_get, cb_script_text_set),
        "textContent" => pair_row(scope, target, member, cb_text_content_get, cb_text_content_set),
        // THE ELEMENT'S OWN innerText. Chrome owns the accessor on this prototype (Trusted
        // Types intercepts script-text writes there) and its bodies are HTMLElement's: the setter makes
        // `#text`, `<br>`, `#text` out of "a\nb", and the getter of a script -- which is not rendered --
        // is its descendant text. It was a stub, so a script filled with `s.innerText = code` and
        // inserted ran NOTHING (who.int, daangn, cursor, mitpress, gamedev: measured with the stub-call
        // counter over the failing replay rows; Chrome 150 runs it).
        "innerText" => pair_row(scope, target, member, crate::core::html::html_element::cb_he_inner_text_get, crate::core::html::html_element::cb_he_inner_text_set),
        "supports" => method_row(scope, target, member, cb_script_supports),
        other => debug_assert!(false, "install_html_script_element: unexpected member {other}"),
    }
}

webapi::interface! {
    // Rows VERBATIM from the `HTMLScriptElement` block of `install/generated_registry.rs` -- names,
    // order, kinds, descriptor flags, lengths, `required` counts and nullability -- which is the
    // capture's own prototype order.
    "HTMLScriptElement" : "HTMLElement" [element, window] {
        ctor          "HTMLScriptElement"                    len 0            = stub,
        attr          "src"            : UsvString cfg "src"                  = reflect_url,
        attr          "type"           : DomString cfg "type"                 = reflect_str,
        attr          "noModule"       : Boolean   cfg "nomodule"             = reflect_bool,
        attr          "charset"        : DomString cfg "charset"              = reflect_str,
        attr          "async"          : Boolean                              = install_html_script_element,
        attr          "defer"          : Boolean   cfg "defer"                = reflect_bool,
        // A NULLABLE enumerated reflection: `null` while the attribute is absent (no missing-value
        // string can express that), and "anonymous" for any present value that is not a keyword.
        attr          "crossOrigin"    : DomString nullable
            cfg "crossorigin|anonymous,use-credentials|anonymous"             = reflect_nullable_enum,
        attr          "text"           : DomString                            = install_html_script_element,
        attr          "referrerPolicy" : DomString
            cfg "referrerpolicy|no-referrer,no-referrer-when-downgrade,same-origin,origin,strict-origin,origin-when-cross-origin,strict-origin-when-cross-origin,unsafe-url||"
                                                                              = reflect_enum,
        attr          "fetchPriority"  : DomString cfg "fetchpriority|high,low,auto|auto|auto" = reflect_enum,
        attr          "event"          : DomString cfg "event"                = reflect_str,
        attr          "htmlFor"        : DomString cfg "for"                  = reflect_str,
        attr          "integrity"      : DomString cfg "integrity"            = reflect_str,
        // `blocking` is a DOMTokenList over the `blocking` content attribute -- the registry's own
        // `reflect_tokens` row, which this table carries verbatim. The deleted JS did not install it
        // at all (its header called the interface "deliberately absent"); the registry's BACKFILL
        // pass supplied it afterwards, which is how it reached the surface.
        attr          "blocking"       : Object    cfg "blocking"             = reflect_tokens,
        attr          "textContent"    : DomString nullable                   = install_html_script_element,
        // Chrome owns `innerText` on this prototype (Trusted Types intercepts writes to a script's
        // text there); its bodies are HTMLElement's.
        attr          "innerText"      : DomString                            = install_html_script_element,
        static_method "supports"       : Boolean   len 1 required 1           = install_html_script_element,
        // THE OTHER DEVICE'S ROW(S): what `chrome-152-linux-20261016T1022`
        // records here and `brave-151-linux-20260907T1006` does not. Installed only when the
        // ACTIVE capture lists them (`install::installer::join`), so neither device grows the
        // other's surface; appended rather than interleaved because this table's order is the
        // fallback a realm with no bundle selector walks, not the order a page reads. D2 stubs.
        attr          "attributionSrc" : Any                                  = stub
    }
}

// ── inserting a <script> into the document RUNS it ──────────────────────────────────────────────
//
// Called from the native insertion path (`core/dom/node.rs::notify_connected`) for every node that
// becomes connected. Before this existed, a script only ever executed if the PARSER found it at load
// time, so `document.body.appendChild(s)` -- how every analytics snippet, every polyfill loader and
// 464 subtests of html/semantics/scripting-1 inject code -- silently did nothing.
//
// It was `KB.__runInsertedScript`, 45 lines of JS behind a bridge slot, and the comment at the call
// site said the JS owned it "because it needs indirect eval to reach global scope and that is not
// something a native callback can hand back". That was already untrue when it was written:
// `core/html/subresource.rs::run_classic_script` -- the path a fetched `<script src>` takes -- is a
// top-level `v8::Script::compile` + `run`, which IS global scope, and is what indirect `eval` was
// standing in for. The two paths now agree by construction instead of by coincidence.

/// HTML's "already started" flag, one entry per script element (arena id). Once set, neither an
/// insertion nor a `src` change prepares the element again, so moving or re-inserting a script never
/// re-runs it. It lives in op state and not on the wrapper because most of its writers never see a
/// wrapper (measured on Chrome 150 by `tools/realweb/seed/dup`): the page's parser marks
/// every script it prepares, the fragment parser marks everything it creates (so an `innerHTML`
/// script moved into the document stays inert), a parsed document where scripting is disabled
/// (`DOMParser`) marks its scripts, and the cloning steps copy the flag -- a clone of an executed
/// `<script src>` inserted into the page does not fetch or run. Arena ids are only meaningful for
/// one document, so a new page document starts from an empty set ([`mark_parser_scripts_started`]).
#[derive(Default)]
pub(crate) struct ScriptsStarted(std::cell::RefCell<std::collections::HashSet<u32>>);

type OpStateRc = std::rc::Rc<std::cell::RefCell<deno_core::OpState>>;

pub(crate) fn script_started(op_state: &OpStateRc, id: u32) -> bool {
    op_state.borrow().try_borrow::<ScriptsStarted>().is_some_and(|s| s.0.borrow().contains(&id))
}

pub(crate) fn mark_script_started(op_state: &OpStateRc, id: u32) {
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<ScriptsStarted>().is_none() {
        st.put(ScriptsStarted::default());
    }
    st.borrow::<ScriptsStarted>().0.borrow_mut().insert(id);
}

fn is_script_element(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.node_type(id) == 1 && dom.tag_name(id).is_some_and(|t| t.rsplit(':').next().unwrap_or(t).eq_ignore_ascii_case("script"))
}

/// "Prepare the script element" as far as its already-started step, for a script a PARSER created:
/// it gets there only with a `src` attribute or non-empty text, and a type that is script, module or
/// import map (steps 5-11). An empty or `type="text/template"` parser script stays unstarted, so a page
/// may still fill or retype it and insert it again.
fn parser_prepares(dom: &korvus_dom::Dom, id: u32) -> bool {
    if dom.get_attribute(id, "src").is_none() && dom.text_content(id).is_empty() {
        return false;
    }
    let ty = dom.get_attribute(id, "type");
    element_script_type_runs(dom, id) || ty.is_some_and(is_import_map_type)
}

/// Mark every script under `roots` (shadow-including) that a parser would have prepared.
pub(crate) fn mark_parsed_scripts(op_state: &OpStateRc, roots: &[u32]) {
    let ids: Vec<u32> = {
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return };
        let dom = &h.0;
        let mut out = Vec::new();
        let mut stack: Vec<u32> = roots.to_vec();
        while let Some(n) = stack.pop() {
            if is_script_element(dom, n) && parser_prepares(dom, n) {
                out.push(n);
            }
            if let Some(sr) = dom.shadow_root_of(n) {
                stack.push(sr);
            }
            stack.extend(dom.children(n));
        }
        out
    };
    for id in ids {
        mark_script_started(op_state, id);
    }
}

/// A new page document: forget the previous arena's flags, then mark the scripts its parser owns
/// (the runtime runs those itself, in document order).
pub fn mark_parser_scripts_started(op_state: &OpStateRc) {
    let root = {
        let mut st = op_state.borrow_mut();
        if let Some(s) = st.try_borrow_mut::<ScriptsStarted>() {
            s.0.borrow_mut().clear();
        }
        let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return };
        (h.0.root(), h.0.pending_patch_scripts())
    };
    let (root, patch_scripts) = root;
    mark_parsed_scripts(op_state, &[root]);
    // The scripts of the `<template for>` patches still to stream are the parser's too: the
    // page's script pass runs them as their patch arrives.
    for id in patch_scripts {
        mark_script_started(op_state, id);
    }
}

/// The script element's cloning steps: the copy inherits "already started". `to` is a clone of `from`
/// (deep or not), so the two subtrees have the same shape and are walked in step.
pub(crate) fn clone_started(op_state: &OpStateRc, from: u32, to: u32) {
    let any = op_state.borrow().try_borrow::<ScriptsStarted>().is_some_and(|s| !s.0.borrow().is_empty());
    if !any {
        return;
    }
    let pairs: Vec<u32> = {
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return };
        let dom = &h.0;
        let started = st.borrow::<ScriptsStarted>().0.borrow();
        let mut out = Vec::new();
        let mut stack = vec![(from, to)];
        while let Some((a, b)) = stack.pop() {
            if started.contains(&a) {
                out.push(b);
            }
            stack.extend(dom.children(a).into_iter().zip(dom.children(b)));
        }
        out
    };
    for id in pairs {
        mark_script_started(op_state, id);
    }
}

/// The script element's "children changed steps": a connected script that has not started runs
/// when a node is inserted into it -- `s = createElement("script"); head.appendChild(s);
/// s.text = code` runs `code` in Chrome. The started flag makes this a no-op for everything else.
/// Set while `moveBefore` reports its move. A script element defines no MOVING steps, so a node moved
/// INTO a script is not that script's children-changed steps and runs nothing (WPT
/// dom/nodes/moveBefore/script-move-before: `script.moveBefore(text, null)` into a parser-made empty
/// script must not execute it).
#[derive(Default)]
pub(crate) struct MovingNodes(std::cell::Cell<bool>);

pub(crate) fn set_moving(op_state: &OpStateRc, on: bool) {
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<MovingNodes>().is_none() {
        st.put(MovingNodes::default());
    }
    st.borrow::<MovingNodes>().0.set(on);
}

pub(crate) fn script_children_changed(scope: &mut v8::PinScope, parent: v8::Local<v8::Value>) {
    let Some(id) = crate::binding::protos::unwrap_id(scope, parent) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let runs = {
        let st = op_state.borrow();
        if st.try_borrow::<MovingNodes>().is_some_and(|m| m.0.get()) {
            return;
        }
        let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return };
        is_script_element(&h.0, id)
    };
    // Connected: in the page's document or a frame's (see `run_inserted_script`).
    let runs = runs && crate::core::html::subresource::home_of(scope, id) != crate::core::html::subresource::Home::Nowhere;
    if runs && !script_started(&op_state, id) {
        run_inserted_script(scope, parent);
    }
}

/// `document.currentScript` for a script about to run: the element, or null when it is in a shadow
/// tree (HTML "execute the script element", step 5.1).
pub(crate) fn current_script_value<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let el = v8::Local::new(scope, el);
    let id = crate::binding::protos::unwrap_id(scope, el);
    let in_shadow = id.is_some_and(|id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<crate::binding::HostDom>().is_some_and(|h| h.0.is_shadow_root(h.0.root_of(id)))
    });
    if in_shadow { v8::null(scope).into() } else { el }
}

/// HTML's "JavaScript MIME type essence match" -- a fixed list, not a substring test. It matters in
/// both directions: `type="javascript"` is NOT a MIME type and must not run, while
/// `type="application/ecmascript"` must.
const JS_TYPES: [&str; 16] = [
    "application/ecmascript", "application/javascript", "application/x-ecmascript",
    "application/x-javascript", "text/ecmascript", "text/javascript", "text/javascript1.0",
    "text/javascript1.1", "text/javascript1.2", "text/javascript1.3", "text/javascript1.4",
    "text/javascript1.5", "text/jscript", "text/livescript", "text/x-ecmascript", "text/x-javascript",
];

/// The legacy `language` attribute's accepted values: the same tail, unprefixed, and NOT the 1.6+ ones.
const LANGUAGES: [&str; 13] = [
    "", "ecmascript", "javascript", "javascript1.0", "javascript1.1", "javascript1.2",
    "javascript1.3", "javascript1.4", "javascript1.5", "jscript", "livescript", "x-ecmascript",
    "x-javascript",
];

/// ASCII whitespace, per HTML: tab, LF, FF, CR, space. NOT Rust's `trim`, which strips every Unicode
/// whitespace -- an embedded NUL is not whitespace either, and makes the type invalid.
fn html_trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '))
}

/// Is `essence` (already lowercased, parameters cut) one of the JavaScript MIME types? The one list,
/// shared by the `type` attribute test and the response checks below.
pub(crate) fn is_javascript_mime_essence(essence: &str) -> bool {
    JS_TYPES.contains(&essence)
}

// ---- which responses a `<script src>` may run --------------------------------------
//
// THE DEFECT THIS SECTION EXISTS FOR. The parser-inserted script path executed a fetched body whatever
// the response said: a 404 whose body is an HTML error page was handed to V8, threw
// `Unexpected identifier`, and the element's `error` event never fired -- so a page that falls back to
// a second CDN from `onerror` took the wrong branch. The dynamically inserted path already refused
// non-2xx, with the wrong boundary. Both paths now ask these two functions.

/// Classic or module: the two script flavours check a response differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScriptFlavor {
    Classic,
    Module,
}

/// Is this response one a `<script src>` of `flavor` executes? `false` means: do not run the body,
/// fire `error` at the element.
///
/// Measured on stock Chrome 150.0.7871.46 over loopback, parser-inserted AND inserted from script
/// (both agree on every row), a JavaScript body in each response:
///
/// ```text
/// STATUS   200 204 206 299 300 301 304 307 399            load (a body, if any, runs)
///          300/301/307 without a Location are served as the response, and run
///          400 401 403 404 410 500                         error, the body does NOT run
///          302 -> 404                                      error;  302 -> 200  runs
///          200 with an empty body, 204, 304                load, nothing runs
///
/// CLASSIC  text/plain text/html application/json application/octet-stream, no Content-Type  run
///          image/png image/svg+xml audio/mpeg video/mp4 text/csv TEXT/CSV text/csv;charset=utf-8
///                                                          error ("Refused to execute script")
///          nosniff + a JavaScript MIME type (any of the 16, parameters allowed)  run
///          nosniff + text/plain, text/html, application/json, or no Content-Type  error
///
/// MODULE   any of the 16 JavaScript MIME types, nosniff or not                     run
///          text/plain text/html application/json application/octet-stream image/* audio/* video/*
///          text/csv, no Content-Type                        error ("Strict MIME type checking")
///          404 / 500 / 302 -> 404                           error;  204, empty 200  load
/// ```
///
/// A refused script is NOT an uncaught error in Chrome: no `Runtime.exceptionThrown`, no `ErrorEvent`
/// at the window -- only the element's `error`, which does not bubble. Chrome's
/// `Failed to load resource` line is a DevTools log entry, not a `console.error` call.
pub fn script_response_ok(flavor: ScriptFlavor, status: u16, headers: &[(String, String)]) -> bool {
    if !(200..400).contains(&status) {
        return false;
    }
    let essence = content_type_essence(headers);
    match flavor {
        ScriptFlavor::Module => essence.as_deref().is_some_and(is_javascript_mime_essence),
        ScriptFlavor::Classic => {
            if nosniff(headers) {
                return essence.as_deref().is_some_and(is_javascript_mime_essence);
            }
            match essence.as_deref() {
                None => true,
                Some(e) => !(e.starts_with("image/") || e.starts_with("audio/") || e.starts_with("video/") || e == "text/csv"),
            }
        }
    }
}

/// The type a script's MIME checks read, or `None` when there is none: Blink's `ResourceResponse::HttpContentType()`
/// (`AllowedByNosniff::MimeTypeAsScript`, `allowed_by_nosniff.cc:178`; `ModuleScriptFetcher`, "we use
/// HttpContentType() instead of MimeType(), as MimeType() may be rewritten by mime sniffer") -- the FIRST value of
/// the field's lines joined with ", ", lowercased (`transport::mime::http_content_type`). This took the LAST entry,
/// which is the network stack's rule for a document's type, not Blink's for a script (A/B C5): Chrome refuses a
/// `nosniff` script served `text/html,text/javascript` and runs one served `text/javascript,text/html`.
fn content_type_essence(headers: &[(String, String)]) -> Option<String> {
    transport::mime::http_content_type(headers).filter(|e| !e.is_empty())
}

/// `X-Content-Type-Options: nosniff` -- the first entry of the field, case-insensitively (Fetch).
fn nosniff(headers: &[(String, String)]) -> bool {
    transport::mime::nosniff(headers)
}

fn attr(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let a = str_val(scope, name);
    crate::core::dom::element::attr_call(scope, el.into(), "getAttribute", &[a])
        .filter(|v| !v.is_null_or_undefined())
        .map(|v| v.to_rust_string_lossy(scope))
}

// ---- which `<script>`s are MODULE scripts, and where a module script goes ---------
//
// A `<script type=module>` the PAGE inserted used to reach exactly the same place a classic one
// reaches -- `v8::Script::compile` -- so its first `export` or `import` was a SyntaxError the page
// never wrote, and the module's own imports were never requested. The element is prepared as a module
// here instead: its response is checked against the MODULE rules ([`script_response_ok`]), its fetch
// goes out in CORS mode as Chrome's does, and its body is QUEUED for the deferred module drain that
// `korvus-runtime`'s `evaluate_pending_modules` already owns -- which is the one module-evaluation
// path in this engine, entered from a second producer rather than duplicated.

// ---- `nomodule`: the script a module-supporting browser does not even REQUEST ------
//
// THE DEFECT THIS SECTION EXISTS FOR. korvus fetched and ran `<script nomodule>`. That is a request
// Chrome never makes, and it is visible in a site's own access log: a client that fetched the module
// graph AND the legacy fallback is a browser that does not exist. 85 of the 504 recorded corpus rows
// carry such a script and 82 carry an external one -- 100 requests korvus made and no modern browser
// does.

/// Does this engine support module scripts? **The premise of the whole rule**, derived from the one
/// list that already answers it to the page: `HTMLScriptElement.supports("module")`.
///
/// `nomodule` is the legacy fallback. A browser that cannot run `<script type=module>` runs the
/// fallback and ignores the module; a browser that CAN runs the module and ignores -- does not fetch,
/// does not execute -- every classic script carrying the attribute. korvus runs module scripts,
/// so it is the second kind, and the attribute's mere PRESENCE is then the whole test.
fn supports_module_scripts() -> bool {
    SUPPORTED.contains(&"module")
}

/// Is this `<script type>` value a **classic** script's?
///
/// HTML: an absent or empty `type` makes the script block's type string "text/javascript", and a type
/// string that is a JavaScript MIME type essence match sets el's type to "classic". `module` and
/// `importmap` are types of their own and are NOT classic, which is exactly what the `nomodule` rule
/// turns on. The raw-empty test comes first for the same reason it does in `is_javascript` above:
/// `type=""` is classic and `type=" "` is not a script type at all.
pub fn is_classic_type(ty: &str) -> bool {
    if ty.is_empty() {
        return true;
    }
    is_javascript_mime_essence(&html_trim(ty).to_ascii_lowercase())
}

/// HTML "prepare the script element": *If el has a `nomodule` content attribute and its type is
/// "classic", then return. (No script is executed.)* — and because that step is reached **before** the
/// script is fetched, no request goes out either.
///
/// Measured on stock Chrome 150.0.7871.46 over loopback, one fixture, the REQUEST LOG taken from the
/// server and the execution log beaconed back by the page. Each external
/// script has a URL of its own, so "requested" is read off the wire and not inferred:
///
/// ```text
///                                                  chrome: requested?  ran?     korvus before
///   <script src nomodule>                              no              no       requested + ran
///   <script src nomodule="nomodule">                   no              no       requested + ran
///   <script src nomodule="false">                      no              no       requested + ran
///   <script src type="text/javascript" nomodule>       no              no       requested + ran
///   <script nomodule>inline</script>                   -               no       ran
///   <script src>                        (the control)  YES             YES      agreed
///   <script type=module src nomodule>                  YES             YES      agreed
///   <script type=module nomodule>inline</script>       -               YES      agreed
///   inserted, `s.noModule = true` BEFORE appendChild   no              no       requested + ran
///   inserted, `s.noModule = true` AFTER  appendChild   YES             YES      agreed
/// ```
///
/// **THE VALUE IS NOT READ.** `nomodule=""`, `nomodule="nomodule"` and `nomodule="false"` all
/// suppress: it is a boolean content attribute and its presence is the answer. **THE TYPE IS.** A
/// module script carrying `nomodule` is fetched and run, inline or external, parser-found or inserted.
/// **AND IT IS DECIDED AT PREPARE TIME**, which is why the last two rows differ: an element already
/// prepared has already been fetched, and `noModule` set afterwards reflects `true` while the script
/// still runs.
///
/// There is no way to measure the module-support flag apart from the attribute in Chrome, because
/// Chrome has no mode with modules off — so what this states, and all it states, is that in a browser
/// that supports modules the attribute alone decides. [`supports_module_scripts`] is the other half,
/// and it is derived from `HTMLScriptElement.supports` rather than asserted here.
pub fn nomodule_skips(has_nomodule_attr: bool, ty: &str) -> bool {
    has_nomodule_attr && supports_module_scripts() && is_classic_type(ty)
}

/// [`nomodule_skips`] over an arena node — the form the parser's fetch pass and the element loader
/// hold. `get_attribute` answers `Some("")` for a bare `nomodule`, which is the presence this reads.
pub(crate) fn nomodule_skips_element(dom: &korvus_dom::Dom, id: u32) -> bool {
    nomodule_skips(dom.get_attribute(id, "nomodule").is_some(), dom.get_attribute(id, "type").unwrap_or(""))
}

/// Is this `type` attribute the MODULE type? HTML strips ASCII whitespace and matches "module"
/// ASCII-case-INSENSITIVELY, and the case matters in practice: Chrome 150, measured over loopback on
/// 2026-09-18, runs `<script type="MODULE" src=…>` as a module both parser-inserted and inserted from
/// script, where a `==` against the lowercase literal ran it as a classic script (inserted) or skipped
/// it entirely (parser-inserted -- korvus fetched the body, ran nothing and fired no event).
pub fn is_module_type(ty: &str) -> bool {
    html_trim(ty).eq_ignore_ascii_case("module")
}

/// Is this `type` attribute the IMPORT MAP type? Same trimming and same
/// case-insensitivity as [`is_module_type`], for the same reason: HTML compares the script block's
/// type string, which is stripped and ASCII-lowercased before anything looks at it.
pub fn is_import_map_type(ty: &str) -> bool {
    html_trim(ty).eq_ignore_ascii_case("importmap")
}

/// Install one `<script type="importmap">`'s text into the document's store, reporting a parse
/// failure to the page the way Chrome reports it -- uncaught, and as the error TYPE Chrome uses,
/// which is a `SyntaxError` for invalid JSON and a `TypeError` for a JSON value of the wrong shape.
///
/// Shared by the two producers: the runtime's page-script driver, for the maps the PARSER found, and
/// [`run_inserted_script`], for the ones the page injected. Both must install into the same store,
/// because HTML has one import map per document however its entries arrived.
pub fn install_import_map(scope: &mut v8::PinScope, text: &str) {
    let (store, base) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(maps) = st.try_borrow::<crate::core::html::import_map::ImportMaps>() else { return };
        (maps.0.clone(), crate::core::dom::base_url_of(&st))
    };
    // A document whose base URL does not parse has nothing for a map's values to resolve against,
    // and HTML's parse step would fail every one of them. Installing nothing is the same outcome
    // with no invented error.
    let Ok(base) = url::Url::parse(&base) else { return };
    let outcome = store.borrow_mut().install(text, &base);
    if let Err(e) = outcome {
        report_page_error(scope, e.error_type, &e.message);
    }
}

/// Report `message` to the page as an uncaught error of the named kind -- the same path a throwing script
/// takes, so `window.onerror` and the engine's tally both see it. The realm's OWN error, as V8 makes one: this
/// constructed it with whatever the page's global `TypeError` was.
fn report_page_error(scope: &mut v8::PinScope, ctor_name: &str, message: &str) {
    let Some(m) = v8::String::new(scope, message) else { return };
    let err = match ctor_name {
        "TypeError" => v8::Exception::type_error(scope, m),
        "SyntaxError" => v8::Exception::syntax_error(scope, m),
        "RangeError" => v8::Exception::range_error(scope, m),
        "ReferenceError" => v8::Exception::reference_error(scope, m),
        _ => v8::Exception::error(scope, m),
    };
    crate::core::events::report::report_exception(scope, err);
}

/// Is the live element a module script? Read through `getAttribute`, like everything else on the
/// inserted-script path, so a `setAttribute("type", …)` before insertion is seen.
fn is_module_script(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    attr(scope, el, "type").is_some_and(|t| is_module_type(&t))
}

/// Is the arena node a module script? The element-loader's half of [`is_module_script`]: `cb_load_ok`
/// and `finish_script` hold a node id and the `HostDom`, never a wrapper.
pub(crate) fn is_module_element(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.get_attribute(id, "type").is_some_and(is_module_type)
}

/// The flavour a `<script>` element's RESPONSE is checked as. A module's check is strict (a
/// JavaScript MIME type, nosniff or not); a classic script's is not. Measured on Chrome 150: an
/// inserted module served `text/plain`, or with no `Content-Type` at all, is NOT run and its element
/// fires `error` -- the same table `script_response_ok` carries for the parser-inserted one.
pub(crate) fn element_flavor(dom: &korvus_dom::Dom, id: u32) -> ScriptFlavor {
    if is_module_element(dom, id) {
        ScriptFlavor::Module
    } else {
        ScriptFlavor::Classic
    }
}

/// One `<script type=module>` waiting for the deferred module drain.
///
/// THE QUEUE LIVES HERE, in the crate BELOW the runtime, because it has two producers in two crates
/// and exactly one consumer. `korvus-runtime`'s `script_exec.rs` queues the `<script type=module>`s
/// the PARSER found; this crate queues the ones the PAGE inserted (an external one from the element
/// loader's completion, an inline one from [`run_inserted_script`]); and `Engine::evaluate_pending_modules`
/// drains both. Declaring it in `korvus-runtime` would have meant either a second queue over here or
/// a second evaluator, and the brief forbids both -- the dependency edge points runtime -> webapi, so
/// the shared vocabulary has to sit at this end of it.
pub struct PendingModule {
    /// The module's source. Empty when [`PendingModule::refused`] is set: there is nothing to compile.
    pub source: String,
    /// The module's own URL -- its resolved `src` for an external module script, the DOCUMENT's url
    /// for an inline one. Page-visible: V8 puts it in every stack frame of the module.
    pub url: String,
    /// `Some(base)` for an INLINE module script, whose relative imports resolve against the document's
    /// base url rather than against the name above (`<base href>` makes the two different). `None` for
    /// an external one, whose own URL is its base.
    pub import_base: Option<String>,
    /// The `<script type=module src>` element, which fires `load` or `error` when the drain reaches
    /// it. `None` for an inline module: an inline script fires neither, in Chrome or in the spec.
    pub element: Option<v8::Global<v8::Value>>,
    /// The element's own response was refused -- a status of 400 or more, a network error, or a
    /// Content-Type that is not a JavaScript MIME type. Nothing is compiled; the element fires `error`.
    pub refused: bool,
    /// Did the PAGE insert this script element, rather than the parser find it? A parser-found module
    /// is DEFERRED to the end of the parse -- it runs before `DOMContentLoaded`, in document order --
    /// while one the page inserted is a task of its own and runs after. Chrome 150, measured over
    /// loopback: a parser-inserted `<script type=module>` produces `ran:parser-module`, `DCL`; a
    /// script that inserts an inline module produces `after-insert`, `DCL`, `ran:inline`. Both kinds
    /// share this queue, so the parse-end drain (`navigation.rs`) asks for the parser's and leaves
    /// these for the event loop's next turn.
    pub inserted: bool,
    /// The "delay the load event" token this module's fetch took, released by the drain once the
    /// module has evaluated -- NOT when its bytes arrived. Chrome 150, measured: a module served 600 ms
    /// late and inserted from script produces `ran:module`, then the element's `load`, and only then
    /// the document's `load`. `None` for a module no element loader fetched (an inline one, and every
    /// parser-inserted one, whose bytes the runtime's own fetch pass already had).
    pub delay_token: Option<u32>,
    /// A PARSER module whose response had not arrived when the parser reached it (the streaming
    /// parse): the index of its staged fetch, its body filled in before the parse's end drains it.
    pub staged_slot: Option<usize>,
    /// A parser module marked `blocking=render` in `<head>`: the document is render-blocked until it has run (HTML
    /// "render-blocking element set"; WPT html/dom/render-blocking/parser-inserted-async-inline-module-with-import).
    pub render_blocking: bool,
}

/// The document's deferred module scripts, in the order they were queued.
#[derive(Default)]
pub struct PendingModules(pub Vec<PendingModule>);

/// Queue one module script for the drain. Creates the queue if the realm has none yet -- the page
/// realm's is put by `Engine::new`, but this path also runs in realms that were never navigated.
pub(crate) fn queue_pending_module(scope: &mut v8::PinScope, pm: PendingModule) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<PendingModules>().is_none() {
        st.put(PendingModules::default());
    }
    st.borrow_mut::<PendingModules>().0.push(pm);
}

/// An inserted `<script type=module src>` whose fetch has settled: queue it, with its element and its
/// load-delay token, instead of compiling the body as a classic script.
///
/// `ok` is [`script_response_ok`]'s answer under [`ScriptFlavor::Module`]; a refused response is
/// QUEUED as a failure rather than dropped, so the element's `error` fires in the drain's order
/// alongside every other module's event rather than ahead of them.
pub(crate) fn queue_inserted_module(scope: &mut v8::PinScope, id: u32, ok: bool, body: String, url: String, delay_token: Option<u32>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el: v8::Local<v8::Value> = crate::binding::protos::wrap_cached(scope, &op_state, id).into();
    let element = Some(v8::Global::new(scope, el));
    queue_pending_module(
        scope,
        PendingModule {
            source: if ok { body } else { String::new() },
            url,
            import_base: None,
            element,
            refused: !ok,
            inserted: true,
            delay_token,
            staged_slot: None,
            render_blocking: false,
        },
    );
}

/// HTML "prepare the script element", the SCRIPT TYPE step: given the raw `type` and `language`
/// attributes (`None` = the attribute is absent), does this element have a script type at all?
///
/// *"If the script block's type string is null, then return. (No script is executed, and no request
/// is made.)"* — and the return is upstream of the fetch, exactly as [`nomodule_skips`]'s is.
///
/// THE THIRD SPELLING OF THIS RULE, AND WHY IT IS NOT A THIRD DEFINITION. `script_exec`'s
/// `script_type_runs(&str)` asks the same question for the PARSER seam, and it already delegates
/// both lists to this file ([`is_module_type`], [`is_javascript_mime_essence`]). This one differs
/// in what it is GIVEN, not in what it decides: the element seams hold an element, so they can see
/// the legacy `language` attribute that the parser seam's caller never reads, and they must see it
/// — `<script language=javascript src=…>` with no `type` is a script and `language=vbscript` is
/// not. `is_javascript` below is the same question over a v8 wrapper, and it now calls this rather
/// than restating it, which is what stops the runner and the loader disagreeing about one element.
pub(crate) fn script_type_or_language_runs(ty: Option<&str>, language: Option<&str>) -> bool {
    // a DevTools client switched scripting off (`Emulation.setScriptExecutionDisabled`): no script element is one
    if spec::devtools::scripts_disabled() {
        return false;
    }
    if let Some(raw) = ty {
        // The empty check is on the RAW value, BEFORE stripping. That is the whole difference between
        // `type=""` (runs) and `type=" "` (does not): both strip to "", so a rule that strips first
        // cannot tell them apart, and the suite tests exactly that pair.
        if raw.is_empty() {
            return true;
        }
        let t = html_trim(raw).to_ascii_lowercase();
        // `importmap`, `speculationrules` and `webbundle` are script types HTML knows and does NOT
        // fetch through this path: an external import map is refused outright. Only the two that
        // produce a request are listed.
        return t == "module" || JS_TYPES.contains(&t.as_str());
    }
    if let Some(l) = language {
        // Unlike `type`, `language` is NOT whitespace-stripped -- " javascript" does not run.
        return LANGUAGES.contains(&l.to_ascii_lowercase().as_str());
    }
    true
}

/// [`script_type_or_language_runs`] over an ARENA node — the loader's seam
/// (`subresource::induced`), which holds a `NodeId` and a `&Dom` and never a wrapper.
pub(crate) fn element_script_type_runs(dom: &korvus_dom::Dom, id: u32) -> bool {
    script_type_or_language_runs(dom.get_attribute(id, "type"), dom.get_attribute(id, "language"))
}

/// Whether the element's `type` / `language` attributes say this is a classic (or module) script.
fn is_javascript(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    let ty = has_attr(scope, el, "type").then(|| attr(scope, el, "type").unwrap_or_default());
    let lang = has_attr(scope, el, "language").then(|| attr(scope, el, "language").unwrap_or_default());
    script_type_or_language_runs(ty.as_deref(), lang.as_deref())
}

/// Run a just-inserted `<script>`. A no-op for every other node, for one that has already started,
/// for one with a `src` (a fetch this path cannot perform), and for a non-JavaScript type.
///
/// IN THE ELEMENT'S OWN REALM. The DOM's functions are the page's, so the callback
/// that inserted the script is running in the PAGE's context even when a frame's script called it:
/// a frame's `head.appendChild(s)` compiled `s` against the page's global, and its `var`s, its
/// `document` and its `currentScript` were the page's.
pub(crate) fn run_inserted_script(scope: &mut v8::PinScope, node: v8::Local<v8::Value>) {
    let realm = crate::binding::protos::unwrap_id(scope, node)
        .and_then(|id| crate::core::html::browsing_context::realm_of_element(scope, id));
    match realm {
        Some(realm) => {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
            run_inserted_script_here(scope, node);
        }
        None => run_inserted_script_here(scope, node),
    }
}

fn run_inserted_script_here(scope: &mut v8::PinScope, node: v8::Local<v8::Value>) {
    let Ok(el) = v8::Local::<v8::Object>::try_from(node) else { return };
    // `tagName === "SCRIPT"` held only because an HTML document uppercases it. An XML document
    // preserves case, so in XHTML/SVG the very same element answers "script" (or, prefixed,
    // "h:script") and every dynamically inserted script silently did not run -- 218 subtests of
    // script-type-and-language-js-xhtml.xhtml alone. Match the LOCAL name, case-insensitively, which
    // is what the spec's "script element" means.
    // Asked of the arena, not of the page-visible `tagName` getter.
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(id) = crate::binding::protos::unwrap_id(scope, node) else { return };
    let is_script = crate::page::with_host_dom(scope, |d| is_script_element(d, id)).unwrap_or(false);
    if !is_script || script_started(&op_state, id) {
        return;
    }
    // "Prepare the script element": a script that is no longer CONNECTED does nothing. The scripts of
    // one insertion run in order, and an earlier one can remove a later one before its turn (WPT
    // dom/nodes/insertion-removing-steps/later-script-removed-by-earlier-script). Connected means in a
    // document that has a browsing context -- the page's or a frame's; `Dom::is_connected` is the
    // page's tree alone and would stop every script a frame inserts.
    if crate::core::html::subresource::home_of(scope, id) == crate::core::html::subresource::Home::Nowhere {
        return;
    }
    // "If scripting is disabled for el, then return" -- after "already started" is set (HTML "prepare the script
    // element"): a sandboxed frame document's script is neither fetched nor run, and fires nothing.
    if let crate::core::html::subresource::Home::Frame(ctx) = crate::core::html::subresource::home_of(scope, id) {
        if crate::core::html::browsing_context::scripts_sandboxed(scope, ctx) {
            mark_script_started(&op_state, id);
            return;
        }
    }
    // AN IMPORT MAP IS NOT A SCRIPT AND IT IS NOT NOTHING. Chrome 150 applies a map the
    // page injects after the document has loaded -- measured, probe `p6b`: a `<script type=importmap>`
    // created with `createElement`, filled with `textContent` and appended, then a dynamic
    // `import("dep")`, which resolved. It is handled before the `src`/`is_javascript` gate below
    // because that gate correctly says an import map is not a script, and used to be the end of it.
    if attr(scope, el, "type").is_some_and(|t| is_import_map_type(&t)) {
        let has_src = has_attr(scope, el, "src");
        // The CHILD text content (HTML "prepare the script element" step 5), read off the arena.
        let text = child_text(scope, el);
        // AN EMPTY IMPORT MAP IS NOT A MAP AT ALL, and it does not START. HTML's "prepare the script
        // element" returns before the already-started step when the element has no `src` and its
        // child text content is the EMPTY STRING -- so nothing is parsed, nothing is reported, and
        // the element can still be retyped and run later.
        //
        // EMPTY, NOT BLANK, and the difference is measured (Chrome 150, probe `p31`): of four empty
        // import maps on one page -- `<script type=importmap></script>`, one holding three spaces,
        // one injected with no text and one injected with `textContent = ""` -- Chrome reported
        // `Failed to parse import map: invalid JSON` for exactly ONE, the three spaces, because
        // whitespace is not the empty string and JSON has nothing to parse in it. It then retyped
        // the truly-empty element to `text/javascript`, re-appended it, and RAN it. An earlier draft
        // of this branch trimmed, errored four times, and left the element started.
        if !has_src && text.is_empty() {
            return;
        }
        mark_script_started(&op_state, id);
        // AN EXTERNAL IMPORT MAP IS REFUSED OUTRIGHT, text content and all: Chrome 150 fires `error`
        // at the element and maps nothing, even when the element ALSO carries a well-formed body
        // (measured, probe `p23`).
        if has_src {
            // Through a `Global` and back, because the element handle and this scope carry unrelated
            // lifetimes -- the same round trip `script_exec::fire_at_element` makes.
            let g = v8::Global::new(scope, node);
            let node = v8::Local::new(scope, &g);
            crate::page::fire_script_event(scope, node, "error");
            return;
        }
        install_import_map(scope, &text);
        return;
    }
    // `src` is a fetch, which this path has no way to perform; leaving it alone is honest, where
    // running its (empty) text content would silently execute nothing and mark it started.
    if has_attr(scope, el, "src") || !is_javascript(scope, el) {
        return;
    }
    // The CHILD text content (HTML "prepare the script element" step 5), read off the arena.
    let code = child_text(scope, el);
    // Step 5: no `src` and EMPTY text returns before the flag is set, so the element can still be
    // filled and run by its children-changed steps ([`script_children_changed`]).
    if code.is_empty() {
        return;
    }
    mark_script_started(&op_state, id);
    // `nomodule` on a classic script, read HERE because here is where the element is PREPARED
    // -- the table is in [`nomodule_skips`]. Two orderings ride on this position and
    // both are HTML's: the attribute is read at prepare time, so `s.noModule = true` set after
    // insertion is too late and Chrome runs the script; and "set el's already started to true" comes
    // BEFORE this step, so an element that was skipped for `nomodule`, had the attribute removed and
    // was re-inserted still does not run.
    if nomodule_skips(has_attr(scope, el, "nomodule"), &attr(scope, el, "type").unwrap_or_default()) {
        return;
    }
    if code.trim().is_empty() {
        return;
    }
    // CSP `script-src`. An inserted script is not parser-inserted, which is what
    // `'strict-dynamic'` trusts.
    if !crate::core::html::csp::allows_inline_script(scope, id, &code, false) {
        return;
    }
    // AN INLINE MODULE IS NOT COMPILED HERE. Running it through the classic compile
    // below raises "Cannot use import statement outside a module" on its first import, and it is
    // deferred anyway: Chrome 150, measured over loopback, runs an inline module the page inserted
    // AFTER the inserting script finishes and after `DOMContentLoaded` (`after-insert`, `DCL`,
    // `ran:inline`), where this path ran it synchronously, first. It fires neither `load` nor `error`
    // -- an inline script never does -- so it travels with no element.
    //
    // The two URLs are the two Chrome keeps apart for any inline module: the module is
    // NAMED by the document url, which is what its stack frames read, and its relative imports resolve
    // against the document's BASE url, which `<base href>` moves.
    if is_module_script(scope, el) {
        let base = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            crate::core::dom::base_url_of(&st)
        };
        let url = document_location_href(scope);
        // it delays the document's load event until it has run (HTML "prepare the script element": an inline module
        // sets "delaying the load event"; Chrome 150: `after-insert, DCL, ran:inline, load`)
        let token = crate::core::html::browsing_context::new_load_delay_token(scope);
        crate::core::html::browsing_context::delay_load(scope, token);
        queue_pending_module(scope, PendingModule { source: code, url, import_base: Some(base), element: None, refused: false, inserted: true, delay_token: Some(token), staged_slot: None, render_blocking: false });
        return;
    }
    // `document.currentScript` is the running inline script, for as long as it runs. The store is the
    // one `core/dom/document.rs::cb_doc_current_script` reads and the runtime's parser pass writes;
    // this SAVES AND RESTORES it, so a script that inserts a script nests -- which is why this slot
    // has five touches and not two, and why `nested_current_script_restores_the_outer_script` probes
    // the nested case rather than the simple one.
    let prev = current_script(scope);
    let cs = current_script_value(scope, node);
    set_current_script(scope, cs);
    let url = document_location_href(scope);
    let text = format!("{code}\n//# sourceURL={url}");
    if let Some(src) = v8::String::new(scope, &text) {
        v8::tc_scope!(let tc, scope);
        let _ = v8::Script::compile(tc, src, None).and_then(|s| s.run(tc));
        if tc.has_caught() {
            if let Some(err) = tc.exception() {
                // reported where it was thrown (`report::caught_site`)
                let m = tc.message();
                let site = crate::core::events::report::caught_site(tc, err, m);
                tc.reset();
                crate::core::events::report::report_exception_at(tc, err, site);
            } else {
                tc.reset();
            }
        }
    }
    // NO CHECKPOINT HERE, and the reason is the condition in HTML's "clean up after running script":
    // it performs a microtask checkpoint only *if the JavaScript execution context stack is now
    // empty*. This function is the INSERTION seam -- it runs synchronously inside the script that
    // called `appendChild`, so the stack is never empty and the inner script's microtasks belong to
    // the OUTER script's checkpoint. Chrome 150.0.7871.46, an inline script inserted and run from
    // inside `#outer` (loopback, 2026-09-19):
    //
    // ```text
    //   inner sync   inner's own microtask   outer sync   outer's microtask
    //   the inner    THE OUTER               the outer    the outer
    // ```
    //
    // So a checkpoint here would hand the inner script's continuation the inner element, which is
    // the one answer Chrome does not give.
    match prev {
        Some(p) => set_current_script(scope, p),
        None => {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            set_current_script(scope, u);
        }
    }
}

/// The V8 PRIVATE on the page's global that backs `document.currentScript`.
///
/// IT WAS THE `__curScript` BRIDGE SLOT, and moving it here is what let `bridge_get`/`bridge_set` be
/// deleted -- invariant I3's last open clause. Both ends were always Rust: this file writes it around
/// a dynamically inserted inline script, `korvus-runtime/src/script_exec.rs` writes it around each
/// script the page driver runs, and `core/dom/document.rs::cb_doc_current_script` reads it. The page
/// realm was a detour between two Rust statements, and an OBSERVABLE one -- the holder is an ordinary
/// object a page can reach through `Symbol.for("korvus.bridge")`, so a page could read, overwrite or
/// freeze the engine's idea of which script is running.
///
/// A `v8::Private` is the closest thing this engine has to an internal slot, which is what
/// `currentScript` is in the spec: no JS reflection path -- `getOwnPropertyNames`,
/// `getOwnPropertySymbols`, `JSON`, a `Proxy` trap -- can see it or set it. The precedents are
/// `binding::protos`'s `korvus.shellInstance` brand and `binding::hidden`'s `korvus_ops` stash.
///
/// PER-REALM BY CONSTRUCTION, because it hangs off the current context's global: a worker realm has
/// its own, which is the same property the holder had and the reason neither reader needs a realm
/// test of its own.
///
/// The key comes from the engine's key cache (`protos::private_key`: the same `for_api` private by name), not a
/// `v8::String` and a registry probe per `document.currentScript` read.
fn cur_script_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Private>> {
    Some(crate::binding::protos::private_key(scope, "korvus.currentScript"))
}

/// The currently-executing script, or `None` outside one. `undefined` reads back as `None`, so a
/// restore to "no script" and never having had one are the same answer.
pub(crate) fn current_script<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    let key = cur_script_key(scope)?;
    let global = { let ctx = scope.get_current_context(); ctx.global(scope) };
    global.get_private(scope, key).filter(|v| !v.is_undefined())
}

/// Set the currently-executing script. Pass `undefined` to clear it.
pub(crate) fn set_current_script(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) {
    let Some(key) = cur_script_key(scope) else { return };
    let global = { let ctx = scope.get_current_context(); ctx.global(scope) };
    global.set_private(scope, key, v);
}

/// HTML "clean up after running script" — the microtask checkpoint a script performs when it ends.
///
/// **IT HAPPENS BEFORE `document.currentScript` IS PUT BACK, AND THAT IS THE WHOLE POINT.** "Run a
/// classic script" ends by cleaning up after running script; "execute the script element" restores
/// `currentScript` only once that has returned. So every microtask a classic script queued runs while
/// that script is still the running script, and the value it reads is the element.
///
/// Measured on Chrome 150.0.7871.46 over loopback, one `<script id=…>` per row, the continuation
/// reading `document.currentScript`:
///
/// ```text
///                            Promise.then   queueMicrotask   await   setTimeout(0)
///   inline classic           the element    the element      elem    null
///   external classic         the element    -                -       -
///   external classic async   the element    -                -       -
///   external classic defer   the element    -                -       -
///   inserted external        the element    -                -       -
///   inline type=module       null           -                -       -
/// ```
///
/// The `setTimeout(0)` column is the control: a TASK is queued after the restore and reads null in
/// both engines, so this is the checkpoint's position and not "currentScript is never cleared".
///
/// korvus read `null` in every classic row. It cleared `currentScript` the moment the script
/// returned and drained the microtask queue at the next event-loop turn, so nothing a script queued
/// could ever see the element — and the queued work ran after EVERY parser script rather than after
/// its own, which is a second observable of the same missing checkpoint.
///
/// FOUR OF THE FIVE CLASSIC ROWS ARE THE ELEMENT HERE NOW; `inserted external` IS STILL `null`, and
/// the reason is not this function — `subresource.rs`'s `run_classic_script` calls it from inside
/// deno_core's own microtask drain, where V8 makes a checkpoint a no-op. That call site states the
/// measurement and what has to move.
///
/// NO CALLER MAY BE NESTED. HTML performs this checkpoint only *if the JavaScript execution context
/// stack is now empty*, and Chrome is measured doing exactly that: a script inserted and run from
/// inside another performs no checkpoint, so its microtask reads the OUTER element. `run_inserted_script`
/// therefore does not call this, and says so where the call would have gone.
///
/// A module gets no call: HTML runs a module script with `currentScript` already null, so there is
/// nothing for the checkpoint's position to change, and deno_core performs its own after evaluation.
pub(crate) fn clean_up_after_running_script(scope: &mut v8::PinScope) {
    scope.perform_microtask_checkpoint();
    crate::core::events::report::notify_rejected_promises(scope);
}

/// `document.location.href`, or `about:blank` -- the `sourceURL` a stack trace shows.
fn document_location_href(scope: &mut v8::PinScope) -> String {
    let fallback = "about:blank".to_string();
    let Some(doc) = global_get(scope, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return fallback };
    let Some(loc) = get_prop(scope, doc, "location").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return fallback };
    get_prop(scope, loc, "href").filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope)).unwrap_or(fallback)
}
