//! `core/html/custom_element_registry.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::binding::protos::get_prop;

// `call_customelements` IS DELETED. It reached `KB.__customElements` -- the last `KB.__`
// bridge slot the window realm had -- to run the bookkeeping the bottom of this file now owns. Every
// body here calls that Rust directly.
fn cb_ce_define(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name_v = args.get(0);
    let ctor_v = args.get(1);
    // Run the bookkeeping: it validates the name, rejects duplicates, resolves any pending whenDefined
    // promise and upgrades the document. It may throw (TypeError/DOMException) and then answers
    // `false` with the exception pending on `scope`, which we propagate by returning early.
    //
    // NO TAG-PROTOTYPE REGISTRATION. This used to put the definition's prototype into
    // `TagProtos`, so every wrapper made for the tag afterwards wore the class whether or not its
    // constructor ever ran: detached `innerHTML`, `cloneNode` and `createContextualFragment` handed out
    // half-built instances, and a template-content clone got a class Chrome never gives it. An element
    // wears its class exactly when it is upgraded, and the constructor puts it there (`super()` retypes
    // to `new.target.prototype`).
    // `optional ElementDefinitionOptions options = {}` is converted with the other arguments, before any step
    // (Chrome 150): `extends` read once; a non-object is no dictionary (korvus read nothing)
    if crate::install::ctor_dicts::convert_dict(scope, args.get(2), "ElementDefinitionOptions", "Failed to execute 'define' on 'CustomElementRegistry': ").is_err() {
        return;
    }
    if !define(scope, name_v, ctor_v) { return; }
    rv.set(v8::undefined(scope).into());
}
fn cb_ce_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match get(scope, args.get(0)) {
        Some(f) => rv.set(f.into()),
        None => rv.set(v8::undefined(scope).into()),
    }
}
fn cb_ce_when_defined(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(p) = when_defined(scope, args.get(0)) { rv.set(p.into()); }
}

// The three the Brave-151 capture has and this prototype did not. All take one REQUIRED argument, so a
// bare call is a WebIDL arity TypeError -- recorded from the browser: `customElements.upgrade()` throws,
// `.length` is 1 for each, and `getName` of an undefined constructor is `null` (not `undefined`).
fn cb_ce_get_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::binding::protos::require_args(scope, &args, 1, "CustomElementRegistry", "getName") { return; }
    // `getName(constructor)`'s argument is a `CustomElementConstructor`, a WebIDL CALLBACK type, so
    // anything not callable fails the conversion with a TypeError BEFORE the body runs -- it is not
    // "a constructor the registry does not know", which is what `null` means. `define` has always
    // made this distinction one screen below; `getName` answered `null` to `undefined`, `null`,
    // `'foo-bar'`, `1`, `{}` and `[]` alike (WPT CustomElementRegistry-getName.html, 2 subtests).
    if v8::Local::<v8::Function>::try_from(args.get(0)).is_err() {
        crate::binding::protos::throw_type_error(scope, "Failed to execute 'getName' on 'CustomElementRegistry': parameter 1 is not of type 'Function'.");
        return;
    }
    match get_name(scope, args.get(0)) {
        Some(n) => { let v = crate::binding::protos::str_val(scope, &n); rv.set(v); }
        None => rv.set_null(),
    }
}

fn cb_ce_upgrade(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::binding::protos::require_args(scope, &args, 1, "CustomElementRegistry", "upgrade") { return; }
    upgrade(scope, args.get(0));
    rv.set_undefined();
}

fn cb_ce_initialize(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::binding::protos::require_args(scope, &args, 1, "CustomElementRegistry", "initialize") { return; }
    initialize(scope, args.get(0));
    rv.set_undefined();
}



// -- The interface, from ONE table ------------------------------------------------
//
// `dom/08g-observers.js` built `CustomElementRegistry` by hand: a JS `function` that threw "Illegal
// constructor", a bare object as its `.prototype`, six `method()` round-trips through the `op_ce_*`
// builders, a `Symbol.toStringTag` accessor, and an `Object.create(CERProto)` singleton written onto
// the window. All of it is the rows below plus [`make_custom_elements`], mounted by THE installer.
//
// The (name -> ctor) registry, the `whenDefined` promises and the upgrade walk are the BOTTOM OF THIS
// FILE. They were `host.js`'s `KB.__customElements`, reached through `call_customelements`
// -- the last `KB.__` bridge slot the window realm had, and the last block of the last JS file in
// `korvus-runtime`.

/// The body every row of [`CUSTOMELEMENTREGISTRY`] names.
pub fn install_custom_element_registry(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::method_row;
    match member.name {
        "define" => method_row(scope, proto, member, cb_ce_define),
        "get" => method_row(scope, proto, member, cb_ce_get),
        "getName" => method_row(scope, proto, member, cb_ce_get_name),
        "upgrade" => method_row(scope, proto, member, cb_ce_upgrade),
        "whenDefined" => method_row(scope, proto, member, cb_ce_when_defined),
        "initialize" => method_row(scope, proto, member, cb_ce_initialize),
        other => debug_assert!(false, "install_custom_element_registry: unexpected member {other}"),
    }
}

/// The `customElements` singleton: a bare object on the table's prototype, which is exactly what
/// `Object.create(CERProto)` produced in the deleted JS. The interface has no public constructor, so
/// nothing else can mint one.
pub fn make_custom_elements<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let obj = crate::install::live::mint_table(scope, "CustomElementRegistry");
    obj
}

