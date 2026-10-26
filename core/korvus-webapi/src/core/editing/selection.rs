//! `core/editing/selection.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use korvus_dom::Dom;
use crate::binding::HostDom;
use crate::core::dom::range::{arg_offset, reject_boundary};
use crate::core::dom::shared::{children_of, cmp_points, collect_text, dom_root, node_type_of, range_point};
use crate::core::events::shared::ON_DOCUMENT_ID;
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::binding::protos::{arg_node_id, get_prop, global_get, int_val, pristine_call, str_val, throw_dom, wrap_cached};

// ── the selection's storage ─────────────────────────────────────────────────────────
//
// The anchor, the focus and the range a page handed to `addRange` are the whole of a Selection's
// state. They used to be OWN PROPERTIES named `_anchor`, `_focus` and `_stored`, written by
// `host.js`'s `class Selection` and read back through `get_prop` here -- so `Object.keys
// (getSelection())` listed three names Chrome does not have, and `JSON.stringify(getSelection())`
// printed the page's own selection back as `{"_anchor":…}`. Three own tells on the one object every
// text-selecting page touches.
//
// They are v8 PRIVATES now: the same store the `Range` boundary slots use, invisible to
// `getOwnPropertyNames`, to `JSON.stringify`, to a `for…in`, and to a page that tries to forge one.

/// Read one of the three state slots. A shadow root's view reads its document's
/// selection, and sees nothing while that selection's anchor or focus is outside the shadow tree.
fn sel_slot<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    if let Some((base, root)) = view_of(scope, this) {
        if !view_sees(scope, base, root) {
            return None;
        }
        return evt_slot_read(scope, base, key).filter(|v| !v.is_null_or_undefined());
    }
    evt_slot_read(scope, this, key).filter(|v| !v.is_null_or_undefined())
}
/// Write one of the three state slots (a shadow root's view writes its document's selection).
fn sel_slot_set(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, key: &str, v: v8::Local<v8::Value>) {
    let target = view_of(scope, this).map(|(base, _)| base).unwrap_or(this);
    evt_slot_write(scope, target, key, v);
}

// ── a shadow root's own Selection ────────────────────────────────────────────────
//
// `ShadowRoot.getSelection()` (Chromium's member) is the shadow root's OWN object -- `!==` the
// document's, the same one on every call -- over the document's one selection state. Measured, stock
// Chrome 150 (tools/realweb/seed/shadowsel): a range in the light tree is invisible through it
// (`rangeCount` 0, `anchorNode` null, `type` "None") while its `toString()` still reads the document
// selection's text; a range inside the shadow tree shows through both. This answered the document's
// Selection itself.

/// The shadow root a view answers for, and the selection it views.
const SEL_VIEW_ROOT: &str = "korvus_sel_view_root";
const SEL_VIEW_BASE: &str = "korvus_sel_view_base";

fn view_of<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<(v8::Local<'s, v8::Object>, u32)> {
    let root = crate::core::css::css_style_declaration::priv_get(scope, this, SEL_VIEW_ROOT)?.uint32_value(scope)?;
    let base = crate::core::css::css_style_declaration::priv_get(scope, this, SEL_VIEW_BASE)
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    Some((base, root))
}

/// Are the base selection's anchor and focus both in the shadow tree `root`?
fn view_sees(scope: &mut v8::PinScope, base: v8::Local<v8::Object>, root: u32) -> bool {
    for k in ["_anchor", "_focus"] {
        let node = evt_slot_read(scope, base, k)
            .and_then(|o| v8::Local::<v8::Object>::try_from(o).ok())
            .and_then(|o| get_prop(scope, o, "node"))
            .and_then(|n| crate::binding::protos::unwrap_id(scope, n));
        let Some(node) = node else { return false };
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if dom_root(&st.borrow::<HostDom>().0, node) != root {
            return false;
        }
    }
    true
}

#[derive(Default)]
struct ShadowSelections(std::cell::RefCell<std::collections::HashMap<u32, v8::Global<v8::Object>>>);

/// The shadow root `root`'s Selection, a view over `base` (its document's), minted once.
pub(crate) fn shadow_selection<'s>(scope: &mut v8::PinScope<'s, '_>, root: u32, base: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let cached = op_state.borrow().try_borrow::<ShadowSelections>().and_then(|m| m.0.borrow().get(&root).cloned());
    if let Some(g) = cached {
        return Some(v8::Local::new(scope, &g));
    }
    let obj = mint_selection(scope)?;
    let r: v8::Local<v8::Value> = v8::Number::new(scope, root as f64).into();
    crate::core::dom::element::priv_set(scope, obj, SEL_VIEW_ROOT, r);
    crate::core::dom::element::priv_set(scope, obj, SEL_VIEW_BASE, base.into());
    let g = v8::Global::new(scope, obj);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<ShadowSelections>().is_none() {
        st.put(ShadowSelections::default());
    }
    st.borrow::<ShadowSelections>().0.borrow_mut().insert(root, g);
    Some(obj)
}
fn nested_prop<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, outer: &str, inner: &str) -> Option<v8::Local<'s, v8::Value>> {
    let o = sel_slot(scope, this, outer)?;
    let obj = v8::Local::<v8::Object>::try_from(o).ok()?;
    get_prop(scope, obj, inner)
}
fn has_prop(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, key: &str) -> bool {
    sel_slot(scope, this, key).is_some()
}
// ── the one selection ───────────────────────────────────────────────────────────────

