//! `core/dom/document.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use std::ffi::CStr;

use deno_core::{v8, GarbageCollected, OpState};
use crate::binding::HostDom;
use crate::core::events::shared::{FocusState, ON_DOCUMENT_ID};
use crate::core::dom::shared::children_of;
use crate::core::dom::tree_walker::tw_accept_node;
use crate::core::dom::node::{insertion_cycles, insertion_type_invalid, notify_childlist_batch};
use crate::binding::protos::{arg_node_id, get_prop, global_get, int_val, make_dom_exc, require_args, str_val, throw_dom, throw_type_error, wrap_cached};
use crate::modules::indexeddb::backend::{idb_throw};
use crate::core::dom::{NodeIterator, TreeWalker};

pub(crate) fn cb_doc_create_range(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(r) = crate::core::dom::range::new_range(scope) else { return };
    // A range created BY a document is collapsed at that document -- `new Range()` starts at the page
    // document, so a `createDocument()` result's range would otherwise point into the page's tree.
    // The write goes through `range_seat_on`: these are getter-only accessors over private slots, and
    // the `set_prop` that used to stand here neither wrote nor complained.
    let slot = doc_root_slot(scope, _args.this().into());
    if slot != PAGE_DOC_ROOT {
        let this = _args.this();
        crate::core::dom::range::range_seat_on(scope, r, this.into());
    }
    rv.set(r.into());
}

/// `document.getSelection()` -- the WINDOW's selection, which a document with no browsing context
/// does not have. The page's document delegates to `window.getSelection()` as before; every other
/// document answers null, which is what the recorded browser answers for `new Document()`.
pub(crate) fn cb_doc_get_selection(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A frame's document, or a shadow root in one, answers that FRAME's selection: the tree
    // it belongs to decides, and a tree with no browsing context has none.
    let this: v8::Local<v8::Value> = args.this().into();
    let slot = doc_root_slot(scope, this);
    let key = if slot != PAGE_DOC_ROOT {
        Some(slot)
    } else {
        crate::binding::protos::unwrap_id(scope, this).map(|id| {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            crate::core::editing::selection::tree_key(&st.borrow::<crate::binding::HostDom>().0, id)
        })
    };
    // A tree whose root is not a document (a detached node, a shadow root of a detached host) still has
    // its NODE DOCUMENT's selection: Blink's `TreeScope::GetSelection` asks the document's frame, so
    // Chrome 150 answers a Selection for `document.createElement("div").attachShadow(...).getSelection()`.
    // Only a frame document's root keys a frame's selection.
    let key = key.filter(|&k| slot != PAGE_DOC_ROOT || crate::core::html::browsing_context::frame_of_document_root(scope, k).is_some());
    if let Some(key) = key.filter(|&k| k != crate::core::events::shared::ON_DOCUMENT_ID) {
        match crate::core::editing::selection::selection_for_key(scope, key) {
            Some(s) => {
                let s = shadow_view(scope, this, s);
                rv.set(s.into())
            }
            None => rv.set_null(),
        }
        return;
    }
    // The SELECTION itself, not a call through `globalThis.getSelection`. That global
    // is a `Window` table row wrapped in the Window RECEIVER GUARD, and an internal caller has no
    // receiver to give it -- V8 substitutes the global proxy for the `undefined` one, so the call does
    // still work, but only by a coincidence of sloppy-mode semantics that nothing here states or
    // tests. Asking the owner directly is one hop shorter and does not depend on it. (The guard is
    // right and stays: Chrome throws `Illegal invocation` for `getSelection.call({})`.)
    match crate::core::editing::selection::the_selection(scope) {
        Some(s) => {
            let s = shadow_view(scope, this, s);
            rv.set(s.into())
        }
        None => rv.set_null(),
    }
}

/// A ShadowRoot receiver gets its own view of `sel`; a document gets `sel` itself.
fn shadow_view<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, sel: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Object> {
    let root = crate::binding::protos::unwrap_id(scope, this).filter(|&id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.borrow::<crate::binding::HostDom>().0.is_shadow_root(id)
    });
    match root {
        Some(r) => crate::core::editing::selection::shadow_selection(scope, r, sel).unwrap_or(sel),
        None => sel,
    }
}

/// The event-interface names `document.createEvent` accepts, ASCII-lowercased.
///
/// The legacy factory is a CLOSED list, not a lookup of whatever interface the name happens to
/// spell: DOM 4.5 fixes the table, and anything outside it is a NotSupportedError -- which is why
/// `document.createEvent("x")` throws rather than handing back a bare Event. The bridge behind this
/// callback defaults unknown names to `new Event("")`, so without this check every misspelling
/// silently produced a working event object.
/// The legacy `document.createEvent` table.
///
/// Twenty-three entries where the DOM Standard's table has twenty-one. One extra is "KeyboardEvents",
/// and it is deliberate: Blink ships it. `core/events/event_interface_aliases.json5` in the vendored
/// Chromium tree lists `{ name: "KeyboardEvents", ImplementedAs: "KeyboardEvent" }` alongside Events,
/// HTMLEvents, MouseEvents, SVGEvents and UIEvents, so `document.createEvent("KeyboardEvents")`
/// returns an event in Chrome and throwing here would be a difference from the browser rather than
/// conformance to the spec. An audit flagged this as over-acceptance; the alias file is the answer.
/// ONE table: the ASCII-lowercased alias, and the interface it constructs.
///
/// IT IS ONE TABLE ON PURPOSE. Until 2026-09-22 this was a list of accepted NAMES beside a separate
/// five-arm `match` that mapped a name to a constructor with `_ => "Event"` as its default -- so
/// every alias the match did not spell was accepted by the list and then answered with a bare
/// `Event`. Ten of the twenty-two were in that gap (`document.createEvent("UIEvent") instanceof
/// UIEvent` was false) even though all ten interfaces are installed as globals. Two productions of
/// one rule is the defect this file is being audited for; the pair is now a single table and the
/// gap cannot reopen.
///
/// `textevent` maps to `TextEvent`, not to the DOM Standard's `CompositionEvent`: Blink's
/// `event_factory` builds a `TextEvent` and WPT's own alias table
/// (`dom/nodes/Document-createEvent.js`) asserts `window.TextEvent.prototype`, so the spec's cell is
/// the one nobody implements.
///
/// THE TABLE IS BLINK 150'S GENERATED FACTORY, swept name by name against Chrome 150
/// (`tests/fixtures/create-event/`): `make_event_factory.py` emits a case-insensitive arm for every
/// event interface on its four lists, minus the nine `create_event_removed_by_flag` names
/// (`RemoveNonAllowlistedCreateEvent` is stable in 150) -- and PLUS `TransitionEvent`, which is on the
/// `create_event_deprecated` list ("deprecated in M148, to be removed in M151"): Chrome 150 still makes
/// one, with a deprecation report. It was missing here. `touchevent` is in the factory and still throws
/// on a desktop: see [`touch_event_feature_detection`].
const CREATE_EVENT_TABLE: [(&str, &str); 23] = [
    ("beforeunloadevent", "BeforeUnloadEvent"),
    ("compositionevent", "CompositionEvent"),
    ("customevent", "CustomEvent"),
    ("devicemotionevent", "DeviceMotionEvent"),
    ("deviceorientationevent", "DeviceOrientationEvent"),
    ("dragevent", "DragEvent"),
    ("event", "Event"),
    ("events", "Event"),
    ("focusevent", "FocusEvent"),
    ("hashchangeevent", "HashChangeEvent"),
    ("htmlevents", "Event"),
    ("keyboardevent", "KeyboardEvent"),
    ("keyboardevents", "KeyboardEvent"),
    ("messageevent", "MessageEvent"),
    ("mouseevent", "MouseEvent"),
    ("mouseevents", "MouseEvent"),
    ("storageevent", "StorageEvent"),
    ("svgevents", "Event"),
    ("textevent", "TextEvent"),
    ("touchevent", "TouchEvent"),
    ("transitionevent", "TransitionEvent"),
    ("uievent", "UIEvent"),
    ("uievents", "UIEvent"),
];

/// Blink's `TouchEventFeatureDetection` runtime flag: `Document::createEvent` refuses `TouchEvent` without it
/// (`document.cc:6628-6634`, crbug.com/392584), and the SAME flag gates GlobalEventHandlers' four `ontouch*`
/// handlers (`global_event_handlers.idl:137-140`). The browser sets it per platform -- on by default on
/// Android and iOS, off on every desktop whatever its screen (`slow_web_preference_cache.cc:159-177`) -- so
/// the device's own recorded surface answers it: its window carries `ontouchstart` exactly when the flag is
/// on. No surface (a bare runtime) is the desktop default, off.
fn touch_event_feature_detection(scope: &mut v8::PinScope) -> bool {
    // ... or DevTools' touch emulation, which sets the flag for the page it emulates (`devtools_log::install_touch_handlers`)
    spec::devtools::touch_points().is_some() || crate::install::surface::active(scope).is_some_and(|s| s.has_global("ontouchstart"))
}

/// Blink's prefix on every exception this operation throws (V8's exception propagation callback adds it in
/// Chrome; this engine composes it).
const CREATE_EVENT_CTX: &str = "Failed to execute 'createEvent' on 'Document': ";

pub(crate) fn cb_doc_create_event(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let iface = args.get(0);
    // WebIDL `DOMString`: ToString, so a Symbol is V8's TypeError (prefixed, as Chrome's is) and a stringifier
    // that throws propagates as thrown.
    if iface.is_symbol() {
        throw_type_error(scope, &format!("{CREATE_EVENT_CTX}Cannot convert a Symbol value to a string"));
        return;
    }
    let Some(name) = iface.to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let lower = name.to_ascii_lowercase();
    let found = CREATE_EVENT_TABLE.iter().find(|(alias, _)| *alias == lower);
    // Blink breaks out of its factory loop for `TouchEvent` when the flag is off and reports it like any
    // other unknown name (Chrome 150: "The provided event type ('TouchEvent') is invalid.").
    let found = found.filter(|(alias, _)| *alias != "touchevent" || touch_event_feature_detection(scope));
    let Some(&(_, ctor_name)) = found else {
        throw_dom(scope, &format!("{CREATE_EVENT_CTX}The provided event type ('{name}') is invalid."), "NotSupportedError");
        return;
    };
    // THE INTERFACE OBJECT MAY NOT BE EXPOSED IN THIS REALM, AND THE FACTORY STILL HAS TO ANSWER.
    //
    // `DeviceMotionEvent` and `DeviceOrientationEvent` are `[SecureContext]`
    // (`tools/sync/data/secure_context.json`), so on `http://web-platform.test` they are simply not
    // on the global and there is no constructor and no `.prototype` to reach. The old `_ => "Event"`
    // default hid that by never looking them up; building them properly surfaced it as a SILENT
    // `undefined` return -- the eighth "None means do nothing" -- and WPT's INSECURE
    // `dom/events/EventTarget-dispatchEvent.html`, which iterates the same alias table, read
    // `e.type` off it and lost two rows that had been green.
    //
    // Chrome builds the real event either way, because Blink's event factory does not go through the
    // global. Korvus has no unexposed interface prototype to build on, so for a name this realm does
    // not expose the honest answer is the one the table gave before -- a plain, uninitialised
    // `Event`, which is exactly what those insecure-origin rows assert. It is never `undefined`.
    //
    // The fallback keys on the LOOKUP, not on the construction: a constructor that throws is a
    // different thing and must propagate.
    // the realm's interface objects as its install left them, never what a page assigned to the globals
    let ctor_name = if crate::binding::protos::iface_object(scope, ctor_name).is_some_and(|v| v.is_object()) { ctor_name } else { "Event" };
    // `BeforeUnloadEvent` and `TextEvent` are the two names here whose interface object is NOT
    // constructible: Chrome answers `new BeforeUnloadEvent()` / `new TextEvent()` with "Illegal
    // constructor", and this legacy factory is their only producer. They are minted in Rust on the
    // table's own prototype instead. Everything else goes through its real constructor
    // with the empty type, so the factory cannot drift from `new X("")`.
    let ev = match ctor_name {
        "BeforeUnloadEvent" => {
            let Some(ev) = crate::core::events::legacy_events::new_before_unload_event(scope) else { return };
            ev
        }
        "TextEvent" => {
            let Some(ev) = crate::core::events::event::mint_legacy_event(scope, "TextEvent") else { return };
            ev
        }
        _ => {
            let Some(ctor) = crate::binding::protos::iface_object(scope, ctor_name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
            let empty = str_val(scope, "");
            let Some(ev) = ctor.new_instance(scope, &[empty]) else { return };
            ev
        }
    };
    // The legacy factory hands back an UNINITIALISED event (DOM 2.9): `type` is "" and dispatching it
    // before `initEvent` is an InvalidStateError. A normal constructor leaves the initialised flag
    // set, so mark it here -- one private slot, cleared by initEvent/initCustomEvent.
    crate::core::events::event::evt_flag_set(scope, ev, "_uninit", true);
    rv.set(ev.into());
}

/// The document's URL/origin, so `document.cookie` reads/writes the right scope in the shared network
/// cookie jar (transport). Set by the engine on navigation; defaults to about:blank.
pub struct CookieUrl(pub String);
impl Default for CookieUrl {
    fn default() -> Self {
        CookieUrl("about:blank".to_string())
    }
}

/// The page document's URL once a SAME-DOCUMENT navigation moved it (Blink `Document::SetURL`): a fragment
/// navigation, `history.pushState` / `replaceState`, a same-document traversal, an intercepted navigation. The
/// document's URL is what `document.URL` answers, what a relative URL resolves against (the base URL without a
/// `<base>`) and what an empty form `action` submits to; the COOKIE URL is not: Blink sets a document's cookie URL
/// once, when the document is made, so `document.cookie` keeps its scope. Absent until the
/// first such navigation, when the document's URL is still [`CookieUrl`].
pub struct DocumentUrl(pub String);

/// The page document's URL: [`DocumentUrl`] when a same-document navigation moved it, else the URL it was loaded
/// from.
pub(crate) fn document_url_of(st: &deno_core::OpState) -> String {
    if let Some(d) = st.try_borrow::<DocumentUrl>() {
        return d.0.clone();
    }
    st.try_borrow::<CookieUrl>().map(|c| c.0.clone()).unwrap_or_else(|| "about:blank".to_string())
}

/// A same-document navigation moved the page document's URL to `url` (the runtime's `location::move_url`).
pub fn set_document_url(scope: &mut v8::PinScope, url: &str) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(DocumentUrl(url.to_string()));
}

// ---- THE document base URL ----------------------------------------------------------

/// The document's BASE URL -- `<base href>` applied, once, in the one place that knows the rule.
///
/// WHY THIS FUNCTION EXISTS. `<base href>` was unimplemented, and the reason it was unimplemented is
/// visible in the shape of what it touches: `Node.baseURI`, every URL-valued content-attribute
/// reflection (`a.href`, `link.href`, `img.src`), `HTMLHyperlinkElementUtils`, `<form action>`, the
/// subresource loader's URL resolution, the classic-script fetch pass and `new Request(rel).url` each
/// reached for [`CookieUrl`] -- the DOCUMENT url -- on their own. SEVEN copies of "what do relative
/// URLs resolve against", so there was nowhere to put the answer. This is that place; every one of
/// them now reads it, and none of them knows the rule.
///
/// THE RULE (HTML "document base URL", and measured in Chrome 151 headless over loopback):
///
///   * the FIRST `<base>` in tree order that HAS an `href` attribute -- a bare `<base>` before it is
///     skipped, which is why the search is `find_map` over the attribute and not over the element;
///   * its `href` resolved against the DOCUMENT url, so `<base href="sub/deep/">` on
///     `/base2.html` is `/sub/deep/` and an absolute href is itself;
///   * the document url when there is no such element;
///   * `about:blank` when there IS one and its href does not parse even against the document url.
///     That last row is Chrome's, not a guess: `<base href="http://[bogus">` makes `document.baseURI`
///     answer `"about:blank"` there, not the document url.
///
/// It is LIVE. Chrome moves `document.baseURI` and every resolved href when the winning `<base>`'s
/// `href` is rewritten, when it is removed, and when one is appended -- so this is computed per call
/// rather than cached at parse.
/// The search is scoped to the DOCUMENT TREE (`get_elements_by_tag_name_in`), not to the arena. The
/// arena-wide variant also returns nodes that were REMOVED and nodes that were never inserted, and
/// Chrome is unambiguous about both: removing every `<base>` puts `document.baseURI` back on the
/// document url, and appending one moves it.
pub fn document_base_url(dom: &korvus_dom::Dom, doc_url: &str) -> String {
    // No `<base>` was ever made: the document url, with no walk. The walk is O(document) and the cache below is keyed
    // on the DOM's revision, which every write moves -- on www.costco.com (no `<base>`; 2026-10-23) React's mounting
    // interleaves writes with URL reads (`node_base_url` for each subresource it inserts, `a.href`), and the walk was
    // 2.9 % of the main thread's samples, `get_elements_by_tag_name_in` the top self symbol.
    if !dom.has_made_base() {
        return doc_url.to_string();
    }
    // an HTML `<base>` alone: an SVG element named `base` is no base element (Chrome 152, r3a.html 2026-10-23: a
    // `createElementNS(SVG, 'base')` with an `href` in the head leaves `document.baseURI` the document's url)
    let Some(href) = dom
        .get_elements_by_tag_name_in(dom.root(), "base")
        .iter()
        .filter(|id| dom.is_html_ns(**id))
        .find_map(|id| dom.get_attribute(*id, "href"))
    else {
        return doc_url.to_string();
    };
    match url::Url::parse(doc_url).ok().and_then(|b| b.join(href).ok()) {
        Some(u) => u.to_string(),
        // A `<base href>` that does not parse is not "no base": Chrome reports about:blank.
        None => url::Url::parse(href).map(|u| u.to_string()).unwrap_or_else(|_| "about:blank".to_string()),
    }
}

/// The last answer [`base_url_of`] computed, and the DOM revision it was computed at.
///
/// WHY A CACHE AT ALL. Before W9.15 the `<base>` scan ran only for `Node.baseURI`, which a page reads
/// approximately never. It now runs for EVERY URL-valued reflection -- `a.href` in a loop over
/// `querySelectorAll("a")` is an entirely ordinary thing for a page to do -- and the scan is O(arena).
/// The key is the DOM's own revision counter, which `set_attribute` bumps as well as every tree
/// mutation, so rewriting the winning `<base href>` invalidates this exactly as removing it does; the
/// document url is in the key too, because a navigation replaces it.
#[derive(Default)]
pub(crate) struct BaseUrlCache(std::cell::RefCell<Option<(u64, String, String)>>);

/// [`document_base_url`] read off `OpState` -- the form every v8 callback wants.
///
/// `try_borrow` for both, because a context that never navigated (the quality harness, a bare worker)
/// has neither, and `borrow` inside a v8 callback aborts the process rather than failing.
pub(crate) fn base_url_of(st: &deno_core::OpState) -> String {
    let doc = document_url_of(st);
    let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return doc };
    let rev = h.0.revision();
    if let Some(c) = st.try_borrow::<BaseUrlCache>() {
        if let Some((r, d, b)) = c.0.borrow().as_ref() {
            if *r == rev && d == &doc {
                return b.clone();
            }
        }
        let out = document_base_url(&h.0, &doc);
        *c.0.borrow_mut() = Some((rev, doc, out.clone()));
        return out;
    }
    document_base_url(&h.0, &doc)
}

/// [`base_url_of`] from a scope, for a callback that holds no borrow of its own.
pub(crate) fn base_url(scope: &mut v8::PinScope) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    base_url_of(&st)
}

/// `document.createElement(tag)` as a raw callback (uninterned, like the op2 version) that tag-types the
/// result via node_proto -- so createElement('img') is an HTMLImageElement, not a flat HTMLElement.
/// `document.createAttribute(localName)`: a detached Attr (`attr.rs::make_detached_attr`), its name
/// lowercased in an HTML document. It was a stub returning a plain object.
pub(crate) fn cb_doc_create_attribute(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let raw = args.get(0).to_rust_string_lossy(scope);
    // A valid ATTRIBUTE local name (WHATWG DOM, 2025), not the XML `Name` production it used to check:
    // Chrome 150 takes `1bad`, `-x`, `a@b`, `·a` here and refuses `""`, `a b`, `a=b` (measured 2026-10-05).
    if crate::core::xml::dom_implementation::attribute_local_name_error(&raw).is_some() {
        throw_dom(scope, &format!("Failed to execute 'createAttribute' on 'Document': The localName provided ('{raw}') contains an invalid character."), "InvalidCharacterError");
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let xml = {
        let slot = doc_root_slot(scope, args.this().into());
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.is_in_xml(resolve_doc_root(dom, slot))
    };
    let name = if xml { raw } else { raw.to_ascii_lowercase() };
    if let Some(a) = crate::core::dom::attr::make_detached_attr(scope, &name, "") {
        rv.set(a.into());
    }
}

/// `document.createAttributeNS(namespace, qualifiedName)`: a detached Attr named `qualifiedName`.
pub(crate) fn cb_doc_create_attribute_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ns_v = args.get(0);
    let ns: Option<String> = if ns_v.is_null_or_undefined() { None } else { Some(ns_v.to_rust_string_lossy(scope)) };
    let ns = ns.filter(|s| !s.is_empty());
    let raw = args.get(1).to_rust_string_lossy(scope);
    // "Validate and extract" in the attribute context, as `setAttributeNS` does (Chrome 150, measured
    // 2026-10-05: `1bad`, `-x`, `a:1` are accepted; `a b`, `a=b`, `:a`, `a:` and `""` refused, each with
    // Blink's own sentence). It checked the XML `Name` production on each colon part instead.
    if let Err((msg, name)) = crate::core::xml::dom_implementation::validate_and_extract_attr(ns.as_deref(), &raw) {
        throw_dom(scope, &format!("Failed to execute 'createAttributeNS' on 'Document': {msg}"), name);
        return;
    }
    if let Some(a) = crate::core::dom::attr::make_detached_attr(scope, &raw, "") {
        rv.set(a.into());
    }
}

/// `(DOMString or ElementCreationOptions) options`, converted as WebIDL converts the union (Chrome 150):
/// an object, `null` or `undefined` is the dictionary -- `customElementRegistry`, `is`, each read once --
/// and anything else a string (a Symbol refused). korvus read neither form.
fn element_creation_options(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, prefix: &str) -> Result<(), ()> {
    // no options (nearly every call): the empty dictionary, which reads nothing and cannot throw (no member is required)
    if v.is_undefined() {
        return Ok(());
    }
    if v.is_null() || v.is_object() {
        return crate::install::ctor_dicts::check_dict(scope, v, "ElementCreationOptions", prefix);
    }
    crate::binding::class::idl_string(scope, v, prefix).map(|_| ()).ok_or(())
}

pub(crate) fn cb_doc_create_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    const PREFIX: &str = "Failed to execute 'createElement' on 'Document': ";
    let Some(tag) = crate::binding::class::idl_string(scope, args.get(0), PREFIX) else { return };
    if element_creation_options(scope, args.get(1), PREFIX).is_err() {
        return;
    }
    // "Create an element", step 1: a name that is not a valid element local name throws. Korvus made an
    // element of ANY string (`createElement("1bad")`, `createElement("")`, `createElement("a b")`), where
    // Chrome 150 throws this, verbatim (measured 2026-10-05).
    if crate::core::xml::dom_implementation::element_local_name_error(&tag).is_some() {
        throw_dom(scope, &format!("Failed to execute 'createElement' on 'Document': The tag name provided ('{tag}') is not a valid name."), "InvalidCharacterError");
        return;
    }
    // a canvas: the GPU may be asked for soon (`webgl::prepare_gl`)
    if tag.eq_ignore_ascii_case("canvas") {
        crate::core::html::webgl::prepare_gl(scope);
    }
    // "Create an element", step 6: a tag with a DEFINITION whose name is its own local name is an
    // autonomous custom element, and the element is produced by CONSTRUCTING that definition -- not by
    // minting a bare wrapper and pointing it at the class's prototype.
    //
    // Korvus did the latter, and the difference is not cosmetic. `class X extends HTMLElement { … }`
    // has a constructor body; `document.createElement("x-thing")` never ran it, so every field it
    // assigns (`this.internals = this.attachInternals()`, a shadow root, an event listener) was simply
    // absent from the element the page got back. `new X()` already went through the spec path
    // (cb_html_element_ctor), so a page that used the constructor worked and the same page using
    // createElement did not -- one of the two ways every framework builds a custom element.
    //
    // The constructor itself creates the arena node with the right local name, so this returns its
    // result directly rather than creating a second node for it to shadow.
    if tag.contains('-') {
        let name_v: v8::Local<v8::Value> = str_val(scope, &tag.to_ascii_lowercase());
        let definition = crate::core::html::custom_element_registry::get(scope, name_v);
        if let Some(ctor) = definition {
            // A throwing constructor leaves its exception pending; propagate it the way `new X()` does.
            match ctor.new_instance(scope, &[]) {
                Some(el) => rv.set(el.into()),
                None => {}
            }
            return;
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // On an XML document the name keeps its case (`create_element_raw`): HTML's lowercasing is an HTML
    // rule, and a `createElement("Foo")` on a `createDocument()` result is a `<Foo>`.
    let xml = {
        let slot = doc_root_slot(scope, args.this().into());
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.is_in_xml(resolve_doc_root(dom, slot))
    };
    // "Create an element", namespace: the HTML namespace when this is an HTML document or its content
    // type is `application/xhtml+xml`, else NULL -- so `new Document().createElement("x")` is an
    // `Element` (Chrome 150; it was an HTMLUnknownElement).
    let ns = if !xml {
        None
    } else {
        match recorded_kind(scope, args.this().into()) {
            Some(DocKind::Xml(ct)) if !ct.eq_ignore_ascii_case("application/xhtml+xml") => Some(korvus_dom::ElementNs::Null),
            _ => Some(korvus_dom::ElementNs::Html),
        }
    };
    let id = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        match ns {
            None => dom.create_element(&tag),
            Some(ns) => dom.create_element_ns(&tag, ns),
        }
    };
    own_node(scope, args.this().into(), id);
    // Route through wrap_cached so this node's wrapper is interned in NodeCache: a later
    // getElementById/querySelector returning the same node yields the SAME JS object (node identity),
    // which `assert_equals(getElementById(...), el)` relies on. Arena ids are never reused (append-only
    // Vec), so a fresh id can never collide with a stale cache entry.
    let obj = wrap_cached(scope, &op_state, id);
    // A `<script>` created HERE was created by script, not by the parser, so its force-async flag
    // starts set -- `script.async` is true before anything touches it. The parser builds its nodes
    // straight into the arena and never reaches this function, which is precisely the distinction
    // the spec draws. See core/html/html_script_element.rs.
    if tag.eq_ignore_ascii_case("script") {
        crate::core::html::html_script_element::mark_force_async(scope, obj);
    }
    rv.set(obj.into());
}