// Rows VERBATIM from the `CustomElementRegistry` block of `install/generated_registry.rs`, in the
// capture's order. Window-only: the bundle's `worker.surface.json` does not list the interface.
webapi::interface! {
    "CustomElementRegistry" [value, window] {
        ctor            "CustomElementRegistry" len 0 = stub,
        method          "define"      : Object len 2 required 2          = install_custom_element_registry,
        method          "get"         : Object len 1 required 1          = install_custom_element_registry,
        method          "getName"     : Object len 1 required 1 nullable = install_custom_element_registry,
        method          "upgrade"     : Object len 1 required 1          = install_custom_element_registry,
        method          "whenDefined" : Promise len 1 required 1         = install_custom_element_registry,
        method          "initialize"  : Object len 1 required 1          = install_custom_element_registry
    }
}

// ── THE CUSTOM-ELEMENT REGISTRY, IN RUST ────────────────────────────────────────────
//
// This was `core/korvus-runtime/src/js/host.js`'s `KB.__customElements`, the LAST block of that file
// and the last `KB.__` bridge slot the window realm had: the (name -> constructor) map, the
// `whenDefined` promises, the construction stack the native `HTMLElement` base constructor pops, the
// one-shot upgrade set, and the upgrade walk. `call_customelements` is deleted with it -- the six
// `CustomElementRegistry` rows above, `html_element.rs`'s base constructor and `node.rs`'s connect
// notification call these functions as plain Rust.
//
// THE ALGORITHM IS PORTED, NOT REWRITTEN. The same validation order in `define` (callable, then the
// name production, then the duplicate name, then the duplicate constructor), the same INCLUSIVE
// upgrade walk, the same one-shot upgrade rule, the same push-construct-pop around the definition's
// constructor, the same retype-on-throw fallback, and the same `connected` gate. Those were right,
// and several of them are scars -- each carries the comment that records what it cost to learn.
//
// TWO THINGS CHANGE, and both are mechanism rather than behaviour:
//
//   * the one-shot upgrade set was a JS `WeakSet`; it is a v8 PRIVATE on the element now. A private
//     is invisible to `Object.getOwnPropertyNames` exactly as WeakSet membership was, and it dies
//     with the element rather than with the realm -- which is what the `WeakSet` was chosen for.
//   * `Reflect.construct(ctor, [], ctor)` is `ctor.new_instance(scope, &[])`. V8 uses the callee as
//     `new.target` there, which is the third argument the JS passed explicitly, so the base
//     constructor's `new_target` test sees the same function.

/// The reserved names the custom-element name production excludes (HTML §4.13.2).
const RESERVED: [&str; 8] = [
    "annotation-xml",
    "color-profile",
    "font-face",
    "font-face-src",
    "font-face-uri",
    "font-face-format",
    "font-face-name",
    "missing-glyph",
];

/// The name production, as the JS's `/^[a-z][a-z0-9._-]*-[a-z0-9._-]*$/` plus the reserved list.
///
/// A PRAGMATIC ASCII SUBSET of the spec's `PCENChar`, not full Unicode -- lowercase start, at least
/// one hyphen, and only `[a-z0-9._-]` after it. That was the JS's rule and it is kept exactly; a
/// wider one is its own change with its own recording.
///
/// `pub(crate)` for `Element::attachShadow`, whose step 2 is "a safelisted local name OR a valid
/// custom element name" (CONF ELEM1.5). This is the tree's one custom-element-name production and
/// a second copy of it in `element.rs` would be the very defect this goal exists to remove.
pub(crate) fn valid_name(s: &str) -> bool {
    let mut cs = s.chars();
    let Some(first) = cs.next() else { return false };
    if !first.is_ascii_lowercase() {
        return false;
    }
    let rest: Vec<char> = cs.collect();
    if !rest.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')) {
        return false;
    }
    rest.contains(&'-') && !RESERVED.contains(&s)
}

/// The realm's definitions, pending `whenDefined` promises and construction stack.
///
/// In `OpState`, which is this realm's lifetime -- the same span the module-scoped JS `Map`s had.
/// One entry of the registry: HTML's "custom element definition", minus the parts this engine has
/// no use for (the form-associated flags, the disabled-features set).
///
/// `observed` IS PART OF THE DEFINITION and is read exactly ONCE, at `define` time -- which is a
/// MEASURED fact and not a reading of the spec. Chrome 150, one fixture on `web-platform.test`, a
/// class whose `static get observedAttributes()` counts its own reads: after three `setAttribute`
/// calls the counter reads **1**, and for a class with no `attributeChangedCallback` on its
/// prototype it reads **0** -- the getter is never touched at all. So a page that mutates the list
/// after `define()` changes nothing, and a page that exposes a counting getter sees one read or
/// none. Storing the list here is what makes both true.
#[derive(Clone)]
pub(crate) struct Definition {
    /// The lowercased tag name.
    pub(crate) name: String,
    pub(crate) ctor: v8::Global<v8::Function>,
    /// `observedAttributes`, as read once at `define`; empty when the prototype has no
    /// `attributeChangedCallback`.
    pub(crate) observed: Vec<String>,
}

#[derive(Default)]
pub(crate) struct CustomElements {
    /// The definitions, in DEFINITION order: `getName` and `__defName` walk it and the first
    /// identity match wins, which is the JS `Map`'s iteration order.
    defined: Vec<Definition>,
    /// Lowercased tag name -> the resolver and the promise `whenDefined` handed out.
    pending: Vec<(String, v8::Global<v8::PromiseResolver>, v8::Global<v8::Promise>)>,
    /// Elements being upgraded, awaiting their definition's constructor. The native `HTMLElement`
    /// base constructor pops the top through [`take_construct`] from inside `super()`.
    stack: Vec<v8::Global<v8::Object>>,
}

fn with_ce<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut CustomElements) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<CustomElements>().is_none() {
        st.put(CustomElements::default());
    }
    f(st.borrow_mut::<CustomElements>())
}