/// The realm's single `Selection`, held so `getSelection()` answers the SAME object every time --
/// which pages rely on (`getSelection() === getSelection()` is true in every browser, and a library
/// that caches it across a mutation must still be looking at the live one).
///
/// ── OPEN MODEL GAP (SEL1.2): ONE SELECTION PER RUNTIME, WHERE THE SPEC HAS ONE PER DOCUMENT ──────
///
/// This is one `v8::Global` in `OpState`, so there is exactly one `Selection` object for the whole
/// runtime, and every entry point funnels into it: `Window.getSelection` ([`cb_window_get_selection`]),
/// `Document.getSelection` and `ShadowRoot.getSelection` (both `core/dom/document.rs`'s
/// `cb_doc_get_selection`, shared onto ShadowRoot by `core/dom/document_fragment.rs`). Measured over
/// loopback on a page with an iframe:
///
/// ```text
///   f.contentWindow.getSelection() === window.getSelection()     korvus: true    Chrome: false
/// ```
///
/// The spec gives every document its own selection, and `addRange`/`collapse`/`extend` compare the
/// point's root against **the document associated with THIS selection**. With one object there is no
/// "this selection's document" to ask, so [`in_this_document`] can only name the page document, and
/// that is the whole of the gap.
///
/// WHY NO GATE SETTING CAN CLOSE IT. Three sibling WPT files add the SAME range -- one in a shadow
/// tree inside an iframe document `d` -- to three different Selections, and demand two accepts and
/// one reject (`shadow-dom/untriaged/user-interaction/ranges-and-selections/test-001,-002.html`,
/// `.../shadowroot-object/shadowroot-methods/test-004.html`):
///
/// ```text
///   test-001  window.getSelection()        .addRange(r)   toString() must be ''      REJECT
///   test-002  d.defaultView.getSelection() .addRange(r)   toString() must be 'Som'   ACCEPT
///   test-004  s.getSelection()             .addRange(r)   toString() must be 'Som'   ACCEPT
/// ```
///
/// All three receivers are THIS object, so korvus must answer all three the same way, and one of the
/// two answers is always wrong. Measured across SEL1.1 (`9342135e` -> `2a5b14a1`), which changed the
/// gate from "accept everything" to "accept the page document":
///
/// ```text
///                     before   after
///   test-001   0/2  ->  2/2     the reject, now right
///   test-002   2/2  ->  0/2     the accepts, now wrong
///   test-004   1/1  ->  0/1
/// ```
///
/// So the two red rows are not a regression to patch out: widening the gate to "any document" hands
/// back test-001 and re-breaks it, and also re-breaks the ~1,803 `selection/addRange-*` subtests that
/// reject a `foreignPara1`/`detachedPara1` boundary. THE FIX IS THE MODEL, not the gate: key this
/// store by owning document, stamp that document on each `Selection` when it is minted, and have
/// `sel_in_document` compare against the receiver's own document rather than `ON_DOCUMENT_ID`.
/// `core/html/browsing_context.rs::context_of_document` is already the document -> context map that
/// needs, and `cb_doc_get_selection` already half-states the association it cannot finish: its guard
/// (`document.rs`, `doc_root_slot(..) != PAGE_DOC_ROOT`) correctly nulls a `createHTMLDocument`
/// document, but also nulls a FRAME's document, which does have a browsing context and in Chrome does
/// have its own selection -- and a ShadowRoot receiver is not a `JsDocument` at all, so it falls
/// through that same guard's default and is handed the PAGE's selection whatever document it is in.
struct TheSelection(v8::Global<v8::Object>);

/// `window.getSelection()` / `document.getSelection()`'s object, minted on first ask.
///
/// It wears `Selection.prototype` and carries the interface brand, and it is the ONLY object that
/// does: `Selection` has no WebIDL constructor, so there is no other way to obtain one and no way for
/// a page to forge one. `host.js` used to mint it with `new Selection()` at ESM evaluation time,
/// which is why `globalThis.Selection` had to be swapped for a throwing native afterwards.
pub(crate) fn the_selection<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(ts) = op_state.borrow().try_borrow::<TheSelection>() {
        return Some(v8::Local::new(scope, &ts.0));
    }
    let obj = mint_selection(scope)?;
    let g = v8::Global::new(scope, obj);
    op_state.borrow_mut().put(TheSelection(g));
    Some(obj)
}

fn mint_selection<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, "Selection")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    crate::core::events::ctor::brand(scope, obj, "Selection");
    sel_clear(scope, obj);
    Some(obj)
}

/// A FRAME document's own Selection, by the frame document's root. Every document with a browsing
/// context has one (`iframe.contentDocument.getSelection()` is the frame's in Chrome, never null); a frame
/// document answered null, which went unseen while a frame's `contentWindow.document` was wrongly the
/// PAGE's document (an earlier change made it the frame's -- WPT shadow-dom/untriaged getSelection tests).
#[derive(Default)]
struct FrameSelections(std::cell::RefCell<std::collections::HashMap<u32, v8::Global<v8::Object>>>);

/// The document a Selection belongs to: absent for the page's, the frame document's root otherwise.
const SEL_DOC: &str = "korvus_sel_doc";

pub(crate) fn frame_selection<'s>(scope: &mut v8::PinScope<'s, '_>, root: u32) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let cached = op_state.borrow().try_borrow::<FrameSelections>().and_then(|m| m.0.borrow().get(&root).cloned());
    if let Some(g) = cached {
        return Some(v8::Local::new(scope, &g));
    }
    let obj = mint_selection(scope)?;
    let k: v8::Local<v8::Value> = v8::Number::new(scope, root as f64).into();
    crate::core::dom::element::priv_set(scope, obj, SEL_DOC, k);
    let g = v8::Global::new(scope, obj);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<FrameSelections>().is_none() {
        st.put(FrameSelections::default());
    }
    st.borrow::<FrameSelections>().0.borrow_mut().insert(root, g);
    Some(obj)
}

/// The selection of the document whose tree `key` names (`dom_root`'s answer: `ON_DOCUMENT_ID` for the
/// page, a frame document's root otherwise); `None` for a document with no browsing context.
pub(crate) fn selection_for_key<'s>(scope: &mut v8::PinScope<'s, '_>, key: u32) -> Option<v8::Local<'s, v8::Object>> {
    if key == ON_DOCUMENT_ID {
        return the_selection(scope);
    }
    crate::core::html::browsing_context::frame_of_document_root(scope, key)?;
    frame_selection(scope, key)
}

