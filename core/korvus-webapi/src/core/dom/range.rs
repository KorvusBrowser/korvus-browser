//! `core/dom/range.rs` — `Range`: the constructor, the boundary-point algebra, the DOM §4.4
//! algorithms, and the LIVE-RANGE REGISTRY that keeps every range a page holds correct across tree
//! mutations.
//!
//! Everything here used to be split in two. The members were already Rust (W5.1 lane L9 named them
//! from the table below), but the CLASS, and with it the registry every mutation notified, were JS
//! in `core/korvus-runtime/src/js/host.js` — `class Range` and the `KB.__ranges` bridge slot. Both
//! are gone: `Range` is a native constructor built by `install/live/ranges.rs`, and the registry is
//! [`LiveRanges`] below, which the native mutation methods call DIRECTLY rather than through a
//! bridge lookup into a JS object.

use deno_core::{v8, OpState};
use korvus_dom::Dom;
use crate::binding::HostDom;
use crate::core::dom::shared::{boundary_id, children_of, cmp_points, collect_text, dom_root, node_len, node_type_of, norm_doc, notify_character_data, range_point};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::core::events::shared::{ON_DOCUMENT_ID};
use crate::binding::protos::{arg_node_id, document_val, global_get, int_val, pristine_call, pristine_remove, throw_dom, wrap_cached};

/// Write one of the four AbstractRange boundary members. Same v8::Private store the slot getter
/// (`make_evt_slot_get`) reads (installed on AbstractRange.prototype by the member backfill) -- a plain `this.startContainer
/// = v` would hit that getter-only accessor and silently fail (V8's `Object::Set` does not throw for a
/// C++ caller), so every boundary write in this file goes through here instead of `set_prop`.
fn set_point(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, key: &str, value: v8::Local<v8::Value>) {
    evt_slot_write(scope, this, key, value);
}

fn cb_range_detach(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

// ── the live-range registry ─────────────────────────────────────────────────────────
//
// DOM §4.4: "Ranges are live". A node inserted before a boundary point shifts that boundary's
// offset; a node removed from under one moves the boundary out to the removed node's old position;
// splitting a text node moves the part of a boundary past the split into the new node. Every range
// the realm has handed out has to see every one of those edits, or a page that holds a range across
// a mutation reads a boundary pointing into the wrong node -- and the failure surfaces far from its
// cause.
//
// This WAS `KB.__ranges` in `core/korvus-runtime/src/js/host.js`: a JS `Set` of instances the JS
// `Range` class added itself to, and six JS functions the native mutation methods reached through
// the bridge (`bridge_get("__ranges")`). Both halves are Rust now -- the registry is [`LiveRanges`],
// and the six notifications are the `ranges_*` functions below, which the SAME native mutation
// methods call DIRECTLY. There is no JS between a mutation and a boundary any more.
//
// WEAK, not strong. A page that builds a range per highlighted word builds thousands, and a registry
// that held them would keep every one -- and the nodes they name -- alive for the life of the realm.
// Each entry is a `v8::Weak`, so a range the page has dropped is collected as before; the entry it
// leaves behind is pruned the next time the registry is walked (`is_empty()` is exactly that test),
// which is the only bookkeeping a weak entry needs.
#[derive(Default)]
pub(crate) struct LiveRanges(std::cell::RefCell<Vec<v8::Weak<v8::Object>>>);

impl LiveRanges {
    pub(crate) fn has_any(&self) -> bool {
        !self.0.borrow().is_empty()
    }
}

/// Add a freshly constructed `Range` to the registry. Called by [`cb_range_ctor`] and by nothing
/// else: a `Range` that is not registered is not live, and a non-`Range` must never be registered.
fn register_live(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if op_state.borrow().try_borrow::<LiveRanges>().is_none() {
        op_state.borrow_mut().put(LiveRanges::default());
    }
    let weak = v8::Weak::new(scope, obj);
    let st = op_state.borrow();
    st.borrow::<LiveRanges>().0.borrow_mut().push(weak);
}

/// Every live range, as a local -- pruning the entries whose range has been collected.
///
/// The prune happens here rather than on a timer because this is the only place that can see the
/// answer: `Weak::is_empty()` is true exactly when V8 has collected the object, so the walk that
/// needs the list is also the walk that can shorten it.
fn live_ranges<'s>(scope: &mut v8::PinScope<'s, '_>) -> Vec<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(lr) = st.try_borrow::<LiveRanges>() else { return Vec::new() };
    let mut held = lr.0.borrow_mut();
    held.retain(|w| !w.is_empty());
    held.iter().filter_map(|w| w.to_local(scope)).collect()
}

/// Whether this realm has any live range at all.
///
/// Every notification below asks first, because the answer is almost always no: a page that never
/// takes a range still calls `appendChild` thousands of times, and the walk's setup (a nested handle
/// scope, a `Vec` of locals) should not be paid for an empty list.
fn no_live_ranges(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    match st.try_borrow::<LiveRanges>() {
        None => true,
        Some(lr) => lr.0.borrow().is_empty(),
    }
}

/// One boundary of one range, as the registry needs it: the arena id of the container and the
/// offset. `None` when the container is not a node this engine can name.
fn live_point(scope: &mut v8::PinScope, r: v8::Local<v8::Object>, start: bool) -> Option<(u32, i64)> {
    use crate::core::events::event::evt_slot_read;
    let (ck, ok) = if start { ("startContainer", "startOffset") } else { ("endContainer", "endOffset") };
    let c = evt_slot_read(scope, r, ck)?;
    let id = boundary_id(scope, c)?;
    let o = evt_slot_read(scope, r, ok)?.integer_value(scope).unwrap_or(0);
    Some((id, o))
}

fn write_offset(scope: &mut v8::PinScope, r: v8::Local<v8::Object>, start: bool, o: i64) {
    let v = int_val(scope, o);
    set_point(scope, r, if start { "startOffset" } else { "endOffset" }, v);
}

fn write_container(scope: &mut v8::PinScope, r: v8::Local<v8::Object>, start: bool, c: v8::Local<v8::Value>) {
    set_point(scope, r, if start { "startContainer" } else { "endContainer" }, c);
}

/// DOM §4.9 "replace data"'s boundary rule, as a pure function of the old offset: `count` code units
/// at `offset` became `len` code units. A boundary past the replaced run shifts by the difference,
/// one INSIDE it collapses to `offset`, one before it is untouched.
///
/// Insertion is this with `count == 0` and deletion is it with `len == 0`, so the three public
/// notifications below are one walk each rather than the delete-then-insert pair the JS registry
/// used. That halves the work on `data =`, `replaceData` and `textContent =` -- the three mutations
/// WPT's range suite performs hundreds of times against hundreds of live ranges, where it is the
/// difference between a file finishing and a file hitting the harness timeout.
fn shift_offset(o: i64, offset: i64, count: i64, len: i64) -> i64 {
    if o > offset + count {
        o + len - count
    } else if o > offset {
        // "set its offset to offset" -- the boundary pointed INTO text that no longer exists, and it
        // collapses to the start of the replacement, NOT to a position scaled into the new text.
        offset
    } else {
        o
    }
}

/// The one walk. `f` is asked for each boundary's new `(container, offset)`, or `None` to leave it.
///
/// The nested handle scope is not tidiness. Every `to_local`, every private read and every integer
/// written allocates a v8 handle in the CALLER's scope, which is the mutation method's own callback
/// frame -- so a page (or a WPT file) holding a thousand ranges and mutating a thousand times
/// accumulated millions of live handles inside one frame before any of them could be released.
fn walk_live(scope: &mut v8::PinScope, mut f: impl FnMut(&mut v8::PinScope, u32, i64, bool) -> Option<(Option<u32>, i64)>) {
    if no_live_ranges(scope) {
        return;
    }
    v8::scope!(let s, scope);
    let op_state = deno_core::JsRuntime::op_state_from(s);
    for r in live_ranges(s) {
        for start in [true, false] {
            let Some((c, o)) = live_point(s, r, start) else { continue };
            let Some((new_container, new_offset)) = f(s, c, o, start) else { continue };
            if let Some(nc) = new_container {
                if let Some(cv) = container_val(s, &op_state, nc) {
                    write_container(s, r, start, cv);
                }
            }
            write_offset(s, r, start, new_offset);
        }
    }
}

/// The walk for a CHARACTER-DATA mutation, where only boundaries inside `node` can move.
///
/// Split from [`walk_live`] for one reason, measured: that one asks `boundary_id` for every boundary
/// of every range, and `boundary_id` is a cppgc unwrap. Here the node is known up front, so its
/// canonical wrapper is resolved ONCE and
/// the per-boundary test is a pointer comparison -- node wrappers are canonical (one per arena node),
/// which is what makes identity the right test and is what the JS registry used before
/// the ids crossed the boundary.
///
/// It matters because this is the hot path: every `data =`, `appendData`, `insertData`,
/// `deleteData`, `replaceData` and `textContent =` on a page runs it against every live range.
fn walk_live_text(scope: &mut v8::PinScope, node: u32, mut f: impl FnMut(i64) -> Option<i64>) {
    if no_live_ranges(scope) {
        return;
    }
    v8::scope!(let s, scope);
    let op_state = deno_core::JsRuntime::op_state_from(s);
    let target: v8::Local<v8::Value> = wrap_cached(s, &op_state, node).into();
    for r in live_ranges(s) {
        for start in [true, false] {
            let (ck, ok) = if start { ("startContainer", "startOffset") } else { ("endContainer", "endOffset") };
            let Some(c) = crate::core::events::event::evt_slot_read(s, r, ck) else { continue };
            if c != target {
                continue;
            }
            let o = crate::core::events::event::evt_slot_read(s, r, ok).and_then(|v| v.integer_value(s)).unwrap_or(0);
            if let Some(n) = f(o) {
                if n != o {
                    write_offset(s, r, start, n);
                }
            }
        }
    }
}