/// "This element's upgrade has begun." The HTML spec upgrades an element EXACTLY once; after that its
/// state is "custom" or "failed" and its constructor must never run again.
///
/// Without it, an element revisited by a later walk (a second `define`, an `upgrade(tree)`, or an
/// overlapping candidate set) had its constructor RE-RUN -- which for a class with private fields or
/// methods (React's element base on github.com) throws "Cannot initialize private methods of class E
/// twice on the same object". That scar is why this exists; the JS kept it in a `WeakSet`.
fn upgraded_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, "korvus.ceUpgraded").unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// Whether this realm has no custom element definition at all -- the common page, and a cheap test.
pub(crate) fn nothing_defined(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<CustomElements>().is_none_or(|ce| ce.defined.is_empty())
}

fn is_upgraded(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    let k = upgraded_key(scope);
    el.get_private(scope, k).map(|v| v.is_true()).unwrap_or(false)
}

/// Mark an element's upgrade as begun. Called from [`upgrade_one`] before it runs the constructor,
/// and from the native `HTMLElement` base constructor for an element the CONSTRUCTOR path minted --
/// `document.createElement("x-thing")` and `new XThing()` both produce an element whose constructor
/// has already run, and without this mark the insertion walk ran it a SECOND time. Measured before
/// the fix: `createElement` of a defined tag followed by `appendChild` logged `ctor ctor-end ctor
/// ctor-end`, two constructions of one element, where Chrome logs one. For a class with private
/// fields that second run is not cosmetic -- it throws "Cannot initialize private methods of class
/// E twice on the same object", which is the scar `is_upgraded` was written for in the first place.
pub(crate) fn mark_upgraded(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) {
    let k = upgraded_key(scope);
    let t = v8::Boolean::new(scope, true);
    el.set_private(scope, k, t.into());
}

/// An element's lowercased tag name, or "" for anything without a string `nodeName`.
/// Both read off the arena: an element insertion must not call the page-visible `nodeName` /
/// `nodeType` getters (a page that wraps them saw every `appendChild` read them).
fn tag_of(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> String {
    let Some(id) = crate::page::node_id(scope, el.into()) else { return String::new() };
    crate::page::with_host_dom(scope, |d| if d.node_type(id) == 1 { d.tag_name(id).map(|t| t.to_ascii_lowercase()) } else { None })
        .flatten()
        .unwrap_or_default()
}

fn is_element(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    crate::page::node_type_of(scope, el.into()) == Some(1)
}

fn lookup(scope: &mut v8::PinScope, tag: &str) -> Option<v8::Global<v8::Function>> {
    with_ce(scope, |ce| ce.defined.iter().find(|d| d.name == tag).map(|d| d.ctor.clone()))
}

/// The whole definition for `tag`, which the attribute reaction needs for `observedAttributes`.
fn definition_of(scope: &mut v8::PinScope, tag: &str) -> Option<Definition> {
    with_ce(scope, |ce| ce.defined.iter().find(|d| d.name == tag).cloned())
}

/// The definition whose constructor IS `ctor`, by IDENTITY -- never by `.prototype`.
///
/// Identity only, deliberately: the spec fetches `new_target.prototype` exactly once, LATER, and only
/// after this lookup succeeds, so a counting `Proxy`'s `.prototype` trap must not fire here.
fn name_of_ctor(scope: &mut v8::PinScope, ctor: v8::Local<v8::Value>) -> Option<String> {
    let Ok(f) = v8::Local::<v8::Function>::try_from(ctor) else { return None };
    let defs: Vec<Definition> = with_ce(scope, |ce| ce.defined.clone());
    defs.into_iter().find(|d| v8::Local::new(scope, &d.ctor) == f).map(|d| d.name)
}

/// `observedAttributes` for a definition being added, read the way Chrome reads it.
///
/// TWO THINGS ARE MEASURED HERE, not read off the spec (Chrome 150, `web-platform.test`):
///
/// * the getter is touched **once**, at `define`, and never again -- three later `setAttribute`
///   calls leave a counting `static get observedAttributes()` at `reads === 1`;
/// * with **no `attributeChangedCallback` on the prototype it is not touched at all** --
///   `reads === 0`. The list exists only to filter that callback, so a definition that cannot
///   receive it never asks for the filter.
///
/// The value is taken as an array-like (`length` plus indices). WebIDL's `sequence<DOMString>`
/// conversion is the full iterable protocol; a `Set` or a generator would be accepted there and is
/// not here, which is a narrowing this notes rather than hides.
fn observed_attributes(scope: &mut v8::PinScope, ctor: v8::Local<v8::Function>) -> Vec<String> {
    let proto = get_prop(scope, ctor.into(), "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let has_cb = proto
        .and_then(|p| get_prop(scope, p.into(), "attributeChangedCallback"))
        .map(|v| v8::Local::<v8::Function>::try_from(v).is_ok())
        .unwrap_or(false);
    if !has_cb {
        return Vec::new();
    }
    let Some(list) = get_prop(scope, ctor.into(), "observedAttributes").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        return Vec::new();
    };
    let len = get_prop(scope, list.into(), "length").and_then(|v| v.number_value(scope)).unwrap_or(0.0) as u32;
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        if let Some(v) = list.get_index(scope, i) {
            out.push(v.to_rust_string_lossy(scope));
        }
    }
    out
}

// ── the upgrade walk ────────────────────────────────────────────────────────────────────────────

/// Is `el` an upgrade candidate? `only_tag` `None` = every element whose tag has a definition;
/// `Some(t)` = that tag alone, which is what `define` passes.
fn wanted(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, only_tag: Option<&str>) -> bool {
    let t = tag_of(scope, el);
    match only_tag {
        Some(want) => t == want,
        None => lookup(scope, &t).is_some(),
    }
}