/// Which document's tree this Selection answers for.
fn sel_key(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> u32 {
    let this = view_of(scope, this).map(|(base, _)| base).unwrap_or(this);
    crate::core::css::css_style_declaration::priv_get(scope, this, SEL_DOC).and_then(|v| v.uint32_value(scope)).unwrap_or(ON_DOCUMENT_ID)
}

/// The tree a node belongs to for selection purposes: its root, through shadow hosts.
pub(crate) fn tree_key(dom: &Dom, node_id: u32) -> u32 {
    let mut root = dom_root(dom, node_id);
    for _ in 0..1024 {
        if root == ON_DOCUMENT_ID || !dom.is_shadow_root(root) { return root; }
        match dom.shadow_host_of(root) { Some(h) => root = dom_root(dom, h), None => return root }
    }
    root
}

/// The `Window.getSelection` row's body. `[Global]`, so the receiver is `globalThis` and there is
/// nothing to check on it -- the answer is the realm's selection either way.
pub(crate) fn cb_window_get_selection(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A frame's window object answers its own document's selection.
    let frame_root = crate::core::html::browsing_context::ctx_of_window(scope, args.this())
        .and_then(|ctx| crate::core::html::browsing_context::frame_document_root(scope, ctx));
    if let Some(root) = frame_root {
        match frame_selection(scope, root) {
            Some(s) => rv.set(s.into()),
            None => rv.set_null(),
        }
        return;
    }
    match the_selection(scope) {
        Some(s) => rv.set(s.into()),
        None => rv.set_null(),
    }
}

fn cb_sel_anchor_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match nested_prop(scope, args.this(), "_anchor", "node") { Some(v) => rv.set(v), None => rv.set_null() }
}
fn cb_sel_anchor_offset(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match nested_prop(scope, args.this(), "_anchor", "offset") { Some(v) => rv.set(v), None => rv.set_int32(0) }
}
fn cb_sel_focus_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match nested_prop(scope, args.this(), "_focus", "node") { Some(v) => rv.set(v), None => rv.set_null() }
}
fn cb_sel_focus_offset(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match nested_prop(scope, args.this(), "_focus", "offset") { Some(v) => rv.set(v), None => rv.set_int32(0) }
}
fn cb_sel_range_count(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_int32(if has_prop(scope, args.this(), "_anchor") { 1 } else { 0 });
}

fn sel_points(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<((u32, i64), (u32, i64))> {
    let an = nested_prop(scope, this, "_anchor", "node")?;
    let an_id = arg_node_id(scope, an)?;
    let ao = nested_prop(scope, this, "_anchor", "offset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let fnode = nested_prop(scope, this, "_focus", "node")?;
    let f_id = arg_node_id(scope, fnode)?;
    let fo = nested_prop(scope, this, "_focus", "offset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    Some(((an_id, ao), (f_id, fo)))
}
fn cb_sel_is_collapsed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match sel_points(scope, args.this()) { None => rv.set_bool(true), Some(((a, ao), (f, fo))) => rv.set_bool(a == f && ao == fo) }
}
fn cb_sel_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = match sel_points(scope, args.this()) { None => "None", Some(((a, ao), (f, fo))) => if a == f && ao == fo { "Caret" } else { "Range" } };
    let v = str_val(scope, t); rv.set(v);
}
fn cb_sel_direction(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = match sel_points(scope, args.this()) {
        None => "none",
        Some(((a, ao), (f, fo))) => {
            if a == f && ao == fo { "none" } else {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
                if cmp_points(dom, a, ao, f, fo) <= 0 { "forward" } else { "backward" }
            }
        }
    };
    let v = str_val(scope, d); rv.set(v);
}

fn make_point<'s>(scope: &mut v8::PinScope<'s, '_>, node: v8::Local<v8::Value>, offset: i64) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    let nk = v8::String::new(scope, "node").unwrap(); o.set(scope, nk.into(), node);
    let ok = v8::String::new(scope, "offset").unwrap(); let ov = int_val(scope, offset); o.set(scope, ok.into(), ov);
    o
}
fn sel_set_af(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, an: v8::Local<v8::Value>, ao: i64, fnode: v8::Local<v8::Value>, fo: i64) {
    let a = make_point(scope, an, ao); sel_slot_set(scope, this, "_anchor", a.into());
    let f = make_point(scope, fnode, fo); sel_slot_set(scope, this, "_focus", f.into());
    let n: v8::Local<v8::Value> = v8::null(scope).into(); sel_slot_set(scope, this, "_stored", n);
}
fn sel_clear(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) {
    for k in ["_anchor", "_focus", "_stored"] { let n: v8::Local<v8::Value> = v8::null(scope).into(); sel_slot_set(scope, this, k, n); }
}
/// The document's "has scheduled selectionchange event" flag (Selection API).
#[derive(Default)]
struct SelectionChangeScheduled(bool);

/// Selection API "schedule a selectionchange event" on the document: at most one queued task, the
/// flag set here and cleared at the START of the task that fires, so a listener that moves the
/// selection schedules the next event instead of re-entering itself.
///
/// It was a SYNCHRONOUS dispatch on every mutation. A listener that calls `setPosition` before it
/// counts (WPT `selection/onselectionchange-on-document.html`) recursed without bound, with native
/// frames under every level, overflowing the stack past V8's margin -- `Check failed:
/// IsOnCentralStack()` and a dead process instead of a `RangeError`. The same file asserts the count
/// is still 0 after `setPosition` returns and 1 once a task has run, which only the queue gives.
fn sel_notify(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<SelectionChangeScheduled>().is_none() {
            st.put(SelectionChangeScheduled::default());
        }
        let flag = st.borrow_mut::<SelectionChangeScheduled>();
        if flag.0 {
            return;
        }
        flag.0 = true;
    }
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_fire_selectionchange, 0, 0.0);
}

/// "Fire a selectionchange event" at the document: the flag first, then a trusted, non-bubbling event.
fn cb_fire_selectionchange(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(flag) = op_state.borrow_mut().try_borrow_mut::<SelectionChangeScheduled>() {
        flag.0 = false;
    }
    let doc = match global_get(scope, "document") { Some(d) => d, None => return };
    let ctor = match crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) { Some(c) => c, None => return };
    let ty = str_val(scope, "selectionchange");
    if let Some(ev) = ctor.new_instance(scope, &[ty]) {
        crate::core::html::html_dialog_element::mark_trusted(scope, ev);
        let evv: v8::Local<v8::Value> = ev.into();
        crate::binding::protos::engine_call(scope, doc, "dispatchEvent", &[evv]);
    }
}
fn cb_sel_remove_all(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    sel_clear(scope, args.this()); sel_notify(scope);
}
// ── the boundary point every setter shares (SEL1) ───────────────────────────────────────────────
//
// `collapse`, `setPosition`, `extend`, `selectAllChildren`, `setBaseAndExtent` and `addRange` all
// take a boundary point, and the Selection API puts the SAME two gates in front of every one of
// them before a single slot is written:
//
//   1. THE POINT MUST BE LEGAL. A DocumentType is never a boundary container (InvalidNodeTypeError)
//      and the offset may not exceed the node's length (IndexSizeError). That is DOM §4.4 "set the
//      start or end of a range", already written once as `core/dom/range.rs::reject_boundary`, and
//      it is the same code Chrome runs here: Blink's `DomSelection::setBaseAndExtent` calls
//      `Range::CheckNodeWOffset`, and `DomSelection::selectAllChildren` is literally
//      `setBaseAndExtent(n, 0, n, n->CountChildren())`
//      (`third_party/blink/renderer/core/editing/dom_selection.cc:408,872`). So this lane PROMOTES
//      `reject_boundary` and `arg_offset` to `pub(crate)` rather than writing a second subset of
//      the same algorithm.
//   2. THE NODE MUST BE IN THIS DOCUMENT. A point outside the page tree is not an error: the member
//      must do NOTHING -- no write, no `selectionchange`, `rangeCount` unmoved.
//
// Not one of the six performed either check. Measured on this tree before the fix, over loopback:
// `collapse(text, 999)` left `anchorOffset` 999, `collapse(text, -1)` left it -1,
// `collapse(document.doctype, 0)` selected the doctype, `extend(detachedText, 0)` moved the focus
// into a node in no document, and `setBaseAndExtent(t, 0, null, 0)` stored `null` as the focusNode.
//
// The offset is a WebIDL `unsigned long`, so it arrives through `arg_offset` (ToUint32) and NOT
// through `integer_value`: `collapse(text, -1)` IS `collapse(text, 4294967295)`, which is an
// IndexSizeError because it is past the end, not because it is negative -- and `collapse(text,
// 65536 * 65536 + 2)` is the perfectly legal `collapse(text, 2)`.