/// `document.getElementById(id)` as a raw callback that tag-types via wrap_cached (so the result is the
/// right HTML*Element with its reflected props). Interned -> also gives getElementById identity.
pub(crate) fn cb_doc_get_element_by_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let id_str = args.get(0).to_rust_string_lossy(scope);
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let nid = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.get_element_by_id_in(resolve_doc_root(dom, slot), &id_str)
    };
    match nid { Some(n) => { let obj = wrap_cached(scope, &op_state, n); rv.set(obj.into()); } None => rv.set_null() }
}

/// `document.body = x` -- HTML's "the body element" setter, all five of its steps.
///
/// It used to be step 3 alone: replace whatever the body-or-nothing walk found, else append to the
/// document element. Everything the spec puts in front of that was missing, and each omission is a
/// WPT row in `html/dom/documents/dom-tree-accessors/Document.body.html`:
///
///   * a non-object (`document.body = "text"`) is a **TypeError** from the IDL conversion, not a
///     silent no-op -- `arg_node_id` answered `None` for it and the setter simply returned;
///   * anything that is not a `body` or `frameset` element (a `<div>`) is a
///     **HierarchyRequestError**, likewise not a no-op;
///   * with NO html element there is nothing to replace and nothing to append to, so the spec throws
///     **HierarchyRequestError** rather than appending to the document node;
///   * the element being replaced is the BODY ELEMENT -- the first `body`-or-`frameset` -- so
///     `doc.body = <body>` over an existing `<frameset>` replaces the frameset instead of appending
///     a second child beside it.
///
/// A document element that is not `html` (the test's `<test>` root) is the one case that neither
/// replaces nor throws: the body element is null AND `html_element_of` is null, yet Chrome appends
/// to the document element and the getter keeps answering null. The append target is therefore the
/// DOCUMENT ELEMENT, while what gets replaced is the BODY ELEMENT; they are two different questions
/// and the old code asked one of them twice.
pub(crate) fn cb_doc_body_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(new_id) = arg_node_id(scope, args.get(0)) else {
        crate::binding::protos::throw_type_error(scope, "Failed to set the 'body' property on 'Document': The provided value is not of type 'HTMLElement'.");
        return;
    };
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    enum Plan { Replace(u32), AppendTo(u32), NotAnElement, NoRoot }
    let plan = {
        let st = op_state.borrow();
        let root = resolve_doc_root(&st.borrow::<HostDom>().0, slot);
        if !is_html_element_named(&st, new_id, "body") && !is_html_element_named(&st, new_id, "frameset") {
            Plan::NotAnElement
        } else if let Some(old) = body_element_of(&st, root) {
            Plan::Replace(old)
        } else {
            match doc_element_child(&st.borrow::<HostDom>().0, root) {
                Some(de) => Plan::AppendTo(de),
                None => Plan::NoRoot,
            }
        }
    };
    match plan {
        Plan::NotAnElement => throw_dom(
            scope,
            "Failed to set the 'body' property on 'Document': The new body element is of type 'HTMLElement'. It must be either a 'BODY' or 'FRAMESET' element.",
            "HierarchyRequestError",
        ),
        Plan::NoRoot => throw_dom(
            scope,
            "Failed to set the 'body' property on 'Document': No document element exists.",
            "HierarchyRequestError",
        ),
        Plan::Replace(old) if old == new_id => {}
        Plan::Replace(old) => {
            let mut st = op_state.borrow_mut();
            let dom = &mut st.borrow_mut::<HostDom>().0;
            if let Some(p) = dom.parent(old) { dom.replace_child(p, new_id, old); }
        }
        Plan::AppendTo(de) => {
            let mut st = op_state.borrow_mut();
            st.borrow_mut::<HostDom>().0.append_child(de, new_id);
        }
    }
}

pub(crate) fn cb_doc_active_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id: Option<u32> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let root = resolve_doc_root(dom, slot);
        // FocusState is the PAGE's focus ring; a nested context has no focused element of its own
        // yet, so it falls straight through to its own body rather than borrowing the page's.
        let focused = if slot == PAGE_DOC_ROOT { st.borrow::<FocusState>().0.get() } else { None };
        focused.or_else(|| body_element_of(&st, root))
    };
    match id {
        Some(c) => { let o = wrap_cached(scope, &op_state, c); rv.set(o.into()); }
        None => rv.set_null(),
    }
}

pub(crate) fn cb_doc_create_fragment(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let new_id = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_fragment();
    own_node(scope, _args.this().into(), new_id);
    let w = wrap_cached(scope, &op_state, new_id);
    rv.set(w.into());
}

/// `document.getElementsByName(name)` — a **LIVE** NodeList (HTML: "return a live NodeList
/// containing all the HTML elements in that document that have a name attribute whose value is
/// equal to the name argument").
///
/// It was `make_static` over a snapshot of ids, so the list an earlier call handed a page kept
/// reporting the tree as it was at that moment; `Kind::DocByElementName` re-runs the arena query
/// through the same interceptor every other live collection uses, which also puts the namespace
/// filter in one place rather than beside this one caller.
pub(crate) fn cb_doc_get_elements_by_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = args.get(0).to_rust_string_lossy(scope);
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = { let st = op_state.borrow(); resolve_doc_root(&st.borrow::<HostDom>().0, slot) };
    let list = crate::core::dom::collections::make_live(
        scope,
        "NodeList",
        root,
        crate::core::dom::collections::Kind::DocByElementName,
        &name,
        None,
    );
    rv.set(list);
}

pub(crate) fn cb_doc_cookie_get(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A sandboxed frame's document (opaque origin) has no cookies: Chrome 150's SecurityError.
    let this: v8::Local<v8::Value> = _args.this().into();
    if crate::core::html::browsing_context::document_is_sandboxed(scope, this) {
        throw_dom(scope, "Failed to read the 'cookie' property from 'Document': The document is sandboxed and lacks the 'allow-same-origin' flag.", "SecurityError");
        return;
    }
    // The one cookie store (`transport::cookies`), read as this document's script may read it: its own URL, its
    // context and partition -- a token an edge set (a bot sensor `a bot cookie`, ...) shows, an HttpOnly cookie
    // never does.
    let s = match cookie_context(scope, this) {
        Some(ctx) => crate::modules::cookie_store::jar::with_harness(scope, |h| transport::cookies::document_cookie(&ctx, h)),
        None => String::new(),
    };
    let v = str_val(scope, &s); rv.set(v);
}

/// The cookie context `document.cookie` of `this` reads and writes in: the page's `CookieUrl` for the
/// page's document, a frame's own URL, cross-site context and top-level document for a frame's (see
/// `browsing_context::frame_doc_cookie_context`). `None` for a URL that does not parse.
fn cookie_context(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<transport::cookies::ScriptContext> {
    let root = doc_root_slot(scope, this);
    if root != PAGE_DOC_ROOT {
        if let Some((url, cross_site, top)) = crate::core::html::browsing_context::frame_doc_cookie_context(scope, root) {
            return transport::cookies::ScriptContext::new(&url, cross_site, Some(top.as_str()));
        }
        // Neither the page's document nor a frame's: a document without a browsing context (`new Document()`,
        // `createHTMLDocument`, DOMParser) is COOKIE-AVERSE -- its `cookie` reads "" and a write is dropped
        // (HTML "cookie-averse Document object"; Blink `Document::cookie` / `setCookie` return on `!GetFrame()`;
        // Chrome 150's oracle rows `new Document().cookie` -> "" before and after `= "x"`).
        return None;
    }
    let page = crate::modules::cookie_store::jar::page_cookie_url(scope);
    transport::cookies::ScriptContext::new(&page, false, None)
}

/// `document.cookie = line`: one `Set-Cookie`-shaped write into the one store, Chrome's rules deciding what it
/// stores (`RestrictedCookieManager::SetCookieFromString`); the realm's `cookieStore` hears what it changed.
pub(crate) fn cb_doc_cookie_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if crate::core::html::browsing_context::document_is_sandboxed(scope, this) {
        throw_dom(scope, "Failed to set the 'cookie' property on 'Document': The document is sandboxed and lacks the 'allow-same-origin' flag.", "SecurityError");
        return;
    }
    let raw = args.get(0).to_rust_string_lossy(scope);
    let Some(ctx) = cookie_context(scope, this) else { return };
    let changes = crate::modules::cookie_store::jar::with_harness(scope, |h| transport::cookies::set_document_cookie(&ctx, &raw, h));
    crate::modules::cookie_store::jar::cookie_emit(scope, &changes);
}

/// The NodeIterator's collection: `node_id` and its descendants, in tree order, keeping the ones
/// DOM's filtering production ACCEPTs. Releases the arena borrow before each filter call (the
/// filter re-enters JS).
///
/// TWO THINGS HERE ARE THE SPEC AND WERE NOT:
///
/// 1. **The root is IN the collection.** The walk used to start at `children_of(node_id)`, so the
///    root was never a member and `createNodeIterator(p).nextNode()` answered `p`'s first CHILD.
///    DOM's iterator collection is "the INCLUSIVE descendants of root" and its `nextNode` starts
///    with `referenceNode = root, pointerBeforeReferenceNode = true`, which returns the root
///    itself first. (dom.spec.whatwg.org #interface-nodeiterator; WPT
///    `dom/traversal/NodeIterator.html`, 148 subtests.)
/// 2. **FILTER_REJECT does not prune.** For a NodeIterator, REJECT and SKIP are the same answer --
///    the traverse loop tests only "result is FILTER_ACCEPT" and otherwise moves to the next node
///    in tree order. Pruning the subtree is the TREEWALKER's rule, and the copy of it that lived
///    here made a rejected element's whole subtree invisible to the iterator.
///
/// The filtering itself is [`tw_accept_node`] -- ONE production for both cursors. The private copy
/// that used to sit here (`tw_show_bit` + `tw_run_filter`) was a strictly narrower subset of it:
/// a four-entry `whatToShow` table (so a DocumentType, a CDATASection or a ProcessingInstruction
/// was invisible even under SHOW_ALL, where the rule is `1 << (nodeType - 1)` for every kind), an
/// `acceptNode` looked up once instead of per call, no WebIDL `unsigned short` truncation of the
/// return, a `JsElement` husk instead of `document` for the sentinel id, and a throwing filter
/// silently converted into FILTER_REJECT instead of propagating.
///
/// `Err(())` means a filter threw (or a WebIDL conversion failed) and the exception is already
/// pending on the scope: the caller must return without touching the scope again.
fn ni_collect(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    node_id: u32,
    what_to_show: u32,
    filter: &Option<v8::Global<v8::Value>>,
    active: &std::cell::Cell<bool>,
    out: &mut Vec<u32>,
) -> Result<(), ()> {
    if tw_accept_node(scope, op_state, what_to_show, filter, active, node_id)? == 1 {
        out.push(node_id);
    }
    let children: Vec<u32> = { let st = op_state.borrow(); children_of(&st.borrow::<HostDom>().0, node_id) };
    for child in children {
        ni_collect(scope, op_state, child, what_to_show, filter, active, out)?;
    }
    Ok(())
}
/// Read (root, whatToShow, filter) from the factory args (whatToShow default SHOW_ALL; filter optional).
/// `(Node root, optional unsigned long whatToShow = 0xFFFFFFFF, optional NodeFilter? filter = null)`, converted by
/// Blink's binding before the operation (`createTreeWalker(document, 0, 5)` is "parameter 3 is
/// not of type 'Object'." in Chrome 150; korvus took the 5): `None` after throwing.
fn tw_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, op: &str) -> Option<(u32, u32, Option<v8::Global<v8::Value>>)> {
    let label = if op == "createTreeWalker" { "Document::createTreeWalker" } else { "Document::createNodeIterator" };
    crate::install::ctor_dicts::convert_method_args(scope, label, args, &format!("Failed to execute '{op}' on 'Document': ")).ok()?;
    let root = arg_node_id(scope, args.get(0))?;
    let a1 = args.get(1);
    let wts = if a1.is_undefined() { 0xFFFF_FFFFu32 } else { a1.uint32_value(scope).unwrap_or(0xFFFF_FFFF) };
    let f = args.get(2);
    let filter = if f.is_undefined() || f.is_null() { None } else { Some(v8::Global::new(scope, f)) };
    Some((root, wts, filter))
}
/// `document.createTreeWalker(root, whatToShow = SHOW_ALL, filter = null)` — builds the LIVE walker
/// (root/whatToShow/filter/currentNode). No tree is flattened and no filter is run here: a walker
/// traverses lazily against the arena as it stands at each call (dom.spec.whatwg.org #interface-treewalker).
/// `root` is required and must be a Node (WebIDL: a missing/non-Node first argument is a TypeError).
pub(crate) fn cb_doc_create_tree_walker(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((root, wts, filter)) = tw_args(scope, &args, "createTreeWalker") else { return };
    let tw = crate::binding::class::mint(scope, TreeWalker {
        root: std::cell::Cell::new(root),
        what_to_show: std::cell::Cell::new(wts),
        filter: std::cell::RefCell::new(filter),
        current_node: std::cell::Cell::new(root),
        active: std::cell::Cell::new(false),
    });
    rv.set(tw.into());
}
pub(crate) fn cb_doc_create_node_iterator(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((root, wts, filter)) = tw_args(scope, &args, "createNodeIterator") else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut list = Vec::new();
    // The recursion guard is LOCAL because the collection is built here, once, and never again: an
    // `active` field on `NodeIterator` would be dead state under the snapshot model (MODEL GAP #17
    // -- a real NodeIterator filters lazily on every traverse, and this one cannot).
    let active = std::cell::Cell::new(false);
    if ni_collect(scope, &op_state, root, wts, &filter, &active, &mut list).is_err() {
        return; // a filter threw; its exception is pending on the scope
    }
    let it = crate::binding::class::mint(scope, NodeIterator { root: std::cell::Cell::new(root), what_to_show: std::cell::Cell::new(wts), filter: std::cell::RefCell::new(filter), list: std::cell::RefCell::new(list), idx: std::cell::Cell::new(-1), ref_node: std::cell::Cell::new(root), pointer_before: std::cell::Cell::new(true) });
    rv.set(it.into());
}

// `document.implementation` is `core/xml/dom_implementation.rs`'s: per DOCUMENT, so the nodes it
// creates owner-mark against the document it came from.
pub(crate) use crate::core::xml::dom_implementation::cb_doc_implementation;

/// XML 1.0 (5th ed.) `NameStartChar`.
fn is_xml_name_start(c: char) -> bool {
    matches!(c,
        ':' | '_' | 'A'..='Z' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}'
        | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}'
        | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}'
        | '\u{10000}'..='\u{EFFFF}')
}

/// XML 1.0 (5th ed.) **`Name`** — the production `document.createProcessingInstruction`'s TARGET
/// must match (DOM 4.5, step 1).
///
/// THIS IS NOT `validate_and_extract`'s "valid element local name" and must not be replaced by it:
/// that is HTML's loose rule (an ASCII letter, then anything but NUL, `/`, `>` and ASCII
/// whitespace), which accepts `"0"`, `"\f"` and `"\\A"` as PI targets where XML — and WPT's
/// `Document-createProcessingInstruction.html` — reject all three. It also has the opposite error:
/// the old ASCII-only check here refused `"A\u{B7}A"`, and U+00B7 MIDDLE DOT is a `NameChar`.
fn is_xml_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    if !is_xml_name_start(first) {
        return false;
    }
    chars.all(|c| {
        is_xml_name_start(c)
            || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
    })
}
pub(crate) fn cb_doc_create_pi(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let target = args.get(0).to_rust_string_lossy(scope);
    // DOM 4.5 has TWO steps and this had one of them, applied to the wrong string: `"?>"` is
    // forbidden in the DATA, not in the target (a target could never contain it and still be a
    // Name), so `createProcessingInstruction("A", "?>")` was built instead of throwing.
    if !is_xml_name(&target) { idb_throw(scope, "InvalidCharacterError", "Invalid processing instruction target"); return; }
    let a1 = args.get(1);
    let data = if a1.is_null() || a1.is_undefined() { String::new() } else { a1.to_rust_string_lossy(scope) };
    if data.contains("?>") { idb_throw(scope, "InvalidCharacterError", "Invalid processing instruction data"); return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = { let mut st = op_state.borrow_mut(); st.borrow_mut::<HostDom>().0.create_processing_instruction(&target, &data) };
    // THIS document's node, as `doc_create!`'s are: on a created document it answered the page's
    // `ownerDocument`, and in a frame it wore the page's prototype.
    own_node(scope, args.this().into(), id);
    let node = wrap_cached(scope, &op_state, id);
    rv.set(node.into());
}

// ---- Native Document class + document-level member ops (moved verbatim from bindings.rs) ----

/// The root slot of a Document that was never told one: "whatever the arena's root is now".
///
/// It is `u32::MAX`, the same value `ON_DOCUMENT_ID`/`arg_node_id` already use to mean "the page
/// document", so the two conventions cannot drift apart. A Document minted over a real arena root
/// (an `<iframe>`'s, a `DOMParser` result's) carries THAT id instead, and every document-level
/// query below resolves through [`resolve_doc_root`].
pub(crate) const PAGE_DOC_ROOT: u32 = u32::MAX;

/// Native `Document`.
///
/// It used to be a field-less unit struct, which is why there was exactly one document here: a
/// Document could not say which tree it was, so every document-level member fell back to the one
/// arena root and an `<iframe>`'s `getElementById` answered with the parent page's element. The
/// root is that missing state, and the main document is not a special case -- its root is simply
/// the arena root ([`PAGE_DOC_ROOT`] resolves to it).
#[repr(C)]
pub struct JsDocument {
    pub(crate) root: u32,
}

unsafe impl GarbageCollected for JsDocument {
    fn trace(&self, _v: &mut deno_core::v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static CStr {
        c"Document"
    }
}

/// The root slot named by a Document receiver. Anything that is not a native Document (there is no
/// such receiver on `Document.prototype`, but a callback must never assume) reads as the page.
pub(crate) fn doc_root_slot(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> u32 {
    deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this)
        .map(|p| unsafe { p.as_ref() }.root)
        .unwrap_or(PAGE_DOC_ROOT)
}

/// Turn a root slot into a live arena node id. Kept separate from [`doc_root_slot`] because the
/// slot is read from the v8 receiver (needs `scope`) and resolved against the arena (needs the
/// `OpState` borrow), and holding both at once is what the borrow checker refuses.
#[inline]
pub(crate) fn resolve_doc_root(dom: &korvus_dom::Dom, slot: u32) -> u32 {
    if slot == PAGE_DOC_ROOT { dom.root() } else { slot }
}

// The `#[op2] impl JsDocument` that stood here declared the document-side arena plumbing
// (`createElementRaw`, `parseXml`, `documentForRoot`, `pageDocument`, ...) on a deno_core class template that the
// bootstrap then stripped (`unexpose_internal_plumbing`, deleted with it): no page and no Rust code reached them.
// A document wrapper is minted by `binding::protos::make_wrapper` / `document::mint_document`.

// getElementsByTagName/ClassName are TABLE ROWS (`install_document` below binds `cb_doc_by_name` /
// `cb_by_tag_ns`, and `core/dom/element.rs` binds `cb_el_by_name` for the element-scoped pair), each
// building the live HTMLCollection in `core/dom/collections.rs`. This comment used to say they went
// through `op_helper_method` over `KB.__docGetElementsByTagName`; that stopped being true when the
// rows landed, and the op was deleted, so it is corrected here rather than left to rot.

/// Document-level getter. The receiver names the document ROOT to answer for: the page's document
/// resolves to the arena root, a nested browsing context's to its own parsed root.
macro_rules! doc_getter {
    ($cb:ident, |$dom:ident, $root:ident| $target:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let slot = doc_root_slot(scope, args.this().into());
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let target: Option<u32> = { let st = op_state.borrow(); let $dom = &st.borrow::<HostDom>().0; let $root = resolve_doc_root($dom, slot); $target };
            match target {
                Some(c) => { let o = wrap_cached(scope, &op_state, c); rv.set(o.into()); }
                None => rv.set_null(),
            }
        }
    };
}
/// The document element, and the two elements hanging off it.
///
/// These are defined by POSITION, not by tag search: `documentElement` is the document's element
/// child, `head` and `body` are children of THAT element. They used to be
/// `get_elements_by_tag_name("html"|"head"|"body").first()`, which searches the whole arena and so
/// kept answering with nodes that had been detached from the tree -- after
/// `document.replaceChildren()` the document had no children at all and `documentElement` still
/// returned the old `<html>`, a contradiction any script can see (`childElementCount === 0` beside a
/// non-null `firstElementChild`). Walking the children costs a handful of comparisons and cannot go
/// stale.
/// `pub(crate)` so the scroll code can answer "which node is the document element" with
/// the SAME walk `document.documentElement` uses — the document's scroll position is that element's
/// `scrollTop`/`scrollLeft`, and two different answers to "which element" would be two scroll
/// positions again.
pub(crate) fn doc_element_child(dom: &korvus_dom::Dom, root: u32) -> Option<u32> {
    first_child_where(dom, root, |c| dom.node_type(c) == 1)
}

/// The first child of `parent` that `f` accepts, walked over the sibling links (no list is made).
fn first_child_where(dom: &korvus_dom::Dom, parent: u32, mut f: impl FnMut(u32) -> bool) -> Option<u32> {
    let mut c = dom.first_child(parent);
    while let Some(id) = c {
        if f(id) {
            return Some(id);
        }
        c = dom.next_sibling(id);
    }
    None
}
// `html_child_named` WAS HERE AND IS DELETED (CONF DOC1.3).
//
// "The child of the document element whose QUALIFIED NAME case-insensitively equals this tag" is
// not a rule HTML has, and its six callers each wanted one of the three productions below instead.
// It answered a foreign-namespace `<body>` as the body element and refused an HTML-namespace
// `<blah:head>` as the head element, and `document.body`'s getter built its tree-order rule on top
// of it by calling it twice. the note -- that the focus ring must answer "which node is
// `<body>`" with the SAME walk `document.activeElement` uses -- still holds and is now
// `body_element_of`, which both call.

/// Is `id` an **HTML element** whose LOCAL name is `local`?
///
/// The predicate HTML means every time it writes "a `body` element" or "the `html` element", and the
/// one this file kept getting wrong in two independent ways at once. The deleted `html_child_named`
/// compared
/// `Dom::tag_name` -- the QUALIFIED name -- to a bare tag and filtered on nothing else, so
/// `createElementNS("http://example.org/test", "body")` was a body element (it is not: the namespace
/// is wrong) and `createElementNS(HTML, "blah:head")` was not a head element (it is: the local name
/// is `head`). Both are in WPT, in the same two files, and both are this one check.
fn is_html_element_named(st: &deno_core::OpState, id: u32, local: &str) -> bool {
    let dom = &st.borrow::<HostDom>().0;
    if dom.node_type(id) != 1 || !dom.is_html_ns(id) {
        return false;
    }
    crate::core::html::html_element::with_local_name(st, id, |n| n.eq_ignore_ascii_case(local)).unwrap_or(false)
}

/// HTML's **"the html element"**: a document's document element, WHEN that element is an `html`
/// element. `null` otherwise -- a document whose root is a `<body>`, or an `html` in a foreign
/// namespace, has no html element, and therefore no body element and no head element either.
pub(crate) fn html_element_of(st: &deno_core::OpState, root: u32) -> Option<u32> {
    let de = doc_element_child(&st.borrow::<HostDom>().0, root)?;
    is_html_element_named(st, de, "html").then_some(de)
}

/// HTML's **"the body element"**: the FIRST child of the html element that is either a `body` or a
/// `frameset` element.
///
/// "First ... that is either" is one pass in TREE ORDER over both tags. It used to be
/// `html_child_named("body").or_else(html_child_named("frameset"))` -- every body anywhere under the
/// html element before any frameset -- so `<html><frameset><body>` answered the body where the spec
/// (and Chrome) answer the frameset, and the `document.body` SETTER inherited it: with a frameset in
/// the tree and no body it found nothing to replace and APPENDED instead, leaving two.
pub(crate) fn body_element_of(st: &deno_core::OpState, root: u32) -> Option<u32> {
    let html = html_element_of(st, root)?;
    first_child_where(&st.borrow::<HostDom>().0, html, |c| is_html_element_named(st, c, "body") || is_html_element_named(st, c, "frameset"))
}

/// HTML's **"the head element"**: the first child of the html element that is a `head` element.
pub(crate) fn head_element_of(st: &deno_core::OpState, root: u32) -> Option<u32> {
    let html = html_element_of(st, root)?;
    first_child_where(&st.borrow::<HostDom>().0, html, |c| is_html_element_named(st, c, "head"))
}

/// HTML's ASCII whitespace: TAB, LF, FF, CR, SPACE -- not `char::is_whitespace`, which would also
/// eat U+00A0 and the other Unicode spaces a title may legitimately carry.
fn is_ascii_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ')
}