/// `count` code units were inserted into `node` at `offset`.
pub(crate) fn ranges_text_inserted(scope: &mut v8::PinScope, node: u32, offset: i64, count: i64) {
    walk_live_text(scope, node, |o| (o > offset).then_some(o + count));
}

/// `count` code units were deleted from `node` over `[offset, offset + count)`.
pub(crate) fn ranges_text_deleted(scope: &mut v8::PinScope, node: u32, offset: i64, count: i64) {
    walk_live_text(scope, node, |o| Some(shift_offset(o, offset, count, 0)));
}

/// A whole data replace (`old_len` code units became `new_len`), in ONE walk.
pub(crate) fn ranges_text_replaced(scope: &mut v8::PinScope, node: u32, old_len: i64, new_len: i64) {
    ranges_data_replaced(scope, node, 0, old_len, new_len);
}

/// `CharacterData.replaceData(offset, count, data)` — the general form, in ONE walk.
pub(crate) fn ranges_data_replaced(scope: &mut v8::PinScope, node: u32, offset: i64, count: i64, len: i64) {
    walk_live_text(scope, node, |o| Some(shift_offset(o, offset, count, len)));
}

/// A node was inserted into `parent` at `index`: boundaries in `parent` after it move right by one.
/// Whether any Range object is alive. The insertion and removal steps only need a child's INDEX to update
/// live ranges, and computing it walks the parent's whole child list: without this guard every
/// `appendChild` was O(n) and 20 k appends took 506 ms where Chrome takes 3 ms (measured 2026-09-24).
pub(crate) fn live_ranges_exist(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<LiveRanges>().is_some_and(|r| !r.0.borrow().is_empty())
}

pub(crate) fn ranges_node_inserted(scope: &mut v8::PinScope, parent: u32, index: i64) {
    walk_live(scope, |_s, c, o, _start| {
        (c == parent && o > index).then(|| (None, o + 1))
    });
}

/// `node` was removed from `parent`, where it had been child `index`. A boundary anywhere INSIDE the
/// removed subtree moves out to `(parent, index)` -- the position the subtree used to occupy; a
/// boundary in `parent` after `index` moves left by one.
///
/// Called AFTER the arena removal, which is why the containment test still answers: the removed
/// subtree keeps its own shape, it has only lost its parent edge.
pub(crate) fn ranges_node_removed(scope: &mut v8::PinScope, parent: u32, node: u32, index: i64) {
    walk_live(scope, |s, c, o, _start| {
        let inside = c == node || {
            let op_state = deno_core::JsRuntime::op_state_from(s);
            let st = op_state.borrow();
            st.borrow::<HostDom>().0.contains(node, c)
        };
        if inside {
            Some((Some(parent), index))
        } else if c == parent && o > index {
            Some((None, o - 1))
        } else {
            None
        }
    });
}

/// `node.splitText(offset)` produced `new_node` — DOM §4.10 "split a Text node", steps 6.2–6.5 and
/// step 8, which are NOT the generic insert adjustment and differ from it in two measured ways.
///
/// **An ATTACHED node.** A boundary in `node` past `offset` belongs to the new node, at the offset
/// it had minus the split point. Then a boundary in the PARENT sitting exactly at the new node's own
/// index moves right by one — the generic insert rule is `offset > index` and would leave it, and
/// the spec says `offset == index of node + 1` precisely so that a boundary between the two halves
/// ends up after the new node rather than between them.
///
/// **A DETACHED node** (`splitText` on a text node with no parent). There is nowhere to insert the
/// new node, so a boundary past the split does NOT follow the text into it: it CLAMPS to `offset`
/// and stays where it is. Moving it anyway is what this engine did, and it is what WPT
/// `Range-mutations-splitText.html`'s twenty-two `detachedTextNode` / `detachedForeignTextNode`
/// subtests measured: "Wrong end container, expected Text node 'U'".
pub(crate) fn ranges_text_split(scope: &mut v8::PinScope, node: u32, offset: i64, new_node: u32) {
    if no_live_ranges(scope) {
        return;
    }
    let parent = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.parent(node).map(|p| {
            let i = dom.children(p).iter().position(|&c| c == node).unwrap_or(0) as i64;
            (norm_doc(dom, p), i)
        })
    };
    walk_live(scope, |_s, c, o, _start| match parent {
        None => (c == node && o > offset).then(|| (None, offset)),
        Some((p, idx)) => {
            if c == node && o > offset {
                Some((Some(new_node), o - offset))
            } else if c == p && o == idx + 1 {
                Some((None, o + 1))
            } else {
                None
            }
        }
    });
}

/// `new Range()` — DOM §4.4: both boundary points at `(the node document, 0)`, and the instance
/// entered in the live-range registry.
///
/// This IS the whole of `host.js`'s `class Range` constructor, minus the JS. The four boundary
/// writes go through [`set_point`] for the same reason every other write in this file does:
/// `AbstractRange.prototype` carries getter-only accessors over these private slots, so a plain
/// property set would be swallowed. `ctor::brand` stamps the interface brand the rows' receiver
/// check reads — a private, so a page can neither read it nor forge it onto a look-alike.
pub(crate) fn cb_range_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::core::events::ctor::{brand, require_new};
    if !require_new(scope, &args, "Range") {
        return;
    }
    let this = args.this();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let doc: v8::Local<v8::Value> = match document_val(scope, &op_state) {
        Some(d) => d,
        None => v8::null(scope).into(),
    };
    let z0 = int_val(scope, 0);
    let z1 = int_val(scope, 0);
    set_point(scope, this, "startContainer", doc);
    set_point(scope, this, "startOffset", z0);
    set_point(scope, this, "endContainer", doc);
    set_point(scope, this, "endOffset", z1);
    brand(scope, this, "Range");
    register_live(scope, this);
    rv.set(this.into());
}

/// Re-seat a freshly minted range on `doc` — what `document.createRange()` needs when the receiver
/// is not the page document.
///
/// It exists because the obvious spelling does not work and does not say so: `startContainer` is a
/// GETTER-ONLY accessor on `AbstractRange.prototype`, so a plain property set from C++ neither
/// writes nor throws. `document.createRange()` on a `DOMParser` document did exactly that and
/// silently kept the PAGE document as its container -- WPT `Range-cloneRange.html`'s twenty-three
/// "doc.createRange() must create Range whose startContainer is doc" subtests.
pub(crate) fn range_seat_on(scope: &mut v8::PinScope, r: v8::Local<v8::Object>, doc: v8::Local<v8::Value>) {
    let z0 = int_val(scope, 0);
    let z1 = int_val(scope, 0);
    set_point(scope, r, "startContainer", doc);
    set_point(scope, r, "startOffset", z0);
    set_point(scope, r, "endContainer", doc);
    set_point(scope, r, "endOffset", z1);
}

/// Mint a `Range` from Rust — `document.createRange()`, `cloneRange()`, `getSelection().getRangeAt`.
/// Goes through the interface object on the global, so the instance wears the realm's own
/// `Range.prototype` and is registered exactly once, by the constructor above.
pub(crate) fn new_range<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, "Range").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    ctor.new_instance(scope, &[])
}