/// Collect the upgrade candidates under `root`, INCLUSIVE of it.
///
/// Inclusive because `customElements.upgrade(el)` on the element itself is the common call.
/// `define`'s old inline version started at the document, where the distinction never showed.
fn candidates(scope: &mut v8::PinScope, root: v8::Local<v8::Object>, only_tag: Option<&str>, out: &mut Vec<v8::Global<v8::Object>>) {
    if is_element(scope, root) && wanted(scope, root, only_tag) {
        let g = v8::Global::new(scope, root);
        out.push(g);
    }
    walk(scope, root, only_tag, out);
}

fn walk(scope: &mut v8::PinScope, node: v8::Local<v8::Object>, only_tag: Option<&str>, out: &mut Vec<v8::Global<v8::Object>>) {
    // `let kids; try { kids = n.childNodes } catch {}` -- a node whose accessor throws is skipped
    // whole, as the JS's `return` did.
    // The realm's own `childNodes` / `length`, never the page's: an import or an upgrade walked the copy
    // through the getters a page may wrap.
    let kids = {
        v8::tc_scope!(let tc, scope);
        let k = crate::binding::protos::pristine_get(tc, node.into(), "Node.childNodes")
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
            .map(|o| v8::Global::new(tc, o));
        tc.reset();
        k
    };
    let Some(kids) = kids else { return };
    let kids = v8::Local::new(scope, &kids);
    let len = crate::binding::protos::pristine_get(scope, kids.into(), "NodeList.length").and_then(|v| v.number_value(scope)).unwrap_or(0.0) as u32;
    for i in 0..len {
        let Some(child) = kids.get_index(scope, i).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { continue };
        if !is_element(scope, child) {
            continue;
        }
        if wanted(scope, child, only_tag) {
            let g = v8::Global::new(scope, child);
            out.push(g);
        }
        walk(scope, child, only_tag, out);
    }
}

/// "Upgrade a tree": walk `root` inclusively and run each candidate's definition constructor ON the
/// existing element.
///
/// Pushing the element makes the native `HTMLElement` base constructor RETURN it from `super()` (see
/// [`take_construct`]), so the subclass's class fields and constructor body initialise the LIVE node
/// -- the difference between `instanceof` passing and Lit's `this.renderOptions` actually existing.
/// This used to be a bare `setPrototypeOf`, which is why a pre-parsed `<lit-element>` upgraded at
/// `define()` time threw "reading 'renderBefore' of undefined".
pub(crate) fn upgrade_tree(scope: &mut v8::PinScope, root: v8::Local<v8::Object>, only_tag: Option<&str>) {
    let mut out: Vec<v8::Global<v8::Object>> = Vec::new();
    candidates(scope, root, only_tag, &mut out);
    upgrade_candidates(scope, out);
}

/// Upgrade each candidate, in order, and run its connected reaction when it is connected.
fn upgrade_candidates(scope: &mut v8::PinScope, out: Vec<v8::Global<v8::Object>>) {
    for g in out {
        let el = v8::Local::new(scope, &g);
        if !upgrade_one(scope, el) {
            continue;
        }
        // `el.isConnected !== false` -- a node that cannot answer is treated as connected, which is
        // the JS's `!==` and not a truthiness test.
        let disconnected = crate::binding::protos::pristine_get(scope, el.into(), "Node.isConnected").map(|v| v.is_false()).unwrap_or(false);
        if !disconnected {
            connected(scope, el.into());
        }
    }
}

