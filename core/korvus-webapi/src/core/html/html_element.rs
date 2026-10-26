//! `core/html/html_element.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::core::html::custom_element_registry::{def_name, take_construct};
use crate::core::dom::element::{JsElement, el_id_of};
use crate::binding::protos::{arg_node_id, ctor_name, get_prop, int_val, set_bool, set_prop, str_val, throw_type_error, use_new_msg};
use crate::binding::host_dom::{HostDom, NodeCache, QNameMap};

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// ---- HTMLElement constructor (constructor-upgrade path; GS obstacle "custom-element"). ----
// Like build_fn, but ConstructorBehavior::Allow -- HTMLElement is the one native raw callback that must
// be `new`-able (so `class X extends HTMLElement` works). Real DOM constructors carry a `.prototype`
// (added here; dom.js overwrites it with JsElement.prototype), unlike the Throw "concise" methods.
// globalThis.HTMLElement's [[Construct]] -- the HTML spec "element constructor steps" for AUTONOMOUS
// custom elements (https://html.spec.whatwg.org/#html-element-constructors). `new MyWidget()` where
// `class MyWidget extends HTMLElement` reaches here via super() with new_target === MyWidget: we find the
// definition by constructor (KB.__customElements.__defName, an IDENTITY compare over the registry that
// never reads new_target.prototype), allocate a real arena element with the definition's local name, wrap
// it as a cppgc JsElement (so every native Element/Node op works on it), retype it to new_target.prototype
// -- reusing the prototype V8 already fetched onto the throwaway receiver (args.this()), so a counting
// Proxy observes exactly ONE `.prototype` get -- and intern it so appendChild/lastChild/querySelector
// return this same wrapper. `new HTMLElement()` (new_target === the interface object) and an unregistered
// constructor both throw TypeError, matching the spec's pre-.prototype sanity checks. Customized built-ins
// (`extends HTMLButtonElement`) never reach here: their super() hits the HTML*Element "Illegal constructor"
// shims (dom.js mk()), which already throw TypeError.
// Element.localName -- the (lowercased, for HTML) tag name. A real native getter so it holds on
// constructor-upgraded custom elements too; replaces the inert generated-shells backfill stub.

pub(crate) fn cb_html_element_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let new_target = args.new_target();
    // All three of this body's TypeErrors carry Chrome's construction-context prefix,
    // and the two shapes are DIFFERENT messages. Measured on `google-chrome --headless=new`
    // 150.0.7871.46 over loopback, 2026-09-12:
    //   HTMLElement()                                  -> Failed to construct 'HTMLElement': Please use the 'new' operator, this DOM object constructor cannot be called as a function.
    //   new HTMLElement()                              -> Failed to construct 'HTMLElement': Illegal constructor
    //   new (class extends HTMLElement {})()           -> Failed to construct 'HTMLElement': Illegal constructor   (unregistered)
    //   new (class extends HTMLButtonElement {})()     -> Failed to construct 'HTMLButtonElement': Illegal constructor
    // The name is the INTERFACE's, never `new.target`'s -- it comes from the `data` the interface
    // object was built with (`build_interface_ctor`), which is why a subclass still says
    // 'HTMLElement'.
    let name = ctor_name(scope, &args);
    // Called without `new` (new_target is undefined) -> not a construct call, but the interface DOES
    // declare a constructor, so this is WebIDL's "you forgot new", not "illegal constructor".
    if !new_target.is_object() { let m = use_new_msg(&name); throw_type_error(scope, &m); return; }
    let illegal = |name: &str| format!("Failed to construct '{name}': Illegal constructor");
    // NewTarget === the HTMLElement interface object itself -> TypeError (even if HTMLElement was define()d).
    // The active function object, as the realm's install left it -- not the global name, which a page may
    // have reassigned (every `super()` read `window.HTMLElement`).
    if let Some(he) = crate::binding::protos::iface_object(scope, "HTMLElement") {
        if new_target.strict_equals(he) { let m = illegal(&name); throw_type_error(scope, &m); return; }
    }
    // Definition lookup by constructor === NewTarget; none -> TypeError (spec: throw before any .prototype get).
    let local = match def_name(scope, new_target) {
        Some(n) => n,
        None => { let m = illegal(&name); throw_type_error(scope, &m); return; }
    };
    // UPGRADE PATH. When `customElements` upgrades an already-parsed element it runs the definition's
    // constructor ON THE EXISTING element (HTML spec "upgrade an element": invoke C with the element on
    // the construction stack), so `super()` here must RETURN that element rather than mint a new one --
    // that is what makes class field initializers (`renderOptions = {host:this}` on a LitElement) run on
    // the live node. `__takeConstruct` pops the element the upgrade pass pushed, or undefined for a plain
    // `new MyEl()` from script. It is consumed during super(), before the subclass body runs, so a nested
    // construction inside that body sees an empty stack.
    if let Some(pending) = take_construct(scope).map(|o| -> v8::Local<v8::Value> { o.into() }) {
        if let Ok(obj) = v8::Local::<v8::Object>::try_from(pending) {
            if let Some(proto) = args.this().get_prototype(scope) {
                obj.set_prototype(scope, proto);
            }
            rv.set(obj.into());
            return;
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = op_state.borrow_mut().borrow_mut::<HostDom>().0.create_element(&local);
    let wrapper = crate::binding::protos::make_wrapper(scope, JsElement { id });
    // Retype to new_target.prototype, already present on the throwaway receiver (no extra Proxy .prototype get).
    if let Some(proto) = args.this().get_prototype(scope) {
        wrapper.set_prototype(scope, proto);
    }
    op_state.borrow().borrow::<NodeCache>().insert(id, v8::Global::new(scope, &wrapper));
    // THIS ELEMENT'S CONSTRUCTOR IS RUNNING RIGHT NOW, so its upgrade has begun and must never begin
    // again. The mark is what tells the insertion walk's `upgrade_one` to leave it
    // alone; without it, `document.createElement("x-thing")` followed by `appendChild` ran the
    // definition's constructor TWICE on one element -- measured `ctor ctor-end ctor ctor-end`
    // against Chrome's single `ctor`. It is also what makes a `setAttribute` inside the constructor
    // body reach `attributeChangedCallback`, which Chrome does: `ctor attr:a:N>v ctor-end`.
    crate::core::html::custom_element_registry::mark_upgraded(scope, wrapper);
    rv.set(wrapper.into());
}

/// An element's LOCAL name — the one production, used by `Element.localName` and by
/// `Element.attachShadow`'s safelist (CONF ELEM1.5).
///
/// A recorded qualified name (createElementNS / DOMImplementation.createDocument / the XML parser)
/// answers with its parsed local name — "prefix::local" splits on the FIRST colon, so localName is
/// ":local"; an unprefixed createElementNS name is its own local name. Absent record = legacy
/// behavior: the tag (already lowercased for HTML at creation, case-preserved for raw-created XML
/// elements).
///
/// IT IS A FUNCTION BECAUSE IT HAS TWO CALLERS. `attachShadow` asked `Dom::tag_name` instead and so
/// read the QUALIFIED name: `<xhtml:div>` in an XHTML document is an HTML-namespace `div` whose
/// tag_name is `"xhtml:div"`, which is in no safelist, and the new refusal turned
/// `shadow-dom/host-with-namespace.xhtml` from 2/2 into 1/2. Re-stating the rule at the second call
/// site instead of sharing it is the defect this goal exists to remove.
pub(crate) fn local_name_of(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32) -> Option<String> {
    local_name_in(&op_state.borrow(), id)
}

/// [`local_name_of`] for a caller that is ALREADY inside the `OpState` borrow -- HTML's "body
/// element" / "head element" walks (`core/dom/document.rs`) resolve a local name per child while
/// holding `st`, and `op_state.borrow()` inside a `borrow_mut()` would panic. Same body, one
/// production: `local_name_of` is this function with the borrow taken for the caller.
pub(crate) fn local_name_in(st: &deno_core::OpState, id: u32) -> Option<String> {
    let q = st.borrow::<QNameMap>().0.borrow().get(&id).map(|(_, l)| l.clone());
    if q.is_some() { return q; }
    let dom = &st.borrow::<HostDom>().0;
    let name = dom.tag_name(id)?;
    // an XML document's parser keeps the qualified name: `<a:c>` is local name `c`, prefix `a` (Chrome 150;
    // an HTML document's `<a:c>` is local name `a:c`, no prefix)
    if let Some((_, local)) = xml_qualified_split(dom, id, name) {
        return Some(local.to_string());
    }
    Some(name.to_string())
}

/// `f` of element `id`'s local name -- what [`local_name_in`] answers -- read in place: a tag test per node (a
/// collection's filter, `document.body`'s walk) cloned a `String` for each node it looked at.
pub(crate) fn with_local_name<R>(st: &deno_core::OpState, id: u32, f: impl FnOnce(&str) -> R) -> Option<R> {
    if let Some((_, l)) = st.borrow::<QNameMap>().0.borrow().get(&id) {
        return Some(f(l));
    }
    let dom = &st.borrow::<HostDom>().0;
    let name = dom.tag_name(id)?;
    if !name.contains(':') {
        return Some(f(name));
    }
    Some(match xml_qualified_split(dom, id, name) {
        Some((_, local)) => f(local),
        None => f(name),
    })
}

/// `name` split at its colon when it is the qualified name an XML document's parser recorded for `id`.
pub(crate) fn xml_qualified_split<'n>(dom: &korvus_dom::Dom, id: u32, name: &'n str) -> Option<(&'n str, &'n str)> {
    let (p, l) = name.split_once(':')?;
    (!p.is_empty() && !l.is_empty() && dom.is_in_xml(id)).then_some((p, l))
}

pub(crate) fn cb_local_name<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let id = match el_id_of(scope, args.this()) { Some(i) => i, None => { rv.set_null(); return; } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match local_name_of(&op_state, id) { Some(n) => { let v = str_val(scope, &n); rv.set(v); }, None => rv.set_null() }
}

// `edit_value_on_selector` WAS HERE AND IS DELETED.
//
// It took a CSS SELECTOR, appended one character to `.value` and fired an `input` -- the whole of
// what `Engine::type_into` could do, and unreachable from CDP, whose `Input.*` carry no selector.
// It was also only half an edit: no `beforeinput` (so a page could not cancel it, and Chrome makes
// that the cancellation point), no selection (so it could only ever append), and an `InputEvent`
// that silently degraded to a plain `Event` when the interface was missing. The editing command
// now lives in `core/events/typing.rs`, keyed on the FOCUSED element, and one body serves
// `Input.insertText`, `Input.dispatchKeyEvent`, `Engine::type_text` and `Engine::type_into`.

pub(crate) fn cb_select_type_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    // the content attribute `multiple` reflects, read off the element -- not the page's `multiple` getter
    let mult = el_id_of(scope, this).is_some_and(|id| {
        let os = deno_core::JsRuntime::op_state_from(scope);
        let st = os.borrow();
        st.borrow::<HostDom>().0.get_attribute(id, "multiple").is_some()
    });
    let s = str_val(scope, if mult { "select-multiple" } else { "select-one" });
    rv.set(s);
}