/// WebIDL `unsigned long` argument conversion (ToUint32).
///
/// `range.comparePoint(node, -1)` IS the call `comparePoint(node, 4294967295)` -- the argument was
/// declared `unsigned long`, so -1 does not mean "before the start", it means an offset far past any
/// node's length, and the answer is `IndexSizeError`. Reading it as a signed integer and rejecting
/// `< 0` reached the same verdict by accident for -1, and the wrong one for every wrapped value the
/// suite passes (`65536 * 65536`, `-1 + 65536`, ...).
pub(crate) fn arg_offset(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> i64 {
    to_uint(scope, v, 4294967296.0)
}

/// WebIDL `unsigned short` argument conversion (ToUint16) for `compareBoundaryPoints(how, ...)`:
/// `65536` IS `START_TO_START`, and `"-1"` is 65535 (so a NotSupportedError), not 0.
fn arg_how(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> i64 {
    to_uint(scope, v, 65536.0)
}

/// ECMAScript ToUint32 / ToUint16, spelled out rather than delegated to v8's `uint32_value`: that
/// helper answered 0 for a NEGATIVE NUMERIC STRING (`"-1"`, `"-65537"`), which is exactly the
/// argument shape the suite pushes at `compareBoundaryPoints`, and 0 is a legal `how` -- so a call
/// that must throw NotSupportedError quietly compared START_TO_START instead.
///
/// ToNumber, then: NaN and the infinities are 0; otherwise truncate toward zero and take the
/// remainder modulo `modulus`, brought back into range if negative.
fn to_uint(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, modulus: f64) -> i64 {
    let n = match v.number_value(scope) { Some(n) => n, None => return 0 };
    if !n.is_finite() || n == 0.0 { return 0; }
    let m = n.trunc() % modulus;
    (if m < 0.0 { m + modulus } else { m }) as i64
}

/// The (node, offset) validation every boundary-taking member shares, in the spec's order:
/// a DocumentType is never a valid boundary container, and the offset must not exceed the node's
/// length. Returns false with an exception pending when the point is rejected.
pub(crate) fn reject_boundary(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, node_id: u32, offset: i64) -> bool {
    let (nt, nlen) = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; (node_type_of(dom, node_id), node_len(dom, node_id)) };
    if nt == 10 {
        throw_dom(scope, "The node provided is of type DocumentType, which may not be used as a boundary point container.", "InvalidNodeTypeError");
        return true;
    }
    if offset > nlen { throw_dom(scope, "offset out of range", "IndexSizeError"); return true; }
    false
}


/// `Range.comparePoint(node, offset)` -- DOM §4.4 "comparePoint", in the spec's order:
/// WrongDocumentError for a foreign root, InvalidNodeTypeError for a doctype, IndexSizeError for a
/// too-large offset, and only then the comparison. The root test was missing entirely, which is why
/// 3,286 `Must throw WrongDocumentError` assertions failed against a member that otherwise worked.
fn cb_range_compare_point(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let node_id = match required_node_id(scope, args.get(0), "comparePoint") { Some(i) => i, None => return };
    let offset = arg_offset(scope, args.get(1));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (sc, so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (ec, eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let same_root = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; dom_root(dom, node_id) == dom_root(dom, sc) };
    if !same_root { throw_dom(scope, "The two Ranges are not in the same tree.", "WrongDocumentError"); return; }
    if reject_boundary(scope, &op_state, node_id, offset) { return; }
    let result = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        if cmp_points(dom, node_id, offset, sc, so) < 0 { -1i32 }
        else if cmp_points(dom, node_id, offset, ec, eo) > 0 { 1 } else { 0 } };
    rv.set_int32(result);
}

/// `Range.isPointInRange(node, offset)`. Same three gates as `comparePoint` EXCEPT the first: a
/// foreign root here is not an error, it is the answer `false`, and it is checked BEFORE the offset
/// so that a bad offset in another tree still answers `false` rather than throwing.
fn cb_range_is_point_in_range(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let node_id = match required_node_id(scope, args.get(0), "isPointInRange") { Some(i) => i, None => return };
    let offset = arg_offset(scope, args.get(1));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (sc, so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => { rv.set_bool(false); return; } };
    let (ec, eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => { rv.set_bool(false); return; } };
    let same_root = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; dom_root(dom, node_id) == dom_root(dom, sc) };
    if !same_root { rv.set_bool(false); return; }
    if reject_boundary(scope, &op_state, node_id, offset) { return; }
    let result = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        cmp_points(dom, node_id, offset, sc, so) >= 0 && cmp_points(dom, node_id, offset, ec, eo) <= 0 };
    rv.set_bool(result);
}

fn copy_prop(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, from: &str, to: &str) {
    let Some(v) = evt_slot_read(scope, this, from) else { return };
    set_point(scope, this, to, v);
}

fn do_set_start(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, node_val: v8::Local<v8::Value>, node_id: u32, offset: i64) {
    if reject_boundary(scope, op_state, node_id, offset) { return; }
    let ov = int_val(scope, offset);
    set_point(scope, this, "startContainer", node_val);
    set_point(scope, this, "startOffset", ov);
    let (ec, eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let collapse = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        dom_root(dom, node_id) != dom_root(dom, ec) || cmp_points(dom, node_id, offset, ec, eo) > 0 };
    if collapse { let ov2 = int_val(scope, offset); set_point(scope, this, "endContainer", node_val); set_point(scope, this, "endOffset", ov2); }
}
fn do_set_end(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, node_val: v8::Local<v8::Value>, node_id: u32, offset: i64) {
    if reject_boundary(scope, op_state, node_id, offset) { return; }
    let ov = int_val(scope, offset);
    set_point(scope, this, "endContainer", node_val);
    set_point(scope, this, "endOffset", ov);
    let (sc, so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let collapse = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        dom_root(dom, node_id) != dom_root(dom, sc) || cmp_points(dom, sc, so, node_id, offset) > 0 };
    if collapse { let ov2 = int_val(scope, offset); set_point(scope, this, "startContainer", node_val); set_point(scope, this, "startOffset", ov2); }
}

/// (parent, index) for a node, with NO "element parents only" filter. A doctype's parent is the
/// document and a child of a DocumentFragment or a created document has that as its parent; all of
/// them are legal boundary containers, and filtering them out made `setStartBefore(doctype)` a
/// silent no-op. `None` means the node genuinely has no parent.
fn parent_and_index(dom: &Dom, node_id: u32) -> Option<(u32, i64)> {
    if node_id == ON_DOCUMENT_ID { return None; }
    let p = dom.parent(node_id)?;
    let i = dom.children(p).iter().position(|&c| c == node_id).unwrap_or(0) as i64;
    Some((norm_doc(dom, p), i))
}

/// The JS value to store as a boundary container for an arena id: the `document` singleton for the
/// page document (whose wrapper identity is `JsDocument`, never an interned element), the interned
/// wrapper otherwise.
fn container_val<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    if id == ON_DOCUMENT_ID { document_val(scope, op_state) } else { Some(wrap_cached(scope, op_state, id).into()) }
}

/// A REQUIRED `Node` argument, resolved the way WebIDL resolves one.
///
/// Every `Range` member that takes a node declares it non-nullable -- `setStart(Node node, …)`,
/// `comparePoint(Node node, …)`, `intersectsNode(Node node)` -- so WebIDL converts the argument
/// BEFORE the method body runs and `null`, `undefined` and any non-Node are a plain `TypeError`.
///
/// WHAT THIS REPLACED, AND WHY IT WAS INVISIBLE (CONF RANGE1.2, 2026-09-22). Seven call sites read
/// `match arg_node_id(scope, arg) { Some(id) => id, None => return }`, and `arg_node_id` resolves a
/// Document or a `JsElement` and NOTHING ELSE. So every one of them answered `undefined` and did
/// nothing at all when handed `null`, `undefined` or `{}`:
///
/// ```text
/// r.comparePoint(null, 0)   -> undefined   (spec: TypeError)
/// r.intersectsNode(null)    -> undefined   (spec: TypeError)
/// r.setStart(null, 0)       -> no throw, and the range is silently left where it was
/// ```
///
/// The arity guard already caught a MISSING argument (`r.intersectsNode()` threw), which is why
/// this looked covered; a wrong-TYPE argument fell straight through it.
///
/// AN ATTR IS DELIBERATELY NOT A TYPEERROR. An `Attr` IS a `Node` in the IDL, so rejecting it here
/// would be a different lie from the one being fixed. korvus models an Attr as a plain object
/// carrying its owner's arena id (`core/dom/attr.rs`), with no id, length or root of its own, so it
/// cannot yet BE a boundary -- that is RANGE1.1's finding and it needs an engine-wide decision
/// about how an Attr is identified, not a throw bolted on here. Until then an Attr keeps today's
/// behaviour and is counted as open, by name, rather than converted into a spec violation that
/// looks tidier.
fn required_node_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, method: &str) -> Option<u32> {
    if let Some(id) = arg_node_id(scope, v) {
        return Some(id);
    }
    let is_attr = crate::core::dom::attr::is_attr(scope, v);
    if !is_attr {
        let msg = format!(
            "Failed to execute '{method}' on 'Range': parameter 1 is not of type 'Node'."
        );
        // Blink's context applied after the error is made
        crate::binding::protos::throw_type_error(scope, &msg);
    }
    None
}