/// "Strip and collapse ASCII whitespace".
pub(crate) fn strip_and_collapse(s: &str) -> String {
    s.split(is_ascii_ws).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// An element's CHILD text content: its Text (and CDATASection) CHILDREN concatenated, in order --
/// not its descendants. Measured on stock Chrome 150.0.7871.46: a `<title>` holding `" A "`,
/// `<b>BOLD</b>` and `" C "` reads `title.text` `" A  C "` and `document.title` `"A C"`.
pub(crate) fn child_text_content(dom: &korvus_dom::Dom, el: u32) -> String {
    let mut out = String::new();
    for c in dom.children(el) {
        if matches!(dom.node_type(c), 3 | 4) {
            out.push_str(&dom.node_value(c).unwrap_or_default());
        }
    }
    out
}

/// Is `root`'s document element an SVG `svg`? That document's title is an SVG `<title>` CHILD of it.
fn svg_document_element(dom: &korvus_dom::Dom, root: u32) -> Option<u32> {
    let de = doc_element_child(dom, root)?;
    (dom.element_ns(de) == Some(&korvus_dom::ElementNs::Svg) && dom.tag_name(de).is_some_and(|t| t == "svg")).then_some(de)
}

/// HTML's "the title element" of the document at `root`, per the document's kind:
///
/// * the document element is an SVG `svg`: the first SVG `title` that is a CHILD of it;
/// * otherwise: the first HTML-namespace `title` in tree order -- an SVG or MathML `<title>` is not
///   one, wherever it sits.
///
/// Measured on stock Chrome 150.0.7871.46: `<svg><title>svg one</title></svg><title>html
/// two</title>` in an HTML body reads `"html two"`; an HTML document whose only `<title>` is inside an
/// `<svg>` or a `<math>` reads `""`; an `image/svg+xml` document whose root has
/// `<g><title>nested</title></g><title>root child</title>` reads `"root child"`, and with only the
/// nested one reads `""`.
pub(crate) fn title_element(dom: &korvus_dom::Dom, root: u32) -> Option<u32> {
    if let Some(svg) = svg_document_element(dom, root) {
        return dom
            .children(svg)
            .into_iter()
            .find(|&c| dom.element_ns(c) == Some(&korvus_dom::ElementNs::Svg) && dom.tag_name(c).is_some_and(|t| t == "title"));
    }
    dom.get_elements_by_tag_name_in(root, "title").into_iter().find(|&t| dom.is_html_ns(t))
}

/// `document.title` for ONE document tree: the title element's child text content, ASCII whitespace
/// stripped and collapsed (`"  a\n b\tc "` reads `"a b c"` in Chrome 150).
fn doc_title(dom: &korvus_dom::Dom, root: u32) -> String {
    title_element(dom, root).map(|t| strip_and_collapse(&child_text_content(dom, t))).unwrap_or_default()
}

/// The page document's `document.title` (D88 step 2: a session-history entry's title, a tab's); `""` before there
/// is a page DOM.
pub fn page_title(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) -> String {
    let st = op_state.borrow();
    let Some(h) = st.try_borrow::<HostDom>() else { return String::new() };
    doc_title(&h.0, resolve_doc_root(&h.0, PAGE_DOC_ROOT))
}
/// `document.body` / `document.head` -- [`doc_getter!`]'s shape, hand-written because the body of
/// each is HTML's own production ([`body_element_of`] / [`head_element_of`]) and those need the
/// `OpState` the macro keeps private (a local name lives in `QNameMap`, not in the arena).
macro_rules! doc_state_getter {
    ($cb:ident, $of:ident) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let slot = doc_root_slot(scope, args.this().into());
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let target: Option<u32> = {
                let st = op_state.borrow();
                let root = resolve_doc_root(&st.borrow::<HostDom>().0, slot);
                $of(&st, root)
            };
            match target {
                Some(c) => { let o = wrap_cached(scope, &op_state, c); rv.set(o.into()); }
                None => rv.set_null(),
            }
        }
    };
}
doc_state_getter!(cb_doc_body, body_element_of);
doc_state_getter!(cb_doc_head, head_element_of);
doc_getter!(cb_doc_document_element, |dom, root| doc_element_child(dom, root));
doc_getter!(cb_doc_doctype, |dom, root| dom.doctype_in(root));
// document.activeElement: the focused element (FocusState, set by el.focus()), else the body. Uses
// wrap_cached so identity holds (activeElement === getElementById of the same node). Hand-written (not
// doc_getter!) because it reads FocusState in addition to HostDom.

/// Document string-arg member (implicit document receiver): global query. `$target`/`$ids` yield a
/// `Result`; an `Err` (invalid selector) throws a DOM `SyntaxError`, which is what `querySelector`
/// and `querySelectorAll` -- the only two members this macro serves -- do with a selector that does
/// not parse. It had a third arm, `list`, for `getElementsByTagName`/`ClassName`; those are
/// `cb_doc_by_name` rows over the live HTMLCollection factory and never went through this macro, so
/// The arm was deleted with the two ops that were its only reachers.
macro_rules! doc_query {
    ($cb:ident, $method:literal, single, |$dom:ident, $root:ident, $sel:ident| $target:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let $sel = match args.get(0).to_string(scope) { Some(s) => s.to_rust_string_lossy(scope), None => return };
            let slot = doc_root_slot(scope, args.this().into());
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let target: Result<Option<u32>, _> = { let st = op_state.borrow(); let $dom = &st.borrow::<HostDom>().0; let $root = resolve_doc_root($dom, slot); $target };
            match target {
                Ok(Some(c)) => { let o = wrap_cached(scope, &op_state, c); rv.set(o.into()); }
                Ok(None) => rv.set_null(),
                Err(_) => crate::core::dom::shared::throw_selector_error(scope, $method, "Document", &$sel),
            }
        }
    };
    // `nodelist` is the same body with the IDL return type the member actually has: Document's
    // querySelectorAll answers a STATIC NodeList, which is what separates it from
    // getElementsByTagName/ClassName -- those answer a LIVE HTMLCollection and are built by
    // `cb_doc_by_name` over `core/dom/collections.rs`, not here.
    ($cb:ident, $method:literal, nodelist, |$dom:ident, $root:ident, $sel:ident| $ids:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let $sel = match args.get(0).to_string(scope) { Some(s) => s.to_rust_string_lossy(scope), None => return };
            let slot = doc_root_slot(scope, args.this().into());
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let ids: Result<Vec<u32>, _> = { let st = op_state.borrow(); let $dom = &st.borrow::<HostDom>().0; let $root = resolve_doc_root($dom, slot); $ids };
            let ids = match ids { Ok(v) => v, Err(_) => { crate::core::dom::shared::throw_selector_error(scope, $method, "Document", &$sel); return; } };
            let arr = v8::Array::new(scope, ids.len() as i32);
            for (i, cid) in ids.iter().enumerate() { let w = wrap_cached(scope, &op_state, *cid); arr.set_index(scope, i as u32, w.into()); }
            let list = crate::core::dom::collections::make_static(scope, "NodeList", arr);
            rv.set(list);
        }
    };
}
doc_query!(cb_doc_query_selector, "querySelector", single, |dom, root, sel| dom.query_selector_in(root, &sel));
doc_query!(cb_doc_query_selector_all, "querySelectorAll", nodelist, |dom, root, sel| dom.query_selector_all_in(root, &sel));

/// Document factory taking a string arg (createTextNode/createComment): mutates the arena, returns
/// the new node's interned+retyped wrapper.
macro_rules! doc_create {
    ($cb:ident, |$dom:ident, $text:ident| $new_id:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let $text = match args.get(0).to_string(scope) { Some(s) => s.to_rust_string_lossy(scope), None => std::string::String::new() };
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let new_id: u32 = { let mut st = op_state.borrow_mut(); let $dom = &mut st.borrow_mut::<HostDom>().0; $new_id };
            // A node a created document makes belongs to THAT document, not to the page whose arena
            // it shares (WPT DOMImplementation-createDocument "ownerDocument").
            own_node(scope, args.this().into(), new_id);
            let w = wrap_cached(scope, &op_state, new_id);
            rv.set(w.into());
        }
    };
}
doc_create!(cb_doc_create_text_node, |dom, text| dom.create_text_node(&text));
/// `document.createCDATASection(data)` — [`doc_create!`]'s body behind DOM 4.5's two guards, which
/// it had neither of.
///
/// A CDATA section is an XML-only node: HTML has no syntax that produces one, so the spec's step 1
/// is "if this is an HTML document, throw NotSupportedError" and Chrome does. This answered an
/// ordinary CDATASection instead, on a document that can never serialise it. Step 2 is the closing
/// delimiter: `]]>` inside the data would end the section early, so it is an InvalidCharacterError.
pub(crate) fn cb_doc_create_cdata(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let text = match args.get(0).to_string(scope) { Some(s) => s.to_rust_string_lossy(scope), None => String::new() };
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let is_html_doc = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        !dom.is_in_xml(resolve_doc_root(dom, slot))
    };
    if is_html_doc {
        throw_dom(scope, "This operation is not supported for HTML documents", "NotSupportedError");
        return;
    }
    if text.contains("]]>") {
        idb_throw(scope, "InvalidCharacterError", "String contains ']]>'");
        return;
    }
    let new_id: u32 = { let mut st = op_state.borrow_mut(); st.borrow_mut::<HostDom>().0.create_cdata_section(&text) };
    own_node(scope, args.this().into(), new_id);
    let w = wrap_cached(scope, &op_state, new_id);
    rv.set(w.into());
}
doc_create!(cb_doc_create_comment, |dom, text| dom.create_comment(&text));

/// `new Text(data)` / `new Comment(data)` — the constructors WebIDL declares and the spine did not
/// have.
///
/// Both are `constructor(optional DOMString data = "")`, and `Text.prototype` was reached only
/// through `document.createTextNode`. `new Text()` threw "Illegal constructor", which is what an
/// interface with NO constructor must do -- so the answer looked right while being wrong for this one,
/// and every recorded case that reaches a Text through its constructor (all nine of them) failed on
/// the receiver instead of the member.
///
/// A raw callback works as a constructor because a constructor that returns an object returns THAT
/// object; the wrapper is the node, with the same interning and prototype `createTextNode` gives.
/// The `new.target` check is the other half of the contract: WebIDL constructors are not callable,
/// and `Text("x")` without `new` is a TypeError in Chrome.
macro_rules! node_ctor {
    ($cb:ident, $iface:literal, |$dom:ident, $text:ident| $new_id:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            if !crate::binding::protos::require_ctor_new_target(scope, &args, $iface) {
                return;
            }
            let a0 = args.get(0);
            let $text = if a0.is_undefined() {
                std::string::String::new()
            } else {
                match a0.to_string(scope) { Some(s) => s.to_rust_string_lossy(scope), None => std::string::String::new() }
            };
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let new_id: u32 = { let mut st = op_state.borrow_mut(); let $dom = &mut st.borrow_mut::<HostDom>().0; $new_id };
            let w = wrap_cached(scope, &op_state, new_id);
            rv.set(w.into());
        }
    };
}
node_ctor!(cb_text_ctor, "Text", |dom, text| dom.create_text_node(&text));
node_ctor!(cb_comment_ctor, "Comment", |dom, text| dom.create_comment(&text));

/// `new DocumentFragment()` — WebIDL declares the constructor and the spine had only the stub.
///
/// Takes no argument, so it does not fit `node_ctor!`; everything else is the same, including that a
/// fragment is an arena node whose tag is the `#fragment` sentinel `document.createDocumentFragment`
/// uses.
pub(crate) fn cb_fragment_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        crate::binding::protos::throw_type_error(
            scope,
            &crate::binding::protos::use_new_msg("DocumentFragment"),
        );
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let new_id = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_fragment();
    let w = wrap_cached(scope, &op_state, new_id);
    rv.set(w.into());
}

// `document.location` === `window.location` (the SAME Location object -- a Document does not own a
// separate one). Native getter over the global `location` binding, replacing the JS accessor
// `get: function () { return globalThis.location; }` that used to live in js/dom/01-interning.js.
// Absent `location` yields `undefined`, exactly as the property read did.
//
// A document with no browsing context has no Location at all: `new Document().location` is null in
// the recorded browser, and null is a different answer from undefined to a page that tests for it.
// A nested or auxiliary browsing context's document has ITS window's Location -- the same object,
// not a copy (core/html/browsing_context.rs).
pub(crate) fn cb_doc_location(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if doc_root_slot(scope, this) != PAGE_DOC_ROOT {
        match crate::core::html::browsing_context::frame_document_location(scope, this) {
            Some(l) => rv.set(l.into()),
            None => rv.set_null(),
        }
        return;
    }
    if let Some(v) = global_get(scope, "location") { rv.set(v); }
}

// `document.importNode(node, deep)`: a clone of `node` adopted into this document. Korvus shares one
// arena across documents, so `cloneNode` already yields a detached node whose casing/namespace resolve
// against this (HTML) document -- e.g. importing an XML <div> yields tagName "DIV" (WPT Element-tagName).
// The delegation to the node's own `cloneNode` is kept (not re-implemented against the arena) so an
// overridden/subclassed cloneNode still wins, which is what the JS shim this replaces did.
pub(crate) fn cb_doc_import_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let node = args.get(0);
    if node.is_null_or_undefined() {
        throw_type_error(scope, "Failed to execute 'importNode' on 'Document': parameter 1 is not of type 'Node'.");
        return;
    }
    let deep: v8::Local<v8::Value> = v8::Boolean::new(scope, args.get(1).boolean_value(scope)).into();
    // WebIDL's `Node` argument, then DOM's two refusals, then the clone -- by the realm's own `cloneNode`, never
    // the page's (it looked `cloneNode` up on the node)
    let kind = if crate::core::dom::shared::document_node_id(scope, node).is_some() {
        Some((9, false))
    } else {
        crate::binding::protos::arg_node_id(scope, node).and_then(|id| {
            let os = deno_core::JsRuntime::op_state_from(scope);
            let st = os.borrow();
            st.try_borrow::<HostDom>().map(|d| (d.0.node_type(id), d.0.is_shadow_root(id)))
        })
    };
    match kind {
        None => {
            throw_type_error(scope, "Failed to execute 'importNode' on 'Document': parameter 1 is not of type 'Node'.");
            return;
        }
        Some((_, true)) => {
            throw_dom(scope, "Failed to execute 'importNode' on 'Document': The node provided is a shadow root, which may not be imported.", "NotSupportedError");
            return;
        }
        Some((9, false)) => {
            throw_dom(scope, "Failed to execute 'importNode' on 'Document': The node provided is a document, which may not be imported.", "NotSupportedError");
            return;
        }
        _ => {}
    }
    let Some(r) = crate::binding::protos::pristine_call(scope, node, "Node.cloneNode", &[deep]) else { return };
    // "A clone of node, with document set to this" (DOM): `cloneNode` made the copy the SOURCE's, so the
    // copy and everything in it are made this document's, and the copy's wrapper takes the prototypes of
    // this document's realm -- `frame.contentDocument.importNode(el).ownerDocument` was the page's
    // (`custom-elements/registries/Document-importNode-cross-document`).
    if let Ok(copy) = v8::Local::<v8::Object>::try_from(r) {
        if let Some(id) = crate::binding::protos::unwrap_id(scope, copy.into()) {
            let this: v8::Local<v8::Value> = args.this().into();
            let target = (doc_root_slot(scope, this) != PAGE_DOC_ROOT).then(|| v8::Global::new(scope, this));
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let to_page = target.is_none();
            crate::core::dom::node::set_node_document(&op_state, target, id);
            if to_page {
                crate::core::html::browsing_context::rehome_to_page(scope, copy);
            } else {
                crate::core::html::browsing_context::rehome_node_wrapper(scope, copy, id);
            }
        }
    }
    // AND THE IMPORTED COPY IS UPGRADED. DOM's clone creates each element with "create
    // an element" against the TARGET document, and for a document with a browsing context that
    // enqueues an upgrade reaction -- drained before `importNode` returns, because `importNode` is
    // [CEReactions]. korvus upgraded only on INSERTION, so a custom element imported from a
    // `<template>` reached JS with its definition's prototype and its constructor never run.
    //
    // MEASURED, openlibrary.org off its own recording: lit-html renders by importing the template,
    // committing property values onto the copies, and inserting the fragment last -- so it assigns
    // `element.items = ...` while the copy is still detached. That setter is a Lit reactive-property
    // accessor, `finalize()` had put it on the prototype, and on an instance whose constructor never
    // ran it reached `this._$changedProperties.has(name)` with `_$changedProperties` undefined:
    //
    //   TypeError: Cannot read properties of undefined (reading 'has')
    //     at requestUpdate ... at set [as items] ... at PropertyPart._$AI ...
    //
    // one unhandled rejection, `ok` -> `threw`, on a page whose DOM was otherwise byte-identical.
    // The prototype-without-a-constructor shape is the one FP14 named for the fragment parser; this
    // is the same shape at the other creation point, and `upgrade_one` is the same answer.
    // `upgrade_tree` runs no connect reaction for a detached copy -- it asks `isConnected` first.
    if let Ok(copy) = v8::Local::<v8::Object>::try_from(r) {
        if doc_root_slot(scope, args.this().into()) == PAGE_DOC_ROOT {
            crate::core::html::custom_element_registry::upgrade_tree(scope, copy, None);
        }
    }
    rv.set(r);
}


// ---- Document's constant-ish readonly surface ------------------------------------------------
//
// `characterSet`, `contentType`, `compatMode` and friends were absent, and every one has a single
// answer for the documents Korvus actually has. Constants, but the honest kind: each is what this
// engine IS, not a value copied out of a recorded expectation.

/// `document.URL` / `document.documentURI` — the document's URL.
///
/// `CookieUrl` is the same string `document.cookie` scopes against; `try_borrow` because a context
/// that never navigated (the quality harness, a bare worker) does not have it, and `borrow` inside a
/// v8 callback aborts the process rather than failing.
pub(crate) fn cb_doc_url(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A FRAME'S DOCUMENT IS ITS CONTEXT'S URL. It answered `about:blank` -- the answer for
    // a document a script created -- while `location.href` in the same frame was right; Chrome 150:
    // `http://.../f.html?q=1#h` for both `URL` and `documentURI`. last.fm's third-party-cookie check
    // reads it.
    if let Some(fid) = frame_of_this_document(scope, _args.this().into()) {
        let u = crate::core::html::browsing_context::frame_context_url(scope, fid);
        let v = str_val(scope, &u);
        rv.set(v);
        return;
    }
    // A document a script CREATED (`createDocument`, `createHTMLDocument`, a DOMParser result) has no
    // browsing context and therefore no URL of its own: `about:blank`, as the recorded browser answers
    // and as `dom/05-xml.js`'s facade did. The page's document and a nested context's still answer the
    // location they were loaded from.
    if recorded_kind(scope, _args.this().into()).is_some() {
        let v = str_val(scope, "about:blank");
        rv.set(v);
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let u = document_url_of(&op_state.borrow());
    let v = str_val(scope, &u);
    rv.set(v);
}

/// A readonly string that is the same for every document this engine parses.
macro_rules! doc_const {
    ($cb:ident, $value:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let v = str_val(scope, $value);
            rv.set(v);
        }
    };
}

// `document.characterSet` / `charset` / `inputEncoding` -- the encoding the document DECLARED, which is
// not the encoding it is stored in. html5ever decoded to UTF-8 and Korvus stores UTF-8, so for years
// this was a constant "UTF-8"; that is right for storage and wrong for the API, because a document with
// `<meta charset="euc-kr">` reports "EUC-KR" and, more consequentially, percent-encodes URL queries as
// EUC-KR. Reads DocEncoding, which load_page resolves from the parsed <meta>.
pub(crate) fn cb_doc_charset(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A frame's document reports ITS encoding, as Chrome 150's does, not the page's.
    let slot = doc_root_slot(scope, args.this().into());
    if slot != PAGE_DOC_ROOT {
        if let Some(e) = crate::core::html::browsing_context::document_root_encoding(scope, slot) {
            let v = str_val(scope, e.name());
            rv.set(v);
            return;
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // `name()` is already the WHATWG-cased label ("EUC-KR", "Shift_JIS", "UTF-8") the API must report.
    let name = op_state.borrow().try_borrow::<DocEncoding>().map(|d| d.0.name()).unwrap_or("UTF-8");
    let v = str_val(scope, name);
    rv.set(v);
}
// THE SURVIVING CONSTANTS, audited against headless Chrome 150 over loopback on 2026-09-11 when
// `readyState` stopped being one of them:
//
//   referrer     "" on a typed top-level navigation, which is what Chrome answers on the same page.
//                Honest today because this engine only performs that navigation; it becomes a lie the
//                moment a document is reached from another (a frame, a link).
//   designMode   "off" in both engines. Not a stand-in: the switch really is off.
//
// `compatMode` was the third, the constant `CSS1Compat`, because html5ever's quirks verdict was not
// carried into the arena. The arena carries it (`Dom::document_mode`), and this reads it: Chrome 150
// answers `BackCompat` with no doctype, with `<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01
// Transitional//EN">` and with the IBM XHTML system id, and `CSS1Compat` for a limited-quirks doctype
// (4.01 Transitional WITH its system id) as for `<!doctype html>`.
//
// It reads the mode of the document it is CALLED ON. It used to unwrap `this` with `unwrap_id`, which
// knows no Document, so every document fell back to the PAGE's root and answered the page's mode: a
// frame's initial `about:blank` (BackCompat in Chrome 150 / Brave 151) and a doctype-less `DOMParser`
// result (BackCompat) both read `CSS1Compat` (measured 2026-10-05; a challenge provider's surface fingerprint
// reads it inside a fresh `about:blank` frame).
pub(crate) fn cb_doc_compat_mode(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let quirks = {
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|d| d.0.document_mode_of(resolve_doc_root(&d.0, slot)) == korvus_dom::DocumentMode::Quirks)
    };
    let v = str_val(scope, if quirks { "BackCompat" } else { "CSS1Compat" });
    rv.set(v);
}
// Nothing navigates to a Korvus document from another, so there is never a referrer.
/// `document.referrer`: a frame document's is fixed when the frame navigates; the page's
/// own, and a document no frame shows, answer "".
pub(crate) fn cb_doc_referrer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    // The page's document: the Referer its navigation carried.
    let r = if slot == PAGE_DOC_ROOT { Some(crate::page::arrival(scope).referrer) } else { crate::core::html::browsing_context::frame_doc_referrer(scope, slot) };
    let v = str_val(scope, r.as_deref().unwrap_or(""));
    rv.set(v);
}
// `document.readyState` USED to be a third constant here -- `doc_const!(cb_doc_ready_state,
// op_doc_ready_state, "complete")` -- with a second copy of the same lie at `cb_doc_ready_state`
// below. The document has a real readiness now
// (`core/html/browsing_context.rs::Readiness`), so the constant and its op are gone: the op had no
// caller left at all, and the surviving getter is the table row's.
// The legacy editing switch, off and not implemented.
doc_const!(cb_doc_design_mode, "off");

/// `document.contentType` — the MIME type the document was parsed as.
///
/// The page's own document answers the type its response declared ([`PageContentType`]): an XML page
/// read `text/html`. Any other document is one `new Document()`,
/// `DOMImplementation.createDocument` or `DOMParser` made, and answers the kind recorded when it was
/// made -- or, recorded nothing, whether the arena parsed HTML for its root.
pub(crate) fn cb_doc_content_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let slot = doc_root_slot(scope, args.this().into());
    // A created or parsed document reports what it was CREATED as. `createDocument` records the
    // content type from the NAMESPACE it was given (Blink makes an XHTML/SVG XMLDocument for those),
    // which must read right even when qualifiedName was "" and there is no element at all; a DOMParser
    // XML result records none and derives it from the root element. -- WPT "metadata" subtests.
    let page_type = if slot == PAGE_DOC_ROOT {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let t = op_state.borrow().try_borrow::<PageContentType>().map(|t| t.0.clone());
        t
    } else {
        None
    };
    // A FRAME's document that a viewer made reports its response's type (A/B C5, `browsing_context.rs`'s
    // `frame_document_source`): a `text/plain` frame is a text document, not `text/html`.
    let frame_type = if slot == PAGE_DOC_ROOT {
        None
    } else {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let t = op_state.borrow().try_borrow::<DocContentTypes>().and_then(|m| m.0.get(&slot).cloned());
        t
    };
    if let Some(t) = frame_type {
        let v = str_val(scope, &t);
        rv.set(v);
        return;
    }
    let s = match (recorded_kind(scope, args.this().into()), page_type) {
        (Some(DocKind::Html), _) => "text/html".to_string(),
        (Some(DocKind::Xml(ct)), _) => ct,
        (None, Some(t)) => t,
        (None, None) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            let is_html = doc_element_child(dom, resolve_doc_root(dom, slot)).is_some();
            if is_html { "text/html".to_string() } else { "application/xml".to_string() }
        }
    };
    let v = str_val(scope, &s);
    rv.set(v);
}


// ---- The rest of Document: the members the spine never had ------------------------------------
//
// `generated_shells.js` skips any interface already on the global, and `Document` is one (it IS the
// native `JsDocument` class), so Document alone received NO shell members: everything below was
// simply absent from `Document.prototype`. Each member here is either a real implementation over the
// arena or the one answer this engine structurally has -- never a value copied out of a recorded
// expectation. Where the honest answer and the recorded one differ, the comment says why: Korvus has
// exactly ONE document, so `new Document()` is another view of the same arena rather than the
// browsing-context-less empty document Chrome makes, and every recorded case that reads a
// document-IDENTITY member through `new Document()` describes a document this engine cannot produce.

/// A getter/method whose only possible answer here is `null`.
pub(crate) fn cb_doc_null(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = scope;
    rv.set_null();
}

/// `document.fragmentDirective` -- the text-fragment directive object.
///
/// It answered `null`, which is not a `FragmentDirective`; the corpus caught `vuejs.org` and
/// `vitejs.dev` reading it. The whole interface is EMPTY in Chrome 151 -- measured over loopback,
/// `Object.getOwnPropertyNames(Object.getPrototypeOf(document.fragmentDirective))` is `["constructor"]`
/// and its own property names are `[]` -- because the `:~:text=` consumption it exists for is a
/// navigation-time algorithm with no members of its own. So the whole of what a page can observe is
/// that the object EXISTS, wears that prototype, and is `[SameObject]`, and all three are real here.
/// The identity slot hangs off the document, exactly as the spec's "same object" wording says.
fn cb_doc_fragment_directive(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    const SLOT: &str = "Document_fragmentDirective";
    let this = args.this();
    if let Some(s) = crate::binding::instance_state::state(scope, this.into(), SLOT) {
        if let Ok(o) = v8::Local::<v8::Object>::try_from(crate::binding::instance_state::get(scope, s, "o")) {
            rv.set(o.into());
            return;
        }
    }
    let Some(proto) = crate::binding::protos::shell_proto(scope, "FragmentDirective") else { return };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    // The brand a shell CONSTRUCTOR stamps, as `js/dom/09g-navigator.js`'s singletons take it: this IS
    // the real instance of the interface, so an `Object.create(FragmentDirective.prototype)` must stay
    // distinguishable from it even though the interface has no member to tell them apart today.
    crate::binding::protos::brand_shell_instance(scope, o);
    let s = crate::binding::instance_state::attach(scope, this, SLOT);
    crate::binding::instance_state::set(scope, s, "o", o.into());
    rv.set(o.into());
}

/// A boolean getter for a capability this engine does not have.
pub(crate) fn cb_doc_false(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = scope;
    rv.set_bool(false);
}

/// `document.scrollingElement`: the document element -- which walks the children like the rest (a tag scan kept
/// naming a detached `<html>` after the document was emptied) -- except in QUIRKS mode, where Blink answers
/// [`quirks_scrolling_element`]. That branch was unreachable while this engine parsed nothing in quirks mode; it
/// does now (an `about:blank` document is `BackCompat`), and Chrome 150 answers `<body>` there where this answered
/// `<html>` (the challenge's blank probe frame).
pub(crate) fn cb_doc_scrolling_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let pick = {
        let mut st = op_state.borrow_mut();
        let root = resolve_doc_root(&st.borrow::<HostDom>().0, slot);
        if st.borrow::<HostDom>().0.document_mode(root) == korvus_dom::DocumentMode::Quirks {
            quirks_scrolling_element(&mut st, root)
        } else {
            doc_element_child(&st.borrow::<HostDom>().0, root)
        }
    };
    match pick {
        Some(id) => {
            let w = wrap_cached(scope, &op_state, id);
            rv.set(w.into());
        }
        None => rv.set_null(),
    }
}