pub(crate) const DS_ELID: &str = "korvus_ds_elid";
pub(crate) fn ds_to_attr(name: &str) -> String {
    let mut out = String::from("data-");
    for c in name.chars() { if c.is_ascii_uppercase() { out.push('-'); out.push(c.to_ascii_lowercase()); } else { out.push(c); } }
    out
}
pub(crate) fn ds_from_attr(attr: &str) -> Option<String> {
    let rest = attr.strip_prefix("data-")?;
    let mut out = String::new();
    let mut upper = false;
    for c in rest.chars() {
        if c == '-' { upper = true; }
        else if upper { out.push(c.to_ascii_uppercase()); upper = false; }
        else { out.push(c); }
    }
    Some(out)
}
/// The element a dataset is a view of. Its private comes from the engine's key cache (`protos::private_key`, the
/// same `for_api` private by name): a `v8::String` + registry probe per read cost every `el.dataset.x` twice.
pub(crate) fn ds_elid(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    let key = crate::binding::protos::private_key(scope, DS_ELID);
    this.get_private(scope, key).and_then(|v| v.uint32_value(scope))
}
pub(crate) fn ds_key_name(scope: &mut v8::PinScope, key: v8::Local<v8::Name>) -> Option<String> {
    let kv: v8::Local<v8::Value> = key.into();
    if !kv.is_string() { return None; }
    Some(kv.to_rust_string_lossy(scope))
}
pub(crate) fn cb_ds_get<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    let name = match ds_key_name(scope, key) { Some(n) => n, None => return v8::Intercepted::kNo };
    let elid = match ds_elid(scope, args.holder()) { Some(i) => i, None => return v8::Intercepted::kNo };
    let attr = ds_to_attr(&name);
    let val = { let op = deno_core::JsRuntime::op_state_from(scope); let st = op.borrow(); st.borrow::<HostDom>().0.get_attribute(elid, &attr).map(|s| s.to_string()) };
    match val { Some(s) => { let sv = str_val(scope, &s); rv.set(sv); v8::Intercepted::kYes } None => v8::Intercepted::kNo }
}
pub(crate) fn cb_ds_set<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, value: v8::Local<'s, v8::Value>, args: v8::PropertyCallbackArguments<'s>, mut _rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let name = match ds_key_name(scope, key) { Some(n) => n, None => return v8::Intercepted::kNo };
    let elid = match ds_elid(scope, args.holder()) { Some(i) => i, None => return v8::Intercepted::kNo };
    let attr = ds_to_attr(&name);
    let val = value.to_rust_string_lossy(scope);
    { let op = deno_core::JsRuntime::op_state_from(scope); op.borrow_mut().borrow_mut::<HostDom>().0.set_attribute(elid, &attr, &val); }
    v8::Intercepted::kYes
}
pub(crate) fn cb_ds_query<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let name = match ds_key_name(scope, key) { Some(n) => n, None => return v8::Intercepted::kNo };
    let elid = match ds_elid(scope, args.holder()) { Some(i) => i, None => return v8::Intercepted::kNo };
    let attr = ds_to_attr(&name);
    let exists = { let op = deno_core::JsRuntime::op_state_from(scope); let st = op.borrow(); st.borrow::<HostDom>().0.get_attribute(elid, &attr).is_some() };
    if exists { let zero = v8::Integer::new(scope, 0); rv.set(zero); v8::Intercepted::kYes } else { v8::Intercepted::kNo }
}
pub(crate) fn cb_ds_delete<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let name = match ds_key_name(scope, key) { Some(n) => n, None => return v8::Intercepted::kNo };
    let elid = match ds_elid(scope, args.holder()) { Some(i) => i, None => return v8::Intercepted::kNo };
    let attr = ds_to_attr(&name);
    { let op = deno_core::JsRuntime::op_state_from(scope); op.borrow_mut().borrow_mut::<HostDom>().0.remove_attribute(elid, &attr); }
    let t = v8::Boolean::new(scope, true); rv.set(t); v8::Intercepted::kYes
}
pub(crate) fn cb_ds_enum<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Array>) {
    let elid = match ds_elid(scope, args.holder()) { Some(i) => i, None => { let a = v8::Array::new(scope, 0); rv.set(a); return; } };
    let names: Vec<String> = { let op = deno_core::JsRuntime::op_state_from(scope); let st = op.borrow(); st.borrow::<HostDom>().0.attribute_names(elid).iter().filter_map(|a| ds_from_attr(a)).collect() };
    let arr = v8::Array::new(scope, names.len() as i32);
    for (i, n) in names.iter().enumerate() { let s = v8::String::new(scope, n).unwrap(); arr.set_index(scope, i as u32, s.into()); }
    rv.set(arr);
}
pub(crate) fn cb_dataset<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let elid = match arg_node_id(scope, args.this().into()) { Some(i) => i, None => return };
    // A dataset is a `DOMStringMap`, made from the interface's template, which carries the named-property
    // interceptor (`binding::protos::configure_dataset`) -- an instance, with the
    // interface's prototype (`Object.prototype.toString.call(el.dataset)` reads `[object DOMStringMap]`,
    // `el.dataset instanceof DOMStringMap`) and its brand. It was a bare ObjectTemplate's instance handed
    // the prototype, and no brand, until then. Every realm with an element has the interface.
    let Some(proto) = crate::binding::protos::iface_object(scope, "DOMStringMap")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|ctor| get_prop(scope, ctor, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    else {
        return;
    };
    let inst = crate::binding::protos::mint_on(scope, proto.into());
    let key = crate::binding::protos::private_key(scope, DS_ELID);
    let idv = int_val(scope, elid as i64);
    inst.set_private(scope, key, idv);
    rv.set(inst.into());
}

// ── the HTMLElement table ──────────────────────────────────────────────────────────
//
// The 142 members of `HTMLElement.prototype`, in the capture's order, installed by THE installer
// over the cppgc `JsElement` class prototype (`core/dom/spine.rs`). The kit rows (`title`, `lang`,
// `dir`, `inert`, `accessKey`, `enterKeyHint`, `inputMode`, `virtualKeyboardPolicy`,
// `writingSuggestions`, `nonce`, `autofocus`, `focusGroup`, `focusGroupStart` and the 102 handlers)
// need no code here; the rules below are the ports of `dom/09g-html-element.js`'s twelve JS bodies --
// `translate`/`spellcheck` inherit, `hidden` is a tri-state, `draggable` has a per-element default,
// `autocapitalize` reads back its STATE, `contentEditable` throws on a bad value, `innerText` runs the
// rendered-text-collection steps, the popover API is a state machine with its two events -- each
// argued at its callback, each checked against Chrome on this host (2026-09-10, probes p2..p4).

use crate::binding::protos::{throw_dom, wrap_cached};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::element::{cb_el_click, cb_el_style, cb_reflect_str_set, parse_html_integer, priv_set};
use crate::core::dom::node::notify_childlist_batch;
use crate::core::dom::shared::{remove_attribute_notify, row_accessor, row_accessor_data, row_getter, row_getter_data, row_method, set_attribute_notify};
use crate::core::html::element_internals::attach_internals;
use crate::install::MemberDef;

type OpStateRc = std::rc::Rc<std::cell::RefCell<deno_core::OpState>>;

const P_EDIT_CONTEXT: &str = "korvus_edit_context";
const P_STYLE_MAP: &str = "korvus_style_map";

/// The attribute, ASCII-lowercased -- the form every enumerated rule below compares against.
fn attr_lower(op_state: &OpStateRc, id: u32, name: &str) -> Option<String> {
    op_state.borrow().borrow::<HostDom>().0.get_attribute(id, name).map(|s| s.to_ascii_lowercase())
}

fn has_attr(op_state: &OpStateRc, id: u32, name: &str) -> bool {
    op_state.borrow().borrow::<HostDom>().0.get_attribute(id, name).is_some()
}

/// The parent ELEMENT (a document or fragment parent ends the walk).
fn parent_element(op_state: &OpStateRc, id: u32) -> Option<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    dom.parent(id).filter(|&p| dom.node_type(p) == 1)
}

fn tag_upper(op_state: &OpStateRc, id: u32) -> String {
    op_state.borrow().borrow::<HostDom>().0.tag_name(id).map(|t| t.to_ascii_uppercase()).unwrap_or_default()
}

/// A "true"/"false" attribute that INHERITS from the nearest ancestor that has one -- the shape
/// `translate` and `spellcheck` share, differing only in the keyword for true and in whether the
/// empty string counts. The walk stops at the document, whose default is `dflt`.
fn inherited(op_state: &OpStateRc, id: u32, name: &str, true_word: &str, false_word: &str, empty_is_true: bool, dflt: bool) -> bool {
    let mut cur = Some(id);
    while let Some(n) = cur {
        if let Some(v) = attr_lower(op_state, n, name) {
            if v == true_word || (empty_is_true && v.is_empty()) { return true; }
            if v == false_word { return false; }
        }
        cur = parent_element(op_state, n);
    }
    dflt
}

fn set_word(scope: &mut v8::PinScope, op_state: &OpStateRc, id: u32, name: &str, on: bool, t: &str, f: &str) {
    set_attribute_notify(scope, op_state, id, name, if on { t } else { f });
}

/// The element receiver, or the "Illegal invocation" TypeError every HTMLElement member throws on a
/// receiver that is not an element (Chrome; and what the op2 members these replace threw).
fn this_el(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    crate::core::dom::element::el_this(scope, args)
}

/// `getComputedStyle(el)`, or None when the realm has no layout pipeline to ask (a bare harness).
fn computed_style<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    let f = crate::binding::protos::intrinsic_fn(scope, "getComputedStyle")?;
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let r = f.call(scope, undef, &[el])?;
    v8::Local::<v8::Object>::try_from(r).ok()
}

fn style_prop(scope: &mut v8::PinScope, st: v8::Local<v8::Object>, name: &str) -> Option<String> {
    get_prop(scope, st, name).filter(|v| !v.is_null_or_undefined()).map(|v| v.to_rust_string_lossy(scope))
}

// ---- translate / spellcheck / hidden / draggable / autocapitalize ------------------------------

/// `translate` is not a boolean reflection: the content attribute is the enumerated `yes`/`no`, an
/// absent one INHERITS from the nearest ancestor that has one, and the document default is true.
fn cb_he_translate_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    rv.set_bool(inherited(&op_state, id, "translate", "yes", "no", true, true));
}
fn cb_he_translate_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let on = args.get(0).boolean_value(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    set_word(scope, &op_state, id, "translate", on, "yes", "no");
}

/// Inherits like `translate`, with `true`/`false` as the keywords and true as the default.
fn cb_he_spellcheck_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    rv.set_bool(inherited(&op_state, id, "spellcheck", "true", "false", true, true));
}
fn cb_he_spellcheck_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let on = args.get(0).boolean_value(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    set_word(scope, &op_state, id, "spellcheck", on, "true", "false");
}

/// `(boolean or unrestricted double or DOMString)? hidden` -- three-valued. `hidden="until-found"`
/// reads back as the STRING "until-found"; any other present value is `true`; absent is `false`.
fn cb_he_hidden_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match attr_lower(&op_state, id, "hidden") {
        None => rv.set_bool(false),
        Some(v) if v == "until-found" => { let s = str_val(scope, "until-found"); rv.set(s); }
        Some(_) => rv.set_bool(true),
    }
}
/// The setter's three arms: the string "until-found" (case-insensitively) stores that state; a falsy
/// value (`false`, `""`, `0`, `NaN`, `null`, `undefined` -- exactly ToBoolean's falsy set) removes the
/// attribute; anything else stores the empty string.
fn cb_he_hidden_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let v = args.get(0);
    let until_found = v.is_string() && v.to_rust_string_lossy(scope).eq_ignore_ascii_case("until-found");
    let truthy = v.boolean_value(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if until_found {
        set_attribute_notify(scope, &op_state, id, "hidden", "until-found");
    } else if !truthy {
        remove_attribute_notify(scope, &op_state, id, "hidden");
    } else {
        set_attribute_notify(scope, &op_state, id, "hidden", "");
    }
}

/// The default is per-element, not false: an `<img>`, and an `<a>`/`<area>` with an href, are
/// draggable with no attribute at all (Chrome, 2026-09-10: img true, a[href] true, a false).
fn cb_he_draggable_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = match attr_lower(&op_state, id, "draggable") {
        Some(v) if v == "true" => true,
        Some(v) if v == "false" => false,
        _ => {
            let t = tag_upper(&op_state, id);
            t == "IMG" || ((t == "A" || t == "AREA") && has_attr(&op_state, id, "href"))
        }
    };
    rv.set_bool(v);
}
fn cb_he_draggable_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let on = args.get(0).boolean_value(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    set_word(scope, &op_state, id, "draggable", on, "true", "false");
}