/// HTML's "upgrade an element", for ONE element: run its definition's constructor ON the existing
/// node, once. Answers whether this call performed the upgrade -- `false` for an element that was
/// already upgraded or has no definition, so a caller can tell "I upgraded it" from "nothing to do"
/// and not double-fire the reactions that follow.
///
/// SPLIT OUT OF [`upgrade_tree`], and the defect it exists for is this: an element
/// the FRAGMENT PARSER created (`innerHTML`, `insertAdjacentHTML`) reaches JS wearing its
/// definition's PROTOTYPE but with its constructor never run. Measured against Chrome 150 over the
/// same fixture, a class that assigns `this.render` in its constructor and calls it from
/// `connectedCallback`:
///
/// ```text
///                                    korvus                        chrome
///   createElement + appendChild      upgraded, own `render`        same
///   connectedParent.innerHTML        upgraded, NO own `render`     upgraded, own `render`
///   detached innerHTML, appended     upgraded, NO own `render`     upgraded, own `render`
///   insertAdjacentHTML               upgraded, NO own `render`     upgraded, own `render`
/// ```
///
/// A prototype without the constructor's own properties is "a name without a body" at instance
/// scope, and it stayed invisible only while `connectedCallback` never reached those elements.
/// `every-layout.dev` is the measurement: all fifteen of its layout components assign `this.render`
/// in the constructor, so the first `connectedCallback` on a parser-built one raises
/// `TypeError: this.render is not a function`.
pub(crate) fn upgrade_one(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    // Nothing to upgrade to (see [`attribute_changed`]).
    if nothing_defined(scope) {
        return false;
    }
    // Never upgrade the same element twice (the HTML spec's one-shot upgrade).
    if is_upgraded(scope, el) {
        return false;
    }
    let tag = tag_of(scope, el);
    let Some(ctor) = lookup(scope, &tag) else { return false };
    let ctor = v8::Local::new(scope, &ctor);
    // Mark BEFORE running the constructor: the state leaves "undefined" the moment the upgrade
    // begins, so a re-entrant walk during construction does not re-run the ctor.
    mark_upgraded(scope, el);
    let pushed = v8::Global::new(scope, el);
    with_ce(scope, |ce| ce.stack.push(pushed));
    // `None` = constructed; `Some(e)` = threw, with the exception when there was one.
    let failed: Option<Option<(v8::Global<v8::Value>, Option<crate::core::events::report::ErrorSite>)>> = {
        v8::tc_scope!(let tc, scope);
        let ok = ctor.new_instance(tc, &[]).is_some();
        // reported where it was thrown (`report::caught_site`)
        let e = if ok {
            None
        } else {
            Some(tc.exception().map(|x| {
                let m = tc.message();
                (v8::Global::new(tc, x), crate::core::events::report::caught_site(tc, x, m))
            }))
        };
        tc.reset();
        e
    };
    // Pop OUR entry wherever it ended up -- the JS's `indexOf`/`splice`, not a bare `pop`: the
    // constructor may have pushed and popped entries of its own.
    let stack: Vec<v8::Global<v8::Object>> = with_ce(scope, |ce| ce.stack.clone());
    if let Some(i) = stack.iter().position(|x| v8::Local::new(scope, x) == el) {
        with_ce(scope, |ce| {
            if i < ce.stack.len() {
                ce.stack.remove(i);
            }
        });
    }
    if let Some(err) = failed {
        // A constructor that threw (or never called `super()`) leaves the element "failed":
        // retype as a fallback so `instanceof` and the prototype methods still hold, and surface
        // the error.
        let proto = get_prop(scope, ctor.into(), "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
        if let Some(p) = proto {
            let pv: v8::Local<v8::Value> = p.into();
            if el.get_prototype(scope).map(|x| x != pv).unwrap_or(true) {
                el.set_prototype(scope, pv);
            }
        }
        if let Some((err, site)) = err {
            let e = v8::Local::new(scope, &err);
            crate::core::events::report::report_exception_at(scope, e, site);
        }
        // A FAILED upgrade is not custom, so none of its reactions run -- including the replay.
        return true;
    }
    // HTML's upgrade steps replay the element's existing attributes before its
    // `connectedCallback`, which is how a component that reads its configuration in
    // `attributeChangedCallback` ever sees the markup it was parsed with. See [`replay_attributes`]
    // for the measured position.
    replay_attributes(scope, el);
    true
}

/// HTML "create an element", for the elements a creation API just made: an element whose name has a
/// definition is UPGRADED before the API returns (the upgrade reaction its [CEReactions] scope
/// drains) -- when its node document has a browsing context, which here is the page's document. A
/// template's contents, a DOMParser or a `createHTMLDocument` product have none and stay HTMLElement.
///
/// And the creation points are: the fragment parser behind `innerHTML`, `outerHTML`,
/// `insertAdjacentHTML`, `setHTMLUnsafe` and `Range.createContextualFragment`, and DOM's clone steps.
/// FP14 (insertion) and FP16 (`importNode`) had constructed two of them; the rest reached JS half-built,
/// and Lit's `requestUpdate` read `this._$changedProperties.has` off `undefined` on lit.dev, MDN and
/// excalidraw. Chrome 150, measured (`I-ce-upgrade-paths`): detached `innerHTML`, `cloneNode` and
/// `createContextualFragment` are all constructed synchronously; `template.content.cloneNode(true)` is
/// not upgraded at all.
///
/// The candidates come off the arena in tree order, shadow-including, filtered to names that can have
/// a definition (a hyphen), so a call with nothing defined costs one emptiness check.
/// [`upgrade_one`] is one-shot, so an element the insertion walk already upgraded is left alone.
pub(crate) fn upgrade_created(scope: &mut v8::PinScope, roots: &[u32]) {
    if with_ce(scope, |ce| ce.defined.is_empty()) {
        return;
    }
    let Some(&first) = roots.first() else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let page = match crate::core::dom::node::node_document(scope, &op_state, first) {
        None => true,
        Some(d) => crate::binding::protos::document_val(scope, &op_state).is_some_and(|p| p == d),
    };
    if !page {
        return;
    }
    let ids: Vec<u32> = {
        let st = op_state.borrow();
        let dom = &st.borrow::<crate::binding::HostDom>().0;
        let mut out = Vec::new();
        for &r in roots {
            let mut stack = vec![r];
            while let Some(n) = stack.pop() {
                if dom.node_type(n) == 1 && dom.tag_name(n).is_some_and(|t| t.contains('-')) {
                    out.push(n);
                }
                let kids: Vec<u32> = dom.children(n).to_vec();
                stack.extend(kids.into_iter().rev());
                if let Some(sr) = dom.shadow_root_of(n) {
                    stack.push(sr);
                }
            }
        }
        out
    };
    for id in ids {
        let el = crate::binding::protos::wrap_cached(scope, &op_state, id);
        upgrade_one(scope, el);
    }
}

// ── the nine entry points ───────────────────────────────────────────────────────────────────────

/// `customElements.define(name, constructor)`. `false` when it threw.
///
/// The four failures are the JS's, in the JS's order, which is the spec's: a non-callable
/// constructor, then the name production, then the duplicate name, then the duplicate constructor.
pub(crate) fn define(scope: &mut v8::PinScope, name_v: v8::Local<v8::Value>, ctor_v: v8::Local<v8::Value>) -> bool {
    use crate::binding::protos::{throw_dom, throw_type_error};
    if v8::Local::<v8::Function>::try_from(ctor_v).is_err() {
        throw_type_error(scope, "customElements.define: the constructor argument is not a function");
        return false;
    }
    let Ok(ctor) = v8::Local::<v8::Function>::try_from(ctor_v) else { return false };
    let key = name_v.to_rust_string_lossy(scope);
    if !valid_name(&key) {
        throw_dom(scope, &format!("\"{key}\" is not a valid custom element name"), "SyntaxError");
        return false;
    }
    let lower = key.to_ascii_lowercase();
    if lookup(scope, &lower).is_some() {
        throw_dom(scope, &format!("the name \"{key}\" has already been used with this registry"), "NotSupportedError");
        return false;
    }
    if name_of_ctor(scope, ctor_v).is_some() {
        throw_dom(scope, "this constructor has already been used with this registry", "NotSupportedError");
        return false;
    }
    // HTML's "element definition is added" reads the callbacks off the prototype and
    // `observedAttributes` off the constructor HERE, once -- see [`Definition`] for the measurement
    // that says once, and says not at all without an `attributeChangedCallback`.
    let observed = observed_attributes(scope, ctor);
    let g = v8::Global::new(scope, ctor);
    let name = lower.clone();
    with_ce(scope, |ce| ce.defined.push(Definition { name, ctor: g, observed }));
    // Resolve any `whenDefined` promise for this name.
    let resolver = with_ce(scope, |ce| ce.pending.iter().position(|(n, _, _)| *n == lower).map(|i| ce.pending.remove(i).1));
    if let Some(r) = resolver {
        let r = v8::Local::new(scope, &r);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        r.resolve(scope, u);
    }
    // The upgrade pass. Blink runs the enqueued upgrade reactions inside `define`'s own
    // [CEReactions] scope, so in a real browser `connectedCallback` runs SYNCHRONOUSLY during
    // `define()` -- verified against Chrome, and upstream WPT
    // `EventTarget-add-listener-platform-object` depends on it (the listener its connectedCallback
    // registers must exist before the same script's `click()`). This walk is that, synchronously.
    let doc = crate::binding::protos::global_get(scope, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    if let Some(doc) = doc {
        match defined_candidates(scope, doc, &lower) {
            Some(out) => upgrade_candidates(scope, out),
            None => upgrade_tree(scope, doc, Some(&lower)),
        }
    }
    true
}

/// `define()`'s upgrade candidates, found in the arena: the elements of the document's tree, in tree
/// order, whose `nodeName` lowercased is `tag` -- the elements [`candidates`] finds walking
/// `childNodes` from the document, with shadow trees not entered and the walk going down through
/// elements only, as there. `None` when `doc` is not a native document; [`upgrade_tree`] walks then.
///
/// The walk through `childNodes`, `nodeType` and `nodeName` minted a wrapper and ran five getters per
/// node, at every `define()`: www.github.com defines dozens of elements, and it was 13 % of the load's
/// cycles until load (agreed 2026-10-03). It also ran getters a page can
/// replace, which Chrome's upgrade walk does not.
fn defined_candidates(scope: &mut v8::PinScope, doc: v8::Local<v8::Object>, tag: &str) -> Option<Vec<v8::Global<v8::Object>>> {
    use crate::core::dom::document::{doc_root_slot, resolve_doc_root, JsDocument};
    deno_core::cppgc::try_unwrap_cppgc_object::<JsDocument>(scope, doc.into())?;
    let slot = doc_root_slot(scope, doc.into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ids: Vec<u32> = {
        let st = op_state.borrow();
        let dom = &st.try_borrow::<crate::binding::HostDom>()?.0;
        let root = resolve_doc_root(dom, slot);
        let mut out = Vec::new();
        let mut stack: Vec<u32> = dom.children(root).into_iter().rev().collect();
        while let Some(n) = stack.pop() {
            if !matches!(dom.node(n).kind, korvus_dom::NodeKind::Element { .. }) {
                continue;
            }
            // `nodeName` is the tag name, ASCII-uppercased in the HTML namespace of an HTML document;
            // `tag` is ASCII-lowercased.
            if dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case(tag)) {
                out.push(n);
            }
            stack.extend(dom.children(n).into_iter().rev());
        }
        out
    };
    Some(
        ids.into_iter()
            .map(|id| {
                let w = crate::binding::protos::wrap_cached(scope, &op_state, id);
                v8::Global::new(scope, w)
            })
            .collect(),
    )
}

/// `customElements.get(name)`: the definition, or `None` for `undefined`.
pub(crate) fn get<'s>(scope: &mut v8::PinScope<'s, '_>, name_v: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Function>> {
    let tag = name_v.to_rust_string_lossy(scope).to_ascii_lowercase();
    let g = lookup(scope, &tag)?;
    Some(v8::Local::new(scope, &g))
}

/// `customElements.getName(constructor)`: a defined constructor's local name, else NULL.
///
/// Recorded from Brave 151: `customElements.getName(class{})` is `null`, not `undefined`.
pub(crate) fn get_name(scope: &mut v8::PinScope, ctor: v8::Local<v8::Value>) -> Option<String> {
    name_of_ctor(scope, ctor)
}

/// The native base constructor's hook: the local name of the definition whose constructor IS `ctor`,
/// else `None` (which that caller turns into "Illegal constructor").
pub(crate) fn def_name(scope: &mut v8::PinScope, ctor: v8::Local<v8::Value>) -> Option<String> {
    name_of_ctor(scope, ctor)
}

/// The native base constructor's second hook: pop the element being upgraded, so `super()` returns it
/// instead of minting a new node.
///
/// Empty for a plain `new MyEl()` from script (the native ctor then mints, as before). Consumed
/// INSIDE `super()`, before the subclass body runs, so a construction nested in that body correctly
/// sees an empty stack.
pub(crate) fn take_construct<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let g = with_ce(scope, |ce| ce.stack.pop())?;
    Some(v8::Local::new(scope, &g))
}