/// Blink's `Document::ScrollingElementNoLayout` in quirks mode: the first `<body>` child of the html element (a
/// `<frameset>` is not one), or null when that body is a SCROLL CONTAINER -- it has a box, its overflow is neither
/// visible nor clip, and the viewport did not take it (it does when the html element's overflow is visible along
/// both axes: `Document::ViewportDefiningElement`).
fn quirks_scrolling_element(st: &mut OpState, root: u32) -> Option<u32> {
    let html = html_element_of(st, root)?;
    let kids = st.borrow::<HostDom>().0.children(html);
    let body = kids.into_iter().find(|&c| is_html_element_named(st, c, "body"))?;
    let read = |st: &mut OpState, id: u32, name: &str| {
        page::style::property::PropertyId::from_name(name)
            .and_then(|p| crate::core::css::cssom::computed_text(st, id, p))
            .unwrap_or_default()
    };
    let scrolls = |v: &str| v != "visible" && v != "clip";
    let has_box = !matches!(read(st, body, "display").as_str(), "" | "none" | "contents");
    let body_scrolls = scrolls(&read(st, body, "overflow-x")) || scrolls(&read(st, body, "overflow-y"));
    let to_viewport = read(st, html, "overflow-x") == "visible" && read(st, html, "overflow-y") == "visible";
    if has_box && body_scrolls && !to_viewport { None } else { Some(body) }
}
// ParentNode.firstElementChild/lastElementChild on the DOCUMENT -- the element children of the
// arena's document node (`<html>`, or nothing at all before a parse).
doc_getter!(cb_doc_first_element_child, |dom, root| dom.child_elements(root).first().copied());
doc_getter!(cb_doc_last_element_child, |dom, root| dom.child_elements(root).last().copied());

pub(crate) fn cb_doc_child_element_count(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let n = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; dom.child_elements(resolve_doc_root(dom, slot)).len() };
    let v = int_val(scope, n as i64);
    rv.set(v);
}

// forms/images/scripts/embeds/plugins/links/anchors/applets used to be static `doc_list!` getters
// here, each building a v8::Array. They are LIVE HTMLCollections now: the same arena queries, as
// `Kind::Forms`..`Kind::Applets` in core/dom/collections.rs, behind the same native exotic object
// every other collection on this surface returns.

/// The document node the ParentNode insertion algorithms below operate on: the RECEIVER's own root
/// (the arena root for the page document, a frame's parsed root for a nested browsing context).
fn doc_root(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
) -> u32 {
    let slot = doc_root_slot(scope, this.into());
    let st = op_state.borrow();
    resolve_doc_root(&st.borrow::<HostDom>().0, slot)
}

/// May this Document receiver mutate its tree?
///
/// A Document that OWNS a root -- a nested browsing context's, a DOMParser result's -- always may:
/// its root is its own, so mutating it cannot touch another document. What stays refused is the
/// `new Document()` case below, which still names the page's tree.
fn may_mutate_document(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    doc_root_slot(scope, this.into()) != PAGE_DOC_ROOT || is_page_document(scope, this)
}

/// Is this receiver the page's document, or one a script built with `new Document()`?
///
/// There is exactly ONE document in this engine: `JsDocument` is a unit struct with no node id, and
/// every Document member reads the single arena root. So `new Document()` returns a fresh JS wrapper
/// over the SAME tree -- and `new Document().replaceChildren()` emptied the live page, which is how a
/// recorded oracle case (whose browser answer is a harmless no-op on a fresh empty XML document)
/// became destructive here. It also poisoned every later case in the corpus, which is how it was
/// found.
///
/// Until a constructed Document owns a real second root, the mutators refuse to act through a
/// receiver that is not the page. A no-op is exactly right for the one shape a browser also treats as
/// a no-op -- `replaceChildren()` with no arguments on an empty document -- and is the conservative
/// answer for the rest: doing nothing to a document with no tree is a gap, mutating a DIFFERENT
/// document is a bug.
fn is_page_document(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    match global_get(scope, "document") {
        Some(d) => d == v8::Local::<v8::Value>::from(this),
        // Before INSTALL_DOCUMENT runs there is no page document to protect.
        None => true,
    }
}

/// Resolve + validate EVERY argument of a Document ParentNode insertion before anything mutates.
///
/// "Ensure pre-insertion validity" with `parent` = a Document is the branch that rejects a Text node
/// outright, and the Element callbacks in node.rs resolve their receiver with `unwrap_id` (JsElement
/// only), so on a Document receiver they silently did nothing. Both halves matter: a skipped check is
/// how a Text node became a direct child of the document once before.
fn doc_insert_common(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    root: u32,
) -> Option<Vec<u32>> {
    // every argument converted first, as the binding does (`node::node_args`)
    let nodes = crate::core::dom::node::node_args(scope, args)?;
    let mut ids: Vec<u32> = Vec::with_capacity(nodes.len());
    for a in &nodes {
        let id = crate::core::dom::node::node_arg_id(scope, op_state, a, root);
        if insertion_cycles(op_state, root, id) {
            throw_dom(scope, "The node to be inserted is an ancestor of the parent.", "HierarchyRequestError");
            return None;
        }
        if insertion_type_invalid(op_state, root, id, true) {
            throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError");
            return None;
        }
        ids.push(id);
    }
    if let Some(msg) = doc_child_constraints(op_state, root, &ids) {
        throw_dom(scope, msg, "HierarchyRequestError");
        return None;
    }
    Some(ids)
}

/// The constraints that make a Document parent different from every other parent: it may hold at
/// most ONE element child and at most ONE doctype child, ever.
///
/// `insertion_type_invalid` above covers the kind-of-node rules (no Text under a Document, no
/// doctype anywhere else). These are the COUNTING rules, and without them
/// `document.append(document.createElement("div"))` quietly gave the document a second element
/// child -- `document.childElementCount === 2`, with `firstElementChild` the `<html>` and
/// `lastElementChild` the `<div>` -- where every browser throws. Five hundred appends gave five
/// hundred and one.
///
/// The batch is checked as one unit because `append(a, b)` is defined as "convert the nodes into a
/// node" first: two elements in one call are already invalid before the document is consulted. A
/// DocumentFragment argument contributes its own element children to that count, for the same
/// reason.
fn doc_child_constraints(
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    root: u32,
    ids: &[u32],
) -> Option<&'static str> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let mut elements = 0usize;
    let mut doctypes = 0usize;
    for &id in ids {
        match dom.node_type(id) {
            1 => elements += 1,
            10 => doctypes += 1,
            // A fragment is flattened into its children on insertion, so it contributes theirs.
            11 => {
                for c in dom.children(id) {
                    match dom.node_type(c) {
                        1 => elements += 1,
                        10 => doctypes += 1,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut have_element = false;
    let mut have_doctype = false;
    for c in dom.children(root) {
        match dom.node_type(c) {
            1 => have_element = true,
            10 => have_doctype = true,
            _ => {}
        }
    }
    if elements > 1 || (elements == 1 && have_element) {
        return Some("Only one element on document allowed.");
    }
    if doctypes > 1 || (doctypes == 1 && have_doctype) {
        return Some("Only one doctype on document allowed.");
    }
    // A doctype may not follow the document element. `append` inserts at the end, so an existing
    // element child always precedes it.
    if doctypes == 1 && have_element {
        return Some("Doctype must be inserted before the document element.");
    }
    None
}

pub(crate) fn cb_doc_append(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !may_mutate_document(scope, args.this()) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = doc_root(scope, args.this(), &op_state);
    let ids = match doc_insert_common(scope, &args, &op_state, root) { Some(v) => v, None => return };
    for &id in &ids { op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(root, id); }
    notify_childlist_batch(scope, &op_state, root, &ids, &[]);
}

pub(crate) fn cb_doc_prepend(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !may_mutate_document(scope, args.this()) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = doc_root(scope, args.this(), &op_state);
    let ids = match doc_insert_common(scope, &args, &op_state, root) { Some(v) => v, None => return };
    for &id in ids.iter().rev() { op_state.borrow_mut().borrow_mut::<HostDom>().0.prepend_child(root, id); }
    notify_childlist_batch(scope, &op_state, root, &ids, &[]);
}

pub(crate) fn cb_doc_replace_children(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !may_mutate_document(scope, args.this()) { return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = doc_root(scope, args.this(), &op_state);
    // Validity FIRST, exactly as the adjacent append/prepend do. `replaceChildren` is "ensure
    // pre-insertion validity, then replace all", so `document.replaceChildren(text)` must throw
    // HierarchyRequestError with the document's existing children still in place -- not empty the
    // document and then quietly install a Text node as its child.
    let ids = match doc_insert_common(scope, &args, &op_state, root) { Some(v) => v, None => return };
    let removed: Vec<u32> = { let st = op_state.borrow(); st.borrow::<HostDom>().0.children(root).to_vec() };
    for &c in &removed { op_state.borrow_mut().borrow_mut::<HostDom>().0.remove_child(root, c); }
    for &id in &ids { op_state.borrow_mut().borrow_mut::<HostDom>().0.append_child(root, id); }
    notify_childlist_batch(scope, &op_state, root, &ids, &removed);
}

/// `document.moveBefore(node, child)` -- the atomic move (the node keeps its state instead of being
/// removed and re-inserted). WebIDL declares `(Node node, Node? child)`, so a non-Node is a TypeError,
/// and only a node already in a tree can be moved.
pub(crate) fn cb_doc_move_before(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "Document", "moveBefore") { return; }
    let node = match arg_node_id(scope, args.get(0)) {
        Some(i) => i,
        None => { throw_type_error(scope, "Failed to execute 'moveBefore' on 'Document': parameter 1 is not of type 'Node'."); return; }
    };
    let a1 = args.get(1);
    let child = if a1.is_null() { None } else {
        match arg_node_id(scope, a1) {
            Some(i) => Some(i),
            None => { throw_type_error(scope, "Failed to execute 'moveBefore' on 'Document': parameter 2 is not of type 'Node'."); return; }
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = doc_root(scope, args.this(), &op_state);
    // A Document is never an insertable node -- and `ON_DOCUMENT_ID` is a SENTINEL, not an arena
    // index, so it has to be rejected before any arena call can use it as one.
    if node == ON_DOCUMENT_ID || insertion_type_invalid(&op_state, root, node, true) || insertion_cycles(&op_state, root, node) {
        throw_dom(scope, "The node cannot be inserted at this point in the hierarchy.", "HierarchyRequestError");
        return;
    }
    let parent = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(node) };
    let parent = match parent {
        Some(p) => p,
        None => { throw_dom(scope, "The node to be moved is not in a tree.", "HierarchyRequestError"); return; }
    };
    if let Some(c) = child {
        let ok = c != ON_DOCUMENT_ID && { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(c) == Some(root) };
        if !ok { throw_dom(scope, "The node before which the new node is to be inserted is not a child of this node.", "NotFoundError"); return; }
    }
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        dom.remove_child(parent, node);
        match child { Some(c) => dom.insert_before(root, node, c), None => dom.append_child(root, node) }
    }
    notify_childlist_batch(scope, &op_state, root, &[node], &[]);
}

/// `document.adoptNode(node)` -- detach `node` from its parent and hand it back owned by this document.
///
/// Korvus has ONE arena shared by every document view, so a node's owner document already IS this one
/// and the adopt steps reduce to the removal the spec performs first. The Document case is the part
/// that must not be skipped: `arg_node_id` answers `ON_DOCUMENT_ID` (`u32::MAX`) for a Document, which
/// is a sentinel and not an arena index -- walking into the arena with it aborted the process
/// ("index out of bounds: the len is 10 but the index is 4294967295") on `document.adoptNode(document)`.
/// A Document is also exactly what the spec rejects here, so the sentinel check and the spec check are
/// the same check.
pub(crate) fn cb_doc_adopt_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "Document", "adoptNode") { return; }
    let node = match arg_node_id(scope, args.get(0)) {
        Some(i) => i,
        None => { throw_type_error(scope, "Failed to execute 'adoptNode' on 'Document': parameter 1 is not of type 'Node'."); return; }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // A SHADOW ROOT IS ITS OWN REJECTION, WITH ITS OWN EXCEPTION. DOM "adopt" step 1 is
    // "if node is a shadow root, throw a HierarchyRequestError"; the Document rejection below is
    // `adoptNode` step 1 and answers NotSupportedError. A shadow root is a DocumentFragment, not a
    // Document, so it must not fall into the second check -- it did, and WPT's
    // `Document-adoptNode-DocumentFragment-with-host` reads the exception name.
    if { let st = op_state.borrow(); st.borrow::<HostDom>().0.is_shadow_root(node) } {
        throw_dom(scope, "The node provided is a shadow root, which may not be adopted.", "HierarchyRequestError");
        return;
    }
    // Two names for the same rejection: the page document arrives as the `ON_DOCUMENT_ID` sentinel,
    // a nested browsing context's document as a real arena node whose type is 9. Both are Documents,
    // and the spec rejects a Document here.
    let is_document = node == ON_DOCUMENT_ID
        || { let st = op_state.borrow(); st.borrow::<HostDom>().0.node_type(node) == 9 };
    if is_document {
        throw_dom(scope, "The node provided is of type 'Document', which may not be adopted.", "NotSupportedError");
        return;
    }
    let parent = { let st = op_state.borrow(); st.borrow::<HostDom>().0.parent(node) };
    if let Some(p) = parent {
        op_state.borrow_mut().borrow_mut::<HostDom>().0.remove_child(p, node);
        let pv = wrap_cached(scope, &op_state, p);
        let nv: v8::Local<v8::Value> = wrap_cached(scope, &op_state, node).into();
        crate::core::dom::shared::notify_childlist_sib(scope, pv.into(), &[], &[nv], None, None);
    }
    // AND THEN IT IS ACTUALLY ADOPTED. The doc comment above used to end at the
    // removal, on the reasoning that one shared arena makes every node's owner document already this
    // one. It does not: `ownerDocument` answers from the `OwnerMap`, and leaving that entry alone
    // meant `d2.adoptNode(el)` returned an `el` whose `ownerDocument` was still the page -- measured
    // against Chrome 150, which answers `d2`. `notify_adopted` rewrites the entry for the whole
    // shadow-including subtree and runs each custom element's `adoptedCallback`, AFTER the removal,
    // which is Chrome's order: `disc` then `adop`.
    let target = if doc_root_slot(scope, args.this().into()) == PAGE_DOC_ROOT {
        None
    } else {
        let this_v: v8::Local<v8::Value> = args.this().into();
        Some(v8::Global::new(scope, this_v))
    };
    crate::core::dom::node::notify_adopted(scope, &op_state, target, &[node]);
    let w = wrap_cached(scope, &op_state, node);
    rv.set(w.into());
}

/// `document.dir` -- the document element's `dir` content attribute, limited to the known values.
///
/// A "limited to only known values" reflection answers "" (not the stored string) for anything outside
/// {ltr, rtl, auto}, which is why `document.dir = "x"` reads back as "" and not as "x".
const DIR_KEYWORDS: [&str; 3] = ["ltr", "rtl", "auto"];
pub(crate) fn cb_doc_dir_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let dir = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        doc_element_child(dom, resolve_doc_root(dom, slot))
            .and_then(|id| dom.get_attribute(id, "dir"))
            .map(|v| v.to_ascii_lowercase())
            .filter(|v| DIR_KEYWORDS.contains(&v.as_str()))
            .unwrap_or_default()
    };
    let v = str_val(scope, &dir);
    rv.set(v);
}
pub(crate) fn cb_doc_dir_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let val = args.get(0).to_rust_string_lossy(scope);
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let html = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        doc_element_child(dom, resolve_doc_root(dom, slot))
    };
    // No document element means there is no attribute to reflect onto, so the assignment is dropped --
    // which is what it does in a browser for the same reason.
    if let Some(id) = html { op_state.borrow_mut().borrow_mut::<HostDom>().0.set_attribute(id, "dir", &val); }
}

/// The five legacy `<body>` colour attributes (`fgColor` -> `text`, `bgColor` -> `bgcolor`, ...).
/// Real reflections onto the body element -- which is why they read back "" when there is no body,
/// and why the value survives a round trip when there is one.
pub(crate) fn cb_doc_body_attr_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let val = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        body_element_of(&st, resolve_doc_root(dom, slot))
            .as_ref()
            .and_then(|&id| dom.get_attribute(id, &attr))
            .unwrap_or("")
            .to_string()
    };
    let v = str_val(scope, &val);
    rv.set(v);
}
pub(crate) fn cb_doc_body_attr_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let val = args.get(0).to_rust_string_lossy(scope);
    let slot = doc_root_slot(scope, args.this().into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let body = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        body_element_of(&st, resolve_doc_root(dom, slot))
    };
    if let Some(id) = body { op_state.borrow_mut().borrow_mut::<HostDom>().0.set_attribute(id, &attr, &val); }
}

pub(crate) fn cb_doc_domain_get(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // An opaque origin has no domain: a sandboxed frame's document answers "" (Chrome 150).
    let this: v8::Local<v8::Value> = _args.this().into();
    if crate::core::html::browsing_context::document_is_sandboxed(scope, this) {
        rv.set(str_val(scope, ""));
        return;
    }
    let h = doc_host(scope, this).unwrap_or_default();
    let v = str_val(scope, &h);
    rv.set(v);
}

/// The host of the receiver DOCUMENT's origin: its frame's -- an `about:blank` document's is
/// its creator's -- or the page's for a document no frame shows. It was always the page's URL's host: the
/// an interactive-widget challenge frame's own blank child read `player.fm` where Chrome 150 reads `a challenge host`.
fn doc_host(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<String> {
    let slot = doc_root_slot(scope, this);
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        resolve_doc_root(&st.borrow::<HostDom>().0, slot)
    };
    let origin = crate::core::html::browsing_context::doc_root_origin(scope, root);
    let u = url::Url::parse(&origin).ok()?;
    u.host_str().filter(|h| !h.is_empty()).map(str::to_string)
}
/// Setting `document.domain` relaxes the origin. A document whose URL has no host has an OPAQUE
/// origin, and an opaque origin has nothing to relax -- the spec's "if this's origin is an opaque
/// origin, throw a SecurityError" step, which is the branch an about:blank/data: document takes here.
/// With a real host there is nothing a relaxed origin could affect either: Korvus runs ONE realm, so
/// no second document could become same-origin with this one. The assignment is then accepted and
/// changes nothing -- the same outcome as a browser's "already this effective domain" case.
/// `document.domain = value` — the setter that can only ever RELAX an origin, and only downward.
///
/// The value must be a registrable-domain suffix of the current host: from `a.b.example.com` a page
/// may set `b.example.com` or `example.com`, and may set `a.b.example.com` itself (a no-op), but not
/// `example.org`, not `com`, and not a longer name. Anything else is a SecurityError.
///
/// This engine never actually widens the origin -- there is no second document to become
/// same-origin with -- so a legal assignment is accepted and changes nothing, which is exactly what
/// a browser does for the "already this effective domain" case. What matters is that an ILLEGAL
/// assignment throws, because a script that probes `document.domain = "evil.com"` and gets no
/// exception has learned it is not in a browser. The first draft of this function accepted every
/// value silently.
pub(crate) fn cb_doc_domain_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(host) = doc_host(scope, this) else {
        throw_dom(scope, "Assignment is forbidden for documents with an opaque origin.", "SecurityError");
        return;
    };
    let value = args.get(0).to_rust_string_lossy(scope);
    // The empty string is not a suffix of anything and Chrome rejects it with the same message.
    let ok = if value.is_empty() {
        false
    } else if value.eq_ignore_ascii_case(&host) {
        true
    } else {
        // A suffix, and a suffix at a LABEL boundary: "example.com" is a suffix of
        // "a.example.com" but "ample.com" is not. An IP-literal host has no labels to strip, so
        // only the exact-match branch above can succeed for it.
        let is_ip = host.parse::<std::net::IpAddr>().is_ok()
            || (host.starts_with('[') && host.ends_with(']'));
        !is_ip
            && host.len() > value.len()
            && host[host.len() - value.len()..].eq_ignore_ascii_case(&value)
            && host.as_bytes()[host.len() - value.len() - 1] == b'.'
    };
    if !ok {
        let msg = format!("Failed to set the 'domain' property on 'Document': '{value}' is not a suffix of '{host}'.");
        throw_dom(scope, &msg, "SecurityError");
    }
}

// `cb_doc_from_point_null` WAS HERE: `caretPositionFromPoint` answered null for every point, "because
// inline text flow does not exist". It does now, and so does the caret position
// (`core/dom/geometry_queries.rs::cb_doc_caret_position_from_point`).

/// True when the receiver Document is NOT an HTML document -- i.e. it is one of the XML documents
/// `new Document()` and `DOMImplementation.createDocument` mint.
///
/// Several HTML-only members begin by rejecting such a document (`document.open`, `write`, the
/// editing commands), and the split has to be asked of the RECEIVER, not of the engine: the page's
/// own document is an HTML one and must keep answering exactly as it did.
pub(crate) fn doc_is_not_html(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    let slot = doc_root_slot(scope, this);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    dom.is_in_xml(resolve_doc_root(dom, slot))
}

/// `document.open` / `close` / `write` / `writeln` on a document that is not an HTML document.
///
/// Every one of the four begins, in the spec, by throwing `InvalidStateError` when `this` is an XML
/// document -- that branch needs no parser and no insertion point, and it is the branch the recorded
/// browser takes for `new Document()`.
///
/// `open` additionally distinguishes its two overloads. With three arguments it is the
/// window-opening form, whose first step rejects a document with no browsing context -- an
/// `InvalidAccessError`, a different exception the capture records separately.
pub(crate) fn cb_doc_write_family(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // A nested browsing context's document IS written to: `open()`/`write()`/`writeln()`/`close()`
    // replace the frame's document with what was written (core/html/browsing_context.rs).
    if let Some(r) = crate::core::html::browsing_context::frame_document_write_family(scope, args.this().into(), &name, &args) {
        rv.set(r);
        return;
    }
    if !doc_is_not_html(scope, args.this().into()) {
        // A Document no browsing context shows has a parser only once a script opens one (the page's is its own)
        let slot = doc_root_slot(scope, args.this().into());
        if slot != PAGE_DOC_ROOT {
            // the window-opening `open(url, name, features)` needs a window, which this Document has not (Chrome 150's
            // sentence, measured)
            if name == "open" && args.length() >= 3 {
                throw_dom(scope, "Failed to execute 'open' on 'Document': The document has no window associated.", "InvalidAccessError");
                return;
            }
            created_doc_write_family(scope, &args, &mut rv, slot, &name);
            return;
        }
        match name.as_str() {
            "open" if args.length() < 3 => {
                page_document_open(scope);
                rv.set(args.this().into());
            }
            "write" | "writeln" => {
                let Some(mut s) = write_args_text(scope, &args, &name) else { return };
                if name == "writeln" { s.push('\n'); }
                // a parser `open()` started (or this write starts, the document having loaded) takes it; the page's
                // own parser's insertion point takes it while the page loads
                let reopened = page_reopened(scope);
                let loaded = crate::core::html::browsing_context::doc_readiness(scope) == crate::core::html::browsing_context::Readiness::Complete;
                if reopened || loaded {
                    if !reopened {
                        page_document_open(scope);
                    }
                    reopened_write(scope, &s);
                } else {
                    page_document_write(scope, args.this().into(), &s);
                }
            }
            "close" => page_document_close(scope),
            _ => {}
        }
        return;
    }
    if name == "open" && args.length() >= 3 {
        throw_dom(scope, "Failed to execute 'open' on 'Document': The document is not fully active.", "InvalidAccessError");
        return;
    }
    // Blink's sentence names the step that refused: `writeln` is `write` (Chrome 150, measured: `createDocument(null,
    // 'r').open()` is "Only HTML documents support open().")
    let step = if name == "writeln" { "write" } else { name.as_str() };
    throw_dom(
        scope,
        &format!("Failed to execute '{name}' on 'Document': Only HTML documents support {step}()."),
        "InvalidStateError",
    );
}

/// `write` / `writeln`'s arguments as Blink's overloads convert them: a TrustedHTML FIRST picks
/// `write(TrustedHTML, (TrustedHTML or DOMString)...)`, whose Trusted arguments are read as themselves, nothing
/// on them called; anything else picks `write(DOMString...)`, every argument ToString'd -- a later TrustedHTML's
/// `toString` included (Chrome 150, `TrustedTypesHTML` stable). `None`: a ToString threw.
///
/// Then Trusted Types (Blink `Document::write`): the joined text goes through the document's default policy
/// ("Document write" / "Document writeln") unless every argument was a TrustedHTML; `writeln`'s line feed is
/// added after.
pub(crate) fn write_args_text(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, name: &str) -> Option<String> {
    let trusted = args.length() > 0 && crate::core::trusted_types::tables::is_trusted(scope, args.get(0), "TrustedHTML");
    let mut all_trusted = trusted;
    let mut s = String::new();
    for i in 0..args.length() {
        let a = args.get(i);
        let t = if trusted {
            all_trusted &= crate::core::trusted_types::tables::is_trusted(scope, a, "TrustedHTML");
            crate::core::trusted_types::tables::sink_text(scope, a, "TrustedHTML")?
        } else {
            let t = a.to_string(scope)?;
            t.to_rust_string_lossy(scope)
        };
        s.push_str(&t);
    }
    if all_trusted {
        return Some(s);
    }
    let (sample, context) = (format!("Document {name}"), format!("Failed to execute '{name}' on 'Document'"));
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: &sample, context: &context };
    let slot = doc_root_slot(scope, args.this().into());
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        resolve_doc_root(&st.borrow::<HostDom>().0, slot)
    };
    crate::core::trusted_types::tables::check_text(scope, s, &sink, crate::core::trusted_types::tables::SinkAt::Node(root))
}

/// HTML's SCRIPT-CREATED PARSER for a Document no browsing context shows (`createHTMLDocument`, a `DOMParser`
/// result), by document root, until `close()`: what `open()` started it with and what has been written since,
/// re-parsed into the SAME Document at every write (as a frame's is). Chrome 152, measured
/// (`tests/fixtures/frame-documents/created-document-write-*`): `d.open(); d.write('<p>w</p>'); d.close()` leaves
/// `d.body.innerHTML === '<p>w</p>'`; a write with no parser open -- after `close()`, or into a fresh
/// `createHTMLDocument` result -- opens first (the document is emptied, its `<title>` too). Chrome 150 made a
/// `createHTMLDocument` result by `open()` and `write()` of its skeleton and never closed that parser, so a write
/// landed in its `<body>`; Chrome 152 builds the tree directly (`CreateHTMLDocumentReadyState`, stable).
#[derive(Default)]
struct CreatedDocParsers(std::collections::HashMap<u32, String>);