/// `[ReflectSetter]`: the setter stores the string verbatim (the `reflect_str` setter), the getter
/// returns the element's own autocapitalization HINT -- the state's keyword, not the attribute's text.
/// `off` is the `none` state and `on` is the `sentences` state, so those two do not read back as themselves.
fn cb_he_autocapitalize_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let out = match attr_lower(&op_state, id, "autocapitalize").as_deref() {
        None => "",
        Some("off") | Some("none") => "none",
        Some("on") | Some("sentences") => "sentences",
        Some("words") => "words",
        Some("characters") => "characters",
        Some("") => "",
        // The INVALID value default is the sentences state (Chrome 150: `autocapitalize = "bogus"`
        // reads "sentences"; it read "").
        Some(_) => "sentences",
    };
    let v = str_val(scope, out);
    rv.set(v);
}

// ---- editContext / contentEditable / isContentEditable -----------------------------------------

/// `attribute EditContext? editContext`. An element has no EditContext until one is assigned (null);
/// the association is a private slot on the element wrapper (no own property grows on the instance).
/// A non-EditContext, non-null assignment is the WebIDL interface-type conversion failure -- Chrome's
/// "Failed to convert value to 'EditContext'." (2026-09-10).
fn cb_he_edit_context_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    match priv_get(scope, args.this(), P_EDIT_CONTEXT) {
        Some(v) if !v.is_null_or_undefined() => rv.set(v),
        _ => rv.set_null(),
    }
}
fn cb_he_edit_context_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let v = args.get(0);
    if v.is_null_or_undefined() {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        priv_set(scope, args.this(), P_EDIT_CONTEXT, u);
        return;
    }
    // Any realm's EditContext (B27): `protos::implements`.
    if !crate::binding::protos::implements(scope, v, "EditContext") {
        throw_type_error(scope, "Failed to set the 'editContext' property on 'HTMLElement': Failed to convert value to 'EditContext'.");
        return;
    }
    priv_set(scope, args.this(), P_EDIT_CONTEXT, v);
}

/// Four states, and "inherit" -- NOT "" -- is the absent one.
fn cb_he_content_editable_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let out = match attr_lower(&op_state, id, "contenteditable").as_deref() {
        None => "inherit",
        Some("") | Some("true") => "true",
        Some("false") => "false",
        Some("plaintext-only") => "plaintext-only",
        Some(_) => "inherit",
    };
    let v = str_val(scope, out);
    rv.set(v);
}
/// The one setter on this interface that rejects a value: `o.contentEditable = "x"` is a SyntaxError.
fn cb_he_content_editable_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let raw = args.get(0).to_rust_string_lossy(scope);
    let s = raw.to_ascii_lowercase();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match s.as_str() {
        "inherit" => remove_attribute_notify(scope, &op_state, id, "contenteditable"),
        "true" | "false" | "plaintext-only" => set_attribute_notify(scope, &op_state, id, "contenteditable", &s),
        _ => throw_dom(
            scope,
            &format!("Failed to set the 'contentEditable' property on 'HTMLElement': The value provided ('{raw}') is not one of 'true', 'false', 'plaintext-only', or 'inherit'."),
            "SyntaxError",
        ),
    }
}

/// The RESOLVED editability, which is why it is read-only and why it is not `contentEditable ===
/// "true"`: an element inside a `contenteditable` ancestor is itself editable.
fn cb_he_is_content_editable_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut cur = Some(id);
    while let Some(n) = cur {
        match attr_lower(&op_state, n, "contenteditable").as_deref() {
            Some("") | Some("true") | Some("plaintext-only") => { rv.set_bool(true); return; }
            Some("false") => { rv.set_bool(false); return; }
            _ => {}
        }
        cur = parent_element(&op_state, n);
    }
    rv.set_bool(false);
}

// ---- offsetParent -----------------------------------------------------------------------------

/// The CSSOM-view offsetParent algorithm, run against the real layout this engine has: null for a
/// detached element, for `<body>`/`<html>`, for `display: none` and `position: fixed`; otherwise the
/// nearest positioned ancestor, a table cell/table, or the body (Chrome, 2026-09-10: detached null,
/// attached child of body -> body).
fn cb_he_offset_parent_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    match offset_parent(scope, id) {
        Some(p) => rv.set(p),
        None => rv.set_null(),
    }
}

/// The element `offsetParent` answers for element `id`, or `None` for null.
fn offset_parent<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    let p = crate::core::dom::geometry_queries::offset_parent_id(scope, id)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    Some(wrap_cached(scope, &op_state, p).into())
}

// ---- popover ----------------------------------------------------------------------------------

/// `attribute DOMString? popover` -- nullable, so an absent attribute is null and not "". The empty
/// string is the `auto` state (that is what `<div popover>` means), `hint` is its own, and any
/// unrecognised value is the invalid default `manual`.
fn popover_kind(op_state: &OpStateRc, id: u32) -> Option<&'static str> {
    match attr_lower(op_state, id, "popover").as_deref() {
        None => None,
        Some("") | Some("auto") => Some("auto"),
        Some("hint") => Some("hint"),
        Some(_) => Some("manual"),
    }
}
fn cb_he_popover_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match popover_kind(&op_state, id) {
        Some(k) => { let s = str_val(scope, k); rv.set(s); }
        None => rv.set_null(),
    }
}
fn cb_he_popover_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let v = args.get(0);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if v.is_null_or_undefined() {
        remove_attribute_notify(scope, &op_state, id, "popover");
    } else {
        let s = v.to_rust_string_lossy(scope);
        set_attribute_notify(scope, &op_state, id, "popover", &s);
    }
}

/// The popover's SHOWING state lives in the arena (`Dom::is_popover_open`), so `:popover-open` and the
/// UA sheet's popover rules see what script did; it was a V8 private, invisible to the matcher
/// (`el.matches(":popover-open")` was false after `showPopover()`; Chrome 150: true).
fn popover_open(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    let Some(id) = crate::page::node_id(scope, this.into()) else { return false };
    crate::page::with_host_dom(scope, |d| d.is_popover_open(id)).unwrap_or(false)
}
fn set_popover_open(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, open: bool) {
    let Some(id) = crate::page::node_id(scope, this.into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_popover_open(id, open);
}

/// Dispatch a `ToggleEvent` (an `Event` when the realm has none) and report whether it was NOT
/// canceled -- the `beforetoggle` gate.
fn fire_toggle(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, ty: &str, old_state: &str, new_state: &str, cancelable: bool) -> bool {
    let type_v = str_val(scope, ty);
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "cancelable", cancelable);
    let os = str_val(scope, old_state);
    set_prop(scope, init, "oldState", os);
    let ns = str_val(scope, new_state);
    set_prop(scope, init, "newState", ns);
    let ev = crate::binding::protos::iface_object(scope, "ToggleEvent")
        .or_else(|| crate::binding::protos::iface_object(scope, "Event"))
        .and_then(|c| v8::Local::<v8::Function>::try_from(c).ok())
        .and_then(|c| c.new_instance(scope, &[type_v, init.into()]));
    let Some(ev) = ev else { return true };
    let evv: v8::Local<v8::Value> = ev.into();
    match crate::binding::protos::engine_call(scope, this.into(), "dispatchEvent", &[evv]) {
        Some(r) => !r.is_false(),
        None => true,
    }
}

/// The popover state machine's two halves. The state and its events are real; what this engine cannot
/// do is the RENDERING half (there is no top layer, so a shown popover does not become visible). Every
/// observable JS effect -- the NotSupportedError for a non-popover, the two InvalidStateError paths,
/// the cancelable `beforetoggle`, the `toggle` that follows, and `togglePopover`'s boolean -- is here,
/// with Chrome's messages (2026-09-10).
fn popover_show(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, id: u32, throws: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if popover_kind(&op_state, id).is_none() {
        if throws { throw_dom(scope, "Failed to execute 'showPopover' on 'HTMLElement': Not supported on elements that are not popovers.", "NotSupportedError"); }
        return;
    }
    if popover_open(scope, this) {
        if throws { throw_dom(scope, "Failed to execute 'showPopover' on 'HTMLElement': Invalid on popover elements which aren't hidden.", "InvalidStateError"); }
        return;
    }
    if !crate::core::html::subresource::connected_anywhere(scope, id) {
        if throws { throw_dom(scope, "Failed to execute 'showPopover' on 'HTMLElement': Invalid on disconnected popover elements.", "InvalidStateError"); }
        return;
    }
    if !fire_toggle(scope, this, "beforetoggle", "closed", "open", true) { return; }
    // The handler may have changed things.
    if popover_kind(&op_state, id).is_none() || popover_open(scope, this) { return; }
    set_popover_open(scope, this, true);
    fire_toggle(scope, this, "toggle", "closed", "open", false);
}
fn popover_hide(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, id: u32, throws: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if popover_kind(&op_state, id).is_none() {
        if throws { throw_dom(scope, "Failed to execute 'hidePopover' on 'HTMLElement': Not supported on elements that are not popovers.", "NotSupportedError"); }
        return;
    }
    if !popover_open(scope, this) {
        if throws { throw_dom(scope, "Failed to execute 'hidePopover' on 'HTMLElement': Invalid on popover elements that aren't already showing.", "InvalidStateError"); }
        return;
    }
    if !fire_toggle(scope, this, "beforetoggle", "open", "closed", false) { return; }
    set_popover_open(scope, this, false);
    fire_toggle(scope, this, "toggle", "open", "closed", false);
}
fn cb_he_show_popover<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    let Some(id) = this_el(scope, &args) else { return };
    // `optional ShowPopoverOptions options = {}`, converted (Chrome 150): `source` read once and
    // checked as an HTMLElement; a non-object is no dictionary (korvus read nothing)
    if crate::install::ctor_dicts::convert_dict(scope, args.get(0), "ShowPopoverOptions", "Failed to execute 'showPopover' on 'HTMLElement': ").is_err() {
        return;
    }
    popover_show(scope, this, id, true);
}
fn cb_he_hide_popover<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let this = args.this();
    let Some(id) = this_el(scope, &args) else { return };
    popover_hide(scope, this, id, true);
}
/// `togglePopover(options)`: `options` is a boolean `force` or a `{ force }` dictionary; answers whether
/// the popover is open afterwards (Chrome, 2026-09-10: true false true true for toggle/toggle/
/// toggle(true)/toggle({force:true})).
fn cb_he_toggle_popover<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(id) = this_el(scope, &args) else { return };
    // `optional (TogglePopoverOptions or boolean) options = {}`, converted as WebIDL converts the union (Chrome 150):
    // an object, `null` or `undefined` is the dictionary -- `source`, then `force`, each read
    // once -- and anything else the boolean `force` (`togglePopover(5)` forces; korvus took a number for "no force")
    let a0 = args.get(0);
    let force: Option<bool> = if a0.is_null_or_undefined() || a0.is_object() {
        let Ok(o) = crate::install::ctor_dicts::convert_dict(scope, a0, "TogglePopoverOptions", "Failed to execute 'togglePopover' on 'HTMLElement': ") else { return };
        get_prop(scope, o, "force").filter(|v| v.is_boolean()).map(|v| v.is_true())
    } else {
        Some(a0.boolean_value(scope))
    };
    // Blink's `togglePopover`: an element that is not a popover throws whatever `force` says.
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        if popover_kind(&op_state, id).is_none() {
            throw_dom(scope, "Failed to execute 'togglePopover' on 'HTMLElement': Not supported on elements that are not popovers.", "NotSupportedError");
            return;
        }
    }
    let open = popover_open(scope, this);
    if force == Some(true) || (force.is_none() && !open) {
        popover_show(scope, this, id, force == Some(true));
    } else if force == Some(false) || (force.is_none() && open) {
        popover_hide(scope, this, id, force == Some(false));
    }
    let now = popover_open(scope, this);
    rv.set_bool(now);
}

// ---- innerText / outerText --------------------------------------------------------------------

