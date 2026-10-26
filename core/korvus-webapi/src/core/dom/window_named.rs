//! The Window named properties object: `<input id="emailinput">` makes `emailinput` a variable.
//!
//! This is not a convenience. Without it a page script that names an element by its id throws
//! ReferenceError and takes the whole file with it -- 853 subtests in html/semantics/forms alone died
//! on `Unhandled rejection ... ReferenceError: emailinput is not defined`, none of them testing this.
//!
//! WHERE IT GOES. Not on `window`. HTML puts these on a separate object that sits BETWEEN
//! `Window.prototype` and `EventTarget.prototype`, which is why `Object.getOwnPropertyNames(window)`
//! does not list them in a real browser.
//!
//! WHAT IT DOES NOT SHADOW. An earlier version of this comment claimed that `<div id="addEventListener">`
//! shadows `window.addEventListener` because the object sits above `EventTarget.prototype`, and called
//! that "confirmed by our own recording". The recording confirmed only that `addEventListener` is not
//! in the captured `globals` -- true, and beside the point. Asked directly, Chrome 151 answers:
//!
//! ```text
//! <script id=addEventListener></script>
//! typeof window.addEventListener                                     -> "function"
//! window.addEventListener === EventTarget.prototype.addEventListener -> true
//! ```
//!
//! because WebIDL's named property visibility algorithm (§3.9.4.1, step 5) hides a named property
//! whenever any object FURTHER DOWN the prototype chain has an own property of that name. The named
//! properties object yields to `EventTarget.prototype` and `Object.prototype`; `window.x` for
//! `<div id=x>` still resolves, because nothing below carries an `x`. DOM clobbering is real, and it
//! works on names the chain does not already have -- which is exactly why a page's
//! `window.addEventListener('scroll', ...)` keeps working on a document that happens to contain an
//! element with that id. an agent-browser reference's events.html fixture does precisely that, and it was the one
//! failing case in that file.
//!
//! WHY NATIVE AND NOT A JS PROXY. The first version was a Proxy spliced into the chain. It was correct
//! and it was five times slower ACROSS THE BOARD: a Proxy anywhere in the global object's prototype
//! chain invalidates V8's fast path for every global load, not merely the ones that miss. A WPT Core
//! run went from 19 URLs/s to 3.4. An interceptor on an ObjectTemplate is what Chrome itself uses and
//! costs nothing until a lookup actually misses.

use deno_core::v8;
use deno_core::v8::MapFnTo;

use crate::binding::HostDom;
use crate::binding::protos::wrap_cached;

/// Elements whose `name` attribute reaches the window. HTML's named-access set -- `a` and `input` are
/// deliberately NOT in it, so `<a name="x">` does not define `window.x`. A named `iframe` is reached
/// through its window, which the frame layer answers.
fn name_accessible(tag: &str) -> bool {
    matches!(tag, "embed" | "form" | "img" | "object")
}

/// The Window's named objects for a name: IN THE DOCUMENT'S TREE, every element whose `id`
/// it is -- SVG, MathML and any other namespace too, as in Chrome 150 (WPT svg/types reads
/// `window.cap_test` off an SVG `<text>`) -- and every named-access element whose `name` it is, in tree
/// order. One is the answer,
/// several an HTMLCollection (Chrome 150, `tools/realweb/seed/winnamed`). It took the whole arena and
/// one element: a detached `<form name=x>` was `window.x`, and two were the first alone.
///
/// The candidates come from the arena's `id` and `name` indexes: a missing name -- every
/// `typeof undeclaredGlobal` reaches here -- is two hash lookups.
fn resolve(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, frame_root: Option<Option<u32>>, name: &str) -> Vec<u32> {
    if name.is_empty() {
        return Vec::new();
    }
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let root = match frame_root {
        Some(Some(r)) => r,
        Some(None) => return Vec::new(),
        None => dom.root(),
    };
    let by_id = dom.elements_with_id(name).iter().copied().filter(|&n| dom.node_type(n) == 1);
    let by_name = dom
        .elements_named(name)
        .iter()
        .copied()
        .filter(|&n| dom.tag_name(n).is_some_and(name_accessible) && dom.get_attribute(n, "name") == Some(name));
    let mut found: Vec<u32> = by_id.chain(by_name).filter(|&n| dom.is_in_tree(root, n)).collect();
    found.sort_unstable();
    found.dedup();
    // (A named iframe's ELEMENT was answered here when nothing else matched. Its WINDOW is the answer, and it is
    // asked first ([`frame_named`]); a frame is not one of the window's named elements by its `name`, so Blink's
    // `WindowNamedItems` never answers the element either.)
    if found.len() > 1 {
        // Tree order, which NodeId order is not after a move (rare: a name two elements share).
        let keep: std::collections::HashSet<u32> = found.into_iter().collect();
        return dom.descendant_elements(root).into_iter().filter(|n| keep.contains(n)).collect();
    }
    found
}