/// Has `root` (a Document no browsing context shows) a script-created parser open -- `open()` or a `write()` since,
/// no `close()` yet? Its `readyState` is `loading` then (Blink's `Document::open` sets it).
fn created_doc_parser_open(scope: &mut v8::PinScope, root: u32) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<CreatedDocParsers>().is_some_and(|p| p.0.contains_key(&root))
}

/// `open()` / `write()` / `writeln()` / `close()` on a Document no browsing context shows (`root` its arena root).
fn created_doc_write_family(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, root: u32, name: &str) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match name {
        "open" => {
            created_doc_open(scope, root);
            rv.set(args.this().into());
        }
        "write" | "writeln" => {
            let Some(mut s) = write_args_text(scope, args, name) else { return };
            if name == "writeln" {
                s.push('\n');
            }
            // no insertion point (no script-created parser): the document open steps first, which empty it
            let open = op_state.borrow().try_borrow::<CreatedDocParsers>().is_some_and(|p| p.0.contains_key(&root));
            if !open {
                created_doc_open(scope, root);
            }
            let markup = {
                let mut st = op_state.borrow_mut();
                let p = st.borrow_mut::<CreatedDocParsers>();
                let Some(b) = p.0.get_mut(&root) else { return };
                b.push_str(&s);
                b.clone()
            };
            let mut st = op_state.borrow_mut();
            let dom = &mut st.borrow_mut::<HostDom>().0;
            let parsed = dom.parse_into(&markup);
            for c in dom.children(root) {
                dom.remove_child(root, c);
            }
            for c in dom.children(parsed) {
                dom.remove_child(parsed, c);
                dom.append_child(root, c);
            }
            // the document's mode is the written markup's doctype verdict, as a frame's is
            let mode = dom.document_mode(parsed);
            dom.set_document_mode(root, mode);
            drop(st);
            // its parser prepares each script it makes -- "already started" -- and runs none: the document has no
            // browsing context. Moved into the page, such a script stays inert (WPT
            // `execution-timing/120.html`: `createHTMLDocument("")`, `write("<script>...")`, the head's first child
            // appended to the page's body, never run).
            crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &[root]);
        }
        "close" => {
            if let Some(p) = op_state.borrow_mut().try_borrow_mut::<CreatedDocParsers>() {
                p.0.remove(&root);
            }
        }
        _ => {}
    }
}

/// HTML's document open steps for a Document no browsing context shows: emptied, with a script-created parser.
fn created_doc_open(scope: &mut v8::PinScope, root: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    {
        let dom = &mut st.borrow_mut::<HostDom>().0;
        for c in dom.children(root) {
            dom.remove_child(root, c);
        }
    }
    if st.try_borrow::<CreatedDocParsers>().is_none() {
        st.put(CreatedDocParsers::default());
    }
    st.borrow_mut::<CreatedDocParsers>().0.insert(root, String::new());
}

// ── the PAGE document reopened ─────────────────────────────────────────────────────────────────────
//
// `document.open()` on the page document, and a `document.write()` once it has loaded -- which opens it first -- were
// no-ops: Playwright's `page.setContent(html)` (an `open(); write(html); close()` from its utility world) left the page
// as it was and waited for a `load` that never came. Chrome runs HTML's document open steps: the listeners of the
// document and its window erased, the document emptied, `loading`, a script-created parser that takes what is written;
// `close()` then runs "the end": the written scripts, `DOMContentLoaded`, and the window's `load` once nothing delays
// it -- which the driver reports as the frame's lifecycle (`Page.lifecycleEvent`), as Chrome's does.

/// The page document's script-created parser between `open()` and `close()`, and the lifecycle its end reached that
/// the driver has not reported yet.
#[derive(Default)]
pub struct PageReopened {
    writing: Option<String>,
    lifecycle: Vec<(&'static str, std::time::Instant)>,
}

fn page_reopened(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<PageReopened>().is_some_and(|r| r.writing.is_some())
}

/// What the reopened page document's end reached since the last call: `DOMContentLoaded` and `load`, each when.
pub fn take_reopened_lifecycle(state: &mut OpState) -> Vec<(&'static str, std::time::Instant)> {
    state.try_borrow_mut::<PageReopened>().map(|r| std::mem::take(&mut r.lifecycle)).unwrap_or_default()
}

/// HTML's document open steps on the page document. Bails out, changing nothing, while the page's own parser is
/// running one of its scripts (that `open()` is a no-op in every browser).
fn page_document_open(scope: &mut v8::PinScope) {
    use crate::core::html::browsing_context::{doc_readiness, set_doc_readiness, Readiness};
    if doc_readiness(scope) == Readiness::Loading && crate::core::html::html_script_element::current_script(scope).is_some() && !page_reopened(scope) {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // the listeners and handlers of the window, the document and every node of it
    let window = crate::core::html::browsing_context::main_global(scope);
    crate::core::events::event_target::et_forget(scope, window.into());
    if let Some(doc) = document_val(scope, &op_state) {
        crate::core::events::event_target::et_forget(scope, doc);
    }
    let nodes: Vec<u32> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut out = Vec::new();
        let mut stack = dom.children(dom.root());
        while let Some(n) = stack.pop() {
            out.push(n);
            if let Some(sr) = dom.shadow_root_of(n) {
                stack.push(sr);
            }
            stack.extend(dom.children(n));
        }
        out
    };
    for n in nodes {
        crate::core::events::event_target::et_forget_node(scope, n);
    }
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        let root = dom.root();
        for c in dom.children(root) {
            dom.remove_child(root, c);
        }
        if st.try_borrow::<PageReopened>().is_none() {
            st.put(PageReopened::default());
        }
        st.borrow_mut::<PageReopened>().writing = Some(String::new());
    }
    set_doc_readiness(scope, Readiness::Loading, true);
}

/// A write into the reopened page document: everything written so far is its content now, parsed into the SAME
/// document (its scripts run at `close()`).
fn reopened_write(scope: &mut v8::PinScope, s: &str) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let markup = {
        let Some(w) = st.try_borrow_mut::<PageReopened>().and_then(|r| r.writing.as_mut()) else { return };
        w.push_str(s);
        w.clone()
    };
    let dom = &mut st.borrow_mut::<HostDom>().0;
    let root = dom.root();
    let parsed = dom.parse_into(&markup);
    for c in dom.children(root) {
        dom.remove_child(root, c);
    }
    for c in dom.children(parsed) {
        dom.remove_child(parsed, c);
        dom.append_child(root, c);
    }
    let mode = dom.document_mode(parsed);
    dom.set_document_mode(root, mode);
}

/// `close()` of the reopened page document: its content connected -- so its scripts run and its subresources load --
/// then `interactive` and `DOMContentLoaded`, and `complete` and the window's `load` once nothing delays the load.
fn page_document_close(scope: &mut v8::PinScope) {
    use crate::core::html::browsing_context::{set_doc_readiness, Readiness};
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let written = op_state.borrow_mut().try_borrow_mut::<PageReopened>().and_then(|r| r.writing.take());
    if written.is_none() {
        return;
    }
    let (root, kids) = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        let root = dom.root();
        let kids = dom.children(root);
        for &c in &kids {
            dom.remove_child(root, c);
        }
        for &c in &kids {
            dom.append_child(root, c);
        }
        (root, kids)
    };
    notify_childlist_batch(scope, &op_state, root, &kids, &[]);
    set_doc_readiness(scope, Readiness::Interactive, true);
    if let Some(doc) = document_val(scope, &op_state) {
        crate::core::html::browsing_context::fire_lifecycle(scope, doc, "DOMContentLoaded", true, None);
    }
    {
        let mut st = op_state.borrow_mut();
        if let Some(r) = st.try_borrow_mut::<PageReopened>() {
            r.lifecycle.push(("DOMContentLoaded", std::time::Instant::now()));
        }
    }
    let f = crate::binding::protos::build_fn(scope, cb_reopened_load, "", 0);
    crate::core::html::browsing_context::when_load_delays_done(scope, f, 10_000);
}

fn cb_reopened_load(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    use crate::core::html::browsing_context::{set_doc_readiness, Readiness};
    set_doc_readiness(scope, Readiness::Complete, true);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let window = crate::core::html::browsing_context::main_global(scope);
    let doc = document_val(scope, &op_state);
    crate::core::html::browsing_context::fire_lifecycle(scope, window.into(), "load", false, doc);
    {
        let mut st = op_state.borrow_mut();
        if let Some(r) = st.try_borrow_mut::<PageReopened>() {
            r.lifecycle.push(("load", std::time::Instant::now()));
        }
    }
}

/// Where the NEXT `document.write()` of the running script lands: the node its previous write
/// inserted last, so two writes from one script arrive in the order they were called.
///
/// Keyed by the script element, because HTML's insertion point is a property of the parse and not
/// of the document: when a different script starts writing, the anchor is that script's element
/// again. Nothing else reads this and no page can see it.
#[derive(Default)]
pub(crate) struct WriteAnchor {
    script: u32,
    after: u32,
}

/// `document.write(markup)` on the PAGE document, while the document is still loading.
///
/// **WHAT THIS IMPLEMENTS AND WHAT IT DOES NOT.** HTML's `document.write()` pushes its argument
/// into the parser's *input stream* at the insertion point, which for a script-inserted write is
/// "immediately after the element of the script that is running". Korvus parses a document once,
/// up front, so there is no re-entrant parse for the markup to be tokenised INTO -- but the
/// insertion point is still well defined, and for markup that parses to a balanced run of nodes
/// (which is what a `document.write` of a `<script>`, a `<link>`, an `<iframe>` or a template's
/// contents is) inserting that run at the insertion point is the same tree the re-entrant parse
/// would have produced. That is the case this implements.
///
/// It is NOT the general case, and the difference is stated rather than hidden: a write of
/// UNBALANCED markup (`document.write("<div>")`, whose open tag the real parser leaves on the stack
/// so the rest of the document nests inside it) is balanced by the fragment parser here, and a
/// write after the parser has finished still does nothing where Chrome would `document.open()` and
/// blow the document away. Both remain gaps; neither is papered over with a plausible tree.
///
/// The insertion goes through `notify_childlist_batch` like every other one, so a written
/// `<script src>` or `<link>` is fetched by the subresource loader and a `MutationObserver` on the
/// parent sees the childList record -- the whole point of inserting into the tree rather than
/// stashing a string.
fn page_document_write(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, markup: &str) {
    if markup.is_empty() { return; }
    // Only the page document has a parser to have an insertion point in; every other Document this
    // engine hands a script is already complete (see `cb_doc_ready_state`).
    if doc_root_slot(scope, this) != PAGE_DOC_ROOT { return; }
    if crate::core::html::browsing_context::doc_readiness(scope)
        != crate::core::html::browsing_context::Readiness::Loading { return; }
    // The insertion point exists only while a script the parser reached is running. A write from a
    // timer or a listener has none, and inventing one would put nodes somewhere no browser does.
    let Some(cur) = crate::core::html::html_script_element::current_script(scope) else { return };
    let Some(script_id) = crate::binding::protos::unwrap_id(scope, cur) else { return };

    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // THE PARSER IS WAITING FOR A WRITTEN SCRIPT: everything written now goes into the
    // input stream BEHIND it, and is parsed only once it has run -- Chrome's `writer2-end:undefined`
    // then `y.js` then `second-write:number` on `tools/realweb/seed/dw`.
    let queued = {
        let st = op_state.borrow();
        st.try_borrow::<WrittenInput>().and_then(|w| w.blocking.map(|b| (b, w.parent)))
    };
    if let Some((_blocking, parent)) = queued {
        let ids = op_state.borrow_mut().borrow_mut::<HostDom>().0.parse_fragment_in(parent, markup);
        op_state.borrow_mut().borrow_mut::<WrittenInput>().pending.extend(ids.iter().copied());
        // held back too, and scanned as it is written
        crate::core::html::subresource::speculate_written(scope, &ids);
        return;
    }
    let (parent, anchor) = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<WriteAnchor>().is_none() { st.put(WriteAnchor::default()); }
        let previous = { let a = st.borrow::<WriteAnchor>(); (a.script == script_id).then_some(a.after) };
        let dom = &st.borrow::<HostDom>().0;
        // A script removed from the tree by the time it writes has no insertion point either.
        let anchor = match previous {
            Some(a) if dom.parent(a).is_some() => a,
            _ => script_id,
        };
        match dom.parent(anchor) { Some(p) => (p, anchor), None => return }
    };

    let mut ids = {
        let mut st = op_state.borrow_mut();
        st.borrow_mut::<HostDom>().0.parse_fragment_in(parent, markup)
    };
    if ids.is_empty() { return; }
    // A PARSER-BLOCKING written script stops the written input there: it and what comes
    // before it are inserted now; what follows waits for it to be fetched and run, ahead of anything
    // an earlier write left waiting (the input stream is spliced at the insertion point).
    let stop = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        ids.iter().position(|&n| written_script_blocks(dom, n))
    };
    let rest: Vec<u32> = match stop {
        Some(k) => ids.split_off(k + 1),
        None => Vec::new(),
    };
    // what the blocking script holds back, scanned now for the loads a preload scanner starts early
    let held = rest.clone();
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        let mut after = anchor;
        for &nid in &ids {
            match dom.next_sibling(after) {
                Some(next) => dom.insert_before(parent, nid, next),
                None => dom.append_child(parent, nid),
            }
            after = nid;
        }
        let a = st.borrow_mut::<WriteAnchor>();
        a.script = script_id;
        a.after = *ids.last().unwrap();
    }
    if stop.is_some() {
        let blocking = *ids.last().unwrap();
        crate::core::html::html_script_element::mark_script_started(&op_state, blocking);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<WrittenInput>().is_none() {
            st.put(WrittenInput::default());
        }
        let w = st.borrow_mut::<WrittenInput>();
        w.blocking = Some(blocking);
        w.parent = parent;
        let older = std::mem::take(&mut w.pending);
        w.pending = rest;
        w.pending.extend(older);
    }
    notify_childlist_batch(scope, &op_state, parent, &ids, &[]);
    if !held.is_empty() {
        crate::core::html::subresource::speculate_written(scope, &held);
    }
}

/// The written input a parser-blocking written `<script src>` holds back.
///
/// HTML's parser pauses at a parser-blocking script until it has been fetched and run, and the input
/// behind it -- the rest of the markup written with it, and anything written while it waits -- is
/// tokenised only afterwards. Korvus parses each write as a fragment, so the input is kept PARSED:
/// detached nodes, in input order, inserted after the script once it has run.
#[derive(Default)]
pub(crate) struct WrittenInput {
    /// The written script the parser is waiting for, until the driver takes it.
    blocking: Option<u32>,
    /// The script the driver took and is running: the insertion point of the flush that follows it.
    running: Option<u32>,
    /// The parent the written input is inserted into.
    parent: u32,
    /// Parsed, not yet inserted, in input order.
    pending: Vec<u32>,
}

/// Is `n` a script the parser must stop at: an HTML `<script src>` of the classic type, neither
/// `async` nor `defer` (a written `defer` or module script waits for the end of the parse instead).
fn written_script_blocks(dom: &korvus_dom::Dom, n: u32) -> bool {
    dom.node_type(n) == 1
        && dom.is_html_ns(n)
        && dom.tag_name(n) == Some("script")
        && dom.get_attribute(n, "src").is_some()
        && dom.get_attribute(n, "async").is_none()
        && dom.get_attribute(n, "defer").is_none()
        && dom.get_attribute(n, "nomodule").is_none()
        && crate::core::html::html_script_element::element_script_type_runs(dom, n)
        && dom.get_attribute(n, "type").is_none_or(crate::core::html::html_script_element::is_classic_type)
}

/// The written parser-blocking script the driver must fetch and run now, if a `document.write` left
/// one: its element and its `src` resolved against the document base. Taking it hands the insertion
/// point to [`flush_written_input`], which the driver calls once the script has run.
pub(crate) fn take_written_blocking<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<(v8::Local<'s, v8::Value>, String)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let mut st = op_state.borrow_mut();
        let w = st.try_borrow_mut::<WrittenInput>()?;
        let id = w.blocking.take()?;
        w.running = Some(id);
        id
    };
    let (src, base) = {
        let st = op_state.borrow();
        let src = st.borrow::<HostDom>().0.get_attribute(id, "src").unwrap_or_default().to_string();
        (src, crate::core::dom::base_url_of(&st))
    };
    let url = url::Url::parse(&base).and_then(|b| b.join(src.trim())).map(|u| u.to_string()).unwrap_or(src);
    let el = wrap_cached(scope, &op_state, id);
    Some((el.into(), url))
}

/// Insert the written input the last written blocking script held back, after it (and after whatever
/// it wrote itself), running inline scripts as they arrive -- until the input runs out or reaches the
/// next parser-blocking script, which the driver then takes like the first.
pub(crate) fn flush_written_input(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (mut queue, parent, ran) = {
        let mut st = op_state.borrow_mut();
        let Some(w) = st.try_borrow_mut::<WrittenInput>() else { return };
        let Some(ran) = w.running.take() else { return };
        // The script that just ran wrote a parser-blocking script of its own: the input stays
        // behind THAT one, and flushes after it has run.
        if w.blocking.is_some() {
            return;
        }
        (std::mem::take(&mut w.pending), w.parent, ran)
    };
    // After the script, or after the last node the script itself wrote there.
    let mut after = {
        let st = op_state.borrow();
        match st.try_borrow::<WriteAnchor>() {
            Some(a) if a.script == ran && st.borrow::<HostDom>().0.parent(a.after) == Some(parent) => a.after,
            _ => ran,
        }
    };
    if op_state.borrow().borrow::<HostDom>().0.parent(after) != Some(parent) {
        return; // the script was removed: nowhere to continue from
    }
    queue.reverse();
    while let Some(n) = queue.pop() {
        {
            let mut st = op_state.borrow_mut();
            let dom = &mut st.borrow_mut::<HostDom>().0;
            match dom.next_sibling(after) {
                Some(next) => dom.insert_before(parent, n, next),
                None => dom.append_child(parent, n),
            }
        }
        after = n;
        let blocks = written_script_blocks(&op_state.borrow().borrow::<HostDom>().0, n);
        if blocks {
            crate::core::html::html_script_element::mark_script_started(&op_state, n);
            let mut st = op_state.borrow_mut();
            let w = st.borrow_mut::<WrittenInput>();
            w.blocking = Some(n);
            queue.reverse();
            let older = std::mem::take(&mut w.pending);
            w.pending = queue;
            w.pending.extend(older);
            drop(st);
            notify_childlist_batch(scope, &op_state, parent, &[n], &[]);
            return;
        }
        notify_childlist_batch(scope, &op_state, parent, &[n], &[]);
        // An inline script just inserted may have written: its own output sits after it, and a
        // blocking script it wrote puts the rest of this input behind that script's.
        {
            let st = op_state.borrow();
            if let Some(a) = st.try_borrow::<WriteAnchor>() {
                if a.script == n && st.borrow::<HostDom>().0.parent(a.after) == Some(parent) {
                    after = a.after;
                }
            }
        }
        let nested = op_state.borrow().try_borrow::<WrittenInput>().is_some_and(|w| w.blocking.is_some());
        if nested {
            queue.reverse();
            let mut st = op_state.borrow_mut();
            st.borrow_mut::<WrittenInput>().pending.extend(queue);
            return;
        }
    }
}

/// Forget the previous document's written input (a new document's arena reuses node ids).
pub(crate) fn reset_written_input(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) {
    let mut st = op_state.borrow_mut();
    if let Some(w) = st.try_borrow_mut::<WrittenInput>() {
        *w = WrittenInput::default();
    }
}

/// `document.ariaNotify(message)` -- queue a message for assistive technology. Nothing assistive is
/// attached to this engine, so the queue it goes on is empty by construction; the call is still a real
/// one, and a missing message is the WebIDL TypeError.
pub(crate) fn cb_doc_aria_notify(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    require_args(scope, &args, 1, "Document", "ariaNotify");
}

/// `document.exitFullscreen()` -- the spec's FIRST step is "if this's fullscreen element is null,
/// reject with a TypeError". No document in this engine is ever the fullscreen element (there is no
/// fullscreen support at all), so that is the step every call takes. A rejected promise, not a
/// resolved one: resolving would tell the page it just left a fullscreen it was never in.
pub(crate) fn cb_doc_exit_fullscreen(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let promise = resolver.get_promise(scope);
    let msg = v8::String::new(scope, "Document not active").unwrap();
    let exc = v8::Exception::type_error(scope, msg);
    resolver.reject(scope, exc);
    rv.set(promise.into());
}

/// `document.exitPictureInPicture()` -- same shape, different first step: with no
/// picture-in-picture element the spec rejects with InvalidStateError.
pub(crate) fn cb_doc_exit_pip(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let promise = resolver.get_promise(scope);
    if let Some(exc) = make_dom_exc(scope, "There is no Picture-in-Picture element in this document.", "InvalidStateError") {
        resolver.reject(scope, exc);
    }
    rv.set(promise.into());
}

/// `document.hasStorageAccess()` / `hasUnpartitionedCookieAccess()` -- "can this document reach its
/// own unpartitioned cookies and storage".
///
/// Korvus has ONE browsing context and no partitioning: `document.cookie`, `localStorage` and IndexedDB
/// all read the document's own first-party jar, with nothing embedding it and no partition key to be
/// on the wrong side of. So the answer is yes, and it is a fact about how this engine's storage is
/// keyed rather than an optimistic default.

/// A document created with `new Document()` has NO browsing context, and the Storage Access API
/// rejects on one with `InvalidStateError` before it considers access at all.
///
/// These three used to answer unconditionally -- `true`, `true`, `undefined` -- which is the right
/// answer for the page's own document and the wrong one for a detached Document, and the difference
/// was invisible while the corpus could only ask a synchronous question about a promise-returning
/// member. A synchronous question sees `[object Promise]` either way.
fn doc_has_browsing_context(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    // The page's document, or a FRAME's: both are shown by a browsing context. It was the
    // page's alone, so `hasStorageAccess()` in any frame rejected "the document has no browsing
    // context" where Chrome 150 resolves `true` (same-site and cross-site frames alike, measured).
    // A frame's document counts only while the frame is still in the document (fully active).
    doc_root_slot(scope, this) == PAGE_DOC_ROOT
        || frame_of_this_document(scope, this).is_some_and(|id| crate::core::html::browsing_context::context_fully_active(scope, id))
}

/// The nested browsing context showing the document `this`, if it is a frame's document.
fn frame_of_this_document(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<u32> {
    let slot = doc_root_slot(scope, this);
    if slot == PAGE_DOC_ROOT {
        return None;
    }
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        resolve_doc_root(&st.borrow::<HostDom>().0, slot)
    };
    crate::core::html::browsing_context::frame_of_document_root(scope, root)
}

fn reject_invalid_state<'s>(scope: &mut v8::PinScope<'s, '_>, method: &str) -> v8::Local<'s, v8::Value> {
    let msg = format!(
        "Failed to execute '{method}' on 'Document': The document has no browsing context."
    );
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return v8::undefined(scope).into() };
    let promise = resolver.get_promise(scope);
    if let Some(e) = crate::binding::protos::make_dom_exc(scope, &msg, "InvalidStateError") {
        resolver.reject(scope, e);
    }
    promise.into()
}

pub(crate) fn cb_doc_has_storage_access(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !doc_has_browsing_context(scope, args.this().into()) {
        let p = reject_invalid_state(scope, "hasStorageAccess");
        rv.set(p);
        return;
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    let p = crate::binding::protos::resolved_promise(scope, t);
    rv.set(p);
}
/// `document.requestStorageAccess()` -- the spec resolves immediately when the document already has
/// access, which (see above) it always does here. No prompt, because there is nothing to grant.
/// `requestStorageAccess()` and `requestStorageAccess(types)` -- the overloads by argument count. The second converts
/// its `StorageAccessTypes` first and refuses one asking for nothing (Chrome 150, 2026-10-11, measured:
/// SecurityError "You must request access for at least one storage/communication medium."; korvus read none).
pub(crate) fn cb_doc_request_storage_access(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() >= 1 {
        let types = match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), "StorageAccessTypes", "Failed to execute 'requestStorageAccess' on 'Document': ") {
            Ok(t) => t,
            Err(e) => {
                rv.set(crate::binding::protos::rejected_promise(scope, e));
                return;
            }
        };
        let names = types.get_own_property_names(scope, v8::GetPropertyNamesArgs::default());
        let mut any = false;
        if let Some(names) = names {
            for i in 0..names.length() {
                let Some(k) = names.get_index(scope, i) else { continue };
                if types.get(scope, k).is_some_and(|v| v.is_true()) {
                    any = true;
                }
            }
        }
        if !any {
            if let Some(e) = crate::binding::protos::make_dom_exc(scope, "You must request access for at least one storage/communication medium.", "SecurityError") {
                rv.set(crate::binding::protos::rejected_promise(scope, e));
            }
            return;
        }
    }
    if !doc_has_browsing_context(scope, args.this().into()) {
        let p = reject_invalid_state(scope, "requestStorageAccess");
        rv.set(p);
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let p = crate::binding::protos::resolved_promise(scope, u);
    rv.set(p);
}

/// `document.lastModified` -- "MM/DD/YYYY hh:mm:ss" in the USER'S LOCAL TIME.
///
/// The value comes from the response's `Last-Modified` header, and the spec's fallback when there is
/// none is the current time -- which is the branch every document here takes, because the engine loads
/// documents from a string with no response metadata attached. The format is not ISO and not UTC (a
/// page can and does compare it against `new Date()`), so it is built from the same `Date` the page
/// sees rather than from a Rust clock that would not know the local zone.
pub(crate) fn cb_doc_last_modified(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let two = |n: f64| format!("{:02}", n as i64);
    let s = (|| -> Option<String> {
        // The realm's own `Date` and its prototype's getters, never the page's.
        let date_ctor = crate::binding::protos::intrinsic_fn(scope, "Date")?;
        let now = date_ctor.new_instance(scope, &[])?;
        let num = |scope: &mut v8::PinScope, o: v8::Local<v8::Object>, m: &str| -> Option<f64> {
            let f = crate::binding::protos::intrinsic_method(scope, "Date", m)?;
            f.call(scope, o.into(), &[])?.number_value(scope)
        };
        // getMonth() is 0-based; the serialisation is 1-based.
        let mo = num(scope, now, "getMonth")? + 1.0;
        let da = num(scope, now, "getDate")?;
        let yr = num(scope, now, "getFullYear")?;
        let hh = num(scope, now, "getHours")?;
        let mi = num(scope, now, "getMinutes")?;
        let ss = num(scope, now, "getSeconds")?;
        Some(format!("{}/{}/{} {}:{}:{}", two(mo), two(da), yr as i64, two(hh), two(mi), two(ss)))
    })()
    .unwrap_or_default();
    let v = str_val(scope, &s);
    rv.set(v);
}