/// The HTML "rendered text collection steps", run against this engine's real computed styles. It is
/// not `textContent`: a `display:none` subtree contributes nothing (which is why `<script>` and
/// `<style>` text does not appear -- the UA sheet gives them display:none and getComputedStyle here
/// reports it), `<br>` is a line break, and a block-level child forces one before and after itself.
/// Whitespace is collapsed for everything except `<pre>`/`<textarea>`.
///
/// # THREE properties decide what is emitted, and they are three DIFFERENT rules
///
/// Measured against stock Google Chrome 150.0.7871.46 over CDP on 2026-09-14, on a loopback
/// fixture. Nothing here is read off a specification; `corpus_text.rs` re-asserts every row.
///
/// * **`display: none`** — no box at all. The subtree is skipped AND the element is transparent to
///   white-space collapsing, so `A <span style=display:none>H</span> B` is `"A B"`: one space.
/// * **`visibility: hidden` / `collapse`** — PER ELEMENT, because `visibility` INHERITS and a
///   descendant may set `visibility: visible` to come back, which `display: none` can never do.
///   `<div style=visibility:hidden>PARENT<span style=visibility:visible>CHILD</span></div>` reads
///   `"CHILD"`, and `A<span style=visibility:hidden>H<span style=visibility:visible>B</span>H2</span>C`
///   reads `"ABC"` — so the rule is on the TEXT NODE'S OWN PARENT, not on the subtree. The hidden
///   element contributes no required line break of its own either (`A<div style=visibility:hidden>H</div>B`
///   is `"AB"`, not `"A\nB"`), but a visible block INSIDE it still contributes its own
///   (`A<div style=visibility:hidden>P<div style=visibility:visible>C</div></div>B` is `"A\nC\nB"`).
///   The hidden text is still LAID OUT, so it still separates two white-space runs:
///   `A <span style=visibility:hidden>H</span> B` is `"A  B"` — two spaces, one either side.
/// * **`content-visibility: hidden`** — PER SUBTREE, the opposite of `visibility`: a
///   `visibility: visible` descendant does NOT come back (`A<div style=content-visibility:hidden>H<span
///   style=visibility:visible>V</span></div>B` is `"AB"`). It needs a box that size containment can
///   apply to, so it is INERT on a non-atomic inline: `A <span style=content-visibility:hidden>H</span> B`
///   is `"A H B"`, while the same with `display:inline-block` is `"A  B"`.
///
/// **`content-visibility: auto` is filtered by the LAYOUT, not by the style.** Chrome skips an `auto`
/// subtree only while it is not relevant to the user: read after a rendering opportunity the
/// IN-VIEWPORT one is its full text and only the below-the-fold one is `""` (measured, `c9/repro/cv`).
/// `getComputedStyle` reports the specified `auto` in both engines, so treating every `auto` as
/// `hidden` would delete the text of every lazily-rendered list; instead the layout decides relevance
/// as Blink does -- the box grown by 150% of the viewport meets it
/// (`BoxTree::compute`) -- and the walk skips exactly the elements it left locked
/// ([`TextWalk::skipped`]). Text read before any layout is not modelled: this engine answers after one.
enum TextItem {
    /// Text, already collapsed unless `preserve`. `visible` is false for text whose own parent
    /// element is `visibility: hidden`/`collapse`: those characters still OCCUPY the line, so they
    /// still terminate a white-space run, and they are never emitted.
    Text { s: String, preserve: bool, visible: bool },
    /// A `<br>`: exactly one LF, never merged into a `Gap` run (`A<br><div>M</div>B` is
    /// `"A\n\nM\nB"`) and never dropped at either end (`<br>A<br>` is `"\nA\n"`).
    Break,
    /// A required line-break count at a block boundary: a run of them is the LARGEST of the run, and
    /// a run always ends the line for white-space purposes. **`Gap(0)` is a line edge that emits
    /// nothing** — what a `visibility: hidden` or `content-visibility: hidden` block contributes:
    /// `A <div style=visibility:hidden>H</div> B` is `"AB"`, both spaces gone and no newline.
    Gap(u32),
    /// A box that occupies its line and emits nothing: a skipped `content-visibility: hidden`
    /// subtree on an inline-level box, which is why `A <span
    /// style=display:inline-block;content-visibility:hidden>H</span> B` keeps BOTH spaces.
    Opaque,
    /// The whole contribution of an **out-of-flow** box -- `position: absolute|fixed`, or a `float`
    /// -- held apart from the white-space run around it.
    ///
    /// # The rule, measured on Chrome 150 on 2026-09-14
    ///
    /// An in-flow block ENDS the white-space run on both sides of it: `A <div>H</div> B` is
    /// `"A\nH\nB"` and both spaces are gone. An out-of-flow box does NOT, because it is not on the
    /// line at all -- the inline content on its two sides is ONE run, collapsing across it exactly
    /// as it collapses across a `display: none` element. So `A <span style=position:absolute>H</span>
    /// B` keeps one space (Chrome: `"A \nH\nB"`) where the `<div>` keeps none, and
    /// `A<span style=position:absolute>H</span> B` is `"A\nH\n B"` -- the surviving space is on
    /// whichever side of the box the run puts it, which is the giveaway that the two sides are one run.
    ///
    /// **This is why the variant exists rather than a flag on `Gap`.** The box still contributes its
    /// required breaks and its own text, and those run through the same `Sink`; if they also drove
    /// the Sink's white-space state they would terminate the surrounding run, which is the thing
    /// that must not happen. Holding the items in their own list lets the resolver save the
    /// white-space state, resolve them, and put it back -- the box writes its characters without
    /// ever being part of the run it interrupts.
    ///
    /// `position: relative` and `position: sticky` are IN flow and never take this path: measured,
    /// `A <div style=position:relative>H</div> B` and the `sticky` twin are both `"A\nH\nB"`, the
    /// same as the plain `<div>`. Nor does `display: contents` or `display: none`, which generate no
    /// box for `position` to take out of flow -- `A <span style="position:absolute;display:contents">
    /// H</span> B` is `"A H B"`, the span entirely transparent.
    OutOfFlow(Vec<TextItem>),
    /// The nodes one `<slot>` of `host`'s shadow tree was assigned, laid out where the slot is.
    /// `key` is the first node's index among `host`'s children: Chrome WRITES slotted text in light
    /// order, so [`reorder_slotted`] may hand this place another slot's nodes.
    Slotted { host: u32, key: usize, items: Vec<TextItem> },
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ') {
            if !in_ws { out.push(' '); in_ws = true; }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

const INLINE_DISPLAYS: &[&str] = &["inline", "inline-block", "inline-flex", "inline-grid", "contents", "ruby", "ruby-text", "ruby-base", "none"];

/// The displays `content-visibility` is INERT on, because size containment cannot apply to them.
/// Measured, not read off the spec: with `content-visibility: hidden` on the span,
/// `A <span>H</span> B` and `A <span style=display:contents>H</span> B` both read `"A H B"` in
/// Chrome 150 — unaffected — while `display:inline-block` and `display:inline-flex` read `"A  B"`.
/// So this is the non-atomic-inline family, NOT `INLINE_DISPLAYS`, which also holds the atomic
/// `inline-*` boxes that containment does apply to.
const NO_CONTAINMENT: &[&str] = &["inline", "contents", "ruby", "ruby-text", "ruby-base"];

/// Each element's own answers to the three questions above, read in ONE `getComputedStyle` call.
struct RenderedAs {
    display: String,
    /// Computed `visibility` is `visible`. Defaults to TRUE when there is no layout pipeline to ask
    /// (a bare harness answers `""`), so a realm without one keeps every character it has today.
    visible: bool,
    /// `content-visibility: hidden` AND a box it can apply to.
    skipped: bool,
    /// The box is **out of flow**: `position: absolute|fixed`, or a `float` other than `none`.
    ///
    /// This is NOT how the line break is decided -- that is `display`, which the cascade already
    /// blockifies for exactly these boxes (`korvus-page`'s `blockify`). It decides a
    /// DIFFERENT question: whether the inline content on the two sides of the box belongs to one
    /// white-space run. See [`TextItem::OutOfFlow`].
    out_of_flow: bool,
    /// Computed `text-transform`, which this element's direct text children are painted through.
    /// See [`WalkCtx::transform`].
    transform: TextTransform,
}

fn rendered_as(scope: &mut v8::PinScope, op_state: &OpStateRc, id: u32) -> RenderedAs {
    let w = wrap_cached(scope, op_state, id);
    let Some(st) = computed_style(scope, w.into()) else {
        return RenderedAs { display: "block".to_string(), visible: true, skipped: false,
                            out_of_flow: false, transform: TextTransform::Unapplied };
    };
    let display = style_prop(scope, st, "display").unwrap_or_else(|| "block".to_string());
    let visible = !matches!(style_prop(scope, st, "visibility").as_deref(), Some("hidden") | Some("collapse"));
    let skipped = style_prop(scope, st, "content-visibility").as_deref() == Some("hidden")
        && !NO_CONTAINMENT.contains(&display.as_str());
    let out_of_flow = matches!(style_prop(scope, st, "position").as_deref(), Some("absolute") | Some("fixed"))
        || !matches!(style_prop(scope, st, "float").as_deref(), None | Some("none"));
    let transform = TextTransform::of(style_prop(scope, st, "text-transform").as_deref());
    RenderedAs { display, visible, skipped, out_of_flow, transform }
}

/// The item resolver's state. Two things are DEFERRED rather than written and trimmed back, which is
/// what makes the leading and trailing trim fall out of the model instead of being a separate pass:
///
///   `gap`   a required-break run. It is written only when something visible follows it, so a run at
///           either end of the result never reaches `out` at all.
///   `space` a collapsible space. Same: it is written only when a glyph follows it on the same line,
///           so `"   LEAD"` is `"LEAD"`, `"TRAIL   "` is `"TRAIL"`, and the space beside a block
///           boundary in `A <div>M</div> B` is gone on both sides -- `"A\nM\nB"`.
struct Sink {
    out: String,
    /// The largest required-break count seen since the last glyph, if any.
    gap: Option<u32>,
    /// A collapsible space is pending, and whether the FIRST space of the run was visible -- which is
    /// the one that survives collapsing. `A<span style=visibility:hidden> </span> B` is `"AB"` (the
    /// run starts hidden) and `A <span style=visibility:hidden> </span>B` is `"A B"` (it starts
    /// visible).
    space: bool,
    space_visible: bool,
    /// Where in `out` a pending space must be INSERTED rather than appended, because the white-space
    /// run it belongs to has since been interrupted by an out-of-flow box that wrote its own
    /// characters. Measured: `A <span style=position:absolute>H</span> B` is `"A \nH\nB"` in Chrome
    /// -- the one surviving space of the run `"A "` + `" B"` sits on the FIRST side of the break,
    /// where the run's first space was, not where its last one was.
    ///
    /// It is deliberately the FIRST such boundary only (`get_or_insert`, never overwritten), so two
    /// out-of-flow boxes in a row still put the run's single space before the first of them:
    /// `A <span abs>H</span> <span abs>I</span> B` is `"A \nH\nI\nB"`.
    ///
    /// It stays `None` when the space is never emitted at all, which is what keeps the trailing trim
    /// working: `A <span style=position:absolute>H</span>` is `"A\nH"`, the space simply gone.
    space_at: Option<usize>,
    /// Some visible character has been written, so a pending gap may now be spent.
    started: bool,
    /// Nothing occupies this line yet, so a pending space is at a line edge and collapses away.
    at_line_start: bool,
}

impl Sink {
    fn new() -> Self {
        Sink { out: String::new(), gap: None, space: false, space_visible: false, space_at: None, started: false, at_line_start: true }
    }

    fn gap(&mut self, n: u32) {
        self.space = false; // a collapsible space at a line edge collapses away
        self.space_at = None; // ... and with it any promise about where that space would have gone
        self.gap = Some(self.gap.unwrap_or(0).max(n));
        self.at_line_start = true;
    }

    fn hard_break(&mut self) {
        self.space = false;
        self.spend_gap();
        self.out.push('\n');
        self.started = true;
        self.at_line_start = true;
    }

    fn space(&mut self, visible: bool) {
        if !self.space { self.space = true; self.space_visible = visible; }
    }

    fn glyphs_char(&mut self, ch: char, visible: bool) {
        let mut buf = [0u8; 4];
        self.glyphs(ch.encode_utf8(&mut buf), visible);
    }

    /// A box that occupies its line and writes nothing: a skipped `content-visibility: hidden`
    /// subtree. It ends the white-space run without breaking the line.
    fn occupy(&mut self) {
        self.glyphs("", false);
    }