/// Whose document answers: the realm the named properties object belongs to. A frame realm's object
/// answers from its frame's document (it read the page's, so in a frame `window.ldiv` was
/// undefined, and since H20 `window.lf` too); the page's object from the page's.
fn realm_root(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>) -> Option<Option<u32>> {
    let ctx = holder.get_creation_context(scope)?;
    let g = ctx.global(scope);
    crate::core::html::browsing_context::realm_frame_document_root(scope, g)
}

/// No element anywhere has this id or name: the common miss (`typeof undeclaredGlobal`) answers from the
/// two indexes before any realm is looked up.
fn no_candidates(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, name: &str) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    name.is_empty() || (dom.elements_with_id(name).is_empty() && dom.elements_named(name).is_empty())
}

fn key_name(scope: &mut v8::PinScope, key: v8::Local<v8::Name>) -> Option<String> {
    let v: v8::Local<v8::Value> = key.into();
    if !v.is_string() {
        return None; // symbols are never supported property names
    }
    Some(v.to_rust_string_lossy(scope))
}

/// WebIDL §3.9.4.1 step 5: a supported name is NOT visible here if an object further down the chain
/// (`EventTarget.prototype`, `Object.prototype`) has an own property of that name. Walked from the
/// holder -- this object -- rather than assumed, so it stays right whatever is spliced beneath it.
///
/// Cost: this runs only on a lookup that has already missed `window` and `Window.prototype` AND
/// resolves to an element, and it is two `has_own_property` calls when it does.
fn shadowed_below<'s>(scope: &mut v8::PinScope<'s, '_>, holder: v8::Local<'s, v8::Object>, key: v8::Local<'s, v8::Name>) -> bool {
    let mut cur = holder.get_prototype(scope);
    while let Some(proto) = cur.and_then(|p| v8::Local::<v8::Object>::try_from(p).ok()) {
        if proto.has_own_property(scope, key).unwrap_or(false) {
            return true;
        }
        cur = proto.get_prototype(scope);
    }
    false
}

/// The child frame this name reaches, BEFORE any element (Blink `WindowProperties::AnonymousNamedGetter`: the
/// document-tree child navigables first, then the document's named items) -- `core/html/browsing_context.rs`'s
/// [`named_child_window`](crate::core::html::browsing_context::named_child_window). A frame's name was an own
/// property of the window once (a bot-detection service reads it).
fn frame_named<'s>(scope: &mut v8::PinScope<'s, '_>, holder: v8::Local<'s, v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    crate::core::html::browsing_context::named_child_window(scope, holder, name)
}

fn cb_get<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    let Some(name) = key_name(scope, key) else { return v8::Intercepted::kNo };
    named_lookup(scope, key, &name, args.holder(), &mut rv)
}