/// The document's character encoding, as resolved from `<meta charset>` / `<meta http-equiv>` at parse
/// time. Not a constant: it is what `document.characterSet` reports, and -- the reason it exists at all
/// -- it is the ENCODING OVERRIDE the URL parser applies to a query string (WHATWG URL, "percent-encode
/// after encoding"). A document declaring `<meta charset="euc-kr">` percent-encodes `a.href`'s query as
/// EUC-KR bytes, not UTF-8, and 19 WPT files across four legacy CJK encodings assert exactly that.
///
/// Storage stays UTF-8 throughout -- html5ever decoded the bytes long before this. This records what the
/// document SAID it was, which is a different fact and the one the URL parser needs.
pub struct DocEncoding(pub &'static encoding_rs::Encoding);

/// The page document's MIME type, as the response declared it -- what `document.contentType` answers
/// for the page. Recorded at load; a runtime that loaded no response has none, and the
/// document's own shape answers instead.
pub struct PageContentType(pub String);

/// A frame document's MIME type when a viewer made it from its response (A/B C5): by document root. A document
/// the HTML parser made from the response answers `text/html` without an entry.
#[derive(Default)]
pub(crate) struct DocContentTypes(pub(crate) std::collections::HashMap<u32, String>);
impl Default for DocEncoding {
    fn default() -> Self {
        DocEncoding(encoding_rs::UTF_8)
    }
}

// ── the Document table ─────────────────────────────────────────────────────────────
//
// The 249 members of `Document.prototype` (+ the two statics), in the capture's order, installed by
// THE installer over the cppgc `JsDocument` class prototype (`core/dom/spine.rs`). The twins of the
// `#[op2] impl JsDocument` members (`title`, `hasFocus`, `hidden`, ...) and the ports of what
// `dom/08c`, `08f`, `08g`, `08h`, `09e`, `09i`, `09j` mounted by hand are below; the 117 handlers and
// the members a later phase still overlays (`all`, `fonts`, `evaluate`, `elementFromPoint`, ...) are
// `event_handler` / `stub` rows.

use crate::binding::host_dom::QNameMap;
use crate::binding::protos::{cb_noop, document_val, set_prop, unwrap_id};
use crate::core::animation::document_timeline::cb_document_timeline_get;
use crate::core::animation::targets::cb_document_get_animations;
use crate::core::feature_policy::feature_policy::cb_document_feature_policy;
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::collections::{builder_data, cb_by_tag_ns, cb_doc_by_name, cb_doc_collection, static_list_reuse, Kind};
use crate::core::dom::element::priv_set;
use crate::core::dom::shared::{row_accessor, row_accessor_data, row_getter, row_getter_data, row_method, row_method_data};
use crate::install::MemberDef;

const P_SHEET_LIST: &str = "korvus_doc_sheet_list";
const P_ADOPTED: &str = "korvus_doc_adopted_sheets";
const P_XML_VERSION: &str = "korvus_doc_xml_version";
const P_XML_STANDALONE: &str = "korvus_doc_xml_standalone";

// ---- the op2 twins ------------------------------------------------------------------------------

/// The Document receiver's root slot, or the "Illegal invocation" TypeError a Document member throws on
/// a receiver that is not a document (what the op2 members these replace threw; Chrome's answer).
fn doc_this(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    let slot = deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, args.this().into()).map(|p| unsafe { p.as_ref() }.root);
    if slot.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    slot
}

pub(crate) fn cb_doc_title_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; doc_title(dom, resolve_doc_root(dom, slot)) };
    let v = str_val(scope, &s);
    rv.set(v);
}
/// `document.title = v`, HTML's setter, through the DOM's own members so every mutation observer,
/// insertion seam and style invalidation sees it exactly as it would a page's own
/// `textContent` / `appendChild`:
///
/// * an SVG document: the title element, else a new SVG `title` inserted as the document element's
///   FIRST child, has its children replaced by `v`;
/// * a document whose element is HTML: the title element, else -- when there is a `<head>` -- a new
///   HTML `title` appended to the head, likewise; with neither, nothing happens;
/// * any other document: nothing.
///
/// Measured on stock Chrome 150.0.7871.46: `title = "  set  me\n now "` on a head-only document
/// creates `<head><title>` holding the string verbatim and reads back `"set me now"`; `title = ""`
/// leaves the element with no children; with `<head>` removed the write is dropped and no `<title>`
/// exists; `createDocument(SVG, "svg")` gains a first-child `title`; `createDocument(null, "root")`
/// ignores the write.
pub(crate) fn cb_doc_title_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    let value = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    enum Target {
        Existing(u32),
        SvgFirstChild,
        HeadAppend,
    }
    let target = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let root = resolve_doc_root(dom, slot);
        match (title_element(dom, root), svg_document_element(dom, root), doc_element_child(dom, root)) {
            (Some(t), _, _) => Target::Existing(t),
            (None, Some(_), _) => Target::SvgFirstChild,
            (None, None, Some(_)) if head_element_of(&st, root).is_some() => Target::HeadAppend,
            _ => return,
        }
    };
    let doc = args.this();
    let el: v8::Local<v8::Value> = match target {
        Target::Existing(t) => crate::binding::protos::wrap_cached(scope, &op_state, t).into(),
        Target::SvgFirstChild | Target::HeadAppend => {
            let svg = matches!(target, Target::SvgFirstChild);
            let ns = str_val(scope, if svg { "http://www.w3.org/2000/svg" } else { "http://www.w3.org/1999/xhtml" });
            let name = str_val(scope, "title");
            // the realm's own members throughout, never the page's: a `document.title = ...` made and filled
            // its <title> through the page's `createElementNS`, `head`, `appendChild` and `textContent`
            // (a bot challenge page sets its title)
            use crate::binding::protos::{pristine_call, pristine_get};
            let Some(el) = pristine_call(scope, doc.into(), "Document.createElementNS", &[ns, name]) else { return };
            let parent_key = if svg { "Document.documentElement" } else { "Document.head" };
            let Some(parent) = pristine_get(scope, doc.into(), parent_key) else { return };
            if svg {
                let first = pristine_get(scope, parent, "Node.firstChild").unwrap_or_else(|| v8::null(scope).into());
                pristine_call(scope, parent, "Node.insertBefore", &[el, first]);
            } else {
                pristine_call(scope, parent, "Node.appendChild", &[el]);
            }
            el
        }
    };
    let v = str_val(scope, &value);
    crate::binding::protos::pristine_set(scope, el, "Node.textContent", v);
}

/// `hasFocus()`: only the page's document is in a browsing context at all; a document minted by
/// `new Document()` or `createDocument` has no view and no focus to have.
pub(crate) fn cb_doc_has_focus(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    // A remote frame's document is not the focused one: focus is the top's.
    let embedded = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.has::<crate::core::html::remote_frame::FrameEmbedding>()
    };
    rv.set_bool(slot == PAGE_DOC_ROOT && !embedded);
}
/// Page Visibility: the page's document is visible; a document with no browsing context is not being
/// displayed anywhere, so it is hidden. `webkitHidden`/`webkitVisibilityState` mirror the two exactly
/// (Chrome, 2026-09-10: `new Document().webkitHidden` is true).
///
/// The page's own document turns hidden once, when it is being UNLOADED: HTML's "unload a
/// document" updates the visibility state to `hidden` and fires `visibilitychange` between `pagehide`
/// and `unload`, and Chrome 150 reports exactly that order on a loopback navigation. [`PageHidden`] is
/// that step's state; nothing else sets it.
pub(crate) fn cb_doc_hidden(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    let hidden = doc_hidden(scope, slot);
    rv.set_bool(hidden);
}
pub(crate) fn cb_doc_visibility_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    let hidden = doc_hidden(scope, slot);
    let v = str_val(scope, if hidden { "hidden" } else { "visible" });
    rv.set(v);
}

/// A frame's document is shown with the page, so it is as visible as the page (it
/// answered `hidden`); a document no browsing context shows is hidden.
fn doc_hidden(scope: &mut v8::PinScope, slot: u32) -> bool {
    if slot == PAGE_DOC_ROOT {
        return page_hidden(scope);
    }
    crate::core::html::browsing_context::frame_doc_readiness(scope, slot).is_none() || page_hidden(scope)
}

/// The page document's visibility, when unloading has hidden it. See [`cb_doc_hidden`].
#[derive(Default)]
pub struct PageHidden(pub bool);

pub(crate) fn page_hidden(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<PageHidden>().is_some_and(|h| h.0)
}
/// `document.readyState` — HTML's "current document readiness", read from the one place that owns
/// it (`core/html/browsing_context.rs`), which the load pipeline moves `loading -> interactive ->
/// complete` across a navigation.
///
/// Only the PAGE document is loaded by that pipeline. Every other Document this engine can hand a
/// script -- `new Document()`, `createDocument`, `createHTMLDocument`, a `DOMParser` result, a
/// frame's finished document -- is complete the moment script can see it, which is what Chrome 152
/// answers for each of them (measured 2026-10-16; Chrome 150's `createHTMLDocument` result read `loading`) and
/// what `html/dom/documents/resource-metadata-management/document-readyState.html` asserts for the four
/// it covers -- and `loading` while a script-created parser is open in it (`open()` / `write()`, no `close()`).
pub(crate) fn cb_doc_ready_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    let s = if slot == PAGE_DOC_ROOT {
        crate::core::html::browsing_context::doc_readiness(scope).as_str()
    } else {
        // A frame's document has a readiness of its own (it was always "complete"); a document with a
        // script-created parser open is "loading" until its `close()` (measured Chrome 152).
        if created_doc_parser_open(scope, slot) {
            "loading"
        } else {
            crate::core::html::browsing_context::frame_doc_readiness(scope, slot).map(|r| r.as_str()).unwrap_or("complete")
        }
    };
    let v = str_val(scope, s);
    rv.set(v);
}

// ---- the ports ---------------------------------------------------------------------------------

/// `defaultView`: the page document's browsing context is the window; a nested or auxiliary context's
/// document answers THAT context's window (core/html/browsing_context.rs); every other document's is
/// null.
pub(crate) fn cb_doc_default_view(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match document_val(scope, &op_state) {
        // The PAGE's window, whichever realm's getter this is (a frame's
        // `Document.prototype.defaultView` getter called on the page's document).
        Some(d) if d == this => { let g = crate::core::html::browsing_context::main_global(scope); rv.set(g.into()); }
        _ => match crate::core::html::browsing_context::frame_document_window(scope, this) {
            Some(w) => rv.set(w.into()),
            None => rv.set_null(),
        },
    }
}

/// `createElementNS(namespace, qualifiedName)`. Validate and extract FIRST
/// ([`crate::core::xml::dom_implementation::validate_and_extract`], the one `createDocument` shares):
/// an invalid name or a reserved-prefix/namespace mismatch throws before anything is created. The
/// QUALIFIED name (prefix + case) is the arena tag, the namespace and parsed qname go to the side
/// tables, and the wrapper's prototype is re-resolved once they are written (an SVG element can only
/// be told apart from an HTML one of the same tag name at that moment).
pub(crate) fn cb_doc_create_element_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    const PREFIX: &str = "Failed to execute 'createElementNS' on 'Document': ";
    let ns_v = args.get(0);
    let ns: Option<String> = if ns_v.is_null_or_undefined() { None } else {
        let Some(n) = crate::binding::class::idl_string(scope, ns_v, PREFIX) else { return };
        Some(n)
    };
    let ns = ns.filter(|s| !s.is_empty());
    // `DOMString qualifiedName` is not nullable: `null` is the name "null" (WebIDL), not ""
    let Some(q) = crate::binding::class::idl_string(scope, args.get(1), PREFIX) else { return };
    if element_creation_options(scope, args.get(2), PREFIX).is_err() {
        return;
    }
    let (prefix, local) = match crate::core::xml::dom_implementation::validate_and_extract(ns.as_deref(), &q) {
        Ok(v) => v,
        Err((msg, name)) => {
            throw_dom(scope, &format!("Failed to execute 'createElementNS' on 'Document': {msg}"), name);
            return;
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The namespace goes on the NODE at creation, and the qualified name into `QNameMap`, so the
    // element is already the right kind before anything wraps it: `wrap_cached`'s `node_proto`
    // reads both. They were recorded AFTER the wrap, and a second `node_proto` then put the PAGE's
    // prototype back over the one the wrap had rehomed into the node's realm -- a frame's
    // `createElementNS(svgNS, "svg")` was not an instance of the frame's `SVGSVGElement` (Chrome 150:
    // it is, measured 2026-09-28). And it belongs to THIS document, as `createElement`'s node does:
    // on a created document it answered the page's `ownerDocument`.
    let id = {
        let mut st = op_state.borrow_mut();
        st.borrow_mut::<HostDom>().0.create_element_ns(&q, korvus_dom::ElementNs::from_uri(ns.as_deref()))
    };
    {
        let st = op_state.borrow();
        st.borrow::<QNameMap>().0.borrow_mut().insert(id, (prefix, local));
    }
    own_node(scope, args.this().into(), id);
    let el = wrap_cached(scope, &op_state, id);
    rv.set(el.into());
}

/// `document.customElementRegistry`: the registry the document's browsing context uses -- the one
/// global `customElements` for the page document; a document with no browsing context (`new
/// Document()`, a DOMParser result) has none and answers null (the recorded browser).
///
/// A FRAME's document answers its own window's registry (Chrome 150 / Brave 151, measured 2026-10-05:
/// `frame.contentDocument.customElementRegistry === frame.contentWindow.customElements` for an
/// `about:blank`, `srcdoc`, same-origin or cross-origin frame). It answered null for every frame.
pub(crate) fn cb_doc_custom_element_registry(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(slot) = doc_this(scope, &args) else { return };
    let win = match doc_context(scope, slot) {
        Some(None) => Some(crate::core::html::browsing_context::main_global(scope)),
        Some(Some(id)) => crate::core::html::browsing_context::frame_window(scope, id),
        None => None,
    };
    match win.and_then(|w| get_prop(scope, w, "customElements")) {
        Some(v) if !v.is_undefined() => rv.set(v),
        _ => rv.set_null(),
    }
}

/// The browsing context showing the document in `slot`: `Some(None)` the page (this engine's own
/// document -- a remote frame's, in its own engine), `Some(Some(id))` frame `id`, `None` a document no
/// browsing context shows (`new Document()`, `DOMParser`, `createHTMLDocument`).
fn doc_context(scope: &mut v8::PinScope, slot: u32) -> Option<Option<u32>> {
    if slot == PAGE_DOC_ROOT {
        return Some(None);
    }
    crate::core::html::browsing_context::frame_of_doc(scope, slot).map(Some)
}

/// The permissions-policy flags a document reports: whether `feature` is enabled in its browsing
/// context's policy (`core/feature_policy`), and false for a document with no browsing context.
fn doc_feature_enabled(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, feature: &str) -> Option<bool> {
    let slot = doc_this(scope, args)?;
    Some(match doc_context(scope, slot) {
        Some(ctx) => crate::core::feature_policy::enabled_in(scope, feature, ctx),
        None => false,
    })
}

/// `fullscreenEnabled` / `webkitFullscreenEnabled`: "fullscreen enabled" is the `fullscreen` policy
/// (default allowlist `'self'`). Chrome 150 / Brave 151, measured 2026-10-05: true on the page and in a
/// same-origin frame, false in a cross-origin frame unless `allow="fullscreen"` or `allowfullscreen`,
/// false under `allow="fullscreen 'none'"` and in a document with no browsing context. It was false always.
pub(crate) fn cb_doc_fullscreen_enabled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(b) = doc_feature_enabled(scope, &args, "fullscreen") {
        rv.set_bool(b);
    }
}

/// `pictureInPictureEnabled`: the `picture-in-picture` policy (default allowlist `*`), measured as above:
/// true in every frame, cross-origin ones included, false under `allow="picture-in-picture 'none'"` and
/// with no browsing context. It was false always.
pub(crate) fn cb_doc_pip_enabled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(b) = doc_feature_enabled(scope, &args, "picture-in-picture") {
        rv.set_bool(b);
    }
}

/// `styleSheets`: a `[SameObject]` StyleSheetList over the document's (or shadow root's) own
/// stylesheets -- every `<style>`, and every `<link>` that is a stylesheet, in tree order. Refilled in
/// place on each read (`static_list_reuse`), so the list is the same object every time.
pub(crate) fn cb_style_sheets(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    // The tree's `<style>` and `<link>` elements off the arena, and each one's sheet through the same
    // body `.sheet` runs -- no `querySelectorAll`, `rel` or `sheet` call a page could watch.
    // A shadow root names its own arena node; a document its root through `doc_root_slot` (the page's
    // document is the `ON_DOCUMENT_ID` sentinel, which `resolve_doc_root` maps), as `querySelectorAll` does.
    let shadow = crate::page::node_id(scope, this.into()).filter(|&id| crate::page::with_host_dom(scope, |d| d.is_shadow_root(id)).unwrap_or(false));
    let slot = doc_root_slot(scope, this.into());
    let ids = sheet_owners(scope, shadow, slot);
    let mut sheets: Vec<v8::Local<v8::Value>> = Vec::new();
    for &id in ids.iter() {
        let el = crate::page::node_object(scope, id);
        let Ok(el) = v8::Local::<v8::Object>::try_from(el) else { continue };
        if let Some(sheet) = crate::core::css::style_sheet::element_sheet(scope, el) {
            sheets.push(sheet.into());
        }
    }
    let arr = v8::Array::new(scope, sheets.len() as i32);
    for (i, s) in sheets.iter().enumerate() { arr.set_index(scope, i as u32, *s); }
    let existing = priv_get(scope, this, P_SHEET_LIST).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    // the first read mints the list in the document's (or shadow root's) realm, whichever realm's getter it was
    // (`collections::in_relevant_realm`; Chrome 152, 2026-10-23, probe r2/l3/r1/www/a1.html: an iframe's getter
    // called on the top document first answers the top realm's `StyleSheetList`); a later read refills it in place
    let out = match existing {
        Some(_) => static_list_reuse(scope, "StyleSheetList", arr, existing),
        None => crate::core::dom::collections::in_relevant_realm(scope, this, |scope| static_list_reuse(scope, "StyleSheetList", arr, None)),
    };
    priv_set(scope, this, P_SHEET_LIST, out);
    rv.set(out);
}

/// The `<style>` and `<link>` elements of one tree, in tree order, as of the DOM's [`structure_revision`] -- which
/// elements those are moves with the tree's shape and nothing else.
///
/// `styleSheets` walked the whole tree for them on every read: 43 us a read on a 2,000-element page where Chrome
/// 152 answers in under 1 us (2026-10-22), and a CSS-in-JS runtime looks its sheet up through `document.styleSheets`
/// around its rule inserts. Each element's sheet is still asked on every read ([`cb_style_sheets`]): a `<link>`'s
/// arrives without the tree moving.
///
/// [`structure_revision`]: korvus_dom::Dom::structure_revision
#[derive(Default)]
pub(crate) struct SheetOwnersCache(std::cell::RefCell<std::collections::HashMap<(u64, u32, bool), (u64, std::rc::Rc<Vec<u32>>)>>);

fn sheet_owners(scope: &mut v8::PinScope, shadow: Option<u32>, slot: u32) -> std::rc::Rc<Vec<u32>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some((key, rev)) = crate::page::with_host_dom(scope, |d| {
        let root = shadow.unwrap_or_else(|| resolve_doc_root(d, slot));
        ((d.instance(), root, shadow.is_some()), d.structure_revision())
    }) else {
        return Default::default();
    };
    {
        let mut st = op_state.borrow_mut();
        if !st.has::<SheetOwnersCache>() {
            st.put(SheetOwnersCache::default());
        }
        let hit = st.borrow::<SheetOwnersCache>().0.borrow().get(&key).filter(|(r, _)| *r == rev).map(|(_, ids)| ids.clone());
        if let Some(ids) = hit {
            return ids;
        }
    }
    let ids = std::rc::Rc::new(
        crate::page::with_host_dom(scope, |d| {
            match shadow {
                Some(sr) => d.shadow_descendants_matching(sr, "style, link"),
                None => d.query_selector_all_in(key.1, "style, link"),
            }
            .unwrap_or_default()
        })
        .unwrap_or_default(),
    );
    let st = op_state.borrow();
    let mut map = st.borrow::<SheetOwnersCache>().0.borrow_mut();
    if map.len() > 256 {
        map.clear();
    }
    map.insert(key, (rev, ids.clone()));
    ids
}

/// `currentScript`: the inline `<script>` the page document is running; null outside that window and
/// for every other document, which runs no scripts.
///
/// The store is a V8 PRIVATE on the page's global, written by the two places that run a page script:
/// `core/html/html_script_element.rs::run_inserted_script` (a dynamically inserted one, saving and
/// restoring so the nested case is right) and `korvus-runtime/src/script_exec.rs` (each script the
/// page driver runs). It was the `__curScript` bridge slot once -- an ordinary property
/// of an ordinary object the page could reach, read, overwrite or freeze.
pub(crate) fn cb_doc_current_script(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    // A frame's document answers its own running script.
    if let Some(slot) = deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, this).map(|p| unsafe { p.as_ref() }.root) {
        if slot != PAGE_DOC_ROOT {
            if let Some(v) = crate::core::html::browsing_context::frame_doc_current_script(scope, slot) {
                rv.set(v);
                return;
            }
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let is_page = document_val(scope, &op_state).is_some_and(|d| d == this);
    match crate::core::html::html_script_element::current_script(scope) {
        Some(v) if is_page && v.is_object() => rv.set(v),
        _ => rv.set_null(),
    }
}

/// `adoptedStyleSheets`: a `[SameObject]` ObservableArray of CSSStyleSheet, held per document (or shadow
/// root) in a private slot; `[]` until set. Setting takes any iterable of CSSStyleSheet (Chrome,
/// 2026-09-10: a Set is accepted; `5` "cannot be converted to a sequence"; `[1]` "Failed to convert
/// value to 'CSSStyleSheet'").
///
/// The array is a Proxy over a plain array: every change -- the setter, `push`,
/// `splice`, an index write, a `length` cut -- tells the page which sheets the tree adopts, so a
/// constructed sheet styles exactly the trees that list it. Chrome 150, measured
/// (`tools/realweb/seed/adopted`): the setter refills the SAME array (`a === doc.adoptedStyleSheets`
/// after `doc.adoptedStyleSheets = []`, and `a.length` is 0), an index write converts its value
/// ("Failed to convert value to 'CSSStyleSheet'."), and an index past the end or a longer `length`
/// is refused without a hole (`a[5] = s` on an empty one leaves `length` 0).
pub(crate) fn cb_adopted_style_sheets_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if let Some((proxy, _)) = adopted_array(scope, this) {
        rv.set(proxy.into());
    }
}

const P_ADOPTED_LIST: &str = "korvus_doc_adopted_list";

/// The tree's observable array and the plain array behind it, made on first use.
fn adopted_array<'s>(scope: &mut v8::PinScope<'s, '_>, owner: v8::Local<v8::Object>) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Array>)> {
    let owner = v8::Local::new(scope, owner);
    if let (Some(p), Some(l)) = (priv_get(scope, owner, P_ADOPTED), priv_get(scope, owner, P_ADOPTED_LIST)) {
        if let (Ok(p), Ok(l)) = (v8::Local::<v8::Object>::try_from(p), v8::Local::<v8::Array>::try_from(l)) {
            return Some((p, l));
        }
    }
    let list = v8::Array::new(scope, 0);
    let handler = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    handler.set_prototype(scope, null);
    let data: v8::Local<v8::Value> = owner.into();
    let set = crate::binding::protos::build_fn_data(scope, cb_adopted_trap_set, "set", 4, data);
    crate::binding::protos::set_prop(scope, handler, "set", set.into());
    let del = crate::binding::protos::build_fn_data(scope, cb_adopted_trap_delete, "deleteProperty", 2, data);
    crate::binding::protos::set_prop(scope, handler, "deleteProperty", del.into());
    let proxy: v8::Local<v8::Object> = v8::Proxy::new(scope, list.into(), handler)?.into();
    priv_set(scope, owner, P_ADOPTED, proxy.into());
    priv_set(scope, owner, P_ADOPTED_LIST, list.into());
    Some((proxy, list))
}

/// `key` as an array index, when it is one (a canonical numeric string below 2^32 - 1).
fn array_index(scope: &mut v8::PinScope, key: v8::Local<v8::Value>) -> Option<u32> {
    if !key.is_string() {
        return None;
    }
    let s = key.to_rust_string_lossy(scope);
    let n: u32 = s.parse().ok()?;
    (n != u32::MAX && n.to_string() == s).then_some(n)
}

/// Is `v` a CSSStyleSheet (what the ObservableArray's type converts to)?
fn is_style_sheet(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let ctor = crate::binding::protos::iface_object(scope, "CSSStyleSheet").and_then(|c| v8::Local::<v8::Object>::try_from(c).ok());
    v.is_object() && ctor.map(|c| v.instance_of(scope, c).unwrap_or(false)).unwrap_or(true)
}

/// Tell the page which sheets the tree `owner` (a document or a shadow root) now adopts.
fn adopted_sync(scope: &mut v8::PinScope, owner: v8::Local<v8::Object>, list: v8::Local<v8::Array>) {
    let tree = unwrap_id(scope, owner.into())
        .or_else(|| deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, owner.into()).map(|p| unsafe { p.as_ref() }.root));
    let Some(tree) = tree else { return };
    let mut handles = Vec::with_capacity(list.length() as usize);
    for i in 0..list.length() {
        if let Some(o) = list.get_index(scope, i).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            handles.push(crate::core::css::style_sheet::sheet_handle(scope, o));
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    crate::core::css::cssom::adopt_sheets(&mut op_state.borrow_mut(), tree, &handles);
}

fn cb_adopted_trap_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    #[cfg(not(feature = "analysis"))]
    let (Ok(owner), Ok(list)) = (v8::Local::<v8::Object>::try_from(args.data()), v8::Local::<v8::Array>::try_from(args.get(0))) else { return };
    // an analysis build makes this function a fused record: its data is the record's
    #[cfg(feature = "analysis")]
    let (Ok(owner), Ok(list)) = (v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)), v8::Local::<v8::Array>::try_from(args.get(0))) else { return };
    let (key, value) = (args.get(1), args.get(2));
    let len = list.length();
    if let Some(i) = array_index(scope, key) {
        if !is_style_sheet(scope, value) {
            throw_type_error(scope, "Failed to convert value to 'CSSStyleSheet'.");
            return;
        }
        if i > len {
            rv.set_bool(false);
            return;
        }
        list.set_index(scope, i, value);
        adopted_sync(scope, owner, list);
        rv.set_bool(true);
        return;
    }
    if key.is_string() && key.to_rust_string_lossy(scope) == "length" {
        let Some(n) = value.uint32_value(scope) else { return };
        if n > len {
            rv.set_bool(false);
            return;
        }
        let k: v8::Local<v8::Value> = str_val(scope, "length");
        let nv: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, n).into();
        list.set(scope, k, nv);
        adopted_sync(scope, owner, list);
        rv.set_bool(true);
        return;
    }
    let ok = list.set(scope, key, value).unwrap_or(false);
    rv.set_bool(ok);
}