    /// One run of non-collapsible characters, or -- with an empty `s` -- a box that occupies the line
    /// and writes nothing. Either way the line is no longer at its start and the white-space run ends.
    fn glyphs(&mut self, s: &str, visible: bool) {
        // NOT gated on `started`: a space after a character that was laid out and not written is
        // not at a line edge, and Chrome 150 keeps it -- `<span style=visibility:hidden>H</span> B`
        // reads `" B"`, and so does a `::before` of `X` ahead of ` B`.
        let emit_space = self.space && self.space_visible && !self.at_line_start;
        self.space = false;
        let space_at = self.space_at.take();
        if emit_space || (visible && !s.is_empty()) {
            self.spend_gap();
            if emit_space {
                // `space_at` is set only when an out-of-flow box has written between this space's
                // run and here, and it is where that run was when the box interrupted it.
                match space_at {
                    Some(i) => self.out.insert(i, ' '),
                    None => self.out.push(' '),
                }
                self.started = true;
            }
            if visible && !s.is_empty() { self.out.push_str(s); self.started = true; }
        }
        self.at_line_start = false;
    }

    fn spend_gap(&mut self) {
        if let Some(n) = self.gap.take() {
            if self.started { for _ in 0..n { self.out.push('\n'); } }
        }
    }

    /// Open an out-of-flow box: note where the interrupted run has got to, and hand back the
    /// white-space state to put back when the box is done.
    ///
    /// `out`, `started` and `gap` are deliberately NOT part of that state: the box really does write
    /// characters and really does contribute line breaks, and only its effect on the surrounding RUN
    /// is undone. See [`TextItem::OutOfFlow`].
    fn open_out_of_flow(&mut self) -> WsState {
        if self.space {
            self.space_at.get_or_insert(self.out.len());
        }
        WsState {
            space: self.space,
            space_visible: self.space_visible,
            space_at: self.space_at,
            at_line_start: self.at_line_start,
        }
    }

    fn close_out_of_flow(&mut self, s: WsState) {
        self.space = s.space;
        self.space_visible = s.space_visible;
        self.space_at = s.space_at;
        self.at_line_start = s.at_line_start;
    }
}

/// The part of [`Sink`] an out-of-flow box must not disturb.
#[derive(Clone, Copy)]
struct WsState {
    space: bool,
    space_visible: bool,
    space_at: Option<usize>,
    at_line_start: bool,
}

/// What one `innerText` walk carries from node to node.
struct TextWalk {
    /// The tree the element `innerText` is read on lives in (its `root_of`). Chrome writes the text
    /// of that tree only: a shadow tree below a host, and a host's light children reached through a
    /// `<slot>` from inside a shadow tree, are LAID OUT -- so they still take part in white-space
    /// collapsing -- and never written. Measured, Chrome 150: `<x-el>` with a shadow root holding
    /// `nothing` and no slot, then a space, then a second host slotting `lightchild_slotted`, reads
    /// `" lightchild_slotted"` -- the leading space survives because the first host's shadow text
    /// occupies the line.
    reading_tree: u32,
    /// Every `::before`/`::after` box the layout generated: `(element, after)` -> (text, level, out
    /// of flow, preserves white space).
    generated: std::collections::HashMap<(u32, bool), (String, page::layout::style::BoxLevel, bool, bool)>,
    /// The persona's `navigator.languages`, for SVG's `systemLanguage`.
    languages: Vec<String>,
    /// The elements whose `content-visibility: auto` contents the layout skipped: read
    /// like `content-visibility: hidden` ones -- Chrome 150's `innerText` of an off-viewport `auto` box
    /// is `""` (`c9/repro/cv`; react.dev and modernfontstacks.com read thousands of characters fewer).
    skipped: std::collections::HashSet<u32>,
}

/// The per-subtree half of the walk.
#[derive(Clone, Copy)]
struct WalkCtx {
    /// Collapse nothing (`<pre>`).
    preserve: bool,
    /// The caller's own `visibility: visible`, which decides whether its direct text nodes are
    /// emitted. It is recomputed per element rather than carried down, because `visibility` inherits
    /// and a descendant can set it back to `visible`.
    visible: bool,
    /// These nodes are in [`TextWalk::reading_tree`], so their text is written.
    written: bool,
    /// The computed `text-transform` of the element whose direct text children these are.
    ///
    /// `innerText` is the RENDERED text, so it carries the case the box paints and not the case the
    /// source holds. Measured, Chrome 150.0.7871.46 over loopback against korvus:
    ///
    /// ```text
    ///                                      chrome 150            korvus (before)
    ///   text-transform: uppercase          "COMMUNITY PROJECTS"  "Community Projects"
    ///   text-transform: lowercase          "see all projects"    "SEE ALL PROJECTS"
    ///   text-transform: capitalize         "Texera Maka Xtable"  "texera maka xtable"
    ///   uppercase, inherited into a child  "OUTER INNER TAIL"    "outer inner tail"
    /// ```
    ///
    /// It is carried rather than read at the text node because the property INHERITS and a text
    /// node has no style of its own: the value that applies is its parent element's, which is the
    /// element this context was built for.
    transform: TextTransform,
}

/// The `text-transform` values this engine applies to `innerText`.
///
/// # `uppercase` and `lowercase` are here, and they are EXACT
///
/// They are per character and need no state, so Rust's own `to_uppercase`/`to_lowercase` -- which
/// are the full Unicode mappings, not ASCII -- give Chrome's answer character for character.
/// Measured, Chrome 150.0.7871.46 on a UTF-8 loopback fixture against this implementation:
/// `straße mañana ıstanbul` under `uppercase` reads `STRASSE MAÑANA ISTANBUL` in BOTH -- note that
/// `ß` becomes two characters, so this is a transform that changes `text_len` and an ASCII-only
/// `make_ascii_uppercase` would have been wrong in a way no ASCII fixture could show.
///
/// # `capitalize` is NOT here, deliberately, and this is what it would have to do
///
/// It needs the word-start state of the whole TEXT RUN, not of one text node, and the boundary is
/// not just white space. Chrome 150, measured, the two rows that decide it:
///
/// ```text
///   <div style="text-transform:capitalize">hello  world 3rd o'neil x-ray</div>
///       chrome  "Hello World 3rd O'neil X-Ray"    <- `-` IS a boundary, `'` is NOT, and a word
///                                                    beginning with a digit is left alone
///   <div style="text-transform:capitalize"><span>ab</span><span>cd</span></div>
///       chrome  "Abcd"                            <- ONE word across two text nodes, so a
///                                                    per-node pass reads `AbCd` and is wrong
/// ```
///
/// A per-text-node `capitalize` is wrong on both of those, which is every case that distinguishes
/// it from a naive one, so it is left UNAPPLIED rather than half-applied: today's answer for a
/// `capitalize` box is the source's own case, which is exactly what it was before this change.
/// `full-width`, `full-size-kana` and `math-auto` are unapplied for the same reason -- and Chrome
/// 150 answered `fw123` for `text-transform: full-width` on `fw123`, so on ASCII that one currently
/// agrees by accident rather than by implementation, which is why it is recorded here and not
/// claimed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextTransform {
    /// `none`, `capitalize`, `full-width`, `full-size-kana`, `math-auto`: the characters pass
    /// through. See this type's own docs for why the last four share this arm.
    Unapplied,
    Uppercase,
    Lowercase,
}

impl TextTransform {
    fn of(v: Option<&str>) -> Self {
        match v {
            Some("uppercase") => Self::Uppercase,
            Some("lowercase") => Self::Lowercase,
            _ => Self::Unapplied,
        }
    }

    fn apply(self, s: &str) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Unapplied => std::borrow::Cow::Borrowed(s),
            Self::Uppercase => std::borrow::Cow::Owned(s.to_uppercase()),
            Self::Lowercase => std::borrow::Cow::Owned(s.to_lowercase()),
        }
    }
}

const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// The HTML elements whose CHILDREN generate no boxes: replaced elements and the form controls whose
/// rendering is their own (a `<textarea>` shows its value, a `<meter>` its gauge). `<object>` is one
/// of them only while it shows its data -- see `subresource::object_shows_fallback`.
///
/// Measured on Chrome 150.0.7871.46, `A<tag>FALLBACK</tag>B` and `A <tag>FALLBACK</tag> B`: every
/// tag here reads `AB` and `A  B` -- the fallback is gone, and the element OCCUPIES its line, so the
/// spaces either side of it both survive. `<button>` (`ABTNB`) and `<select>` (`A\nO1\nO2\nB`) render
/// their children and are not here; `<audio>` without `controls` is `display: none`.
const CHILDLESS_HTML: &[&str] = &["video", "audio", "canvas", "iframe", "frame", "embed", "img", "input", "textarea", "meter", "progress", "object"];

/// SVG elements whose element children are laid out (Blink's SVG containers). Any other SVG element --
/// `<desc>`, `<title>`, `<metadata>`, a shape, a gradient -- lays out none of its children.
/// Measured, Chrome 150: `A<svg><defs><text>DEFS</text></defs></svg>B` reads `"A\nDEFS\nB"`, and the
/// same for `<symbol>`, `<clipPath>`, `<mask>`, `<pattern>` and `<marker>`; `<desc>`, `<title>` and
/// `<metadata>` read `"AB"`.
const SVG_CONTAINERS: &[&str] = &["svg", "g", "a", "switch", "defs", "symbol", "clipPath", "mask", "pattern", "marker"];

fn is_svg(dom: &korvus_dom::Dom, n: u32) -> bool {
    dom.namespace_uri(n) == Some(SVG_NS)
}

/// Is SVG element `a` inside a `<text>`? An `<a>` there is an inline run of the text; anywhere else
/// it is a container, and text directly in it is not laid out (Chrome: `<svg><a>ARAW</a></svg>` reads
/// nothing).
fn svg_inside_text(dom: &korvus_dom::Dom, a: u32) -> bool {
    let mut cur = dom.parent(a);
    while let Some(p) = cur {
        if !is_svg(dom, p) {
            return false;
        }
        match dom.tag_name(p) {
            Some("text" | "tspan" | "textPath") => return true,
            Some("svg" | "foreignObject") | None => return false,
            _ => cur = dom.parent(p),
        }
    }
    false
}

/// SVG conditional processing: `systemLanguage` and `requiredExtensions`. An element that fails is
/// not rendered, inside a `<switch>` or not. Measured on Chrome 150 whose `navigator.languages` was
/// `ko-KR,ko,en-US,en`, on `<svg><text systemLanguage=...>IN</text></svg>`:
///
/// ```text
/// systemLanguage   en en-US en-GB EN ko ko-XX 'fr, en' '  fr ,  en  '    rendered
///                  '' fr enx e en_US 'fr en' ','  'fr-CA,enx'            not rendered
/// requiredExtensions  http://www.w3.org/1999/xhtml, the MathML namespace, both     rendered
///                     '' '   ' foo  'http://www.w3.org/1999/xhtml foo'             not rendered
/// requiredFeatures=foo                                                             rendered (ignored)
/// ```
///
/// So a `systemLanguage` token matches when its primary subtag (before `-`) equals, ignoring ASCII
/// case, the primary subtag of ANY of the user's languages.
fn svg_conditions_pass(dom: &korvus_dom::Dom, languages: &[String], el: u32) -> bool {
    if let Some(v) = dom.get_attribute(el, "systemLanguage") {
        let primary = |s: &str| s.split('-').next().unwrap_or("").to_string();
        let ok = v.split(',').map(str::trim).filter(|t| !t.is_empty()).any(|t| {
            let p = primary(t);
            languages.iter().any(|l| primary(l).eq_ignore_ascii_case(&p))
        });
        if !ok {
            return false;
        }
    }
    if let Some(v) = dom.get_attribute(el, "requiredExtensions") {
        let tokens: Vec<&str> = v.split_ascii_whitespace().collect();
        if tokens.is_empty() || tokens.iter().any(|t| !matches!(*t, "http://www.w3.org/1999/xhtml" | "http://www.w3.org/1998/Math/MathML")) {
            return false;
        }
    }
    true
}