/// The named getter's body, for a string key and for an index ([`cb_index_get`]).
fn named_lookup<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, name: &str, holder: v8::Local<'s, v8::Object>, rv: &mut v8::ReturnValue) -> v8::Intercepted {
    if let Some(w) = frame_named(scope, holder, name) {
        if shadowed_below(scope, holder, key) {
            return v8::Intercepted::kNo;
        }
        rv.set(w.into());
        return v8::Intercepted::kYes;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if no_candidates(&op_state, name) {
        return v8::Intercepted::kNo;
    }
    let frame_root = realm_root(scope, holder);
    let ids = resolve(&op_state, frame_root, name);
    if ids.is_empty() || shadowed_below(scope, holder, key) {
        return v8::Intercepted::kNo;
    }
    if let [id] = ids[..] {
        let obj = wrap_cached(scope, &op_state, id);
        rv.set(obj.into());
        return v8::Intercepted::kYes;
    }
    let arr = v8::Array::new(scope, ids.len() as i32);
    for (i, id) in ids.into_iter().enumerate() {
        let w: v8::Local<v8::Value> = wrap_cached(scope, &op_state, id).into();
        arr.set_index(scope, i as u32, w);
    }
    rv.set(crate::core::dom::collections::make_static(scope, "HTMLCollection", arr));
    v8::Intercepted::kYes
}

/// The `query` interceptor answers `in` / HasProperty. Returning kNo lets the lookup continue up the
/// chain, which is exactly what must happen for every name that is not an element -- that is the whole
/// reason `EventTarget.prototype` is still reachable through this object.
fn cb_query<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let Some(name) = key_name(scope, key) else { return v8::Intercepted::kNo };
    named_supported(scope, key, &name, args.holder(), &mut rv)
}

/// The query's body, for a string key and for an index ([`cb_index_query`]).
fn named_supported<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, name: &str, holder: v8::Local<'s, v8::Object>, rv: &mut v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let supported = frame_named(scope, holder, name).is_some() || {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        if no_candidates(&op_state, name) {
            false
        } else {
            let frame_root = realm_root(scope, holder);
            !resolve(&op_state, frame_root, name).is_empty()
        }
    };
    if supported && !shadowed_below(scope, holder, key) {
        // Attributes: configurable + writable, NOT enumerable -- Window is [LegacyUnenumerableNamedProperties]
        // (WebIDL's named properties object [[GetOwnProperty]]: Chrome 150 answers `enumerable: false` for
        // `<div id=foo>`'s `foo`).
        let attrs = v8::Integer::new(scope, v8::PropertyAttribute::DONT_ENUM.as_u32() as i32);
        rv.set(attrs);
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

/// A fresh object carrying the interceptor. Only a native `ObjectTemplate` can carry one, which is
/// why this object has always been built here; what changed is that the SPLICE is
/// here too, so no JS ever holds it (it used to be handed out through `op_window_named_object` and
/// chained by `dom/09h-window-named.js`). The template is the builder's, made once per isolate
/// (`templates::object_template`); each realm's splice built one of its own until then.
fn make_named_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let tmpl = crate::install::templates::object_template(scope, "WindowProperties", configure_named_object);
    tmpl.new_instance(scope).unwrap()
}

/// An array-index key reaches V8's INDEXED interceptor, never the named one, so a supported name that spells an
/// index -- `<iframe name="123">`, `<div id="7">` -- needs this one: the same lookup under the index's string
/// (Blink's bindings forward an index to the named getter of an interface that has only that one). Chrome 150:
/// `window["123"]` is the frame named "123"; `window[i]` below the frame count is the
/// window's own property and never gets here.
fn cb_index_get<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    let name = index.to_string();
    let Some(key) = v8::String::new(scope, &name) else { return v8::Intercepted::kNo };
    named_lookup(scope, key.into(), &name, args.holder(), &mut rv)
}

fn cb_index_query<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let name = index.to_string();
    let Some(key) = v8::String::new(scope, &name) else { return v8::Intercepted::kNo };
    named_supported(scope, key.into(), &name, args.holder(), &mut rv)
}