/// `customElements.upgrade(root)`: walk `root` inclusively and upgrade every element that has a
/// definition. Returns undefined.
///
/// Verified against Brave: `customElements.upgrade(document.createElement("div"))` is a silent no-op
/// rather than a throw -- an element with no definition is simply not a candidate.
pub(crate) fn upgrade(scope: &mut v8::PinScope, root: v8::Local<v8::Value>) {
    let Ok(root) = v8::Local::<v8::Object>::try_from(root) else { return };
    upgrade_tree(scope, root, None);
}

/// `customElements.whenDefined(name)`.
///
/// A name that fails the production gives a REJECTED promise, not a throw -- which is what the JS's
/// `Promise.reject(...)` did and what Chrome answers.
///
/// THE PRODUCTION IS RUN ON THE RAW ARGUMENT, and FIRST -- as [`define`] 90 lines above always did.
/// It used to be handed `raw.to_ascii_lowercase()` instead, and after a lookup, so `whenDefined`
/// answered a DIFFERENT question from `define` for the same string: `"A-element"` lowered to
/// `"a-element"`, passed the production, and either resolved (when `a-element` was defined) or sat
/// pending forever, where HTML §4.13.4 step 1 rejects with a SyntaxError before the registry is
/// consulted at all. 209 subtests of
/// `custom-elements/registries/valid-custom-element-names.html`, which asserts exactly that
/// `define` and `whenDefined` agree on every name.
///
/// The lowercasing survives BELOW the production, for the registry keys, because that is where it
/// is harmless and where [`define`] does it too: a name that passes [`valid_name`] is already
/// all-lowercase (the production admits only `[a-z0-9._-]`), so `lower == raw` from here on and the
/// two entry points key the same map the same way.
pub(crate) fn when_defined<'s>(scope: &mut v8::PinScope<'s, '_>, name_v: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Promise>> {
    let raw = name_v.to_rust_string_lossy(scope);
    if !valid_name(&raw) {
        let resolver = v8::PromiseResolver::new(scope)?;
        let promise = resolver.get_promise(scope);
        // `throw_dom` mints the DOMException by THROWING it; the trap turns that back into a value so
        // it can be the rejection reason instead.
        let err = {
            v8::tc_scope!(let tc, scope);
            crate::binding::protos::throw_dom(tc, &format!("\"{raw}\" is not a valid custom element name"), "SyntaxError");
            let e = tc.exception().map(|e| v8::Global::new(tc, e));
            tc.reset();
            e
        };
        let e: v8::Local<v8::Value> = match err {
            Some(e) => v8::Local::new(scope, &e),
            None => v8::undefined(scope).into(),
        };
        resolver.reject(scope, e);
        return Some(promise);
    }
    // The registry key, derived exactly as `define` derives it (and a no-op after the production).
    let lower = raw.to_ascii_lowercase();
    if lookup(scope, &lower).is_some() {
        let resolver = v8::PromiseResolver::new(scope)?;
        let promise = resolver.get_promise(scope);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        resolver.resolve(scope, u);
        return Some(promise);
    }
    // ONE promise per name: a second `whenDefined("x-y")` answers the SAME promise, as the JS's
    // `pending` map did.
    let existing = with_ce(scope, |ce| ce.pending.iter().find(|(n, _, _)| *n == lower).map(|(_, _, p)| p.clone()));
    if let Some(p) = existing {
        return Some(v8::Local::new(scope, &p));
    }
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let rg = v8::Global::new(scope, resolver);
    let pg = v8::Global::new(scope, promise);
    with_ce(scope, |ce| ce.pending.push((lower, rg, pg)));
    Some(promise)
}