/// Is SVG `child` laid out under SVG `parent`? Structural only; `display` is asked separately.
///
/// Measured on Chrome 150, all inside `A<svg>...</svg>B`:
///
/// ```text
/// <text>TX</text>                               A\nTX\nB     <text> is a block of its own
/// <text>T1<tspan>TS</tspan>T2</text>            A\nT1TST2\nB
/// <text>TX<desc>TD</desc><title>TT</title></text> A\nTX\nB
/// RAW<g>GRAW</g><rect>RECT</rect>               AB           text outside a <text> is not laid out
/// <a href=#>ARAW</a>  <tspan>TSO</tspan>        AB
/// <g> + an HTML <div> appended by script        AB
/// <foreignObject><div>FO1</div><span>FO2</span> A\nFO1\nFO2\nB
/// <switch><text>SW1</text><text>SW2</text>      A\nSW1\nB    the first child that passes its conditions
/// <switch><desc>D</desc><text>1</text>          AB           ...whatever element it is
/// <switch>RAW<text>1</text>                     A\n1\nB      ...and a text node is not a candidate
/// ```
fn svg_child_renders(dom: &korvus_dom::Dom, languages: &[String], parent: u32, ptag: &str, child: u32, child_el: bool) -> bool {
    if ptag == "foreignObject" {
        return !(child_el && is_svg(dom, child) && dom.tag_name(child) != Some("svg"));
    }
    let in_text = matches!(ptag, "text" | "tspan" | "textPath") || (ptag == "a" && svg_inside_text(dom, parent));
    if !child_el {
        return in_text;
    }
    if !is_svg(dom, child) || !svg_conditions_pass(dom, languages, child) {
        return false;
    }
    let ctag = dom.tag_name(child).unwrap_or("");
    if in_text {
        return matches!(ctag, "tspan" | "textPath" | "a");
    }
    // A `<tspan>` or `<textPath>` outside a `<text>` is not laid out (Chrome: `<svg><tspan>TSO</tspan>`
    // reads nothing), and neither is anything under an element that is not a container.
    if !SVG_CONTAINERS.contains(&ptag) || matches!(ctag, "tspan" | "textPath") {
        return false;
    }
    if ptag == "switch" {
        let first = dom.children(parent).into_iter().find(|&c| dom.node_type(c) == 1 && is_svg(dom, c) && svg_conditions_pass(dom, languages, c));
        return first == Some(child);
    }
    true
}

/// Does `child` generate a box when `parent` does? The STRUCTURAL rules -- the ones no computed style
/// answers -- for an element or text child: a `<noscript>` (P11c), the children of a replaced element
/// or of an `<object>` showing its data, and SVG, where only `<text>` content and `<foreignObject>`
/// lay text out. `display` and a shadow host's slots are asked by the caller.
fn child_renders(scope: &mut v8::PinScope, op_state: &OpStateRc, languages: &[String], parent: u32, child: u32) -> bool {
    let object = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let child_el = match dom.node_type(child) {
            1 => true,
            3 => false,
            _ => return false,
        };
        if child_el && page::layout::tree::renders_nothing(dom, child) {
            return false;
        }
        let Some(ptag) = dom.tag_name(parent) else { return true };
        if dom.is_html_ns(parent) {
            let object = ptag.eq_ignore_ascii_case("object");
            if !object && CHILDLESS_HTML.iter().any(|t| ptag.eq_ignore_ascii_case(t)) {
                return false;
            }
            // An SVG element under an HTML one is laid out only if it is an `<svg>` root.
            if child_el && is_svg(dom, child) && dom.tag_name(child) != Some("svg") {
                return false;
            }
            object
        } else if is_svg(dom, parent) {
            return svg_child_renders(dom, languages, parent, ptag, child, child_el);
        } else {
            return true;
        }
    };
    !object || crate::core::html::subresource::object_shows_fallback(scope, parent)
}

/// Is `id` "being rendered" as far as its FLAT-tree ancestors say -- every edge up to the document
/// generates a box? A light child of a shadow host is laid out only in the slot it is assigned to
/// (Chrome: an unslotted child reads its `textContent`, like any element that is not rendered).
fn flat_ancestors_render(scope: &mut v8::PinScope, op_state: &OpStateRc, languages: &[String], id: u32) -> bool {
    let mut cur = id;
    loop {
        enum Up {
            Top,
            ShadowRoot(Option<u32>),
            Slot(Option<u32>),
            Parent(u32),
        }
        let up = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            match dom.parent(cur) {
                None => Up::Top,
                Some(p) if dom.is_shadow_root(p) => Up::ShadowRoot(dom.shadow_host_of(p)),
                Some(p) if dom.shadow_root_of(p).is_some() => Up::Slot(dom.assigned_slot(cur)),
                Some(p) => Up::Parent(p),
            }
        };
        let next = match up {
            Up::Top => return true,
            Up::ShadowRoot(host) => match host {
                Some(h) => h,
                None => return true,
            },
            Up::Slot(slot) => match slot {
                Some(s) => s,
                None => return false,
            },
            Up::Parent(p) => {
                if !child_renders(scope, op_state, languages, p, cur) {
                    return false;
                }
                p
            }
        };
        let element = op_state.borrow().borrow::<HostDom>().0.node_type(next) == 1;
        if element && rendered_as(scope, op_state, next).display == "none" {
            return false;
        }
        cur = next;
    }
}

/// A generated box's contribution: laid out, never written. Measured on Chrome 150:
///
/// ```text
/// li{display:inline} li::after{content:"\a0 · "}, newline between <li>s   CoolidgeMenchesMelisende
/// A<span>B</span> C,  span::after{content:' '}                            ABC    (its space starts the run)
/// A <span>B</span>C,  span::before{content:' '}                           A BC
/// A <i></i> B,  i::before{content:'\2605'} (inline or inline-block)       A  B   (the glyph occupies the line)
/// A <i></i> B,  i::before{content:''}                                     A B
/// A <i></i> B,  i::before{content:'';display:inline-block;width:10px}     A  B
/// A<span>IN</span>B,  span::before{content:'GEN';display:block}           AINB   (no break of its own)
/// <div id=t> MID </div>, #t::before 'PRE ', #t::after ' POST'             "MID "
/// A <span>B</span> C,  span::before{content:'ABS ';position:absolute}     A B C
/// ```
fn push_generated(walk: &TextWalk, node: u32, after: bool, out: &mut Vec<TextItem>) {
    use page::layout::style::BoxLevel;
    let Some((text, level, out_of_flow, preserve)) = walk.generated.get(&(node, after)) else { return };
    if *out_of_flow {
        return; // writes nothing, and the run it interrupts carries on across it
    }
    match level {
        BoxLevel::AtomicInline => out.push(TextItem::Opaque),
        BoxLevel::Block => {
            out.push(TextItem::Gap(0));
            out.push(TextItem::Text { s: if *preserve { text.clone() } else { collapse_ws(text) }, preserve: *preserve, visible: false });
            out.push(TextItem::Gap(0));
        }
        BoxLevel::Inline => {
            out.push(TextItem::Text { s: if *preserve { text.clone() } else { collapse_ws(text) }, preserve: *preserve, visible: false });
        }
    }
}

/// Collect `parent`'s children in the FLAT tree: a shadow host's shadow-root children, anyone else's
/// own.
fn collect_text(scope: &mut v8::PinScope, op_state: &OpStateRc, walk: &TextWalk, parent: u32, out: &mut Vec<TextItem>, ctx: WalkCtx) {
    let (kids, shadow) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        match dom.shadow_root_of(parent) {
            Some(r) => (dom.children(r), Some(r)),
            None => (dom.children(parent), None),
        }
    };
    match shadow {
        None => collect_nodes(scope, op_state, walk, parent, &kids, out, ctx),
        Some(root) => {
            let start = out.len();
            collect_nodes(scope, op_state, walk, parent, &kids, out, WalkCtx { written: root == walk.reading_tree, ..ctx });
            reorder_slotted(&mut out[start..], parent);
        }
    }
}

/// `kids`, whose rules are asked of `parent` -- the element they are children of, or the host whose
/// light children a slot was assigned.
fn collect_nodes(scope: &mut v8::PinScope, op_state: &OpStateRc, walk: &TextWalk, parent: u32, kids: &[u32], out: &mut Vec<TextItem>, ctx: WalkCtx) {
    for &c in kids {
        // A closed `<details>`' content past its summary is laid out in a slot that takes no space:
        // Chrome 150's `document.body.innerText` has no line for it.
        if page::layout::tree::closed_details_content(&op_state.borrow().borrow::<HostDom>().0, parent, c) {
            continue;
        }
        if !child_renders(scope, op_state, &walk.languages, parent, c) {
            continue;
        }
        let nt = op_state.borrow().borrow::<HostDom>().0.node_type(c);
        if nt == 3 {
            let data = op_state.borrow().borrow::<HostDom>().0.node_value(c).unwrap_or_default();
            let s = if ctx.preserve { data } else { collapse_ws(&data) };
            let s = ctx.transform.apply(&s).into_owned();
            out.push(TextItem::Text { s, preserve: ctx.preserve, visible: ctx.visible && ctx.written });
            continue;
        }
        let mut r = rendered_as(scope, op_state, c);
        if r.display == "none" { continue; } // no box: transparent to white-space collapsing too
        if walk.skipped.contains(&c) {
            r.skipped = true;
        }
        struct Kind {
            html_p: bool,
            br: bool,
            pre: bool,
            /// A replaced element or control (`CHILDLESS_HTML`), or an `<object>` showing its data.
            childless: bool,
            /// An `<svg>` under an HTML element: an atomic inline replaced box that still lays its
            /// `<text>` out.
            svg_root: bool,
            /// Any other SVG element: blocks are exactly `<text>` and `<foreignObject>`.
            svg_block: Option<bool>,
            /// A `<slot>` in a shadow tree: the host and the nodes it was assigned.
            slot: Option<(u32, Vec<u32>)>,
        }
        let kind = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            let tag = dom.tag_name(c).unwrap_or("");
            let html = dom.is_html_ns(c);
            let svg = is_svg(dom, c);
            let svg_root = svg && tag == "svg" && !is_svg(dom, parent);
            let slot = dom.assigned_nodes(c).filter(|n| !n.is_empty()).and_then(|n| {
                let root = dom.root_of(c);
                dom.shadow_host_of(root).map(|h| (h, n))
            });
            Kind {
                html_p: html && tag.eq_ignore_ascii_case("p"),
                br: html && tag.eq_ignore_ascii_case("br"),
                pre: html && tag.eq_ignore_ascii_case("pre"),
                childless: html && CHILDLESS_HTML.iter().any(|t| tag.eq_ignore_ascii_case(t)),
                svg_root,
                svg_block: (svg && !svg_root).then(|| matches!(tag, "text" | "foreignObject")),
                slot,
            }
        };
        if kind.br { if r.visible && ctx.written { out.push(TextItem::Break); } continue; }
        // `<object>` is childless only while it shows its data.
        let childless = kind.childless && !(dom_is_object(op_state, c) && crate::core::html::subresource::object_shows_fallback(scope, c));
        let block = match kind.svg_block {
            Some(b) => b,
            None => !INLINE_DISPLAYS.contains(&r.display.as_str()),
        };
        // An atomic inline that is replaced OCCUPIES its line and writes nothing of its own.
        let atomic = !block && (childless || kind.svg_root);
        // A hidden element's box still ends the line, but carries NO required break of its own --
        // `<p style=visibility:hidden>` is `Gap(0)`, not `Gap(2)`. Nor does a box in a tree whose text
        // is not written.
        let gap = if !r.visible || r.skipped || !ctx.written { 0 } else if kind.html_p { 2 } else { 1 };
        // An out-of-flow box is gathered into its own list so the white-space run around it can be
        // put back afterwards. `block` is the right guard and not a second condition: the cascade
        // blockifies every out-of-flow box that generates one, so the only displays that are out of
        // flow and NOT block here are `contents` and `none`, which generate no box to take out of
        // flow. See `TextItem::OutOfFlow`.
        let isolate = block && r.out_of_flow;
        let mut nested: Vec<TextItem> = Vec::new();
        let sink = if isolate { &mut nested } else { &mut *out };
        if block { sink.push(TextItem::Gap(gap)); } else if atomic { sink.push(TextItem::Opaque); }
        if r.skipped {
            if !block && !atomic { sink.push(TextItem::Opaque); }
        } else if !childless {
            let inner = WalkCtx { preserve: ctx.preserve || kind.pre, visible: r.visible, written: ctx.written,
                                  transform: r.transform };
            push_generated(walk, c, false, sink);
            match kind.slot {
                // A slot with assigned nodes lays THEM out, not its own fallback children -- and they
                // are the host's light children, in the host's tree. Chrome 150: a slot's fallback
                // reads `W1W2` once a light child is assigned (whitespace-only text included) and
                // `W1FALLBACKW2` with none.
                Some((host, nodes)) => {
                    let (written, key) = {
                        let st = op_state.borrow();
                        let dom = &st.borrow::<HostDom>().0;
                        let key = dom.children(host).iter().position(|&k| k == nodes[0]).unwrap_or(usize::MAX);
                        (dom.root_of(host) == walk.reading_tree, key)
                    };
                    let mut items = Vec::new();
                    collect_nodes(scope, op_state, walk, host, &nodes, &mut items, WalkCtx { written, ..inner });
                    sink.push(TextItem::Slotted { host, key, items });
                }
                None => collect_text(scope, op_state, walk, c, sink, inner),
            }
            push_generated(walk, c, true, sink);
        }
        if block { sink.push(TextItem::Gap(gap)); } else if atomic && !childless { sink.push(TextItem::Opaque); }
        if isolate { out.push(TextItem::OutOfFlow(nested)); }
    }
}