/// The named properties object's template: the named getter and query over the window's named elements and
/// child frames, and the same two for an index.
///
/// TODO(1021-verify): F2 (goal 1021) also wants a plugin able to CLAIM a named-property interceptor so its
/// `on_call` sees the named lookup. The build_fn members are claimed through `templates::claim_untemplated`
/// (`cb_claimed`), but that seam is built on `plugin_call::V8Call`, which reads a `FunctionCallbackArguments`;
/// an interceptor body (`cb_get` / `named_lookup`) is handed a `PropertyCallbackArguments` instead, so a new
/// `CallCtx` adapter over the interceptor's args (exposing the looked-up `name` as `arg_string(0)` and the
/// result slot via `set_*`) is needed before `plugin_host::claims("WindowProperties.<named getter>")` can gate a
/// call into `on_call_pre`/`on_call` here. Left for the integration pass to design the interceptor-side CallCtx.
fn configure_named_object(tmpl: v8::Local<v8::ObjectTemplate>) {
    let config = v8::NamedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(named_get, "WindowProperties", cb_get).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(named_query, "WindowProperties", cb_query).map_fn_to()));
    tmpl.set_named_property_handler(config);
    let config = v8::IndexedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(idx_get, "WindowProperties", cb_index_get).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(idx_query, "WindowProperties", cb_index_query).map_fn_to()));
    tmpl.set_indexed_property_handler(config);
}

/// Splice the named properties object into the Window chain, LAST (this is all of
/// `dom/09h-window-named.js`, whose 42 lines and one `Object.defineProperty` are deleted with it).
///
/// The chain HTML specifies, and the one this produces:
///
/// ```text
/// window -> Window.prototype -> [named properties] -> EventTarget.prototype -> Object.prototype
/// ```
///
/// ORDER is the whole reason this was ever its own module: `generated_shells.js` rebuilds every
/// interface's prototype chain when it runs, so a splice done before it is silently undone. Running
/// at context creation AFTER the entire ESM layer -- which is where every other table install of this
/// lane runs -- is strictly later than the JS module's slot was, so the constraint is met by
/// construction rather than by an import position.
///
/// `Symbol.toStringTag` makes `Object.prototype.toString.call()` report `[object WindowProperties]`,
/// as Chrome's does. The descriptor is `{writable:false, enumerable:false, configurable:true}`, which
/// is what `Object.defineProperty(np, Symbol.toStringTag, { value, configurable: true })` produced.
///
/// Guarded so a second call cannot chain a second copy, and `EventTarget.prototype` is named
/// EXPLICITLY rather than read off whatever currently sits above `Window.prototype`: a stale
/// intermediate is then dropped rather than inherited, which is the five-links-where-Chrome-has-four
/// defect `tests/automation_tells.rs` caught.
pub fn splice_window_named<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::binding::protos::get_prop;
    let proto_of = |scope: &mut v8::PinScope<'s, '_>, name: &str| -> Option<v8::Local<'s, v8::Object>> {
        let k = v8::String::new(scope, name)?;
        let c = global.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
        get_prop(scope, c, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    };
    let (Some(wp), Some(etp)) = (proto_of(scope, "Window"), proto_of(scope, "EventTarget")) else { return };
    // Already spliced (a second context-creation pass): leave the chain alone.
    if wp.get_prototype(scope).map(|p| is_named_object(scope, p)).unwrap_or(false) {
        return;
    }
    let np = make_named_object(scope);
    let tag = v8::Symbol::get_to_string_tag(scope);
    let name = v8::String::new(scope, "WindowProperties").unwrap();
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(name.into(), false);
    pd.set_enumerable(false);
    pd.set_configurable(true);
    np.define_property(scope, tag.into(), &pd);
    np.set_prototype(scope, etp.into());
    wp.set_prototype(scope, np.into());
}

/// Is `v` a named properties object this module minted? Its `Symbol.toStringTag` is
/// "WindowProperties" and it owns nothing else -- the interceptor answers every name. Used only to
/// keep [`splice_window_named`] idempotent.
fn is_named_object(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let tag = v8::Symbol::get_to_string_tag(scope);
    o.get(scope, tag.into()).map(|t| t.to_rust_string_lossy(scope) == "WindowProperties").unwrap_or(false)
}