/// Gate 2: is `node_id` SHADOW-INCLUDING inside the page document?
///
/// THE PAGE document, not "this Selection's document", and that is a known shortfall rather than a
/// choice -- see the model gap recorded on [`TheSelection`]. Do not "fix" a red shadow-dom or frame
/// selection row by widening this to any document node: the three-file trade documented there shows
/// the widening is measured to break more than it mends.
///
/// Shadow-including, not a plain `dom_root(..) == ON_DOCUMENT_ID`: korvus parents a shadow root to
/// nothing, so the light-tree root of a node inside `host.attachShadow()` is that shadow root. A
/// plain root test would have called every shadow-tree boundary foreign and turned `collapse` into
/// a shadow tree into a silent no-op -- swapping one silent wrong answer for another.
fn in_this_document(dom: &Dom, node_id: u32, key: u32) -> bool {
    tree_key(dom, node_id) == key && (key == ON_DOCUMENT_ID || !dom.is_shadow_root(key))
}
fn sel_in_document(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, key: u32, node_id: u32) -> bool {
    let st = op_state.borrow();
    in_this_document(&st.borrow::<HostDom>().0, node_id, key)
}

/// The WebIDL `Node` conversion for a NON-nullable `Selection` argument.
///
/// `setBaseAndExtent(title, 0, null, 0)` is a `TypeError` -- both node parameters are plain `Node`
/// in the IDL, and Blink says so explicitly (`dom_selection.cc:379-390`, "anchorNode is null" /
/// "focusNode is null"). This used to store the `null` as the focusNode and report a `rangeCount`
/// of 1 for a selection with no focus node at all.
///
/// Only null/undefined is rejected here. An object that is not one of korvus's wrapped nodes -- an
/// `Attr`, notably -- keeps today's silent no-op: that is `range.rs::required_node_id`'s open
/// finding, it needs the same engine-wide decision about how an Attr is identified, and a second
/// guess bolted on here would be a different lie, not a fix.
fn required_sel_node(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, method: &str, which: u32) -> Option<u32> {
    if let Some(id) = arg_node_id(scope, v) { return Some(id); }
    if v.is_null_or_undefined() {
        let msg = format!("Failed to execute '{method}' on 'Selection': parameter {which} is not of type 'Node'.");
        // Blink's context applied after the error is made
        crate::binding::protos::throw_type_error(scope, &msg);
    }
    None
}

/// `Selection.collapse(node, offset)` -- and `setPosition`, which is the same row under a second
/// name (see the dispatch in [`install_selection`]).
///
/// Selection API "collapse", in the spec's order, which is also the order
/// `selection/collapse.js` asserts in: null empties the selection, then the boundary gate (doctype
/// before offset), then the document gate, and only then the write.
fn cb_sel_collapse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let node = args.get(0);
    if node.is_null_or_undefined() { sel_clear(scope, this); sel_notify(scope); return; }
    let node_id = match arg_node_id(scope, node) { Some(i) => i, None => return };
    let offset = arg_offset(scope, args.get(1));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if reject_boundary(scope, &op_state, node_id, offset) { return; }
    if !sel_in_document(&op_state, sel_key(scope, this), node_id) { return; }
    sel_set_af(scope, this, node, offset, node, offset);
    sel_notify(scope);
}
/// `Selection.setBaseAndExtent(anchorNode, anchorOffset, focusNode, focusOffset)`.
///
/// Both points go through the SAME gate, in the order Blink runs them
/// (`dom_selection.cc:379-414`): the null check on each node, then `CheckNodeWOffset` on the anchor,
/// then on the focus, then the document gate on both. The doctype half of the boundary gate is not
/// in the Selection API's prose for this member, but it IS what Chrome does, because Chrome reaches
/// it through `Range::CheckNodeWOffset` -- the same function `Range.setStart` uses.
fn cb_sel_set_base_and_extent(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let an = args.get(0); let fnode = args.get(2);
    let a_id = match required_sel_node(scope, an, "setBaseAndExtent", 1) { Some(i) => i, None => return };
    let f_id = match required_sel_node(scope, fnode, "setBaseAndExtent", 3) { Some(i) => i, None => return };
    let ao = arg_offset(scope, args.get(1));
    let fo = arg_offset(scope, args.get(3));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if reject_boundary(scope, &op_state, a_id, ao) { return; }
    if reject_boundary(scope, &op_state, f_id, fo) { return; }
    if !sel_in_document(&op_state, sel_key(scope, this), a_id) || !sel_in_document(&op_state, sel_key(scope, this), f_id) { return; }
    sel_set_af(scope, this, an, ao, fnode, fo);
    sel_notify(scope);
}
/// `Selection.selectAllChildren(node)`: anchor at (node, 0), focus at (node, node's CHILD COUNT).
///
/// The child count, not the node's DOM **length**. For every node but one they are the same number,
/// and the one is a CharacterData node, whose length is its data length: this used to call
/// `shared::node_len` and so answered 17 for `selectAllChildren(someTextNode)` where Chrome
/// answers 0 -- `selectAllChildren.html`'s "focusOffset must be the given node's number of
/// children" against every text node in the corpus. `node_len` is right for its OTHER callers (it
/// IS §4.4 "length", which is what a Range boundary is measured against), so the fix is to ask a
/// different question here -- `children_of`, which also knows the `ON_DOCUMENT_ID` convention --
/// not to redefine `node_len` for everyone.
fn cb_sel_select_all_children(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let node = args.get(0);
    let node_id = match required_sel_node(scope, node, "selectAllChildren", 1) { Some(i) => i, None => return };
    let key = sel_key(scope, this);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (nt, kids, inside) = {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        (node_type_of(dom, node_id), children_of(dom, node_id).len() as i64, in_this_document(dom, node_id, key))
    };
    if nt == 10 {
        throw_dom(scope, "The node provided is of type DocumentType, which may not be used as a boundary point container.", "InvalidNodeTypeError");
        return;
    }
    if !inside { return; }
    sel_set_af(scope, this, node, 0, node, kids);
    sel_notify(scope);
}
/// `Selection.extend(node, offset)`.
///
/// The gates are the same two, but THE ORDER IS NOT `collapse`'s: the document gate comes FIRST,
/// ahead of the empty-selection check, so `extend(nodeInNoDocument, 0)` on an empty selection does
/// nothing at all rather than throwing InvalidStateError. That is the Selection API's step 1, and
/// `selection/extend.js` asserts it in that order (`document.contains` before the
/// `rangeCount == 0` case). This used to reach the InvalidStateError first.
fn cb_sel_extend(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let node = args.get(0);
    let node_id = match required_sel_node(scope, node, "extend", 1) { Some(i) => i, None => return };
    let offset = arg_offset(scope, args.get(1));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !sel_in_document(&op_state, sel_key(scope, this), node_id) { return; }
    if !has_prop(scope, this, "_anchor") { throw_dom(scope, "There is no selection to extend.", "InvalidStateError"); return; }
    if reject_boundary(scope, &op_state, node_id, offset) { return; }
    let f = make_point(scope, node, offset); sel_slot_set(scope, this, "_focus", f.into());
    let n: v8::Local<v8::Value> = v8::null(scope).into(); sel_slot_set(scope, this, "_stored", n);
    sel_notify(scope);
}