/// `customElements.initialize(root)` associates a tree with a SCOPED registry.
///
/// This engine has one global registry and no scoped ones, so there is nothing to associate and the
/// call is a no-op returning undefined -- which is what Brave answers for a plain tree (verified). It
/// is NOT a stub standing in for scoped-registry support: when scoped registries exist, this is where
/// they attach.
pub(crate) fn initialize(_scope: &mut v8::PinScope, _root: v8::Local<v8::Value>) {}

// ── the four reactions ──────────────────────────────────────────────────────────────────────────
//
// ONE MECHANISM, FOUR NAMES. [`fire`] is the whole of "invoke a custom element reaction" in this
// engine: the element test, the definition test, the late callback lookup and the report-and-carry-on
// on a throw. `connected`, `disconnected`, `adopted` and `attribute_changed` differ only in the name
// they pass and the arguments they build. Anything that grows a second way to reach a page's
// reaction is the two-mechanisms defect this goal has spent its life on.
//
// THE REACTIONS ARE RUN SYNCHRONOUSLY, AND THAT IS MEASURED. HTML drains the reaction queue at the
// end of the outermost [CEReactions] algorithm, and only the BACKUP element queue waits for a
// microtask checkpoint -- so the two are distinguishable from a page. Chrome 150 and korvus, same
// fixture on `web-platform.test`:
//
// ```text
//   log("pre"); host.appendChild(el); log("post"); queueMicrotask(() => log("microtask"))
//   chrome   pre conn post | microtask
//   korvus   pre conn post | microtask
// ```
//
// The reaction lands BEFORE `post`, so the drain is synchronous at the end of the DOM call and a
// queue would be a second mechanism with nothing to buy. The same measurement settles the nested
// case: with two parser-built siblings under one `innerHTML`, Chrome logs
// `ctor:a conn:a during-a:b.built=false ctor:b conn:b` -- each element is upgraded and connected
// before the next one is upgraded at all, which is exactly this walk.

/// Invoke one reaction on `node`, by name.
///
/// The gate is "this element's tag has a definition", not "this element is an instance of it": the
/// callback is looked up on the INSTANCE at call time, as `connected` has always done. (HTML grabs
/// the four callbacks off the prototype at `define` time; the difference shows only for a page that
/// installs a callback after `define`, and keeping one rule for all four is worth more here than
/// matching that corner.)
fn fire(scope: &mut v8::PinScope, node: v8::Local<v8::Value>, name: &str, args: &[v8::Local<v8::Value>]) {
    let Ok(node) = v8::Local::<v8::Object>::try_from(node) else { return };
    if !is_element(scope, node) {
        return;
    }
    let tag = tag_of(scope, node);
    if tag.is_empty() || lookup(scope, &tag).is_none() {
        return;
    }
    let Some(cb) = get_prop(scope, node, name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return;
    };
    let recv: v8::Local<v8::Value> = node.into();
    v8::tc_scope!(let tc, scope);
    cb.call(tc, recv, args);
    if tc.has_caught() {
        match tc.exception() {
            Some(err) => {
                // reported where it was thrown (`report::caught_site`)
                let m = tc.message();
                let site = crate::core::events::report::caught_site(tc, err, m);
                tc.reset();
                crate::core::events::report::report_exception_at(tc, err, site);
            }
            None => tc.reset(),
        }
    }
}