fn cb_range_set_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let arg = args.get(0);
    let node_id = match required_node_id(scope, arg, "setStart") { Some(id) => id, None => return };
    let offset = arg_offset(scope, args.get(1));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    do_set_start(scope, this, &op_state, arg, node_id, offset);
}
/// `range.expand(unit)` — Blink's legacy selection helper, in the Brave-151 capture and absent here.
///
/// Non-standard and undocumented, so every rule below is a RECORDING from the real browser rather than
/// a reading of a spec. Measured on `<div>hello world here</div>` attached to the document, with the
/// range collapsed at offset 2 (inside "hello"):
///
/// ```text
/// expand("word")      [2,2]  -> [0,5]     the word under the boundary
/// expand("word")      [2,7]  -> [0,11]    each END moves to its own word boundary
/// expand("sentence")  [2,2]  -> [0,16]    the whole text
/// expand("block")     [2,2]  -> [0,16]    likewise
/// expand()                   -> [2,2]     no-op, NOT a throw -- `.length` is 0, the arg is optional
/// expand("nonsense")         -> [2,2]     no-op
/// the same range on a DETACHED element    -> [2,2]     no-op
/// returns `undefined` in every case
/// ```
///
/// The no-op cases matter as much as the working ones: a page that calls `expand()` with no argument
/// gets silence from a browser, and an implementation that threw there would be the difference.
fn cb_range_expand(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_undefined();
    let unit_v = args.get(0);
    if unit_v.is_undefined() || unit_v.is_null() { return; }
    let unit = unit_v.to_rust_string_lossy(scope);
    // Blink implements exactly these three; anything else is silent.
    if !matches!(unit.as_str(), "word" | "sentence" | "block") { return; }

    let this = args.this();
    let Some((sc, so)) = range_point(scope, this, "startContainer", "startOffset") else { return };
    let Some((ec, eo)) = range_point(scope, this, "endContainer", "endOffset") else { return };

    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (start, end) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        // A range whose root is not the document expands to nothing -- measured, and it is why the
        // detached-element case above reads [2,2]. (`dom_root` walks to the true top now, so a root
        // that is not the document IS a detached one; the old form asked the second question
        // separately because the old walk stopped at the topmost element.)
        if dom_root(dom, sc) != ON_DOCUMENT_ID {
            return;
        }
        // SCOPE, NAMED: both boundaries in ONE text node. That is the shape the browser was measured
        // on and the shape `expand` is used in; a range spanning several nodes needs Blink's full
        // visible-position walk, which is not reproduced here. Out of that shape this is a no-op --
        // the same answer a detached range gets, and never a guess.
        if sc != ec || node_type_of(dom, sc) != 3 { return; }
        let text: Vec<char> = dom.text_content(sc).chars().collect();
        let (mut a, mut b) = (so.max(0) as usize, eo.max(0) as usize);
        if a > text.len() || b > text.len() { return; }
        match unit.as_str() {
            "word" => {
                // Each end walks to its own boundary: start back over non-space, end forward over it.
                while a > 0 && !text[a - 1].is_whitespace() { a -= 1; }
                while b < text.len() && !text[b].is_whitespace() { b += 1; }
            }
            // Blink treats both as "the enclosing block's text", and with a single text node that is
            // the whole node -- which is what [0,16] on a 16-character node says.
            _ => { a = 0; b = text.len(); }
        }
        (a as i64, b as i64)
    };
    let sv = int_val(scope, start);
    set_point(scope, this, "startOffset", sv);
    let ev = int_val(scope, end);
    set_point(scope, this, "endOffset", ev);
}
fn cb_range_set_end(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let arg = args.get(0);
    let node_id = match required_node_id(scope, arg, "setEnd") { Some(id) => id, None => return };
    let offset = arg_offset(scope, args.get(1));
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    do_set_end(scope, this, &op_state, arg, node_id, offset);
}