fn sel_make_range<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let ((a, ao), (f, fo)) = sel_points(scope, this)?;
    let an_node = nested_prop(scope, this, "_anchor", "node")?;
    let f_node = nested_prop(scope, this, "_focus", "node")?;
    let fwd = { let op_state = deno_core::JsRuntime::op_state_from(scope); let st = op_state.borrow(); cmp_points(&st.borrow::<HostDom>().0, a, ao, f, fo) <= 0 };
    let (sn, so, en, eo) = if fwd { (an_node, ao, f_node, fo) } else { (f_node, fo, an_node, ao) };
    let ctor = crate::binding::protos::iface_object(scope, "Range").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let r = ctor.new_instance(scope, &[])?;
    let rv: v8::Local<v8::Value> = r.into();
    // the realm's own Range steps, never what a page put on `Range.prototype`
    let so_v = int_val(scope, so); pristine_call(scope, rv, "Range.setStart", &[sn, so_v]);
    let eo_v = int_val(scope, eo); pristine_call(scope, rv, "Range.setEnd", &[en, eo_v]);
    Some(r)
}
fn cb_sel_get_range_at(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let i = args.get(0).integer_value(scope).unwrap_or(0);
    if i != 0 || !has_prop(scope, this, "_anchor") { throw_dom(scope, "index out of range", "IndexSizeError"); return; }
    if let Some(s) = sel_slot(scope, this, "_stored") { rv.set(s); return; }
    if let Some(r) = sel_make_range(scope, this) { rv.set(r.into()); }
}
fn cb_sel_to_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // A shadow root's view reads its document selection's text whatever tree it is in (Chrome 150).
    let this = view_of(scope, args.this()).map(|(base, _)| base).unwrap_or(args.this());
    match sel_make_range(scope, this) {
        Some(r) => { let rv2: v8::Local<v8::Value> = r.into(); if let Some(s) = pristine_call(scope, rv2, "Range.toString", &[]) { rv.set(s); } }
        None => { let e = str_val(scope, ""); rv.set(e); }
    }
}
fn cb_sel_get_composed_ranges(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match sel_make_range(scope, args.this()) {
        Some(r) => { let arr = v8::Array::new(scope, 1); arr.set_index(scope, 0, r.into()); rv.set(arr.into()); }
        None => { let arr = v8::Array::new(scope, 0); rv.set(arr.into()); }
    }
}
fn sel_collapse_to(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, end: bool) {
    let method = if end { "collapseToEnd" } else { "collapseToStart" };
    let r = match sel_make_range(scope, this) {
        Some(r) => r,
        None => {
            // Chrome's words.
            throw_dom(scope, &format!("Failed to execute '{method}' on 'Selection': there is no selection."), "InvalidStateError");
            return;
        }
    };
    let (ck, ok) = if end { ("endContainer", "endOffset") } else { ("startContainer", "startOffset") };
    let node = match evt_slot_read(scope, r, ck) { Some(v) => v, None => return };
    let off = evt_slot_read(scope, r, ok).and_then(|v| v.integer_value(scope)).unwrap_or(0);
    sel_set_af(scope, this, node, off, node, off);
    sel_notify(scope);
}
fn cb_sel_collapse_to_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { sel_collapse_to(scope, args.this(), false); }
fn cb_sel_collapse_to_end(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { sel_collapse_to(scope, args.this(), true); }
fn cb_sel_delete_from_document(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let r = match sel_make_range(scope, this) { Some(r) => r, None => return };
    let rv2: v8::Local<v8::Value> = r.into();
    pristine_call(scope, rv2, "Range.deleteContents", &[]);
    let node = match evt_slot_read(scope, r, "startContainer") { Some(v) => v, None => return };
    let off = evt_slot_read(scope, r, "startOffset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    sel_set_af(scope, this, node, off, node, off);
    sel_notify(scope);
}
/// `Selection.addRange(range)`. The document gate again, and it comes BEFORE the "already has a
/// range" test (Selection API step 1, then step 2): a range whose endpoints are outside the page
/// tree is added to nothing, and `rangeCount` stays 0. The offsets do NOT go through `arg_offset`
/// here -- they are read off a `Range`, which validated them when they were set.
fn cb_sel_add_range(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let range = match v8::Local::<v8::Object>::try_from(args.get(0)) { Ok(o) => o, Err(_) => return };
    // the range's boundary off its own slots, never through `AbstractRange`'s getters a page may wrap
    let sc = match evt_slot_read(scope, range, "startContainer") { Some(v) => v, None => return };
    let so = evt_slot_read(scope, range, "startOffset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let ec = match evt_slot_read(scope, range, "endContainer") { Some(v) => v, None => return };
    let eo = evt_slot_read(scope, range, "endOffset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let (sc_id, ec_id) = (arg_node_id(scope, sc), arg_node_id(scope, ec));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match (sc_id, ec_id) {
        (Some(a), Some(b)) if sel_in_document(&op_state, sel_key(scope, this), a) && sel_in_document(&op_state, sel_key(scope, this), b) => {}
        _ => return,
    }
    if has_prop(scope, this, "_anchor") { return; }
    let a = make_point(scope, sc, so); sel_slot_set(scope, this, "_anchor", a.into());
    let f = make_point(scope, ec, eo); sel_slot_set(scope, this, "_focus", f.into());
    let rv2: v8::Local<v8::Value> = range.into(); sel_slot_set(scope, this, "_stored", rv2);
    sel_notify(scope);
}
fn cb_sel_remove_range(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let range = match v8::Local::<v8::Object>::try_from(args.get(0)) { Ok(o) => o, Err(_) => return };
    if let Some(stored) = sel_slot(scope, this, "_stored") {
        if let Ok(stored_obj) = v8::Local::<v8::Object>::try_from(stored) {
            let rsc = evt_slot_read(scope, range, "startContainer").and_then(|v| arg_node_id(scope, v));
            let ssc = evt_slot_read(scope, stored_obj, "startContainer").and_then(|v| arg_node_id(scope, v));
            if rsc != ssc { throw_dom(scope, "range not in selection", "NotFoundError"); return; }
        }
    }
    sel_clear(scope, this); sel_notify(scope);
}
fn cb_sel_contains_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let node_id = match arg_node_id(scope, args.get(0)) { Some(i) => i, None => { rv.set_bool(false); return; } };
    let allow_partial = args.get(1).boolean_value(scope);
    let r = match sel_make_range(scope, this) { Some(r) => r, None => { rv.set_bool(false); return; } };
    let (rsc, rso) = match range_point(scope, r, "startContainer", "startOffset") { Some(p) => p, None => { rv.set_bool(false); return; } };
    let (rec, reo) = match range_point(scope, r, "endContainer", "endOffset") { Some(p) => p, None => { rv.set_bool(false); return; } };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
    let p = match dom.parent(node_id).filter(|&p| dom.node_type(p) == 1) { Some(p) => p, None => { drop(st); rv.set_bool(false); return; } };
    let i = dom.children(p).iter().position(|&c| c == node_id).unwrap_or(0) as i64;
    let result = if allow_partial {
        cmp_points(dom, p, i, rec, reo) < 0 && cmp_points(dom, p, i + 1, rsc, rso) > 0
    } else {
        cmp_points(dom, p, i, rsc, rso) >= 0 && cmp_points(dom, p, i + 1, rec, reo) <= 0
    };
    rv.set_bool(result);
}

fn first_text_in(dom: &Dom, n: u32) -> Option<u32> {
    if dom.node_type(n) == 3 { return Some(n); }
    for c in dom.children(n) { if let Some(r) = first_text_in(dom, c) { return Some(r); } }
    None
}
fn next_text_after(dom: &Dom, el: u32) -> Option<u32> {
    let mut cur = el;
    loop {
        let p = dom.parent(cur)?;
        let sibs = dom.children(p);
        let idx = sibs.iter().position(|&c| c == cur)?;
        for &sib in &sibs[idx + 1..] { if let Some(t) = first_text_in(dom, sib) { return Some(t); } }
        cur = p;
    }
}
fn is_ws16(u: u16) -> bool {
    matches!(u, 0x09..=0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}
fn cb_sel_modify(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let focus_node = match nested_prop(scope, this, "_focus", "node") { Some(v) => v, None => return };
    let focus_id = match arg_node_id(scope, focus_node) { Some(i) => i, None => return };
    let focus_off = nested_prop(scope, this, "_focus", "offset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let alter = args.get(0).to_rust_string_lossy(scope);
    let dir = args.get(1).to_rust_string_lossy(scope);
    let granularity = args.get(2).to_rust_string_lossy(scope);
    let forward = dir == "forward" || dir == "right";
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let resolved: Option<(u32, i64)> = {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let root = dom.get_elements_by_tag_name("html").first().copied()
            .or_else(|| dom.get_elements_by_tag_name("body").first().copied())
            .unwrap_or_else(|| dom.root());
        let mut texts: Vec<u32> = Vec::new();
        collect_text(dom, root, &mut texts);
        let pos = |id: u32| -> i64 { texts.iter().position(|&t| t == id).map(|i| i as i64).unwrap_or(-1) };
        let (mut ti, mut to): (i64, i64) = if dom.node_type(focus_id) == 3 {
            (pos(focus_id), focus_off)
        } else {
            let kids = dom.children(focus_id);
            let mut tti = -1i64;
            if focus_off >= 0 { if let Some(&target) = kids.get(focus_off as usize) {
                if let Some(txt) = first_text_in(dom, target) { tti = pos(txt); }
            } }
            if tti < 0 { if let Some(txt) = next_text_after(dom, focus_id) { tti = pos(txt); } }
            (tti, 0)
        };
        if ti < 0 || texts.is_empty() { None } else {
            let n = texts.len() as i64;
            let val_of = |i: i64| -> Vec<u16> { dom.node_value(texts[i as usize]).unwrap_or_default().encode_utf16().collect() };
            match granularity.as_str() {
                "character" => {
                    let len = val_of(ti).len() as i64;
                    if forward { if to < len { to += 1; } else if ti + 1 < n { ti += 1; to = 1; } }
                    else if to > 0 { to -= 1; } else if ti > 0 { ti -= 1; to = (val_of(ti).len() as i64 - 1).max(0); }
                }
                "word" => {
                    let s = val_of(ti); let l = s.len() as i64;
                    if forward {
                        while to < l && is_ws16(s[to as usize]) { to += 1; }
                        while to < l && !is_ws16(s[to as usize]) { to += 1; }
                    } else {
                        while to > 0 && is_ws16(s[(to - 1) as usize]) { to -= 1; }
                        while to > 0 && !is_ws16(s[(to - 1) as usize]) { to -= 1; }
                    }
                }
                "lineboundary" | "documentboundary" => { to = if forward { val_of(ti).len() as i64 } else { 0 }; }
                _ => {}
            }
            Some((texts[ti as usize], to))
        }
    };
    let (new_text_id, to_final) = match resolved { Some(p) => p, None => return };
    let wrapped = wrap_cached(scope, &op_state, new_text_id);
    let wv: v8::Local<v8::Value> = wrapped.into();
    if alter == "extend" {
        let f = make_point(scope, wv, to_final); sel_slot_set(scope, this, "_focus", f.into());
        let nn: v8::Local<v8::Value> = v8::null(scope).into(); sel_slot_set(scope, this, "_stored", nn);
    } else {
        sel_set_af(scope, this, wv, to_final, wv, to_final);
        sel_notify(scope);
    }
}


// ── the one table ───────────────────────────────────────────────────────────
//
// `Selection` was 25 of its 29 prototype rows installed by `js/dom/08i-range.js` -- eight
// `Object.defineProperty` getters and seventeen `method()` calls over 23 `op_sel_*` factory ops --
// with the remaining four (`baseNode`, `baseOffset`, `extentNode`, `extentOffset`) left to the
// generated registry's D2 stub. The bodies never moved: the `cb_sel_*` callbacks above ARE those
// bodies, and the table names them directly, so the 23 op builders are gone with the file.
//
// The CLASS stays in `core/korvus-runtime/src/js/host.js` until W4.b -- it is the single instance
// `getSelection()` hands out, and its `_anchor`/`_focus`/`_stored` properties are the storage every
// callback above reads. What changes hands is the PROTOTYPE, taken by
// `installer::overwrite_prototype` from `install::live::install_window_ranges`, which also moves
// `constructor` to LAST where Chrome 150 has it (measured over loopback).

/// The bodies the [`SELECTION`] rows name. One `match` over the row's name; an unexpected name is a
/// debug assertion, never a silent skip.
pub fn install_selection(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{branded_getter_row, branded_method_row, Brand};
    // Nested generic `fn`s rather than closures: each `cb_*` is its own fn ITEM type, and a
    // closure would unify to whichever one called it first.
    fn getter_row<F: v8::MapFnTo<v8::FunctionCallback>>(s: &mut v8::PinScope, p: v8::Local<v8::Object>, m: &'static crate::install::MemberDef, cb: F) {
        branded_getter_row(s, p, m, cb, Brand::Selection)
    }
    fn method_row<F: v8::MapFnTo<v8::FunctionCallback>>(s: &mut v8::PinScope, p: v8::Local<v8::Object>, m: &'static crate::install::MemberDef, cb: F) {
        branded_method_row(s, p, m, cb, Brand::Selection)
    }
    match member.name {
        "anchorNode" => getter_row(scope, proto, member, cb_sel_anchor_node),
        "anchorOffset" => getter_row(scope, proto, member, cb_sel_anchor_offset),
        "focusNode" => getter_row(scope, proto, member, cb_sel_focus_node),
        "focusOffset" => getter_row(scope, proto, member, cb_sel_focus_offset),
        "isCollapsed" => getter_row(scope, proto, member, cb_sel_is_collapsed),
        "rangeCount" => getter_row(scope, proto, member, cb_sel_range_count),
        "type" => getter_row(scope, proto, member, cb_sel_type),
        "direction" => getter_row(scope, proto, member, cb_sel_direction),
        // The four legacy WebKit aliases. Chrome 150, measured over loopback on a live selection:
        //   s.baseNode === s.anchorNode      true        empty selection: null
        //   s.baseOffset === s.anchorOffset  true        empty selection: 0
        //   s.extentNode === s.focusNode     true        empty selection: null
        //   s.extentOffset === s.focusOffset true        empty selection: 0
        // so each is the anchor/focus body under a second name, with its own `get <name>`. Before
        // this lane they were the generated registry's D2 stub installed with `Flavor::Shell`,
        // whose receiver check (`protos::shell_receiver_ok`) demands a brand the host.js `Selection`
        // instance does not carry -- so `getSelection().baseNode` THREW `TypeError: Illegal
        // invocation` on the one real selection there is. Measured on this tree before the cutover.
        "baseNode" => getter_row(scope, proto, member, cb_sel_anchor_node),
        "baseOffset" => getter_row(scope, proto, member, cb_sel_anchor_offset),
        "extentNode" => getter_row(scope, proto, member, cb_sel_focus_node),
        "extentOffset" => getter_row(scope, proto, member, cb_sel_focus_offset),
        "addRange" => method_row(scope, proto, member, cb_sel_add_range),
        "collapse" | "setPosition" => method_row(scope, proto, member, cb_sel_collapse),
        "collapseToEnd" => method_row(scope, proto, member, cb_sel_collapse_to_end),
        "collapseToStart" => method_row(scope, proto, member, cb_sel_collapse_to_start),
        "containsNode" => method_row(scope, proto, member, cb_sel_contains_node),
        "deleteFromDocument" => method_row(scope, proto, member, cb_sel_delete_from_document),
        "empty" | "removeAllRanges" => method_row(scope, proto, member, cb_sel_remove_all),
        "extend" => method_row(scope, proto, member, cb_sel_extend),
        "getComposedRanges" => method_row(scope, proto, member, cb_sel_get_composed_ranges),
        "getRangeAt" => method_row(scope, proto, member, cb_sel_get_range_at),
        "modify" => method_row(scope, proto, member, cb_sel_modify),
        "removeRange" => method_row(scope, proto, member, cb_sel_remove_range),
        "selectAllChildren" => method_row(scope, proto, member, cb_sel_select_all_children),
        "setBaseAndExtent" => method_row(scope, proto, member, cb_sel_set_base_and_extent),
        "toString" => method_row(scope, proto, member, cb_sel_to_string),
        other => debug_assert!(false, "install_selection: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `Selection` block of `install/generated_registry.rs` (`I125`), in the
// capture's order: the twelve read-only attributes, then the seventeen methods. Every
// `len`/`required`/`nullable` is the registry's; `backfill` is the registry's flag for this
// interface. The last four attributes are the legacy `base`/`extent` aliases, which this lane
// IMPLEMENTS (see `install_selection`) because the stub they had was not silent -- it threw.
webapi::interface! {
    "Selection" [interface, window, backfill] {
    ctor            "Selection" len 0                                = stub,
    attr            "anchorNode" : Object readonly nullable          = install_selection,
    attr            "anchorOffset" : Object readonly                 = install_selection,
    attr            "focusNode" : Object readonly nullable           = install_selection,
    attr            "focusOffset" : Object readonly                  = install_selection,
    attr            "isCollapsed" : Object readonly                  = install_selection,
    attr            "rangeCount" : Object readonly                   = install_selection,
    attr            "type" : Object readonly                         = install_selection,
    attr            "direction" : Object readonly                    = install_selection,
    attr            "baseNode" : Object readonly                     = install_selection,
    attr            "baseOffset" : Object readonly                   = install_selection,
    attr            "extentNode" : Object readonly                   = install_selection,
    attr            "extentOffset" : Object readonly                 = install_selection,
    method          "addRange" : Object len 1 required 1             = install_selection,
    method          "collapse" : Object len 1 required 1             = install_selection,
    method          "collapseToEnd" : Object                         = install_selection,
    method          "collapseToStart" : Object                       = install_selection,
    method          "containsNode" : Object len 1 required 1         = install_selection,
    method          "deleteFromDocument" : Object                    = install_selection,
    method          "empty" : Object                                 = install_selection,
    method          "extend" : Object len 1 required 1               = install_selection,
    method          "getComposedRanges" : Object                     = install_selection,
    method          "getRangeAt" : Object len 1 required 1           = install_selection,
    method          "modify" : Object                                = install_selection,
    method          "removeAllRanges" : Object                       = install_selection,
    method          "removeRange" : Object len 1 required 1          = install_selection,
    method          "selectAllChildren" : Object len 1 required 1    = install_selection,
    method          "setBaseAndExtent" : Object len 4 required 4     = install_selection,
    method          "setPosition" : Object len 1 required 1          = install_selection,
    method          "toString" : Object                              = install_selection
    }
}


/// Focusing an element puts the CARET there (Chrome 150): an editing host collapses the document's
/// selection at the start of its first text (the host itself at 0 when it has none); a text control collapses
/// it before the control, in its parent. Any other element leaves the selection as it was. sqlime.org focuses
/// its editor and calls collapseToEnd, which threw on an empty selection.
pub(crate) fn caret_on_focus(scope: &mut v8::PinScope, id: korvus_dom::NodeId) {
    let point = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(dom) = st.try_borrow::<HostDom>().map(|h| &h.0) else { return };
        let tag = dom.tag_name(id).map(|t| t.to_ascii_lowercase()).unwrap_or_default();
        if tag == "textarea" || (tag == "input" && is_text_input(dom, id)) {
            let Some(parent) = dom.parent(id) else { return };
            let idx = dom.children(parent).iter().position(|&c| c == id).unwrap_or(0);
            (parent, idx as i64)
        } else if crate::core::events::focus::contenteditable(dom, id) {
            (first_text(dom, id).unwrap_or(id), 0)
        } else {
            return;
        }
    };
    // the realm's selection and its own `collapse`, never the page's `getSelection` or `Selection.prototype`
    // (a focus called both where Chrome calls neither)
    let Some(sel) = realm_selection(scope) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let node: v8::Local<v8::Value> = wrap_cached(scope, &op_state, point.0).into();
    let off = int_val(scope, point.1);
    pristine_call(scope, sel.into(), "Selection.collapse", &[node, off]);
}

/// The Selection of document `doc` (a Document node: the page's or a frame's), `None` for a document no browsing
/// context shows -- what Blink's editing commands act on (`document->GetFrame()->Selection()`).
pub(crate) fn document_selection<'s>(scope: &mut v8::PinScope<'s, '_>, doc: korvus_dom::NodeId) -> Option<v8::Local<'s, v8::Object>> {
    let key = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.try_borrow::<HostDom>()?.0;
        tree_key(dom, doc)
    };
    selection_for_key(scope, key)
}

/// A Selection's anchor and focus, `(node, offset)` each, or `None` when it has no range.
pub(crate) fn selection_points(scope: &mut v8::PinScope, sel: v8::Local<v8::Object>) -> Option<((korvus_dom::NodeId, i64), (korvus_dom::NodeId, i64))> {
    sel_points(scope, sel)
}

/// Empty a Selection, as its `removeAllRanges()` does (`execCommand("unselect")`).
pub(crate) fn selection_clear(scope: &mut v8::PinScope, sel: v8::Local<v8::Object>) {
    sel_clear(scope, sel);
    sel_notify(scope);
}

/// A Selection over `anchor`..`focus` (node, offset), by its own `setBaseAndExtent` (`execCommand("selectAll")`,
/// `findString`).
pub(crate) fn selection_set(scope: &mut v8::PinScope, sel: v8::Local<v8::Object>, anchor: (korvus_dom::NodeId, u32), focus: (korvus_dom::NodeId, u32)) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let a: v8::Local<v8::Value> = wrap_cached(scope, &op_state, anchor.0).into();
    let ao = int_val(scope, anchor.1 as i64);
    let f: v8::Local<v8::Value> = wrap_cached(scope, &op_state, focus.0).into();
    let fo = int_val(scope, focus.1 as i64);
    pristine_call(scope, sel.into(), "Selection.setBaseAndExtent", &[a, ao, f, fo]);
}

/// The current realm's selection over `anchor`..`focus` (node, offset) -- Blink's `FrameSelection::SelectSubString`,
/// which `SVGTextContentElement.selectSubString` calls -- by the selection's own `setBaseAndExtent`.
pub(crate) fn select_text_range(scope: &mut v8::PinScope, anchor: (korvus_dom::NodeId, u32), focus: (korvus_dom::NodeId, u32)) {
    let Some(sel) = realm_selection(scope) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let a: v8::Local<v8::Value> = wrap_cached(scope, &op_state, anchor.0).into();
    let ao = int_val(scope, anchor.1 as i64);
    let f: v8::Local<v8::Value> = wrap_cached(scope, &op_state, focus.0).into();
    let fo = int_val(scope, focus.1 as i64);
    pristine_call(scope, sel.into(), "Selection.setBaseAndExtent", &[a, ao, f, fo]);
}

/// The current realm's selection, as its window's `getSelection()` answers ([`cb_window_get_selection`]).
fn realm_selection<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let g = scope.get_current_context().global(scope);
    let frame_root = crate::core::html::browsing_context::ctx_of_window(scope, g)
        .and_then(|ctx| crate::core::html::browsing_context::frame_document_root(scope, ctx));
    match frame_root {
        Some(root) => frame_selection(scope, root),
        None => the_selection(scope),
    }
}

fn first_text(dom: &Dom, id: korvus_dom::NodeId) -> Option<korvus_dom::NodeId> {
    for c in dom.children(id) {
        if dom.node_type(c) == 3 {
            return Some(c);
        }
        if let Some(t) = first_text(dom, c) {
            return Some(t);
        }
    }
    None
}

fn is_text_input(dom: &Dom, id: korvus_dom::NodeId) -> bool {
    let ty = dom.get_attribute(id, "type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
    matches!(ty.as_str(), "" | "text" | "search" | "url" | "tel" | "email" | "password" | "number")
}