fn dom_is_object(op_state: &OpStateRc, id: u32) -> bool {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    dom.is_html_ns(id) && dom.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("object"))
}

/// Chrome writes a host's slotted children in LIGHT-tree order, while it lays them out in slot order.
/// Measured, Chrome 150: light `<span slot=a>FIRST</span><span slot=b>SECOND</span>` into shadow
/// `<slot name=b></slot><slot name=a></slot>` reads `FIRSTSECOND`. So the groups keep the places the
/// flat tree gave them -- the white-space around each slot is the slot's -- and take their contents
/// in light order. `host`'s groups only: a nested host has already ordered its own.
fn reorder_slotted(items: &mut [TextItem], host: u32) {
    fn take(items: &mut [TextItem], host: u32, groups: &mut Vec<(usize, Vec<TextItem>)>) {
        for it in items.iter_mut() {
            match it {
                TextItem::Slotted { host: h, key, items: inner } if *h == host => groups.push((*key, std::mem::take(inner))),
                TextItem::Slotted { items: inner, .. } | TextItem::OutOfFlow(inner) => take(inner, host, groups),
                _ => {}
            }
        }
    }
    fn give(items: &mut [TextItem], host: u32, groups: &mut std::vec::IntoIter<(usize, Vec<TextItem>)>) {
        for it in items.iter_mut() {
            match it {
                TextItem::Slotted { host: h, items: inner, .. } if *h == host => {
                    if let Some((_, g)) = groups.next() {
                        *inner = g;
                    }
                }
                TextItem::Slotted { items: inner, .. } | TextItem::OutOfFlow(inner) => give(inner, host, groups),
                _ => {}
            }
        }
    }
    let mut groups = Vec::new();
    take(items, host, &mut groups);
    if groups.is_empty() {
        return;
    }
    groups.sort_by_key(|g| g.0);
    give(items, host, &mut groups.into_iter());
}

fn inner_text_of<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &OpStateRc, this: v8::Local<'s, v8::Object>, id: u32) -> String {
    // "If this element is not being rendered, return textContent" -- a detached element, and anything
    // inside display:none, takes this path in every browser (Chrome, 2026-09-10: a detached
    // `a<br>b<script>x</script><p>c</p>` reads "abxc").
    // A node of a FRAME document is rendered when its frame is -- it is not in the page's
    // tree, and it read as its text content (script source, hidden nodes and all).
    let frame_root = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if dom.is_connected(id) { None } else { Some(dom.shadow_including_root(id)) }
    };
    let connected = match frame_root {
        None => true,
        Some(r) => crate::core::dom::geometry_queries::frame_document_rendered(&mut op_state.borrow_mut(), r),
    };
    // A `<noscript>` generates no box with scripting enabled, so it is "not being
    // rendered" and reads its text content (Chrome 150: the raw `<p id=nsp>no script text</p>`). An
    // element in a closed `<details>`' content IS rendered -- in `::details-content`, whose
    // `content-visibility` is `hidden` -- so its contents are skipped: Chrome reads `""`.
    let (noscript, in_closed_details) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut hidden = false;
        let mut cur = id;
        while let Some(parent) = dom.node(cur).parent {
            if page::layout::tree::closed_details_content(dom, parent, cur) {
                hidden = true;
                break;
            }
            cur = parent;
        }
        (page::layout::tree::renders_nothing(dom, id), hidden)
    };
    if connected && in_closed_details {
        return String::new();
    }
    let languages = crate::core::navigator::navigator::persona(scope).map(|p| p.navigator().languages).unwrap_or_default();
    // An element inside a replaced element's fallback, inside SVG outside a `<text>`, or
    // an unslotted light child of a shadow host is not being rendered either. Chrome 150:
    // `<canvas><p id=t>INCAN</p></canvas>` reads `INCAN`, and so does an unslotted `<span>`.
    let rendered = connected
        && !noscript
        && computed_style(scope, this.into()).map(|st| style_prop(scope, st, "display").as_deref() != Some("none")).unwrap_or(false)
        && flat_ancestors_render(scope, op_state, &languages, id);
    if !rendered {
        return op_state.borrow().borrow::<HostDom>().0.text_content(id);
    }
    // The element innerText is READ ON answers for its own `visibility` and `content-visibility` too:
    // `<div style=content-visibility:hidden>X</div>` reads `""` (NOT textContent -- it IS rendered,
    // its contents are skipped), and `<div style=visibility:hidden>SELF<span
    // style=visibility:visible>CHILD</span></div>` reads `"CHILD"`, so a hidden root drops its own
    // direct text and keeps walking.
    let root = rendered_as(scope, op_state, id);
    if root.skipped { return String::new(); }
    let mut skipped = std::collections::HashSet::new();
    let generated = {
        let mut st = op_state.borrow_mut();
        let mut map = std::collections::HashMap::new();
        match frame_root {
            Some(r) => {
                if crate::core::dom::geometry_queries::refresh_frame(&mut st, r) {
                    if let Some(laid) = crate::core::dom::geometry_queries::frame_laid_out(&st, r) {
                        for (node, after, text, level, out_of_flow, preserve) in laid.generated_boxes() {
                            map.insert((node, after), (text.to_string(), level, out_of_flow, preserve));
                        }
                        skipped.extend(laid.skipped_auto_nodes());
                    }
                }
            }
            None => {
                if crate::core::dom::geometry_queries::refresh_layout_cache(&mut st) {
                    if let Some(crate::core::dom::geometry_queries::LayoutCache(Some((_, laid)))) = st.try_borrow::<crate::core::dom::geometry_queries::LayoutCache>() {
                        for (node, after, text, level, out_of_flow, preserve) in laid.generated_boxes() {
                            map.insert((node, after), (text.to_string(), level, out_of_flow, preserve));
                        }
                        skipped.extend(laid.skipped_auto_nodes());
                    }
                }
            }
        }
        map
    };
    let (reading_tree, pre) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        (dom.root_of(id), dom.is_html_ns(id) && dom.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("pre")))
    };
    // The element read on can be a skipped `auto` box itself.
    if skipped.contains(&id) {
        return String::new();
    }
    let walk = TextWalk { reading_tree, generated, languages, skipped };
    let mut items: Vec<TextItem> = Vec::new();
    push_generated(&walk, id, false, &mut items);
    collect_text(scope, op_state, &walk, id, &mut items,
                 WalkCtx { preserve: pre, visible: root.visible, written: true, transform: root.transform });
    push_generated(&walk, id, true, &mut items);
    // Resolve the items -- `Sink`'s own header says why the trim is not a separate pass. A preserved
    // run goes through whole, because `<pre>` is exactly the case whose spaces must not be counted.
    let mut sink = Sink::new();
    resolve_items(items, &mut sink);
    sink.out
}

/// Run one list of items through the sink. Recursive only for [`TextItem::OutOfFlow`], whose nesting
/// depth is the nesting depth of out-of-flow boxes in the subtree.
fn resolve_items(items: Vec<TextItem>, sink: &mut Sink) {
    for it in items {
        match it {
            TextItem::Gap(n) => sink.gap(n),
            TextItem::Break => sink.hard_break(),
            TextItem::Opaque => sink.occupy(),
            TextItem::Slotted { items: nested, .. } => resolve_items(nested, sink),
            TextItem::OutOfFlow(nested) => {
                // The box writes its characters and its breaks; the white-space run it interrupted
                // carries on afterwards as though it had never been there.
                let saved = sink.open_out_of_flow();
                resolve_items(nested, sink);
                sink.close_out_of_flow(saved);
            }
            TextItem::Text { s, preserve, visible } => {
                if s.is_empty() { continue; }
                if preserve {
                    sink.glyphs(&s, visible);
                } else {
                    for ch in s.chars() {
                        if ch == ' ' { sink.space(visible); } else { sink.glyphs_char(ch, visible); }
                    }
                }
            }
        }
    }
}

fn split_lines(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => { if chars.peek() == Some(&'\n') { chars.next(); } parts.push(std::mem::take(&mut cur)); }
            '\n' => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

/// The nodes `innerText = s` / `outerText = s` build: text nodes with a `<br>` for every newline.
fn text_lines_nodes(op_state: &OpStateRc, s: &str, empty_text_when_blank: bool) -> Vec<u32> {
    let mut st = op_state.borrow_mut();
    let dom = &mut st.borrow_mut::<HostDom>().0;
    let mut out = Vec::new();
    if empty_text_when_blank && s.is_empty() {
        out.push(dom.create_text_node(""));
    }
    for (i, part) in split_lines(s).iter().enumerate() {
        if i > 0 { out.push(dom.create_element("br")); }
        if !part.is_empty() { out.push(dom.create_text_node(part)); }
    }
    out
}

pub(crate) fn cb_he_inner_text_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = inner_text_of(scope, &op_state, this, id);
    let v = str_val(scope, &s);
    rv.set(v);
}
/// Setting replaces the children with the text, with every newline becoming a `<br>` -- that is the
/// difference from `textContent`, and the reason `el.innerText = "a\nb"` renders as two lines.
pub(crate) fn cb_he_inner_text_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let a0 = args.get(0);
    // ([LegacyNullToEmptyString] DOMString or TrustedScript): `null` is "", `undefined` its ToString; a
    // TrustedScript read as itself, nothing on it called
    // a `<script>`'s is a Trusted Types sink ("HTMLScriptElement innerText"), any other element's is not
    let s = match crate::core::trusted_types::tables::html_script_id(scope, args.this()) {
        Some(sid) => {
            let Some(t) = crate::core::trusted_types::tables::script_text_sink(scope, sid, (!a0.is_null()).then_some(a0), "innerText") else { return };
            t
        }
        None if a0.is_null() => String::new(),
        None => {
            let Some(t) = crate::core::trusted_types::tables::sink_text(scope, a0, "TrustedScript") else { return };
            t
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let before: Vec<u32> = op_state.borrow().borrow::<HostDom>().0.children(id);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        for &c in &before { dom.remove_child(id, c); }
    }
    let added = text_lines_nodes(&op_state, &s, false);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        for &n in &added { dom.append_child(id, n); }
    }
    notify_childlist_batch(scope, &op_state, id, &added, &before);
}
/// `outerText` replaces the ELEMENT, so it needs a parent to replace it in (Chrome's
/// NoModificationAllowedError, 2026-09-10).
fn cb_he_outer_text_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let a0 = args.get(0);
    let s = if a0.is_null_or_undefined() { String::new() } else { a0.to_rust_string_lossy(scope) };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let parent = op_state.borrow().borrow::<HostDom>().0.parent(id);
    let Some(parent) = parent else {
        throw_dom(scope, "Failed to set the 'outerText' property on 'HTMLElement': The element has no parent.", "NoModificationAllowedError");
        return;
    };
    let added = text_lines_nodes(&op_state, &s, true);
    {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        for &n in &added { dom.insert_before(parent, n, id); }
        dom.remove_child(parent, id);
    }
    notify_childlist_batch(scope, &op_state, parent, &added, &[id]);
}