/// setStartBefore / setStartAfter / setEndBefore / setEndAfter.
///
/// "If node's parent is null, throw an InvalidNodeTypeError" -- a parentless node has no position
/// to be before or after, so there is nothing to set and silence is the wrong answer.
fn range_set_at_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, start: bool, after: bool) {
    let this = args.this();
    let node_id = match required_node_id(scope, args.get(0), "setStartBefore") { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let pi = { let st = op_state.borrow(); parent_and_index(&st.borrow::<HostDom>().0, node_id) };
    let (parent_id, index) = match pi {
        Some(pi) => pi,
        None => { throw_dom(scope, "the given Node has no parent.", "InvalidNodeTypeError"); return; }
    };
    let off = if after { index + 1 } else { index };
    let parent_val = match container_val(scope, &op_state, parent_id) { Some(v) => v, None => return };
    if start { do_set_start(scope, this, &op_state, parent_val, parent_id, off); } else { do_set_end(scope, this, &op_state, parent_val, parent_id, off); }
}
fn cb_range_set_start_before(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { range_set_at_node(scope, args, true, false); }
fn cb_range_set_start_after(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { range_set_at_node(scope, args, true, true); }
fn cb_range_set_end_before(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { range_set_at_node(scope, args, false, false); }
fn cb_range_set_end_after(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { range_set_at_node(scope, args, false, true); }

/// `Range.selectNode(node)` -- "If node's parent is null, throw an InvalidNodeTypeError", then set
/// both boundaries around it inside its parent.
fn cb_range_select_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let node_id = match required_node_id(scope, args.get(0), "selectNode") { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let pi = { let st = op_state.borrow(); parent_and_index(&st.borrow::<HostDom>().0, node_id) };
    let (parent_id, index) = match pi {
        Some(pi) => pi,
        None => { throw_dom(scope, "the given Node has no parent.", "InvalidNodeTypeError"); return; }
    };
    let parent_val = match container_val(scope, &op_state, parent_id) { Some(v) => v, None => return };
    let (i0, i1) = (int_val(scope, index), int_val(scope, index + 1));
    set_point(scope, this, "startContainer", parent_val);
    set_point(scope, this, "startOffset", i0);
    set_point(scope, this, "endContainer", parent_val);
    set_point(scope, this, "endOffset", i1);
}

/// `Range.selectNodeContents(node)` -- "If node is a doctype, throw an InvalidNodeTypeError", then
/// span (node, 0) to (node, node's length).
fn cb_range_select_node_contents(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let arg = args.get(0);
    let node_id = match required_node_id(scope, arg, "selectNodeContents") { Some(id) => id, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (nt, nlen) = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; (node_type_of(dom, node_id), node_len(dom, node_id)) };
    if nt == 10 { throw_dom(scope, "The node provided is of type DocumentType, which may not be used as a boundary point container.", "InvalidNodeTypeError"); return; }
    let (z, n) = (int_val(scope, 0), int_val(scope, nlen));
    set_point(scope, this, "startContainer", arg);
    set_point(scope, this, "startOffset", z);
    set_point(scope, this, "endContainer", arg);
    set_point(scope, this, "endOffset", n);
}

fn slice_utf16(s: &str, a: i64, b: i64) -> String {
    let u: Vec<u16> = s.encode_utf16().collect();
    let a = (a.max(0) as usize).min(u.len());
    let b = (b.max(0) as usize).min(u.len());
    String::from_utf16_lossy(&u[a.min(b)..b])
}

fn common_ancestor(dom: &Dom, sc: u32, ec: u32) -> u32 {
    let mut cur = sc;
    loop {
        if cur == ON_DOCUMENT_ID { return ON_DOCUMENT_ID; }
        if cur == ec || dom.contains(cur, ec) { return cur; }
        // DOM §4.4: "while container is not an inclusive ancestor of end node, set container to
        // container's parent". THERE IS NO NODE-TYPE FILTER IN THAT SENTENCE, and this line carried
        // one -- `node_type_of(p) == 1` -- so the walk stopped at the first non-element parent and
        // answered the document. A `DocumentFragment` is node type 11, so a range spanning two
        // children of a fragment reported the DOCUMENT as their common ancestor
        // (`dom/ranges/Range-commonAncestorContainer-2.html`, 3/6: "expected DocumentFragment node
        // with 2 children but got Document node with 2 children").
        //
        // `norm_doc` REPLACES THE FILTER RATHER THAN THE FILTER SIMPLY GOING. Without it the walk
        // reaches the document's own arena id, `dom.contains(doc, ec)` is true, and this returns
        // that raw id -- so `cb_range_common_ancestor` takes the `wrap_cached` branch instead of
        // the `ON_DOCUMENT_ID` one and answers a wrapped node where the page expects `document`.
        // That would have broken the SAME FILE's first subtest ("Detached Range"), which passes
        // today. `norm_doc` maps the root to `ON_DOCUMENT_ID`, which is the sentinel every caller
        // here already reads.
        //
        // The six callers were read before this changed, because the doc comment on
        // `range_box_element` warns that one of them depends on the old answer. None does, and for
        // four of them the filter was actively wrong: `toString` collected nothing for a fragment
        // range, `deleteContents` and `extractContents` walked the document's children instead of
        // the fragment's, and `surroundContents` never reached its terminating `common`.
        // `range_box_element` runs its own element walk afterwards and is unaffected.
        match dom.parent(cur).map(|p| norm_doc(dom, p)) { Some(p) => cur = p, None => return ON_DOCUMENT_ID }
    }
}

fn cb_range_to_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let (sc, so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (ec, eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let out = {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        if sc == ec && node_type_of(dom, sc) == 3 {
            slice_utf16(&dom.node_value(sc).unwrap_or_default(), so, eo)
        } else {
            let ca = common_ancestor(dom, sc, ec);
            if ca == ON_DOCUMENT_ID { String::new() } else {
                let mut texts = Vec::new(); collect_text(dom, ca, &mut texts);
                let mut out = String::new();
                for t in texts {
                    let len = node_len(dom, t);
                    if cmp_points(dom, t, len, sc, so) <= 0 { continue; }
                    if cmp_points(dom, t, 0, ec, eo) >= 0 { continue; }
                    let a = if t == sc { so } else { 0 };
                    let b = if t == ec { eo } else { len };
                    out.push_str(&slice_utf16(&dom.node_value(t).unwrap_or_default(), a, b));
                }
                out
            }
        }
    };
    let v = v8::String::new(scope, &out).unwrap();
    rv.set(v.into());
}

/// What a range's rects are made of (Blink's `Range::GetBorderAndTextQuads`): the border boxes of the elements it
/// selects whole whose parent it does not -- an element containing neither boundary's container -- and the part of
/// every text node it reaches, from the start offset in the start container to the end offset in the end container.
/// A collapsed range covers nothing (Chrome 150: an empty list).
enum RangePart {
    Element(u32),
    Text(u32, u32, u32),
}

fn range_parts(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Vec<RangePart> {
    let (Some((sc, so)), Some((ec, eo))) = (range_point(scope, this, "startContainer", "startOffset"), range_point(scope, this, "endContainer", "endOffset")) else {
        return Vec::new();
    };
    if sc == ec && so == eo {
        return Vec::new();
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    let real = |n: u32| if n == ON_DOCUMENT_ID { dom.root() } else { n };
    let (sc, ec) = (real(sc), real(ec));
    // the node after `n` in tree order, its subtree skipped
    let skip = |mut n: u32| -> Option<u32> {
        loop {
            if let Some(s) = dom.next_sibling(n) {
                return Some(s);
            }
            n = dom.parent(n)?;
        }
    };
    let next = |n: u32| dom.first_child(n).or_else(|| skip(n));
    let character_data = |n: u32| matches!(dom.node_type(n), 3 | 4 | 7 | 8);
    let first = if character_data(sc) { Some(sc) } else { dom.children(sc).get(so.max(0) as usize).copied().or_else(|| skip(sc)) };
    let past = if character_data(ec) { skip(ec) } else { dom.children(ec).get(eo.max(0) as usize).copied().or_else(|| skip(ec)) };
    let mut out = Vec::new();
    let mut selected: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut cur = first;
    let mut budget = dom.len();
    while let Some(n) = cur {
        if Some(n) == past || budget == 0 {
            break;
        }
        budget -= 1;
        match dom.node_type(n) {
            1 => {
                let parent_selected = dom.parent(n).is_some_and(|p| selected.contains(&p));
                if parent_selected || (!dom.contains(n, sc) && !dom.contains(n, ec)) {
                    selected.insert(n);
                    if !parent_selected {
                        out.push(RangePart::Element(n));
                    }
                }
            }
            3 => {
                let len = node_len(dom, n).max(0) as u32;
                let a = if n == sc { (so.max(0) as u32).min(len) } else { 0 };
                let b = if n == ec { (eo.max(0) as u32).min(len) } else { len };
                if a < b {
                    out.push(RangePart::Text(n, a, b));
                }
            }
            _ => {}
        }
        cur = next(n);
    }
    out
}

/// The range's rects, `[x, y, width, height]`, in tree order ([`range_parts`]).
fn range_rects(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Vec<[f64; 4]> {
    let mut out = Vec::new();
    for part in range_parts(scope, this) {
        match part {
            RangePart::Element(n) => out.extend(crate::core::dom::geometry_queries::client_rects_of(scope, n)),
            RangePart::Text(n, a, b) => out.extend(crate::core::dom::geometry_queries::text_range_rects_of(scope, n, a, b)),
        }
    }
    out
}

fn new_dom_rect<'s>(scope: &mut v8::PinScope<'s, '_>, r: [f64; 4]) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, "DOMRect").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let vals: Vec<v8::Local<v8::Value>> = r.iter().map(|&n| v8::Number::new(scope, n).into()).collect();
    ctor.new_instance(scope, &vals)
}

/// `Range.getBoundingClientRect()` -- the union of [`range_rects`] that have an area (Blink unites their bounding
/// boxes, and an empty one adds nothing), or an empty `DOMRect` when there is none. Chrome 150, measured over
/// loopback: the type is `[object DOMRect]` for a fresh `new Range()`.
fn cb_range_bounding_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut union: Option<[f64; 4]> = None;
    for [x, y, w, h] in range_rects(scope, args.this()) {
        if w <= 0.0 && h <= 0.0 {
            continue;
        }
        union = Some(match union {
            None => [x, y, w, h],
            Some([ux, uy, uw, uh]) => {
                let (l, t) = (ux.min(x), uy.min(y));
                let (r, b) = ((ux + uw).max(x + w), (uy + uh).max(y + h));
                [l, t, r - l, b - t]
            }
        });
    }
    if let Some(r) = new_dom_rect(scope, union.unwrap_or([0.0; 4])) {
        rv.set(r.into());
    }
}

/// `Range.getClientRects()` -- [`range_rects`], an EMPTY `DOMRectList` when the range covers nothing.
///
/// A `DOMRectList` and not an Array: the bundle's oracle records
/// `Object.prototype.toString.call(new Range().getClientRects())` as `"[object DOMRectList]"`, and
/// `Array.isArray` is false on it in every browser. Returning a bare `v8::Array` (which both this
/// row and the bootstrap's JS body did) is exactly the recorded case that kept `getClientRects` off
/// the verified list.
fn cb_range_client_rects(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let rects = range_rects(scope, args.this());
    let arr = v8::Array::new(scope, rects.len() as i32);
    for (i, r) in rects.into_iter().enumerate() {
        if let Some(o) = new_dom_rect(scope, r) {
            arr.set_index(scope, i as u32, o.into());
        }
    }
    let list = crate::core::dom::collections::static_list_reuse(scope, "DOMRectList", arr, None);
    rv.set(list);
}

fn cb_range_clone(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let ctor = match crate::binding::protos::iface_object(scope, "Range").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) { Some(c) => c, None => return };
    let new_range = match ctor.new_instance(scope, &[]) { Some(r) => r, None => return };
    for key in ["startContainer", "startOffset", "endContainer", "endOffset"] {
        if let Some(v) = evt_slot_read(scope, this, key) { set_point(scope, new_range, key, v); }
    }
    rv.set(new_range.into());
}

/// `Range.createContextualFragment` steps 2-5: the context element is the range's start node when
/// that is an Element, its parent element when it is a Text/Comment node, and the spec's synthetic
/// `<body>` when it is null or the `<html>` element of an HTML document. `None` means "use `<body>`".
fn contextual_fragment_context(dom: &korvus_dom::Dom, start: korvus_dom::NodeId) -> Option<korvus_dom::NodeId> {
    let mut cur = start;
    loop {
        if dom.node_type(cur) == 1 {
            // "element's local name is html" -> the body fallback, not an <html> context.
            if dom.tag_name(cur).is_some_and(|t| t.eq_ignore_ascii_case("html")) {
                return None;
            }
            return Some(cur);
        }
        match dom.parent(cur) {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

fn cb_range_create_contextual_fragment(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // (DOMString or TrustedHTML): a TrustedHTML read as itself, nothing on it called; a string through the
    // range's document's Trusted Types ("Range createContextualFragment")
    let start = evt_slot_read(scope, args.this(), "startContainer").and_then(|v| arg_node_id(scope, v));
    let sink = crate::core::trusted_types::tables::Sink { kind: "TrustedHTML", sample: "Range createContextualFragment", context: "Failed to execute 'createContextualFragment' on 'Range'" };
    let at = match start {
        Some(n) => crate::core::trusted_types::tables::SinkAt::Node(n),
        None => crate::core::trusted_types::tables::SinkAt::Realm(scope.get_current_context()),
    };
    let Some(html) = crate::core::trusted_types::tables::compliant_text(scope, args.get(0), &sink, at) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let frag_id = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<HostDom>().0;
        let ctx = start.and_then(|s| contextual_fragment_context(dom, s));
        let frag = dom.create_fragment();
        let ids = match ctx {
            Some(c) => dom.parse_fragment_in(c, &html),
            None => dom.parse_fragment_in_body(&html),
        };
        for id in ids { dom.append_child(frag, id); }
        frag
    };
    let w = wrap_cached(scope, &op_state, frag_id);
    // The fragment parser runs against the range's document: its custom elements are constructed
    // before the call returns (Chrome 150 measured).
    crate::core::html::custom_element_registry::upgrade_created(scope, &[frag_id]);
    rv.set(w.into());
}

fn cb_range_insert_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let node_arg = args.get(0);
    // the boundary and the tree read off the range's own slots and the arena, the split and the insertion run
    // by the realm's own `splitText` / `insertBefore` -- never what a page put on the prototypes
    let sc_val = match evt_slot_read(scope, this, "startContainer") { Some(v) => v, None => return };
    let so = evt_slot_read(scope, this, "startOffset").and_then(|v| v.integer_value(scope)).unwrap_or(0);
    let sc_id = match arg_node_id(scope, sc_val) { Some(i) => i, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let is_text = { let st = op_state.borrow(); node_type_of(&st.borrow::<HostDom>().0, sc_id) == 3 };
    if is_text {
        let so_val = int_val(scope, so);
        pristine_call(scope, sc_val, "Text.splitText", &[so_val]);
        let (parent, next) = { let st = op_state.borrow(); let d = &st.borrow::<HostDom>().0; (d.parent(sc_id), d.next_sibling(sc_id)) };
        let Some(parent) = parent else { return };
        let parent: v8::Local<v8::Value> = wrap_cached(scope, &op_state, parent).into();
        let next: v8::Local<v8::Value> = match next { Some(n) => wrap_cached(scope, &op_state, n).into(), None => v8::null(scope).into() };
        pristine_call(scope, parent, "Node.insertBefore", &[node_arg, next]);
    } else {
        let at = { let st = op_state.borrow(); st.borrow::<HostDom>().0.children(sc_id).get(so.max(0) as usize).copied() };
        let ref_node: v8::Local<v8::Value> = match at { Some(n) => wrap_cached(scope, &op_state, n).into(), None => v8::null(scope).into() };
        pristine_call(scope, sc_val, "Node.insertBefore", &[node_arg, ref_node]);
    }
}

fn range_intersects(dom: &Dom, node_id: u32, sc: u32, so: i64, ec: u32, eo: i64) -> bool {
    match dom.parent(node_id).filter(|&p| node_type_of(dom, p) == 1) {
        None => dom.contains(node_id, sc),
        Some(p) => {
            let i = dom.children(p).iter().position(|&c| c == node_id).unwrap_or(0) as i64;
            cmp_points(dom, p, i, ec, eo) < 0 && cmp_points(dom, p, i + 1, sc, so) > 0
        }
    }
}

fn collect_contained(dom: &Dom, root: u32, sc: u32, so: i64, ec: u32, eo: i64, out: &mut Vec<u32>) {
    if root == ON_DOCUMENT_ID { return; }
    let kids: Vec<u32> = dom.children(root).to_vec();
    for k in kids {
        if k == sc || k == ec || dom.contains(k, sc) || dom.contains(k, ec) {
            collect_contained(dom, k, sc, so, ec, eo, out);
        } else if range_intersects(dom, k, sc, so, ec, eo) {
            out.push(k);
        }
    }
}
fn node_value_of(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> String {
    op_state.borrow().borrow::<HostDom>().0.node_value(id).unwrap_or_default()
}

/// DOM §4.9 "replace data" — the ONE primitive every range algorithm below edits text through.
///
/// It is a primitive rather than a `set_node_value` because the spec attaches two side effects to
/// it, and a raw arena write has neither: the character-data mutation record, and the live-range
/// adjustment (a boundary past the replaced run shifts by the length difference; a boundary inside
/// it collapses to `offset`). `extractContents` and `deleteContents` used a raw write and told the
/// live ranges nothing, so a SECOND range over the same text node kept offsets into text that had
/// been cut away.
fn replace_data_live(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32, offset: i64, count: i64, data: &str) {
    let old = op_state.borrow().borrow::<HostDom>().0.node_value(id);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.replace_data(id, offset.max(0) as usize, count.max(0) as usize, data);
    ranges_text_deleted(scope, id, offset, count);
    ranges_text_inserted(scope, id, offset, data.encode_utf16().count() as i64);
    let target = wrap_cached(scope, op_state, id);
    notify_character_data(scope, target.into(), old);
}

/// `anc` is an inclusive ancestor of `node` (DOM: a node is its own inclusive ancestor).
fn inclusive_ancestor(dom: &Dom, anc: u32, node: u32) -> bool {
    anc == node || dom.contains(anc, node)
}

/// The CharacterData node types: Text(3), CDATASection(4), ProcessingInstruction(7), Comment(8).
/// The extract/clone algorithm branches on this, not on "is a Text node" -- a Comment straddling a
/// boundary is sliced exactly like a Text node is.
fn is_chardata(dom: &Dom, id: u32) -> bool {
    matches!(node_type_of(dom, id), 3 | 4 | 7 | 8)
}

/// DOM "contained in a range": the node's own start position is at or after the range's start, and
/// its end position at or before the range's end. This is the FULLY-contained test -- distinct from
/// [`range_intersects`], which is the partial one.
fn fully_contained(dom: &Dom, node: u32, sc: u32, so: i64, ec: u32, eo: i64) -> bool {
    match parent_and_index(dom, node) {
        None => false,
        Some((p, i)) => cmp_points(dom, p, i, sc, so) >= 0 && cmp_points(dom, p, i + 1, ec, eo) <= 0,
    }
}

/// DOM §4.4 step 9 of "extract"/"delete the contents of": where the range collapses to afterwards.
///
/// NOT "the start boundary point". When the start node is an inclusive ancestor of the end node the
/// two coincide, and that is the only case this engine used to get right (it copied start onto end).
/// Otherwise the range collapses OUTSIDE the subtree it just emptied: walk up from the start node
/// until the parent is an inclusive ancestor of the end node, and the answer is that parent, just
/// after the child walked up through. A range spanning two paragraphs collapsed to an offset inside
/// the first paragraph's text instead of to the point between them.
fn collapse_target(dom: &Dom, sc: u32, so: i64, ec: u32) -> (u32, i64) {
    if inclusive_ancestor(dom, sc, ec) {
        return (sc, so);
    }
    let mut reference = sc;
    loop {
        let Some(p) = dom.parent(reference) else { return (sc, so) };
        if inclusive_ancestor(dom, norm_doc(dom, p), ec) || inclusive_ancestor(dom, p, ec) {
            let i = dom.children(p).iter().position(|&c| c == reference).unwrap_or(0) as i64;
            return (norm_doc(dom, p), i + 1);
        }
        reference = p;
    }
}

/// Write both boundary points of `this` to `(node, offset)`.
fn collapse_range_to(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, this: v8::Local<v8::Object>, node: u32, offset: i64) {
    let Some(nv) = container_val(scope, op_state, node) else { return };
    let o0 = int_val(scope, offset);
    let o1 = int_val(scope, offset);
    set_point(scope, this, "startContainer", nv);
    set_point(scope, this, "startOffset", o0);
    set_point(scope, this, "endContainer", nv);
    set_point(scope, this, "endOffset", o1);
}
fn cb_range_delete_contents(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let (sc, so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (ec, eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (collapsed, sc_type) = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; (cmp_points(dom, sc, so, ec, eo) == 0, node_type_of(dom, sc)) };
    if collapsed { return; }
    // Step 4: both boundaries in one CharacterData node -- a single "replace data", nothing else.
    if sc == ec && is_chardata_id(&op_state, sc) {
        replace_data_live(scope, &op_state, sc, so, eo - so, "");
        collapse_range_to(scope, &op_state, this, sc, so);
        return;
    }
    let (new_node, new_offset) = { let st = op_state.borrow(); collapse_target(&st.borrow::<HostDom>().0, sc, so, ec) };
    let root = { let st = op_state.borrow(); common_ancestor(&st.borrow::<HostDom>().0, sc, ec) };
    let contained: Vec<u32> = { let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0; let mut v = Vec::new(); collect_contained(dom, root, sc, so, ec, eo, &mut v); v };
    let ec_type = { let st = op_state.borrow(); node_type_of(&st.borrow::<HostDom>().0, ec) };
    // The boundary text is trimmed through "replace data", so a second live range over the same
    // node moves with it. The tail of the start node goes first, then the contained nodes, then the
    // head of the end node -- the spec's order, and the one the record queue reflects.
    if sc_type == 3 {
        let len = { let st = op_state.borrow(); node_len(&st.borrow::<HostDom>().0, sc) };
        replace_data_live(scope, &op_state, sc, so, len - so, "");
    }
    for id in contained { let w = wrap_cached(scope, &op_state, id); pristine_remove(scope, w.into()); }
    if ec_type == 3 { replace_data_live(scope, &op_state, ec, 0, eo, ""); }
    collapse_range_to(scope, &op_state, this, new_node, new_offset);
}

/// `node_type_of` == CharacterData, with the `OpState` borrow taken here rather than at every site.
fn is_chardata_id(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> bool {
    let st = op_state.borrow();
    is_chardata(&st.borrow::<HostDom>().0, id)
}

/// A SHALLOW clone of `id`, as a JS value. Through `cloneNode(false)` rather than the arena's own
/// `clone_node`, so that whatever the DOM's clone step does (attributes, the per-tag prototype the
/// wrapper wears) happens exactly once, here, and not in two spellings.
fn clone_shallow<'s>(scope: &mut v8::PinScope<'s, '_>, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    let w: v8::Local<v8::Value> = wrap_cached(scope, op_state, id).into();
    let f: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    pristine_call(scope, w, "Node.cloneNode", &[f])
}

/// DOM §4.4 "extract" and "clone the contents of", which are ONE walk differing in a single
/// question: does a piece leave the original tree, or is it copied out of it?
///
/// The pair used to be an approximation, and `host.js` said so in its own comment ("extract / clone /
/// surroundContents are best-effort"). The approximation was in the middle of the walk: it took only
/// the fully-contained CHILDREN of the common ancestor, plus the text at each boundary, and had no
/// notion of a **partially contained child** at all. So a range from inside one `<p>` to inside the
/// next produced a fragment of two bare text nodes, where a browser produces two `<p>` elements each
/// holding its slice — every element the range entered but did not swallow whole was silently
/// dropped, and the deeper the range reached the more of the document's shape went missing.
///
/// The spec's answer, implemented here, is recursion. The common ancestor has at most one child that
/// contains the start and not the end (the FIRST partially contained child) and at most one that
/// contains the end and not the start (the LAST). Each is cloned SHALLOW, and the sub-range from the
/// boundary to that child's edge is extracted or cloned by this same function and appended into the
/// clone. Everything strictly between them is a fully contained child and moves or copies whole.
///
/// The two boundary CharacterData cases are the recursion's base: a clone of the node carrying only
/// the slice inside the range, and — when extracting — a "replace data" that cuts that slice out of
/// the original, which is what makes a live range over the same node move with it.
///
/// Returns the fragment, or `None` with an exception pending (a doctype among the contained children
/// is a `HierarchyRequestError`, and the spec raises it before anything has been moved).
#[allow(clippy::too_many_arguments)]
fn extract_or_clone_range<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    op_state: &std::rc::Rc<std::cell::RefCell<OpState>>,
    doc: v8::Local<'s, v8::Value>,
    sc: u32,
    so: i64,
    ec: u32,
    eo: i64,
    extract: bool,
) -> Option<v8::Local<'s, v8::Value>> {
    let frag = pristine_call(scope, doc, "Document.createDocumentFragment", &[])?;
    let (collapsed, sc_chardata) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        (cmp_points(dom, sc, so, ec, eo) == 0, is_chardata(dom, sc))
    };
    if collapsed {
        return Some(frag);
    }
    // Step 4: one CharacterData node holds both boundaries.
    if sc == ec && sc_chardata {
        let slice = slice_utf16(&node_value_of(op_state, sc), so, eo);
        let clone = clone_shallow(scope, op_state, sc)?;
        set_clone_data(scope, op_state, clone, &slice);
        pristine_call(scope, frag, "Node.appendChild", &[clone]);
        if extract {
            replace_data_live(scope, op_state, sc, so, eo - so, "");
        }
        return Some(frag);
    }
    // Steps 5-8: the common ancestor, its two partially contained children, and the whole children
    // between them. A doctype among the latter is refused before anything moves.
    let (common, first_partial, last_partial, contained, has_doctype) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let common = common_ancestor(dom, sc, ec);
        let kids = children_of(dom, common);
        let first = kids.iter().copied().find(|&k| inclusive_ancestor(dom, k, sc) && !inclusive_ancestor(dom, k, ec));
        let last = kids.iter().copied().rev().find(|&k| inclusive_ancestor(dom, k, ec) && !inclusive_ancestor(dom, k, sc));
        let contained: Vec<u32> = kids.iter().copied().filter(|&k| fully_contained(dom, k, sc, so, ec, eo)).collect();
        let has_doctype = contained.iter().any(|&k| node_type_of(dom, k) == 10);
        (common, first, last, contained, has_doctype)
    };
    let _ = common;
    if has_doctype {
        throw_dom(scope, "The range contains a doctype, which cannot be moved into a DocumentFragment.", "HierarchyRequestError");
        return None;
    }
    // Step 10: the first partially contained child.
    if let Some(fp) = first_partial {
        if is_chardata_id(op_state, fp) {
            // `fp` IS the start node in this branch -- a CharacterData node has no children, so the
            // only way it can contain the start and not the end is by BEING the start.
            let len = { let st = op_state.borrow(); node_len(&st.borrow::<HostDom>().0, sc) };
            let slice = slice_utf16(&node_value_of(op_state, sc), so, len);
            let clone = clone_shallow(scope, op_state, sc)?;
            set_clone_data(scope, op_state, clone, &slice);
            pristine_call(scope, frag, "Node.appendChild", &[clone]);
            if extract {
                replace_data_live(scope, op_state, sc, so, len - so, "");
            }
        } else {
            let clone = clone_shallow(scope, op_state, fp)?;
            pristine_call(scope, frag, "Node.appendChild", &[clone]);
            let sub_end = { let st = op_state.borrow(); node_len(&st.borrow::<HostDom>().0, fp) };
            let sub = extract_or_clone_range(scope, op_state, doc, sc, so, fp, sub_end, extract)?;
            pristine_call(scope, clone, "Node.appendChild", &[sub]);
        }
    }
    // Step 11: the fully contained children, whole.
    for id in contained {
        let w: v8::Local<v8::Value> = wrap_cached(scope, op_state, id).into();
        let node = if extract {
            w
        } else {
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            pristine_call(scope, w, "Node.cloneNode", &[t]).unwrap_or(w)
        };
        pristine_call(scope, frag, "Node.appendChild", &[node]);
    }
    // Step 12: the last partially contained child -- step 10 mirrored onto the end boundary.
    if let Some(lp) = last_partial {
        if is_chardata_id(op_state, lp) {
            let slice = slice_utf16(&node_value_of(op_state, ec), 0, eo);
            let clone = clone_shallow(scope, op_state, ec)?;
            set_clone_data(scope, op_state, clone, &slice);
            pristine_call(scope, frag, "Node.appendChild", &[clone]);
            if extract {
                replace_data_live(scope, op_state, ec, 0, eo, "");
            }
        } else {
            let clone = clone_shallow(scope, op_state, lp)?;
            pristine_call(scope, frag, "Node.appendChild", &[clone]);
            let sub = extract_or_clone_range(scope, op_state, doc, lp, 0, ec, eo, extract)?;
            pristine_call(scope, clone, "Node.appendChild", &[sub]);
        }
    }
    Some(frag)
}

/// Write `data` onto a freshly minted clone, straight into the arena.
///
/// Deliberately NOT through the `data` setter: the clone is one statement old, nothing observes it
/// and no range names it, so the setter's mutation record and live-range sweep would be pure cost --
/// and a characterData record for a node the page has never seen would be a record Chrome does not
/// queue.
fn set_clone_data(scope: &mut v8::PinScope, op_state: &std::rc::Rc<std::cell::RefCell<OpState>>, clone: v8::Local<v8::Value>, data: &str) {
    let Some(id) = arg_node_id(scope, clone) else { return };
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_node_value(id, data);
}

fn range_extract_or_clone(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, extract: bool) {
    let this = args.this();
    let (sc, so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (ec, eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let doc = match global_get(scope, "document") { Some(d) => d, None => return };
    // Step 9, computed BEFORE the walk and applied after it, exactly as the spec orders it: the walk
    // adjusts live ranges as it edits, and this range's own final position is not one of those
    // adjustments, it is stated.
    let (new_node, new_offset) = { let st = op_state.borrow(); collapse_target(&st.borrow::<HostDom>().0, sc, so, ec) };
    let Some(frag) = extract_or_clone_range(scope, &op_state, doc, sc, so, ec, eo, extract) else { return };
    if extract {
        collapse_range_to(scope, &op_state, this, new_node, new_offset);
    }
    rv.set(frag);
}
fn cb_range_extract_contents(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { range_extract_or_clone(scope, args, rv, true); }
fn cb_range_clone_contents(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) { range_extract_or_clone(scope, args, rv, false); }

/// `Range.surroundContents(newParent)` — DOM §4.4, all seven steps.
///
/// Three of them were missing, and each was a way to get a wrong answer silently rather than an
/// exception:
///
/// 1. **The partial-containment refusal.** A range that half-selects a non-Text node cannot be
///    surrounded -- `<p>a<b>bc</b></p>` selected from `a` into the middle of `bc` has no single
///    element that could wrap it -- and the spec says `InvalidStateError`. Without the check the
///    extract silently reshaped the tree instead.
/// 2. **The node-type refusal.** A Document, DocumentType or DocumentFragment is not a legal parent;
///    the spec says `InvalidNodeTypeError`.
/// 3. **Emptying `newParent` first.** The spec replaces all of its children before it inserts the
///    extracted fragment; an element that already had children ended up holding both.
///
/// The ORDER also matters and was wrong: the fragment is appended AFTER `newParent` is in the tree,
/// not before. Appending first put the extracted content into a detached node and then moved the
/// whole thing, which is observably different to a MutationObserver.
fn cb_range_surround_contents(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let node_arg = args.get(0);
    let (sc, _so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (ec, _eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Step 1. The partially contained nodes of a range are exactly the inclusive ancestors of one
    // boundary node that stop short of the common ancestor. Walking the two chains names them all,
    // and no other node can be partially contained.
    let partial_non_text = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let common = common_ancestor(dom, sc, ec);
        let mut bad = false;
        for &end in &[sc, ec] {
            let mut n = end;
            while n != common && !bad {
                if !matches!(node_type_of(dom, n), 3 | 4) { bad = true; }
                match dom.parent(n) { Some(p) => n = norm_doc(dom, p), None => break }
            }
        }
        bad
    };
    if partial_non_text {
        throw_dom(scope, "The Range has partially selected a non-Text node.", "InvalidStateError");
        return;
    }
    // Step 2.
    if let Some(nid) = arg_node_id(scope, node_arg) {
        let nt = { let st = op_state.borrow(); node_type_of(&st.borrow::<HostDom>().0, nid) };
        if matches!(nt, 9 | 10 | 11) {
            throw_dom(scope, "The node provided is of type Document, DocumentType or DocumentFragment, which may not be used as the parent.", "InvalidNodeTypeError");
            return;
        }
    }
    // Steps 3-7, each by the realm's own Range and Node steps (never the page's replacements).
    let Some(frag) = pristine_call(scope, this.into(), "Range.extractContents", &[]) else { return };
    if let Some(nid) = arg_node_id(scope, node_arg) {
        let kids: Vec<u32> = { let st = op_state.borrow(); st.borrow::<HostDom>().0.children(nid).to_vec() };
        for k in kids {
            let w = wrap_cached(scope, &op_state, k);
            pristine_remove(scope, w.into());
        }
    }
    pristine_call(scope, this.into(), "Range.insertNode", &[node_arg]);
    pristine_call(scope, node_arg, "Node.appendChild", &[frag]);
    pristine_call(scope, this.into(), "Range.selectNode", &[node_arg]);
}

fn cb_range_common_ancestor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let (sc, _) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (ec, _) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ca = { let st = op_state.borrow(); common_ancestor(&st.borrow::<HostDom>().0, sc, ec) };
    if ca == ON_DOCUMENT_ID {
        if let Some(doc) = global_get(scope, "document") { rv.set(doc); } else { rv.set_null(); }
    } else {
        let w = wrap_cached(scope, &op_state, ca);
        rv.set(w.into());
    }
}

fn cb_range_collapse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let to_start = args.get(0).boolean_value(scope);
    if to_start { copy_prop(scope, this, "startContainer", "endContainer"); copy_prop(scope, this, "startOffset", "endOffset"); }
    else { copy_prop(scope, this, "endContainer", "startContainer"); copy_prop(scope, this, "endOffset", "startOffset"); }
}

fn cb_range_compare_boundary_points(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let how = arg_how(scope, args.get(0));
    let other = match v8::Local::<v8::Object>::try_from(args.get(1)) { Ok(o) => o, Err(_) => return };
    if !(0..=3).contains(&how) { throw_dom(scope, "The comparison method provided must be one of 'START_TO_START', 'START_TO_END', 'END_TO_END', or 'END_TO_START'.", "NotSupportedError"); return; }
    let (tsc, tso) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (tec, teo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let (osc, oso) = match range_point(scope, other, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (oec, oeo) = match range_point(scope, other, "endContainer", "endOffset") { Some(p) => p, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
    if dom_root(dom, tsc) != dom_root(dom, osc) { drop(st); throw_dom(scope, "The two Ranges are not in the same tree.", "WrongDocumentError"); return; }
    let (a, oa, b, ob) = match how { 0 => (tsc, tso, osc, oso), 1 => (tec, teo, osc, oso), 2 => (tec, teo, oec, oeo), _ => (tsc, tso, oec, oeo) };
    rv.set_int32(cmp_points(dom, a, oa, b, ob));
}

/// `Range.intersectsNode(node)` -- DOM §4.4:
///   1. if node's root differs from this range's root, return false;
///   2. if node's parent is null, return TRUE (the node is that tree's root, so it contains the
///      whole range);
///   3. otherwise the half-open (parent, index) interval test.
/// Step 1 was missing and step 2 answered `contains(node, startContainer)` instead, so a parentless
/// node in another tree reported an intersection with a range that cannot reach it.
fn cb_range_intersects_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let node_id = match required_node_id(scope, args.get(0), "intersectsNode") { Some(i) => i, None => return };
    let (sc, so) = match range_point(scope, this, "startContainer", "startOffset") { Some(p) => p, None => return };
    let (ec, eo) = match range_point(scope, this, "endContainer", "endOffset") { Some(p) => p, None => return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
    let result = if dom_root(dom, node_id) != dom_root(dom, sc) {
        false
    } else {
        match parent_and_index(dom, node_id) {
            None => true,
            Some((p, i)) => cmp_points(dom, p, i, ec, eo) < 0 && cmp_points(dom, p, i + 1, sc, so) > 0,
        }
    };
    rv.set_bool(result);
}


// ── the one table ───────────────────────────────────────────────────────────
//
// `Range` was 30 prototype rows + 4 interface-object constants installed by
// `js/dom/08i-range.js`, which asked 26 `op_range_*` factory ops for a `v8::Function` each and
// `Object.defineProperty`'d them onto `globalThis.Range.prototype`. The bodies never moved: the
// `cb_range_*` callbacks above ARE those bodies, and the table names them directly, so the 26 op
// builders (a factory wrapper apiece) are gone with the file.
//
// The CLASS is gone: `install::live::install_window_ranges` calls `build_newable`
// with [`cb_range_ctor`], so the interface object, its prototype and the `AbstractRange` splice are
// all built from this table and `new Range()` runs Rust. The prototype order the table gives puts
// `constructor` LAST, where Chrome 150 has it (measured over loopback:
// `Object.getOwnPropertyNames(Range.prototype)` ends `...surroundContents, toString, constructor`);
// the class body had put it first, and the post-hoc sort that once stood behind the installer
// (`generated_order.rs`, deleted) could not move it -- `constructor` is
// non-configurable, so the table getting it right in the first place was always the only fix.

/// The bodies the [`RANGE`] rows name. One `match` over the row's name, so a row and its callback
/// cannot drift apart silently: an unexpected name is a debug assertion, never a silent skip.
pub fn install_range(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{branded_getter_row, branded_method_row, Brand};
    // Nested generic `fn`s rather than closures: each `cb_*` is its own fn ITEM type, and a
    // closure would unify to whichever one called it first.
    fn getter_row<F: v8::MapFnTo<v8::FunctionCallback>>(s: &mut v8::PinScope, p: v8::Local<v8::Object>, m: &'static crate::install::MemberDef, cb: F) {
        branded_getter_row(s, p, m, cb, Brand::Range)
    }
    fn method_row<F: v8::MapFnTo<v8::FunctionCallback>>(s: &mut v8::PinScope, p: v8::Local<v8::Object>, m: &'static crate::install::MemberDef, cb: F) {
        branded_method_row(s, p, m, cb, Brand::Range)
    }
    match member.name {
        "commonAncestorContainer" => getter_row(scope, proto, member, cb_range_common_ancestor),
        "cloneContents" => method_row(scope, proto, member, cb_range_clone_contents),
        "cloneRange" => method_row(scope, proto, member, cb_range_clone),
        "collapse" => method_row(scope, proto, member, cb_range_collapse),
        "compareBoundaryPoints" => method_row(scope, proto, member, cb_range_compare_boundary_points),
        "comparePoint" => method_row(scope, proto, member, cb_range_compare_point),
        "createContextualFragment" => method_row(scope, proto, member, cb_range_create_contextual_fragment),
        "deleteContents" => method_row(scope, proto, member, cb_range_delete_contents),
        "detach" => method_row(scope, proto, member, cb_range_detach),
        "expand" => method_row(scope, proto, member, cb_range_expand),
        "extractContents" => method_row(scope, proto, member, cb_range_extract_contents),
        "getBoundingClientRect" => method_row(scope, proto, member, cb_range_bounding_rect),
        "getClientRects" => method_row(scope, proto, member, cb_range_client_rects),
        "insertNode" => method_row(scope, proto, member, cb_range_insert_node),
        "intersectsNode" => method_row(scope, proto, member, cb_range_intersects_node),
        "isPointInRange" => method_row(scope, proto, member, cb_range_is_point_in_range),
        "selectNode" => method_row(scope, proto, member, cb_range_select_node),
        "selectNodeContents" => method_row(scope, proto, member, cb_range_select_node_contents),
        "setEnd" => method_row(scope, proto, member, cb_range_set_end),
        "setEndAfter" => method_row(scope, proto, member, cb_range_set_end_after),
        "setEndBefore" => method_row(scope, proto, member, cb_range_set_end_before),
        "setStart" => method_row(scope, proto, member, cb_range_set_start),
        "setStartAfter" => method_row(scope, proto, member, cb_range_set_start_after),
        "setStartBefore" => method_row(scope, proto, member, cb_range_set_start_before),
        "surroundContents" => method_row(scope, proto, member, cb_range_surround_contents),
        "toString" => method_row(scope, proto, member, cb_range_to_string),
        other => debug_assert!(false, "install_range: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `Range` block of `install/generated_registry.rs` (`I235`), in the capture's
// order: `commonAncestorContainer`, the four comparison constants, the 25 methods, then the same four
// constants again as the interface object's statics. Every `len`/`required` is the registry's.
webapi::interface! {
    "Range" : "NodeRange" [value, window] {
    ctor            "Range" len 0                                        = stub,
    attr            "commonAncestorContainer" : Object readonly          = install_range,
    constant        "START_TO_START" : Object                            = 0,
    constant        "START_TO_END" : Object                              = 1,
    constant        "END_TO_END" : Object                                = 2,
    constant        "END_TO_START" : Object                              = 3,
    method          "cloneContents" : Object                             = install_range,
    method          "cloneRange" : Object                                = install_range,
    method          "collapse" : Object                                  = install_range,
    method          "compareBoundaryPoints" : Object len 2 required 2    = install_range,
    method          "comparePoint" : Object len 2 required 2             = install_range,
    method          "createContextualFragment" : Object len 1 required 1 = install_range,
    method          "deleteContents" : Object                            = install_range,
    method          "detach" : Object                                    = install_range,
    method          "expand" : Object                                    = install_range,
    method          "extractContents" : Object                           = install_range,
    method          "getBoundingClientRect" : Object                     = install_range,
    method          "getClientRects" : Object                            = install_range,
    method          "insertNode" : Object len 1 required 1               = install_range,
    method          "intersectsNode" : Object len 1 required 1           = install_range,
    method          "isPointInRange" : Object len 2 required 2           = install_range,
    method          "selectNode" : Object len 1 required 1               = install_range,
    method          "selectNodeContents" : Object len 1 required 1       = install_range,
    method          "setEnd" : Object len 2 required 2                   = install_range,
    method          "setEndAfter" : Object len 1 required 1              = install_range,
    method          "setEndBefore" : Object len 1 required 1             = install_range,
    method          "setStart" : Object len 2 required 2                 = install_range,
    method          "setStartAfter" : Object len 1 required 1            = install_range,
    method          "setStartBefore" : Object len 1 required 1           = install_range,
    method          "surroundContents" : Object len 1 required 1         = install_range,
    method          "toString" : Object                                  = install_range,
    static_constant "START_TO_START" : Object                            = 0,
    static_constant "START_TO_END" : Object                              = 1,
    static_constant "END_TO_END" : Object                                = 2,
    static_constant "END_TO_START" : Object                              = 3
    }
}