/// The native connect-reaction hook, called for every node freshly attached under a connected parent
/// (`core/dom/node.rs::notify_connected`).
///
/// Only an element whose lowercased tag was `define()`d AND whose instance exposes a
/// `connectedCallback` function receives the call; anything else is a silent no-op.
pub(crate) fn connected(scope: &mut v8::PinScope, node: v8::Local<v8::Value>) {
    fire(scope, node, "connectedCallback", &[]);
}

/// The disconnect half, called for every node a removal DISCONNECTED
/// (`core/dom/node.rs::notify_disconnected`).
///
/// And the reason it is a unit: until it existed, korvus fired `connectedCallback`
/// faithfully and its partner never. A component that is connected and never disconnected leaks its
/// own state -- observers stay registered, timers keep running -- and a framework that pairs setup
/// here with teardown there runs every setup twice on a re-attach and every teardown never. The
/// pairing is now an assertion: `a.appendChild(e); e.remove(); b.appendChild(e); e.remove();
/// a.appendChild(e)` logs `conn disc conn disc conn` on Chrome 150 and here.
pub(crate) fn disconnected(scope: &mut v8::PinScope, node: v8::Local<v8::Value>) {
    fire(scope, node, "disconnectedCallback", &[]);
}

/// `adoptedCallback(oldDocument, newDocument)`, for a node whose node document changed
/// (`core/dom/node.rs::notify_adopted`).
pub(crate) fn adopted(scope: &mut v8::PinScope, node: v8::Local<v8::Value>, old_doc: v8::Local<v8::Value>, new_doc: v8::Local<v8::Value>) {
    fire(scope, node, "adoptedCallback", &[old_doc, new_doc]);
}

/// `attributeChangedCallback(name, oldValue, newValue, namespace)`, for an element that is already
/// CUSTOM (its upgrade has begun) and whose definition observes `name`.
///
/// `old` / `new` are `None` for "the attribute was not there" and reach the page as `null`. The
/// namespace argument is always `null`: this engine's arena has no namespaced attributes, so an
/// answer other than `null` would be an invention.
///
/// WHERE IT FIRES FROM, AND WHY THERE. `core/dom/shared.rs::notify_attributes` -- the one choke
/// point every attribute mutation already reports an `attributes` MutationObserver record through.
/// That is deliberate and it is also honest about its reach: the handful of paths that write the
/// arena directly (`classList`, `style`, `img.width`) bypass the MutationObserver record today and
/// therefore bypass this too. One defect, not two.
pub(crate) fn attribute_changed(scope: &mut v8::PinScope, node: v8::Local<v8::Value>, name: &str, old: Option<String>, new: Option<String>) {
    // No definition, no custom element: every attribute write of a page that defines none paid a
    // private-key build and lookup here (6 % of a `classList.add` loop). A definition is
    // never removed, so an upgraded element implies a non-empty list.
    if nothing_defined(scope) {
        return;
    }
    let Ok(el) = v8::Local::<v8::Object>::try_from(node) else { return };
    // "Custom" means the upgrade has BEGUN -- the same private `upgrade_one` sets before it runs the
    // constructor, which is what lets a `setAttribute` inside that constructor be heard. Measured:
    // Chrome logs `ctor attr:a:N>fromctor ctor-end`, the reaction inside the constructor body.
    if !is_upgraded(scope, el) {
        return;
    }
    let tag = tag_of(scope, el);
    let Some(def) = definition_of(scope, &tag) else { return };
    if !def.observed.iter().any(|a| a == name) {
        return;
    }
    let name_v: v8::Local<v8::Value> = crate::binding::protos::str_val(scope, name);
    let old_v: v8::Local<v8::Value> = match &old {
        Some(s) => crate::binding::protos::str_val(scope, s),
        None => v8::null(scope).into(),
    };
    let new_v: v8::Local<v8::Value> = match &new {
        Some(s) => crate::binding::protos::str_val(scope, s),
        None => v8::null(scope).into(),
    };
    let ns_v: v8::Local<v8::Value> = v8::null(scope).into();
    fire(scope, el.into(), "attributeChangedCallback", &[name_v, old_v, new_v, ns_v]);
}

/// The UPGRADE REPLAY: every attribute the element already carries, in attribute-list order, with a
/// `null` old value, filtered by `observedAttributes`.
///
/// MEASURED POSITION, Chrome 150 over the fixture: an element the fragment parser built with
/// `a="1" zz="9"` and a definition observing `["a"]` alone logs `ctor attr:a:N>1 conn` -- so the
/// replay runs AFTER the constructor returns and BEFORE `connectedCallback`, the unobserved name is
/// not replayed, and the source order of the attributes is kept (`a="1" zz="9" b="2"` replays as
/// `a` then `b`). The same three hold for `customElements.upgrade()` on a detached tree, which logs
/// `ctor attr:a:N>1` and no `conn`.
fn replay_attributes(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) {
    let tag = tag_of(scope, el);
    let Some(def) = definition_of(scope, &tag) else { return };
    if def.observed.is_empty() {
        return;
    }
    let Some(id) = crate::core::dom::element::el_id_of(scope, el.into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let names: Vec<String> = { let st = op_state.borrow(); st.borrow::<crate::binding::HostDom>().0.attribute_names(id) };
    for n in names {
        if !def.observed.iter().any(|a| *a == n) {
            continue;
        }
        let v = { let st = op_state.borrow(); st.borrow::<crate::binding::HostDom>().0.get_attribute(id, &n).map(str::to_string) };
        attribute_changed(scope, el.into(), &n, None, v);
    }
}