// ---- tabIndex / attributeStyleMap / attachInternals / focus / blur ----------------------------

/// An element's `tabIndex` when its `tabindex` attribute is absent or unparseable, as
/// Chrome 150 answers it (measured, `tools/realweb/seed/ti`): 0 for `a`, `area`, `audio`, `video`,
/// `button`, `input`, `select`, `textarea`, `iframe` and `object` -- disabled or not, with or without
/// `href` or `controls` -- for SVG's `a`, and for a `<summary>` that is its `<details>`' summary; -1 for
/// everything else, `contenteditable` and MathML included. korvus answered -1 for all of them, so
/// tailscale.com's consent banner had no tabbable node and its focus-trap threw.
pub(crate) fn default_tab_index(dom: &korvus_dom::Dom, id: u32) -> i32 {
    let Some(tag) = dom.tag_name(id) else { return -1 };
    let local = tag.rsplit(':').next().unwrap_or(tag).to_ascii_lowercase();
    if dom.is_html_ns(id) {
        return match local.as_str() {
            "a" | "area" | "audio" | "video" | "button" | "input" | "select" | "textarea" | "iframe" | "object" => 0,
            "summary" => {
                let parent = dom.node(id).parent;
                let first_summary = parent.filter(|p| dom.tag_name(*p).is_some_and(|t| t.eq_ignore_ascii_case("details"))).and_then(|p| {
                    dom.child_elements(p).into_iter().find(|c| dom.tag_name(*c).is_some_and(|t| t.eq_ignore_ascii_case("summary")))
                });
                if first_summary == Some(id) { 0 } else { -1 }
            }
            _ => -1,
        };
    }
    if matches!(dom.element_ns(id), Some(korvus_dom::ElementNs::Svg)) && local == "a" {
        return 0;
    }
    -1
}

/// The `tabindex` attribute parsed with HTML's integer rules ("\t7" is 7, "5%" is 5, "1.5" is 1);
/// absent or unparseable, the element's own default ([`default_tab_index`]). Shared by
/// `HTMLElement`, `SVGElement` and `MathMLElement` (the `tab_index` kit).
pub(crate) fn cb_he_tab_index_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let n = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.get_attribute(id, "tabindex")
            .and_then(|v| parse_html_integer(v, true))
            .filter(|n| *n >= i32::MIN as i64 && *n <= i32::MAX as i64)
            .map(|n| n as i32)
            .unwrap_or_else(|| default_tab_index(dom, id))
    };
    rv.set_int32(n);
}
/// WebIDL converts to `long` first, so `el.tabIndex = "1"` is an ordinary assignment: ToNumber, then
/// ToInt32 (a non-finite value is 0).
pub(crate) fn cb_he_tab_index_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let raw = args.get(0).number_value(scope).unwrap_or(0.0);
    let n: i32 = if raw.is_finite() { (raw.trunc() as i64) as i32 } else { 0 };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    set_attribute_notify(scope, &op_state, id, "tabindex", &n.to_string());
}

/// The CSS Typed OM view of the inline style attribute. `[SameObject]`, so it is cached per element in
/// a private slot. The object's own METHODS are the StylePropertyMap shells: this engine has no Typed
/// OM, so the map reads and writes nothing; the identity, class and prototype chain are right.
fn cb_he_attribute_style_map_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if let Some(m) = priv_get(scope, this, P_STYLE_MAP) {
        if !m.is_undefined() { rv.set(m); return; }
    }
    // An element's, or nothing: `Object.create(HTMLElement.prototype)` passes the installer's chain check,
    // and Chrome 150.0.7871.46 throws `Illegal invocation` for it (measured).
    if crate::core::dom::element::el_id_of(scope, this).is_none() {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let m: v8::Local<v8::Value> = match crate::core::css::style_property_map::inline_map(scope, this) {
        Some(o) => o.into(),
        None => v8::null(scope).into(),
    };
    priv_set(scope, this, P_STYLE_MAP, m);
    rv.set(m);
}

/// Custom elements only: `document.createElement("div").attachInternals()` is a NotSupportedError, as
/// is a second attach (`element_internals::attach_internals`).
fn cb_he_attach_internals<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if let Some(o) = attach_internals(scope, args.this()) {
        rv.set(o.into());
    }
}

/// `focus()` / `blur()` move the native focus ring that `document.activeElement` reads, and answer
/// `undefined` (the op2 twins answered null: op2 renders `()` as null).
///
/// They used to write `FocusState` and dispatch NOTHING, so no page ever saw a `focus`,
/// `blur`, `focusin` or `focusout` from this engine. `core/events/focus.rs` is the one routine that
/// moves the ring now, and firing the four events in Chrome's order is what it is for. The
/// timeStamp is the page clock, like every other trusted event's.
fn cb_he_focus<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    // `optional FocusOptions options = {}`, converted (Chrome 150): `focusVisible`,
    // `preventScroll`, each read once; a non-object is no dictionary (korvus read nothing)
    if crate::install::ctor_dicts::convert_dict(scope, args.get(0), "FocusOptions", "Failed to execute 'focus' on 'HTMLElement': ").is_err() {
        return;
    }
    let ts = crate::core::events::focus::page_now(scope);
    crate::core::events::focus::focus_element(scope, id, ts);
}
fn cb_he_blur<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_el(scope, &args) else { return };
    let ts = crate::core::events::focus::page_now(scope);
    crate::core::events::focus::blur_element(scope, id, ts);
}

/// The `HTMLElement` rows the kits do not express.
pub fn install_html_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "translate" => row_accessor(scope, proto, member, cb_he_translate_get, cb_he_translate_set),
        "hidden" => row_accessor(scope, proto, member, cb_he_hidden_get, cb_he_hidden_set),
        "draggable" => row_accessor(scope, proto, member, cb_he_draggable_get, cb_he_draggable_set),
        "spellcheck" => row_accessor(scope, proto, member, cb_he_spellcheck_get, cb_he_spellcheck_set),
        "autocapitalize" => { let d = str_val(scope, "autocapitalize"); row_accessor_data(scope, proto, member, cb_he_autocapitalize_get, d, cb_reflect_str_set, d) }
        "editContext" => row_accessor(scope, proto, member, cb_he_edit_context_get, cb_he_edit_context_set),
        "contentEditable" => row_accessor(scope, proto, member, cb_he_content_editable_get, cb_he_content_editable_set),
        "isContentEditable" => row_getter(scope, proto, member, cb_he_is_content_editable_get),
        "offsetParent" => row_getter(scope, proto, member, cb_he_offset_parent_get),
        // The layout metrics, over the page pipeline (core/dom/geometry_queries.rs, W5.0d).
        "offsetTop" | "offsetLeft" | "offsetWidth" | "offsetHeight" => {
            let d: v8::Local<v8::Value> = v8::Integer::new(scope, crate::core::dom::geometry_queries::metric_index(member.name)).into();
            row_getter_data(scope, proto, member, crate::core::dom::geometry_queries::cb_metric, d)
        }
        "popover" => row_accessor(scope, proto, member, cb_he_popover_get, cb_he_popover_set),
        "innerText" => row_accessor(scope, proto, member, cb_he_inner_text_get, cb_he_inner_text_set),
        "outerText" => row_accessor(scope, proto, member, cb_he_inner_text_get, cb_he_outer_text_set),
        "dataset" => row_getter(scope, proto, member, cb_dataset),
        "tabIndex" => row_accessor(scope, proto, member, cb_he_tab_index_get, cb_he_tab_index_set),
        // `[PutForwards=cssText]` (kits::put_forwards).
        "style" => { let d = str_val(scope, "style"); let g = crate::binding::protos::build_fn_data(scope, cb_el_style, "get style", 0, d); crate::install::kits::put_forwards(scope, proto, member, g, "cssText") }
        "attributeStyleMap" => row_getter(scope, proto, member, cb_he_attribute_style_map_get),
        "attachInternals" => row_method(scope, proto, member, cb_he_attach_internals),
        "blur" => row_method(scope, proto, member, cb_he_blur),
        "click" => row_method(scope, proto, member, cb_el_click),
        "focus" => row_method(scope, proto, member, cb_he_focus),
        "hidePopover" => row_method(scope, proto, member, cb_he_hide_popover),
        "showPopover" => row_method(scope, proto, member, cb_he_show_popover),
        "togglePopover" => row_method(scope, proto, member, cb_he_toggle_popover),
        other => debug_assert!(false, "install_html_element: unexpected member {other}"),
    }
}

// The capture's 142 rows, verbatim and in its order. What is deliberately ABSENT: `accessKeyLabel`,
// `autocorrect`, `headingOffset`, `headingReset` are in the webref IDL and NOT on the recorded
// prototype -- the baseline wins.
webapi::interface! {
    "HTMLElement" : "Element" [element, window] {
        ctor "HTMLElement" len 0 = stub,
        attr "title" : Object cfg "title" = reflect_str,
        attr "lang" : Object cfg "lang" = reflect_str,
        attr "translate" : Object = install_html_element,
        attr "dir" : Object cfg "dir|ltr,rtl,auto||" = reflect_enum,
        attr "hidden" : Object nullable = install_html_element,
        attr "inert" : Object cfg "inert" = reflect_bool,
        attr "accessKey" : Object cfg "accesskey" = reflect_str,
        attr "draggable" : Object = install_html_element,
        attr "spellcheck" : Object = install_html_element,
        attr "autocapitalize" : Object cfg "autocapitalize" = install_html_element,
        attr "editContext" : Object nullable = install_html_element,
        attr "contentEditable" : Object = install_html_element,
        attr "enterKeyHint" : Object cfg "enterkeyhint|enter,done,go,next,previous,search,send||" = reflect_enum,
        attr "isContentEditable" : Object readonly = install_html_element,
        attr "inputMode" : Object cfg "inputmode|none,text,tel,url,email,numeric,decimal,search||" = reflect_enum,
        attr "virtualKeyboardPolicy" : Object cfg "virtualkeyboardpolicy|auto,manual||" = reflect_enum,
        attr "offsetParent" : Object readonly nullable = install_html_element,
        attr "offsetTop" : Object readonly = install_html_element,
        attr "offsetLeft" : Object readonly = install_html_element,
        attr "offsetWidth" : Object readonly = install_html_element,
        attr "offsetHeight" : Object readonly = install_html_element,
        attr "popover" : Object nullable = install_html_element,
        attr "innerText" : Object = install_html_element,
        attr "outerText" : Object = install_html_element,
        attr "writingSuggestions" : Object cfg "writingsuggestions|true,false|true|true" = reflect_enum,
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
        attr "dataset" : Object readonly = install_html_element,
        attr "nonce" : Object cfg "nonce" = reflect_str,
        attr "autofocus" : Object cfg "autofocus" = reflect_bool,
        attr "tabIndex" : Object cfg "tabindex|0|" = install_html_element,
        attr "style" : Object = install_html_element,
        attr "attributeStyleMap" : Object readonly = install_html_element,
        method "attachInternals" : Object len 0 = install_html_element,
        method "blur" : Object len 0 = install_html_element,
        method "click" : Object len 0 = install_html_element,
        method "focus" : Object len 0 = install_html_element,
        method "hidePopover" : Object len 0 = install_html_element,
        method "showPopover" : Object len 0 = install_html_element,
        method "togglePopover" : Object len 0 = install_html_element,
        handler "onscrollsnapchange" = event_handler,
        handler "onscrollsnapchanging" = event_handler,
        attr "focusGroup" : Object cfg "focusgroup" = reflect_str,
        attr "focusGroupStart" : Object cfg "focusgroupstart" = reflect_bool,
        handler "onpointerrawupdate" = event_handler
    }
}