fn cb_adopted_trap_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    #[cfg(not(feature = "analysis"))]
    let (Ok(owner), Ok(list)) = (v8::Local::<v8::Object>::try_from(args.data()), v8::Local::<v8::Array>::try_from(args.get(0))) else { return };
    // an analysis build makes this function a fused record: its data is the record's
    #[cfg(feature = "analysis")]
    let (Ok(owner), Ok(list)) = (v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)), v8::Local::<v8::Array>::try_from(args.get(0))) else { return };
    let key = args.get(1);
    if let Some(i) = array_index(scope, key) {
        // Only the last item can go, and the array shrinks with it -- no holes.
        let len = list.length();
        if len == 0 || i != len - 1 {
            rv.set_bool(i >= len);
            return;
        }
        let k: v8::Local<v8::Value> = str_val(scope, "length");
        let nv: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, i).into();
        list.set(scope, k, nv);
        adopted_sync(scope, owner, list);
        rv.set_bool(true);
        return;
    }
    let ok = list.delete(scope, key).unwrap_or(false);
    rv.set_bool(ok);
}

pub(crate) fn cb_adopted_style_sheets_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let iface = if unwrap_id(scope, this.into()).is_some() { "ShadowRoot" } else { "Document" };
    let v = args.get(0);
    if !v.is_object() {
        throw_type_error(scope, &format!("Failed to set the 'adoptedStyleSheets' property on '{iface}': The provided value cannot be converted to a sequence."));
        return;
    }
    // Any iterable: `Array.from` performs the sequence conversion the way WebIDL does (an object with
    // no @@iterator throws its own TypeError, which stays thrown).
    let seq = crate::binding::protos::call_intrinsic(scope, "Array.from", &[v])
        .and_then(|r| v8::Local::<v8::Array>::try_from(r).ok());
    let Some(seq) = seq else { return };
    for i in 0..seq.length() {
        let Some(item) = seq.get_index(scope, i) else { continue };
        if !is_style_sheet(scope, item) {
            throw_type_error(scope, &format!("Failed to set the 'adoptedStyleSheets' property on '{iface}': Failed to convert value to 'CSSStyleSheet'."));
            return;
        }
    }
    // The same array, refilled.
    let Some((_, list)) = adopted_array(scope, this) else { return };
    let k: v8::Local<v8::Value> = str_val(scope, "length");
    let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
    list.set(scope, k, zero);
    for i in 0..seq.length() {
        if let Some(item) = seq.get_index(scope, i) {
            list.set_index(scope, i, item);
        }
    }
    adopted_sync(scope, this, list);
}

/// `xmlVersion`: null for an HTML document, "1.0" for an XML one, and whatever "1.0" a script stored
/// (the only version this engine's parser supports -- any other is Chrome's NotSupportedError,
/// 2026-09-10). `xmlStandalone` is a per-document flag, false until set; `xmlEncoding` is always null.
pub(crate) fn cb_doc_xml_version_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if let Some(v) = priv_get(scope, this, P_XML_VERSION) {
        if v.is_string() { rv.set(v); return; }
    }
    if doc_is_not_html(scope, this.into()) { let s = str_val(scope, "1.0"); rv.set(s); } else { rv.set_null(); }
}
pub(crate) fn cb_doc_xml_version_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let v = args.get(0).to_rust_string_lossy(scope);
    if v != "1.0" {
        throw_dom(scope, &format!("Failed to set the 'xmlVersion' property on 'Document': This document does not support the XML version '{v}'."), "NotSupportedError");
        return;
    }
    let s = str_val(scope, &v);
    priv_set(scope, args.this(), P_XML_VERSION, s);
}
pub(crate) fn cb_doc_xml_standalone_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let on = priv_get(scope, args.this(), P_XML_STANDALONE).map(|v| v.is_true()).unwrap_or(false);
    rv.set_bool(on);
}
pub(crate) fn cb_doc_xml_standalone_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let on: v8::Local<v8::Value> = v8::Boolean::new(scope, args.get(0).boolean_value(scope)).into();
    priv_set(scope, args.this(), P_XML_STANDALONE, on);
}

/// `Document.parseHTMLUnsafe(html)` / `Document.parseHTML(html)`: a NEW document parsed from the
/// string, with its own root -- the same arena parse `DOMParser` uses, and the same mint, so it is an
/// `HTMLDocument` as Chrome's is. It answered `[object Document]` while the HTMLDocument/XMLDocument
/// split lived in `dom/05-xml.js` and only its own facade wore those prototypes.
pub(crate) fn cb_doc_parse_html(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // (DOMString or TrustedHTML): a TrustedHTML read as itself, nothing on it called; a string through the
    // current realm's Trusted Types ("Document parseHTMLUnsafe")
    // then `ParseHTMLUnsafeOptions` (`sanitizer`), converted before the operation's steps
    let prefix = "Failed to execute 'parseHTMLUnsafe' on 'Document': ";
    let Some(html_v) = crate::core::dom::element::html_union(scope, args.get(0), prefix) else { return };
    if crate::install::ctor_dicts::convert_dict(scope, args.get(1), "ParseHTMLUnsafeOptions", prefix).is_err() {
        return;
    }
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: "Document parseHTMLUnsafe", context: "Failed to execute 'parseHTMLUnsafe' on 'Document'" };
    let at = crate::core::trusted_types::tables::SinkAt::Realm(scope.get_current_context());
    let Some(html) = crate::core::trusted_types::tables::compliant_text(scope, html_v, &sink, at) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = op_state.borrow_mut().borrow_mut::<HostDom>().0.parse_into(&html);
    crate::core::html::html_script_element::mark_parsed_scripts(&op_state, &[root]);
    let d = mint_document_for(scope, root, DocKind::Html, DocOrigin::CreatedAboutBlank);
    rv.set(d.into());
}

// ---- the document object ----------------------------------------------------------------------

/// `document.location = url` is `[PutForwards=href]` on the page's Location -- or, for a nested
/// browsing context's document, on THAT context's Location, which navigates the context; a document
/// with no browsing context has no Location to forward to.
fn cb_doc_location_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if doc_root_slot(scope, this) != PAGE_DOC_ROOT {
        crate::core::html::browsing_context::frame_document_navigate(scope, this, args.get(0));
        return;
    }
    let v = args.get(0);
    if let Some(loc) = global_get(scope, "location").and_then(|l| v8::Local::<v8::Object>::try_from(l).ok()) {
        set_prop(scope, loc, "href", v);
    }
}

/// Mint a Document over `root` on the class prototype (`Document.prototype` after the spine install)
/// with its one `[LegacyUnforgeable]` member: `location` is an OWN accessor of every document --
/// `Object.getOwnPropertyNames(document)` is `["location"]` in Chrome, `{ get, set, enumerable: true,
/// configurable: false }`, and the getter is per document (2026-09-10).
///
/// THE `configurable` KNOB IS GONE, and with it the last of `dom/05-xml.js`. A document that another
/// layer finished -- a DOMParser / `createDocument` result, a frame's -- got `configurable: true`,
/// because the JS facade copied its own `get location() { return null; }` over this accessor and a
/// non-configurable property cannot be redefined. W5.0e deleted that facade; the only redefinition
/// left was `browsing_context.rs::set_ctx_document` re-installing the IDENTICAL accessor on a frame
/// document, which its own comment already said was unnecessary ("it answers a nested browsing
/// context's own Location by itself now"). Both are gone, so every document gets the flag Chrome
/// gives it. Measured on headless Chrome 150.0.7871.46, 2026-09-12 -- the page's document, a
/// `createHTMLDocument`, a `DOMParser` result, a `createDocument` and an `<iframe>`'s all report
/// `{ get, set, enumerable: true, configurable: false }`; korvus reported `configurable: true` for
/// four of those five.
///
/// `iface` is the document's interface, the realm's own: a page's or an HTML
/// document's `HTMLDocument` (what `wire_document_identity` re-parented a page's onto), an XML
/// document's `XMLDocument`, `new Document()`'s `Document`. Each is minted from that interface's template.
pub(crate) fn mint_document<'s>(scope: &mut v8::PinScope<'s, '_>, root: u32, iface: &str) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::protos::make_document_wrapper(scope, JsDocument { root }, iface);
    install_own_location(scope, obj);
    intern_document(scope, obj, root);
    obj
}

/// Record this Document as the canonical JS object for its arena root node -- in the SAME cache every
/// other node's wrapper lives in, so that one arena node has exactly one JS object.
///
/// [`wrap_cached`](crate::binding::protos::wrap_cached) mints one wrapper per arena id and hands that
/// same wrapper back forever; it is what makes `getElementById(x) === querySelector("#x")`. A DOCUMENT
/// node was the hole in that rule. Nothing ever registered the Document object, so the first walk that
/// reached a document root -- `documentElement.parentNode`, `getRootNode()`, `commonAncestorContainer`
/// -- minted a SECOND object for it: a `JsElement` retyped through the nodeType-9 entry of
/// `NODE_TYPE_IFACES`, which is `DocumentFragment` (a shadow root reuses `NodeKind::Document` too, and
/// is caught before that map by `Dom::is_shadow_root`). It carried no Document member at all.
///
/// Measured on headless Chrome 150.0.7871.46, 2026-09-12: for `createHTMLDocument`, `createDocument`,
/// both `DOMParser` types and `Document.parseHTMLUnsafe`, `d.documentElement.parentNode === d` and its
/// constructor is the document's own (`HTMLDocument`/`XMLDocument`). Korvus answered
/// `DocumentFragment` and a foreign object for all five.
///
/// THE PAGE'S DOCUMENT IS INTERNED HERE TOO, under the arena root its `PAGE_DOC_ROOT` sentinel
/// resolves to. It had TWO hand-written escapes from the same defect -- `cb_parent_node` and
/// `cb_get_root_node` each carried an "if this is the page root, answer `document`" branch, added
/// separately (the second after React 19's hoistable hydration threw on react.dev). Both are gone:
/// there is one rule, in one place, and it covers every document and every upward walk.
///
/// `install_document` re-mints the page's document after every arena swap, so the entry is rewritten
/// whenever it could go stale. (Nothing clears the rest of the cache on a swap; that is pre-existing
/// and is a defect for element wrappers exactly as much as for this one.)
fn intern_document(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, root: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let st = op_state.borrow();
        resolve_doc_root(&st.borrow::<HostDom>().0, root)
    };
    let g = v8::Global::new(scope, obj);
    op_state.borrow().borrow::<crate::binding::NodeCache>().insert(id, g);
}

/// The kind a document that is not the page's reports: which interface prototype it wears, and what
/// `contentType` answers.
#[derive(Clone, Debug)]
pub(crate) enum DocKind {
    /// An HTML document -- `HTMLDocument.prototype`, `text/html`.
    Html,
    /// An XML document -- `XMLDocument.prototype` -- carrying the content type recorded at CREATION:
    /// `createDocument` derives it from the namespace it was given, and `DOMParser.parseFromString`
    /// records the MIME type IT was given, verbatim.
    ///
    /// A DOMParser result used to record `None` and derive the type from the root element's namespace
    /// on read. Chrome 150, measured: `parseFromString(s, t).contentType === t` for every one of
    /// `text/xml`, `application/xml`, `application/xhtml+xml` and `image/svg+xml`, whatever the root
    /// element's namespace is -- the derivation answered `application/xml` for three of the four, and
    /// once the XML parser started resolving `xmlns` it answered `application/xhtml+xml` for an XHTML
    /// root that had been asked for as `application/xml`. The type is a recorded fact, not a guess.
    Xml(String),
}

/// Every created or parsed document's kind, by arena root -- the one place a document that is not the
/// page records what it is.
#[derive(Default)]
pub struct DocKinds(pub(crate) std::cell::RefCell<std::collections::HashMap<u32, DocKind>>);

/// How a non-page document was made. Chrome's `baseURI` and `readyState` differ by
/// this, not by HTML-vs-XML: measured 2026-10-04, createHTMLDocument is `readyState` "loading" and base
/// `about:blank`; createDocument and `parseHTMLUnsafe` are "complete" + `about:blank`; a DOMParser result
/// is "complete" and keeps the CREATING document's URL as its base.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocOrigin {
    /// `DOMImplementation.createHTMLDocument` -- the one that reports `readyState` "loading".
    CreateHtml,
    /// `createDocument` / `Document.parseHTMLUnsafe` -- `about:blank` base, `readyState` "complete".
    CreatedAboutBlank,
    /// A `DOMParser.parseFromString` result -- keeps the creating document's URL as its base.
    Parsed,
}

/// Every non-page document's origin, by arena root ([`DocOrigin`]).
#[derive(Default)]
pub struct DocOrigins(pub(crate) std::cell::RefCell<std::collections::HashMap<u32, DocOrigin>>);

/// THE mint for a document that is not the page's: a DOMParser result, `createDocument`,
/// `createHTMLDocument`. It is [`mint_document`] -- the SAME function the runtime's
/// `INSTALL_DOCUMENT` mints `globalThis.document` with, so there is one way to make a Document -- plus
/// the two things such a document has that the page's does not: the interface prototype that names its
/// kind (`HTMLDocument`/`XMLDocument`, whose own prototypes chain to `Document.prototype`), and its
/// recorded content type.
///
/// Nothing is copied onto the instance. `dom/05-xml.js` shadowed thirty-five members with own
/// properties because a facade document was a plain object; every one of them is a table row with a
/// Rust body now, and each reads THIS document's root through [`doc_root_slot`], so the instance
/// carries exactly what the page document carries: `location`, and nothing else.
pub(crate) fn mint_document_for<'s>(scope: &mut v8::PinScope<'s, '_>, root: u32, kind: DocKind, origin: DocOrigin) -> v8::Local<'s, v8::Object> {
    let iface = match kind {
        DocKind::Html => "HTMLDocument",
        DocKind::Xml(_) => "XMLDocument",
    };
    mint_document_as(scope, root, iface, kind, origin)
}

/// [`mint_document_for`] wearing `iface` -- for `new Document()`, which is an XML-flavoured document
/// (`application/xml`) whose interface is plain `Document`.
pub(crate) fn mint_document_as<'s>(scope: &mut v8::PinScope<'s, '_>, root: u32, iface: &str, kind: DocKind, origin: DocOrigin) -> v8::Local<'s, v8::Object> {
    // the REALM's interface, from its template (measured: F-engine-mints-realm-protos):
    // it was a `Document` re-parented onto `window[iface].prototype`, a name the page can write
    let doc = mint_document(scope, root, iface);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<DocKinds>().is_none() {
            st.put(DocKinds::default());
        }
    }
    op_state.borrow().borrow::<DocKinds>().0.borrow_mut().insert(root, kind);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<DocOrigins>().is_none() {
            st.put(DocOrigins::default());
        }
    }
    op_state.borrow().borrow::<DocOrigins>().0.borrow_mut().insert(root, origin);
    doc
}

/// The kind recorded for the receiver's root, if it is a created/parsed document.
fn recorded_kind(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<DocKind> {
    let slot = doc_root_slot(scope, this);
    if slot == PAGE_DOC_ROOT {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);

    let k = op_state.borrow().try_borrow::<DocKinds>().and_then(|m| m.0.borrow().get(&slot).cloned());
    k
}

/// The [`DocOrigin`] recorded for arena root `root` (a created/parsed document's), if any.
pub(crate) fn root_origin(scope: &mut v8::PinScope, root: u32) -> Option<DocOrigin> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let o = op_state.borrow().try_borrow::<DocOrigins>().and_then(|m| m.0.borrow().get(&root).copied());
    o
}

/// Does node `id` belong to a created document whose base URL is `about:blank` (createHTMLDocument /
/// createDocument / parseHTMLUnsafe -- NOT a DOMParser result, which keeps the creating URL)?
/// `node_base_url` answered the page URL for these.
pub(crate) fn node_in_about_blank_doc(scope: &mut v8::PinScope, id: u32) -> bool {
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if dom.is_connected(id) {
            return false; // in the page tree
        }
        dom.shadow_including_root(id)
    };
    matches!(root_origin(scope, root), Some(DocOrigin::CreateHtml | DocOrigin::CreatedAboutBlank))
}

/// Record `id` as owned by the receiver document -- the arena is SHARED with the page, so without the
/// override every node a created document makes would report the PAGE document as its `ownerDocument`
/// (WPT DOMImplementation-createDocument "ownerDocument"). A no-op for the page's own document, whose
/// documents are every node's default and need no record.
pub(crate) fn own_node(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, id: u32) {
    if doc_root_slot(scope, this) == PAGE_DOC_ROOT {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let g = v8::Global::new(scope, this);
    let st = op_state.borrow();
    st.borrow::<crate::binding::OwnerMap>().0.borrow_mut().insert(id, g);
}

/// `location` as the OWN accessor every document carries, `[LegacyUnforgeable]` as Chrome has it:
/// `{ get, set, enumerable: true, configurable: false }`. Installed exactly once per document, by
/// [`mint_document`], and never redefined -- `cb_doc_location` already answers the page's Location,
/// a nested browsing context's own (core/html/browsing_context.rs) and `null` for a document with no
/// browsing context, so there is nothing for a later layer to correct.
///
/// ONE PAIR PER REALM, from its template: every document of a realm carries the same getter and setter,
/// and a frame's documents their realm's -- Chrome 150 (B25, F-document-location-pair). A pair built per
/// document was also a page-visible function no template made.
pub(crate) fn install_own_location(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) {
    let g = crate::binding::protos::template_fn(scope, cb_doc_location, "get location", 0, None);
    let s = crate::binding::protos::template_fn(scope, cb_doc_location_set, "set location", 1, None);
    let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), s.into());
    d.set_enumerable(true);
    d.set_configurable(false);
    let k = v8::String::new(scope, "location").unwrap();
    obj.define_property(scope, k.into(), &d);
}

/// `startViewTransition()` on a document with no browsing context answers `null` (Blink's
/// `Document::startViewTransition` returns early without a window): `new Document()`, a
/// `createHTMLDocument` result. Chrome 150, the Chrome oracle 2026-10-08; it started a transition.
fn cb_doc_start_view_transition_guarded<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if recorded_kind(scope, args.this().into()).is_some() {
        rv.set_null();
        return;
    }
    crate::core::css::view_transition::cb_doc_start_view_transition(scope, args, rv)
}

/// `hasPrivateToken(issuer)` / `hasRedemptionRecord(issuer)` -- Blink's `Document::hasPrivateToken`
/// steps in order: the issuer must be an HTTP(S), potentially trustworthy origin (`TypeError`); the
/// document must have a top-frame origin (`InvalidStateError` -- a created document has none); that
/// origin must be HTTP(S) (`NotAllowedError`); then the browser is asked whether this profile holds a
/// token / a redemption record from the issuer, and a profile this engine runs holds none: `false`,
/// stock Chrome 150's answer in a fresh profile (measured 2026-10-08). The bodies were stubs that
/// resolved `undefined` for everything.
fn cb_doc_private_token_query<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let reject = |scope: &mut v8::PinScope<'s, '_>, rv: &mut v8::ReturnValue, e: v8::Local<'s, v8::Value>| {
        let p = crate::binding::protos::rejected_promise(scope, e);
        rv.set(p);
    };
    let issuer = args.get(0).to_rust_string_lossy(scope);
    let trustworthy_http = url::Url::parse(&issuer).ok().is_some_and(|u| match u.scheme() {
        "https" => true,
        "http" => match u.host() {
            Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
            Some(url::Host::Ipv4(a)) => a.is_loopback(),
            Some(url::Host::Ipv6(a)) => a.is_loopback(),
            None => false,
        },
        _ => false,
    });
    if !trustworthy_http {
        // Blink's context applied after the error is made
        let e = deno_core::error::native_error(scope, "TypeError", &format!("Failed to execute '{name}' on 'Document': {name}: Private Token issuer origins must be both HTTP(S) and secure (\"potentially trustworthy\")."));
        return reject(scope, &mut rv, e);
    }
    if recorded_kind(scope, args.this().into()).is_some() {
        let e = make_dom_exc(scope, &format!("Failed to execute '{name}' on 'Document': {name}: Cannot execute in documents lacking top-frame origins."), "InvalidStateError")
            .unwrap_or_else(|| v8::undefined(scope).into());
        return reject(scope, &mut rv, e);
    }
    let top = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<CookieUrl>().map(|c| c.0.clone()).unwrap_or_default()
    };
    if !(top.starts_with("https:") || top.starts_with("http:")) {
        let e = make_dom_exc(scope, &format!("Failed to execute '{name}' on 'Document': {name}: Cannot execute in documents without secure, HTTP(S), top-frame origins."), "NotAllowedError")
            .unwrap_or_else(|| v8::undefined(scope).into());
        return reject(scope, &mut rv, e);
    }
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    let p = crate::binding::protos::resolved_promise(scope, f);
    rv.set(p);
}

/// `new Document()`: an empty XML document with its own arena root (no doctype, no document element,
/// no browsing context, `application/xml`). Not callable without `new`; `class X extends Document`
/// gets its subclass prototype the way every WebIDL constructor does.
pub(crate) fn cb_document_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("Document"));
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_xml_document();
    // Recorded like every created document (its URL is `about:blank`, its content type
    // `application/xml`, its elements null-namespaced) -- it answered as the PAGE: `new Document().URL`
    // was the page's URL and `createElement("x")` an HTMLUnknownElement (Chrome 150: `about:blank`,
    // `[object Element]`; the Chrome oracle, 2026-10-08).
    let obj = mint_document_as(scope, root, "Document", DocKind::Xml("application/xml".to_string()), DocOrigin::CreatedAboutBlank);
    if let Some(p) = args.this().get_prototype(scope) {
        obj.set_prototype(scope, p);
    }
    rv.set(obj.into());
}

// ---- the table ---------------------------------------------------------------------------------

/// The `Document` rows the kits do not express.
fn coll<'s>(scope: &mut v8::PinScope<'s, '_>, kind: Kind) -> v8::Local<'s, v8::Value> {
    builder_data(scope, "HTMLCollection", kind)
}

pub fn install_document(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "implementation" => row_getter(scope, proto, member, cb_doc_implementation),
        // XPath: the bodies are `core/xml/xpath.rs`'s, shared with `XPathEvaluator`.
        "createExpression" => row_method(scope, proto, member, crate::core::xml::xpath::cb_doc_create_expression),
        "createNSResolver" => row_method(scope, proto, member, crate::core::xml::xpath::cb_create_ns_resolver),
        "evaluate" => row_method(scope, proto, member, crate::core::xml::xpath::cb_doc_evaluate),
        "URL" | "documentURI" => row_getter(scope, proto, member, cb_doc_url),
        "compatMode" => row_getter(scope, proto, member, cb_doc_compat_mode),
        "characterSet" | "charset" | "inputEncoding" => row_getter(scope, proto, member, cb_doc_charset),
        "contentType" => row_getter(scope, proto, member, cb_doc_content_type),
        "doctype" => row_getter(scope, proto, member, cb_doc_doctype),
        "documentElement" => row_getter(scope, proto, member, cb_doc_document_element),
        "xmlEncoding" => row_getter(scope, proto, member, cb_doc_null),
        "xmlVersion" => row_accessor(scope, proto, member, cb_doc_xml_version_get, cb_doc_xml_version_set),
        "xmlStandalone" => row_accessor(scope, proto, member, cb_doc_xml_standalone_get, cb_doc_xml_standalone_set),
        "domain" => row_accessor(scope, proto, member, cb_doc_domain_get, cb_doc_domain_set),
        "referrer" => row_getter(scope, proto, member, cb_doc_referrer),
        "cookie" => row_accessor(scope, proto, member, cb_doc_cookie_get, cb_doc_cookie_set),
        "lastModified" => row_getter(scope, proto, member, cb_doc_last_modified),
        "readyState" => row_getter(scope, proto, member, cb_doc_ready_state),
        "title" => row_accessor(scope, proto, member, cb_doc_title_get, cb_doc_title_set),
        "dir" => row_accessor(scope, proto, member, cb_doc_dir_get, cb_doc_dir_set),
        "body" => row_accessor(scope, proto, member, cb_doc_body, cb_doc_body_set),
        "head" => row_getter(scope, proto, member, cb_doc_head),
        "images" => { let d = coll(scope, Kind::Images); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        "embeds" | "plugins" => { let d = coll(scope, Kind::Embeds); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        "links" => { let d = coll(scope, Kind::Links); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        "forms" => { let d = coll(scope, Kind::Forms); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        "scripts" => { let d = coll(scope, Kind::Scripts); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        "anchors" => { let d = coll(scope, Kind::Anchors); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        // `document.all` -- the undetectable `HTMLAllCollection` (core/dom/collections.rs).
        "all" => row_getter(scope, proto, member, crate::core::dom::collections::cb_doc_all),
        "applets" => { let d = coll(scope, Kind::Applets); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        "children" => { let d = coll(scope, Kind::Children); row_getter_data(scope, proto, member, cb_doc_collection, d) }
        "currentScript" => row_getter(scope, proto, member, cb_doc_current_script),
        "defaultView" => row_getter(scope, proto, member, cb_doc_default_view),
        // The legacy editing switch, "off" forever: the setter is the same native no-op as before.
        "designMode" => row_accessor(scope, proto, member, cb_doc_design_mode, cb_noop),
        "fgColor" => { let d = str_val(scope, "text"); row_accessor_data(scope, proto, member, cb_doc_body_attr_get, d, cb_doc_body_attr_set, d) }
        "linkColor" => { let d = str_val(scope, "link"); row_accessor_data(scope, proto, member, cb_doc_body_attr_get, d, cb_doc_body_attr_set, d) }
        "vlinkColor" => { let d = str_val(scope, "vlink"); row_accessor_data(scope, proto, member, cb_doc_body_attr_get, d, cb_doc_body_attr_set, d) }
        "alinkColor" => { let d = str_val(scope, "alink"); row_accessor_data(scope, proto, member, cb_doc_body_attr_get, d, cb_doc_body_attr_set, d) }
        "bgColor" => { let d = str_val(scope, "bgcolor"); row_accessor_data(scope, proto, member, cb_doc_body_attr_get, d, cb_doc_body_attr_set, d) }
        "scrollingElement" => row_getter(scope, proto, member, cb_doc_scrolling_element),
        "hidden" | "webkitHidden" => row_getter(scope, proto, member, cb_doc_hidden),
        "visibilityState" | "webkitVisibilityState" => row_getter(scope, proto, member, cb_doc_visibility_state),
        "wasDiscarded" | "prerendering" | "webkitIsFullScreen" => row_getter(scope, proto, member, cb_doc_false),
        "webkitFullscreenEnabled" => row_getter(scope, proto, member, cb_doc_fullscreen_enabled),
        "pictureInPictureEnabled" => row_getter(scope, proto, member, cb_doc_pip_enabled),
        "timeline" => row_getter(scope, proto, member, cb_document_timeline_get),
        // Was a `stub` row answering `null`, which made
        // `gtag`'s `featurePolicy.allowedFeatures().includes("attribution-reporting")`
        // throw and dropped `are=1` from every GA4 hit on 28 replay rows.
        "featurePolicy" => row_getter(scope, proto, member, cb_document_feature_policy),
        // The deprecated fullscreen attributes carry Blink's lenient setter (the capture records
        // `set: true`), so assigning to them is silently ignored instead of throwing in strict mode.
        "fullscreen" => row_accessor(scope, proto, member, cb_doc_false, cb_noop),
        "fullscreenEnabled" => row_accessor(scope, proto, member, cb_doc_fullscreen_enabled, cb_noop),
        "fullscreenElement" => row_accessor(scope, proto, member, cb_doc_null, cb_noop),
        "webkitCurrentFullScreenElement" | "webkitFullscreenElement" | "rootElement" | "pointerLockElement" | "pictureInPictureElement" | "activeViewTransition" => row_getter(scope, proto, member, cb_doc_null),
        "firstElementChild" => row_getter(scope, proto, member, cb_doc_first_element_child),
        "lastElementChild" => row_getter(scope, proto, member, cb_doc_last_element_child),
        "childElementCount" => row_getter(scope, proto, member, cb_doc_child_element_count),
        "activeElement" => row_getter(scope, proto, member, cb_doc_active_element),
        "styleSheets" => row_getter(scope, proto, member, cb_style_sheets),
        // `document.fonts` is the ONE row of this table whose body lives in another lane
        // (`core/fonts/font_face.rs`), for the reason `elementFromPoint` above is the same shape:
        // minting a `FontFaceSet` means minting its state, and that state is that file's. The ROW is
        // here, which is what changed -- it was a `stub` row that `install_window_media_rtc`
        // defined over afterwards, i.e. a second writer the install ledger scored `clobber:engine`.
        "fonts" => row_getter(scope, proto, member, crate::core::fonts::font_face::cb_document_fonts),
        // Hit testing. The bodies live beside the layout cache they read
        // (`core/dom/geometry_queries.rs`); they were two `KB.__*` bridge slots inside the kernel's
        // `BOOTSTRAP` string until this unit.
        "elementFromPoint" => row_method(scope, proto, member, crate::core::dom::geometry_queries::cb_doc_element_from_point),
        "elementsFromPoint" => row_method(scope, proto, member, crate::core::dom::geometry_queries::cb_doc_elements_from_point),
        "adoptedStyleSheets" => row_accessor(scope, proto, member, cb_adopted_style_sheets_get, cb_adopted_style_sheets_set),
        "customElementRegistry" => row_getter(scope, proto, member, cb_doc_custom_element_registry),
        "fragmentDirective" => row_getter(scope, proto, member, cb_doc_fragment_directive),
        "adoptNode" => row_method(scope, proto, member, cb_doc_adopt_node),
        "append" => row_method(scope, proto, member, cb_doc_append),
        "prepend" => row_method(scope, proto, member, cb_doc_prepend),
        "replaceChildren" => row_method(scope, proto, member, cb_doc_replace_children),
        "moveBefore" => row_method(scope, proto, member, cb_doc_move_before),
        // The genuine no-ops: `clear`, `captureEvents`, `releaseEvents` are specified as doing nothing
        // at all; `exitPointerLock` is a no-op when nothing is pointer-locked, and nothing ever is here.
        // `webkitCancelFullScreen`/`webkitExitFullscreen`: nothing is ever fullscreen.
        "captureEvents" | "releaseEvents" | "clear" | "exitPointerLock" | "webkitCancelFullScreen" | "webkitExitFullscreen" => row_method(scope, proto, member, cb_noop),
        "caretPositionFromPoint" => row_method(scope, proto, member, crate::core::dom::geometry_queries::cb_doc_caret_position_from_point),
        "open" | "close" | "write" | "writeln" => { let d = str_val(scope, member.name); row_method_data(scope, proto, member, cb_doc_write_family, d) }
        "createCDATASection" => row_method(scope, proto, member, cb_doc_create_cdata),
        "createComment" => row_method(scope, proto, member, cb_doc_create_comment),
        "createAttribute" => row_method(scope, proto, member, cb_doc_create_attribute),
        "createAttributeNS" => row_method(scope, proto, member, cb_doc_create_attribute_ns),
        "createDocumentFragment" => row_method(scope, proto, member, cb_doc_create_fragment),
        "createElement" => row_method(scope, proto, member, cb_doc_create_element),
        "createElementNS" => row_method(scope, proto, member, cb_doc_create_element_ns),
        "createEvent" => row_method(scope, proto, member, cb_doc_create_event),
        "createNodeIterator" => row_method(scope, proto, member, cb_doc_create_node_iterator),
        "createProcessingInstruction" => row_method(scope, proto, member, cb_doc_create_pi),
        "createRange" => row_method(scope, proto, member, cb_doc_create_range),
        "createTextNode" => row_method(scope, proto, member, cb_doc_create_text_node),
        "createTreeWalker" => row_method(scope, proto, member, cb_doc_create_tree_walker),
        "execCommand" => row_method(scope, proto, member, crate::core::editing::commands::cb_exec_command),
        "queryCommandEnabled" => row_method(scope, proto, member, crate::core::editing::commands::cb_query_command_enabled),
        "queryCommandIndeterm" => row_method(scope, proto, member, crate::core::editing::commands::cb_query_command_indeterm),
        "queryCommandState" => row_method(scope, proto, member, crate::core::editing::commands::cb_query_command_state),
        "queryCommandSupported" => row_method(scope, proto, member, crate::core::editing::commands::cb_query_command_supported),
        "queryCommandValue" => row_method(scope, proto, member, crate::core::editing::commands::cb_query_command_value),
        "exitFullscreen" => row_method(scope, proto, member, cb_doc_exit_fullscreen),
        "exitPictureInPicture" => row_method(scope, proto, member, cb_doc_exit_pip),
        "getAnimations" => row_method(scope, proto, member, cb_document_get_animations),
        "getElementById" => row_method(scope, proto, member, cb_doc_get_element_by_id),
        "getElementsByClassName" => { let d = coll(scope, Kind::DocByClass); row_method_data(scope, proto, member, cb_doc_by_name, d) }
        "getElementsByTagName" => { let d = coll(scope, Kind::DocByTag); row_method_data(scope, proto, member, cb_doc_by_name, d) }
        "getElementsByTagNameNS" => { let d = coll(scope, Kind::DocByTagNs); row_method_data(scope, proto, member, cb_by_tag_ns, d) }
        "getElementsByName" => row_method(scope, proto, member, cb_doc_get_elements_by_name),
        "getSelection" => row_method(scope, proto, member, cb_doc_get_selection),
        "hasFocus" => row_method(scope, proto, member, cb_doc_has_focus),
        "hasStorageAccess" | "hasUnpartitionedCookieAccess" => row_method(scope, proto, member, cb_doc_has_storage_access),
        "importNode" => row_method(scope, proto, member, cb_doc_import_node),
        "querySelector" => row_method(scope, proto, member, cb_doc_query_selector),
        "querySelectorAll" => row_method(scope, proto, member, cb_doc_query_selector_all),
        "requestStorageAccess" => row_method(scope, proto, member, cb_doc_request_storage_access),
        "startViewTransition" => row_method(scope, proto, member, cb_doc_start_view_transition_guarded),
        "hasPrivateToken" | "hasRedemptionRecord" => {
            let d = str_val(scope, member.name);
            row_method_data(scope, proto, member, cb_doc_private_token_query, d)
        }
        "ariaNotify" => row_method(scope, proto, member, cb_doc_aria_notify),
        "parseHTMLUnsafe" | "parseHTML" => row_method(scope, proto, member, cb_doc_parse_html),
        other => debug_assert!(false, "install_document: unexpected member {other}"),
    }
}

// The capture's 249 rows and two statics, verbatim and in its order.
webapi::interface! {
    "Document" : "Node" [interface, window] {
        ctor "Document" len 0 = stub,
        attr "implementation" : Object readonly = install_document,
        attr "URL" : Object readonly = install_document,
        attr "documentURI" : Object readonly = install_document,
        attr "compatMode" : Object readonly = install_document,
        attr "characterSet" : Object readonly = install_document,
        attr "charset" : Object readonly = install_document,
        attr "inputEncoding" : Object readonly = install_document,
        attr "contentType" : Object readonly = install_document,
        attr "doctype" : Object readonly nullable = install_document,
        attr "documentElement" : Object readonly nullable = install_document,
        attr "xmlEncoding" : Object readonly = install_document,
        attr "xmlVersion" : Object = install_document,
        attr "xmlStandalone" : Object = install_document,
        attr "domain" : Object = install_document,
        attr "referrer" : Object readonly = install_document,
        attr "cookie" : Object = install_document,
        attr "lastModified" : Object readonly = install_document,
        attr "readyState" : Object readonly = install_document,
        attr "title" : Object = install_document,
        attr "dir" : Object = install_document,
        attr "body" : Object nullable = install_document,
        attr "head" : Object readonly nullable = install_document,
        attr "images" : Object readonly = install_document,
        attr "embeds" : Object readonly = install_document,
        attr "plugins" : Object readonly = install_document,
        attr "links" : Object readonly = install_document,
        attr "forms" : Object readonly = install_document,
        attr "scripts" : Object readonly = install_document,
        attr "currentScript" : Object readonly nullable = install_document,
        attr "defaultView" : Object readonly nullable = install_document,
        attr "designMode" : Object = install_document,
        handler "onreadystatechange" lenient = event_handler,
        attr "anchors" : Object readonly = install_document,
        attr "applets" : Object readonly = install_document,
        attr "fgColor" : Object = install_document,
        attr "linkColor" : Object = install_document,
        attr "vlinkColor" : Object = install_document,
        attr "alinkColor" : Object = install_document,
        attr "bgColor" : Object = install_document,
        attr "all" : Object readonly = install_document,
        attr "scrollingElement" : Object readonly nullable = install_document,
        handler "onpointerlockchange" = event_handler,
        handler "onpointerlockerror" = event_handler,
        attr "hidden" : Object readonly = install_document,
        attr "visibilityState" : Object readonly = install_document,
        attr "wasDiscarded" : Object readonly = install_document,
        attr "prerendering" : Object readonly = install_document,
        attr "featurePolicy" : Object readonly = install_document,
        attr "webkitVisibilityState" : Object readonly = install_document,
        attr "webkitHidden" : Object readonly = install_document,
        handler "onbeforecopy" = event_handler,
        handler "onbeforecut" = event_handler,
        handler "onbeforepaste" = event_handler,
        handler "onfreeze" = event_handler,
        handler "onprerenderingchange" = event_handler,
        handler "onresume" = event_handler,
        handler "onsearch" = event_handler,
        handler "onvisibilitychange" = event_handler,
        attr "timeline" : Object readonly = install_document,
        attr "fullscreenEnabled" : Object = install_document,
        attr "fullscreen" : Object = install_document,
        handler "onfullscreenchange" = event_handler,
        handler "onfullscreenerror" = event_handler,
        attr "webkitIsFullScreen" : Object readonly = install_document,
        attr "webkitCurrentFullScreenElement" : Object readonly = install_document,
        attr "webkitFullscreenEnabled" : Object readonly = install_document,
        attr "webkitFullscreenElement" : Object readonly = install_document,
        handler "onwebkitfullscreenchange" = event_handler,
        handler "onwebkitfullscreenerror" = event_handler,
        attr "rootElement" : Object readonly nullable = install_document,
        attr "pictureInPictureEnabled" : Object readonly = install_document,
        handler "onabort" = event_handler,
        handler "onbeforeinput" = event_handler,
        handler "onbeforematch" = event_handler,
        handler "onbeforetoggle" = event_handler,
        handler "onblur" = event_handler,
        handler "oncancel" = event_handler,
        handler "oncanplay" = event_handler,
        handler "oncanplaythrough" = event_handler,
        handler "onchange" = event_handler,
        handler "onclick" = event_handler,
        handler "onclose" = event_handler,
        handler "oncommand" = event_handler,
        handler "oncontentvisibilityautostatechange" = event_handler,
        handler "oncontextlost" = event_handler,
        handler "oncontextmenu" = event_handler,
        handler "oncontextrestored" = event_handler,
        handler "oncuechange" = event_handler,
        handler "ondblclick" = event_handler,
        handler "ondrag" = event_handler,
        handler "ondragend" = event_handler,
        handler "ondragenter" = event_handler,
        handler "ondragleave" = event_handler,
        handler "ondragover" = event_handler,
        handler "ondragstart" = event_handler,
        handler "ondrop" = event_handler,
        handler "ondurationchange" = event_handler,
        handler "onemptied" = event_handler,
        handler "onended" = event_handler,
        handler "onerror" = event_handler,
        handler "onfocus" = event_handler,
        handler "onformdata" = event_handler,
        handler "oninput" = event_handler,
        handler "oninvalid" = event_handler,
        handler "onkeydown" = event_handler,
        handler "onkeypress" = event_handler,
        handler "onkeyup" = event_handler,
        handler "onload" = event_handler,
        handler "onloadeddata" = event_handler,
        handler "onloadedmetadata" = event_handler,
        handler "onloadstart" = event_handler,
        handler "onmousedown" = event_handler,
        handler "onmouseenter" lenient = event_handler,
        handler "onmouseleave" lenient = event_handler,
        handler "onmousemove" = event_handler,
        handler "onmouseout" = event_handler,
        handler "onmouseover" = event_handler,
        handler "onmouseup" = event_handler,
        handler "onmousewheel" = event_handler,
        handler "onpause" = event_handler,
        handler "onplay" = event_handler,
        handler "onplaying" = event_handler,
        handler "onprogress" = event_handler,
        handler "onratechange" = event_handler,
        handler "onreset" = event_handler,
        handler "onresize" = event_handler,
        handler "onscroll" = event_handler,
        handler "onscrollend" = event_handler,
        handler "onsecuritypolicyviolation" = event_handler,
        handler "onseeked" = event_handler,
        handler "onseeking" = event_handler,
        handler "onselect" = event_handler,
        handler "onslotchange" = event_handler,
        handler "onstalled" = event_handler,
        handler "onsubmit" = event_handler,
        handler "onsuspend" = event_handler,
        handler "ontimeupdate" = event_handler,
        handler "ontoggle" = event_handler,
        handler "onvolumechange" = event_handler,
        handler "onwaiting" = event_handler,
        handler "onwebkitanimationend" = event_handler,
        handler "onwebkitanimationiteration" = event_handler,
        handler "onwebkitanimationstart" = event_handler,
        handler "onwebkittransitionend" = event_handler,
        handler "onwheel" = event_handler,
        handler "onauxclick" = event_handler,
        handler "ongotpointercapture" = event_handler,
        handler "onlostpointercapture" = event_handler,
        handler "onpointerdown" = event_handler,
        handler "onpointermove" = event_handler,
        handler "onpointerup" = event_handler,
        handler "onpointercancel" = event_handler,
        handler "onpointerover" = event_handler,
        handler "onpointerout" = event_handler,
        handler "onpointerenter" = event_handler,
        handler "onpointerleave" = event_handler,
        handler "onselectstart" = event_handler,
        handler "onselectionchange" = event_handler,
        handler "onanimationcancel" = event_handler,
        handler "onanimationend" = event_handler,
        handler "onanimationiteration" = event_handler,
        handler "onanimationstart" = event_handler,
        handler "ontransitionrun" = event_handler,
        handler "ontransitionstart" = event_handler,
        handler "ontransitionend" = event_handler,
        handler "ontransitioncancel" = event_handler,
        handler "onbeforexrselect" = event_handler,
        handler "oncopy" = event_handler,
        handler "oncut" = event_handler,
        handler "onpaste" = event_handler,
        attr "children" : Object readonly = install_document,
        attr "firstElementChild" : Object readonly nullable = install_document,
        attr "lastElementChild" : Object readonly nullable = install_document,
        attr "childElementCount" : Object readonly = install_document,
        attr "activeElement" : Object readonly nullable = install_document,
        attr "styleSheets" : Object readonly = install_document,
        attr "pointerLockElement" : Object readonly nullable = install_document,
        attr "fullscreenElement" : Object nullable = install_document,
        attr "adoptedStyleSheets" : Object = install_document,
        attr "pictureInPictureElement" : Object readonly nullable = install_document,
        attr "fonts" : Object readonly = install_document,
        method "adoptNode" : Object len 1 required 1 = install_document,
        method "append" : Object len 0 = install_document,
        method "captureEvents" : Object len 0 = install_document,
        method "caretPositionFromPoint" : Object len 2 required 2 nullable = install_document,
        method "caretRangeFromPoint" : Object len 0 = stub,
        method "clear" : Object len 0 = install_document,
        method "close" : Object len 0 = install_document,
        method "createAttribute" : Object len 1 required 1 = install_document,
        method "createAttributeNS" : Object len 2 required 2 = install_document,
        method "createCDATASection" : Object len 1 required 1 = install_document,
        method "createComment" : Object len 1 required 1 = install_document,
        method "createDocumentFragment" : Object len 0 = install_document,
        method "createElement" : Object len 1 required 1 = install_document,
        method "createElementNS" : Object len 2 required 2 = install_document,
        method "createEvent" : Object len 1 required 1 = install_document,
        method "createExpression" : Object len 1 required 1 = install_document,
        method "createNSResolver" : Object len 1 required 1 = install_document,
        method "createNodeIterator" : Object len 1 required 1 = install_document,
        method "createProcessingInstruction" : Object len 2 required 2 = install_document,
        method "createRange" : Object len 0 = install_document,
        method "createTextNode" : Object len 1 required 1 = install_document,
        method "createTreeWalker" : Object len 1 required 1 = install_document,
        method "elementFromPoint" : Object len 2 required 2 nullable = install_document,
        method "elementsFromPoint" : Object len 2 required 2 = install_document,
        method "evaluate" : Object len 2 required 2 = install_document,
        method "execCommand" : Object len 1 required 1 = install_document,
        method "exitFullscreen" : Promise len 0 = install_document,
        method "exitPictureInPicture" : Promise len 0 = install_document,
        method "exitPointerLock" : Object len 0 = install_document,
        method "getAnimations" : Object len 0 = install_document,
        method "getElementById" : Object len 1 required 1 nullable = install_document,
        method "getElementsByClassName" : Object len 1 required 1 = install_document,
        method "getElementsByName" : Object len 1 required 1 = install_document,
        method "getElementsByTagName" : Object len 1 required 1 = install_document,
        method "getElementsByTagNameNS" : Object len 2 required 2 = install_document,
        method "getSelection" : Object len 0 nullable = install_document,
        method "hasFocus" : Object len 0 = install_document,
        method "hasStorageAccess" : Promise len 0 = install_document,
        method "hasUnpartitionedCookieAccess" : Promise len 0 = install_document,
        method "importNode" : Object len 1 required 1 = install_document,
        method "moveBefore" : Object len 2 required 2 = install_document,
        method "open" : Object len 0 = install_document,
        method "prepend" : Object len 0 = install_document,
        method "queryCommandEnabled" : Object len 1 required 1 = install_document,
        method "queryCommandIndeterm" : Object len 1 required 1 = install_document,
        method "queryCommandState" : Object len 1 required 1 = install_document,
        method "queryCommandSupported" : Object len 1 required 1 = install_document,
        method "queryCommandValue" : Object len 1 required 1 = install_document,
        method "querySelector" : Object len 1 required 1 nullable = install_document,
        method "querySelectorAll" : Object len 1 required 1 = install_document,
        method "releaseEvents" : Object len 0 = install_document,
        method "replaceChildren" : Object len 0 = install_document,
        method "requestStorageAccess" : Promise len 0 = install_document,
        method "requestStorageAccessFor" : Promise len 1 = stub,
        method "startViewTransition" : Object len 0 = install_document,
        method "webkitCancelFullScreen" : Object len 0 = install_document,
        method "webkitExitFullscreen" : Object len 0 = install_document,
        method "write" : Object len 0 = install_document,
        method "writeln" : Object len 0 = install_document,
        attr "fragmentDirective" : Object readonly = install_document,
        handler "onpointerrawupdate" = event_handler,
        attr "activeViewTransition" : Object readonly nullable = install_document,
        handler "onscrollsnapchange" = event_handler,
        handler "onscrollsnapchanging" = event_handler,
        attr "customElementRegistry" : Object readonly nullable = install_document,
        method "ariaNotify" : Object len 1 required 1 = install_document,
        static_method "parseHTMLUnsafe" : Object len 1 required 1 = install_document,
        static_method "parseHTML" : Object len 1 required 1 = install_document,
        // THE OTHER DEVICE'S ROWS: Topics and Private State Tokens, which
        // `chrome-152-linux-20261016T1022` records on `Document` and `brave-151-linux-20260907T1006`
        // does not. Installed only when the active capture lists them (`install::installer::join`);
        // appended rather than interleaved because this table's order is the fallback a realm with no
        // bundle selector walks, not the order a page reads. D2 stubs -- the engine implements none
        // of the three.
        method "browsingTopics" : Promise len 0 = stub,
        method "hasPrivateToken" : Promise len 1 required 1 = install_document,
        method "hasRedemptionRecord" : Promise len 1 required 1 = install_document,
        method "@@unscopables" : Object len 0 hidden cfg "append,fullscreen,prepend,replaceChildren" = unscopables
    }
}

// ── the Document's named properties ────────────────────────────────────────────────
//
// HTML's supported names: the `name` of an `embed`, `form`, `iframe`, `img` or `object`; the `id` of an
// `object`; the `id` of an `img` that also has a `name`. One element is the answer (an iframe's is its
// window), several are an HTMLCollection in tree order -- Chrome 150, measured (`tools/realweb/seed/np`).
// The handler is NON-MASKING: it answers only names nothing else on the document answers. The
// [LegacyOverrideBuiltIns] case -- `<form name=forms>` shadowing `document.forms` -- is given up so that
// every ordinary member read does not walk the tree first.

fn doc_named_elements(dom: &korvus_dom::Dom, root: u32, name: &str) -> Vec<u32> {
    let root = resolve_doc_root(dom, root);
    // Every supported name is an element's `name` or `id`, so the candidates come from the arena's
    // two indexes: a miss is two hash lookups, where the walk over every element --
    // lowercasing each tag -- cost 72 µs at 5,000 elements. The filter is the walk's.
    let exposed = |id: u32| {
        if !dom.is_html_ns(id) || !dom.is_in_tree(root, id) { return false; }
        let Some(tag) = dom.tag_name(id) else { return false };
        let is = |t: &str| tag.eq_ignore_ascii_case(t);
        let by_name = dom.get_attribute(id, "name") == Some(name);
        if is("embed") || is("form") || is("iframe") {
            by_name
        } else if is("object") {
            by_name || dom.get_attribute(id, "id") == Some(name)
        } else if is("img") {
            by_name || (dom.get_attribute(id, "id") == Some(name) && dom.get_attribute(id, "name").is_some_and(|n| !n.is_empty()))
        } else {
            false
        }
    };
    let mut found: Vec<u32> = dom.elements_named(name).iter().chain(dom.elements_with_id(name)).copied().filter(|&id| exposed(id)).collect();
    found.sort_unstable();
    found.dedup();
    if found.len() > 1 {
        // Several are an HTMLCollection in TREE order, which NodeId order is not after a move: the
        // walk puts them in it (rare: a name shared by two exposed elements).
        let keep: std::collections::HashSet<u32> = found.into_iter().collect();
        return dom.descendant_elements(root).into_iter().filter(|id| keep.contains(id)).collect();
    }
    found
}

fn doc_named_root(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>) -> Option<u32> {
    deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, holder.into()).map(|d| unsafe { d.as_ref() }.root)
}

fn cb_doc_named_get<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    if !key.is_string() { return v8::Intercepted::kNo; }
    let name = key.to_rust_string_lossy(scope);
    if name.is_empty() { return v8::Intercepted::kNo; }
    let Some(root) = doc_named_root(scope, args.holder()) else { return v8::Intercepted::kNo };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ids = { let st = op_state.borrow(); doc_named_elements(&st.borrow::<HostDom>().0, root, &name) };
    match ids.len() {
        0 => v8::Intercepted::kNo,
        1 => {
            let id = ids[0];
            let el: v8::Local<v8::Value> = crate::binding::protos::wrap_cached(scope, &op_state, id).into();
            let is_iframe = { let st = op_state.borrow(); st.borrow::<HostDom>().0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("iframe")) };
            let v = if is_iframe {
                crate::binding::protos::pristine_get(scope, el, "HTMLIFrameElement.contentWindow").unwrap_or(el)
            } else { el };
            rv.set(v);
            v8::Intercepted::kYes
        }
        n => {
            let arr = v8::Array::new(scope, n as i32);
            for (i, id) in ids.into_iter().enumerate() {
                let w: v8::Local<v8::Value> = crate::binding::protos::wrap_cached(scope, &op_state, id).into();
                arr.set_index(scope, i as u32, w);
            }
            rv.set(crate::core::dom::collections::make_static(scope, "HTMLCollection", arr));
            v8::Intercepted::kYes
        }
    }
}

fn cb_doc_named_query<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    if !key.is_string() { return v8::Intercepted::kNo; }
    let name = key.to_rust_string_lossy(scope);
    let Some(root) = doc_named_root(scope, args.holder()) else { return v8::Intercepted::kNo };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let hit = !name.is_empty() && { let st = op_state.borrow(); !doc_named_elements(&st.borrow::<HostDom>().0, root, &name).is_empty() };
    if !hit { return v8::Intercepted::kNo; }
    rv.set(v8::Integer::new(scope, v8::PropertyAttribute::DONT_ENUM.as_u32() as i32));
    v8::Intercepted::kYes
}

pub(crate) fn doc_named_config<'s>() -> v8::NamedPropertyHandlerConfiguration<'s> {
    use deno_core::v8::MapFnTo;
    v8::NamedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(named_get, "Document", cb_doc_named_get).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(named_query, "Document", cb_doc_named_query).map_fn_to()))
        .flags(v8::PropertyHandlerFlags::NON_MASKING)
}
