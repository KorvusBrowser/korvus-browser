//! `core/events/focus.rs` — moving the focus ring, and the four events that report it.
//!
//! # What this is
//!
//! [`FocusState`] is the one piece of focus state this engine has: the focused element's node id,
//! `None` meaning "the body". `document.activeElement` reads it. Once it was only ever
//! WRITTEN — `el.focus()` set it and `el.blur()` cleared it, silently — so no page ever saw a
//! `focus`, `blur`, `focusin` or `focusout` event, and no mouse press moved it at all. This is the
//! piece that runs when the focus actually moves: commit the new ring, then dispatch the four
//! events the way a browser does.
//!
//! # The order and the targets, asked of the browser rather than read off the spec
//!
//! Stock Chrome 150.0.7871.46 headed under Xvfb, launched with `--remote-debugging-port` (never by
//! Playwright's launcher, which adds `--enable-automation`), driven by `Input.dispatchMouseEvent`
//! `mousePressed`/`mouseReleased` at an element's centre, with a capturing listener on `window`
//! for all of `mousedown`/`mouseup`/`click`/`focus`/`blur`/`focusin`/`focusout`. Writing `o` for
//! the previously focused element and `n` for the newly focused one:
//!
//! ```text
//! 1. mousedown  on the hit element                             (the caller's, not ours)
//! 2. change     on o     bubbles     ONLY when o is a text control the user edited -- see
//!                                    `typing::UserEditState` for the condition, which is stricter
//!                                    than "the value differs"
//! 3. blur       on o     no bubble   relatedTarget = n (null when focus goes to the body)
//! 4. focusout   on o     bubbles     relatedTarget = n
//! 5. focus      on n     no bubble   relatedTarget = o (null when focus came from the body)
//! 6. focusin    on n     bubbles     relatedTarget = o
//! 7. mouseup    on the hit element                             (the caller's)
//! 8. click      on the hit element                             (the caller's)
//! ```
//!
//! Step 2 is absent from the verbatim run below because nothing had been typed into `#f`. With a
//! typed edit in front of it the same press produces `change/#f, blur/#f, focusout/#f, focus/#g,
//! focusin/#g` -- measured, and pinned by `typing_live.rs`.
//!
//! Verbatim, from the run that settled it — a press on `#g` while `#f` was focused:
//!
//! ```text
//! mousedown  tgt=g  rel=null  activeElement=f
//! blur       tgt=f  rel=g     activeElement=BODY
//! focusout   tgt=f  rel=g     activeElement=BODY
//! focus      tgt=g  rel=f     activeElement=g
//! focusin    tgt=g  rel=f     activeElement=g
//! mouseup    tgt=g  rel=null  activeElement=g
//! click      tgt=g  rel=null  activeElement=g
//! ```
//!
//! Two details that fall out of it and are not guesses. **`document.activeElement` reads `BODY`
//! inside the `blur`/`focusout` pair and the new element only from `focus` onwards** — the old
//! element is unfocused before the new one is focused, so the ring is empty in between, and this
//! file commits it in exactly that order. And **a press on something unfocusable still blurs**: a
//! press on a `<span>` inside a plain `<div>` fired `blur`+`focusout` on `#f` with
//! `relatedTarget = null` and no `focus` at all, leaving `activeElement` at `BODY`.
//!
//! # Which element a press focuses
//!
//! The nearest focusable ANCESTOR-OR-SELF of the element the pointer hit, else nothing:
//!
//! ```text
//! press on #btn   (a <button>)                   -> focus moves to #btn
//! press on #inner (a <span> inside a plain <div>) -> focus is cleared to the body
//! ```
//!
//! [`focusable`] is that predicate, over the tag and the attributes — the sequential-focus
//! navigation set HTML defines, minus what this DOM cannot answer.

use deno_core::v8;
use korvus_dom::NodeId;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{set_bool, set_prop, str_val, wrap_cached};
use crate::core::events::shared::FocusState;
use crate::core::html::html_dialog_element::mark_trusted;

/// This realm's page clock, for a focus event's `timeStamp` -- the same `spec::PerfClock`
/// `performance.now()` reads, so a handler comparing the two sees one timeline. The input
/// scheduler passes its own (it anchors a whole gesture to one reading); `el.focus()` from a page
/// script has no schedule and reads it here.
pub(crate) fn page_now(scope: &mut v8::PinScope) -> f64 {
    crate::core::events::ctor::now_ms(scope)
}

/// The focused element's node id, or `None` when the focus ring is empty (the body is focused).
///
/// The ONE reader of `FocusState` outside this file's own writers: `document.activeElement`'s row
/// resolves `None` to `<body>` because that is what the getter answers, while a keystroke resolves
/// it the same way because that is where Chrome dispatches one. Both go through here.
pub(crate) fn focused_node(state: &deno_core::OpState) -> Option<NodeId> {
    state.try_borrow::<FocusState>()?.0.get()
}

/// The node a trusted keystroke is dispatched at: the focused element, or the page's `<body>` when
/// the ring is empty. `None` only for a document with no body element at all.
///
/// This is `document.activeElement`'s answer by construction, which is the point — Chrome routes a
/// key event to the focused node and reports `<body>` as the target when nothing is focused
/// (measured: `Input.dispatchKeyEvent` on a fresh page fired `keydown`/`keypress`/`keyup` with
/// `e.target === document.body`, never at the document).
pub(crate) fn key_target(state: &deno_core::OpState) -> Option<NodeId> {
    if let Some(id) = focused_node(state) {
        return Some(id);
    }
    let dom = &state.try_borrow::<HostDom>()?.0;
    let root = crate::core::dom::document::resolve_doc_root(dom, crate::core::dom::document::PAGE_DOC_ROOT);
    crate::core::dom::document::body_element_of(state, root)
}

/// Is `id` in the sequential-focus navigation order — i.e. would a mouse press on it focus it?
///
/// HTML's set, restricted to what a tag name and an attribute can answer: the form controls that
/// are not disabled, a link or area WITH an href, an iframe, a `<summary>`, anything carrying a
/// `tabindex`, and anything inside a `contenteditable` subtree. A `<div>` with no tabindex is not
/// focusable, which is the case that makes a press CLEAR the focus rather than move it.
pub(crate) fn focusable(dom: &korvus_dom::Dom, id: NodeId) -> bool {
    let Some(tag) = dom.tag_name(id) else { return false };
    let tag = tag.to_ascii_lowercase();
    if dom.get_attribute(id, "tabindex").is_some() {
        return true;
    }
    match tag.as_str() {
        "input" => {
            dom.get_attribute(id, "disabled").is_none()
                && !dom
                    .get_attribute(id, "type")
                    .map(|t| t.trim().eq_ignore_ascii_case("hidden"))
                    .unwrap_or(false)
        }
        "textarea" | "select" | "button" => dom.get_attribute(id, "disabled").is_none(),
        "a" | "area" => dom.get_attribute(id, "href").is_some(),
        "iframe" | "summary" => true,
        "audio" | "video" => dom.get_attribute(id, "controls").is_some(),
        _ => contenteditable(dom, id),
    }
}

/// Is `id` inside a `contenteditable` subtree? Walks up to the nearest ancestor that states it, so
/// `contenteditable="false"` on a nearer ancestor wins, as it does in a browser.
pub(crate) fn contenteditable(dom: &korvus_dom::Dom, id: NodeId) -> bool {
    let mut cur = Some(id);
    while let Some(n) = cur {
        if let Some(v) = dom.get_attribute(n, "contenteditable") {
            let v = v.trim().to_ascii_lowercase();
            return v.is_empty() || v == "true" || v == "plaintext-only";
        }
        cur = dom.parent(n);
    }
    false
}

/// ABI 3 (step 12, C3): does `id` take text input -- a text `<input>`, a `<textarea>`, or a node inside a
/// `contenteditable` subtree? This is `on_focus_state`'s `editable` bit: whether a keystroke or an IME commit
/// would edit the focused node. (`typing::editable` is narrower on purpose -- it excludes `contenteditable`,
/// which this engine fires key events on but runs no editing command for -- so the two do not share a predicate.)
pub(crate) fn takes_text_input(dom: &korvus_dom::Dom, id: NodeId) -> bool {
    match dom.tag_name(id).map(|t| t.to_ascii_lowercase()).as_deref() {
        Some("textarea") => true,
        Some("input") => {
            let ty = dom.get_attribute(id, "type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
            crate::core::events::typing::text_entry_type(&ty)
        }
        _ => contenteditable(dom, id),
    }
}

/// The element a press at this node focuses: the nearest focusable ancestor-or-self, else `None`
/// (which clears the ring, exactly as a press on unfocusable content does in Chrome).
pub(crate) fn focus_target(dom: &korvus_dom::Dom, hit: NodeId) -> Option<NodeId> {
    let mut cur = Some(hit);
    while let Some(n) = cur {
        if focusable(dom, n) {
            return Some(n);
        }
        cur = dom.parent(n);
    }
    None
}

/// Dispatch one trusted `FocusEvent` of `kind` on `node`, with `related` as `relatedTarget`.
/// `bubbles` also decides nothing else: none of the four is cancelable.
fn fire(
    scope: &mut v8::PinScope,
    op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>,
    node: NodeId,
    kind: &str,
    related: Option<NodeId>,
    bubbles: bool,
    timestamp_ms: f64,
) {
    let Some(ctor) =
        crate::binding::protos::iface_object(scope, "FocusEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return;
    };
    let target = wrap_cached(scope, op_state, node);
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", bubbles);
    set_bool(scope, init, "cancelable", false);
    // Blink builds every focus event COMPOSED (`FocusEvent::Create(..., ComposedMode::kComposed)`): a focus inside
    // a shadow tree reaches the host's document (measured: `focus`/`focusin` `composed` true, `view` the window).
    set_bool(scope, init, "composed", true);
    let view: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    set_prop(scope, init, "view", view);
    if let Some(r) = related {
        let o = wrap_cached(scope, op_state, r);
        let v: v8::Local<v8::Value> = o.into();
        set_prop(scope, init, "relatedTarget", v);
    }
    let ty = str_val(scope, kind);
    let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) else { return };
    mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", timestamp_ms);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, target.into(), "dispatchEvent", &[evv]);
}

/// Move the focus ring to `to` (`None` = the body) and fire the four events in the order Chrome
/// fires them. Returns how many events were dispatched — `0` when the ring did not move, which is
/// what a second press on an already-focused field does.
///
/// The ring is CLEARED before `blur` and SET before `focus`, so `document.activeElement` reads the
/// body inside the leaving pair and the new element inside the arriving pair. Measured; see the
/// module doc.
pub fn move_focus_to(scope: &mut v8::PinScope, to: Option<NodeId>, timestamp_ms: f64) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let from = {
        let st = op_state.borrow();
        match st.try_borrow::<FocusState>() {
            Some(f) => f.0.get(),
            None => return 0,
        }
    };
    if from == to {
        return 0;
    }
    let mut n = 0;
    if let Some(old) = from {
        // `change` FIRST, before `blur`, and only when the user's own editing changed the value and
        // left it changed. Chrome's order and Chrome's condition, both measured -- see
        // `typing::UserEditState`. It is read here rather than in `typing.rs` because the focus
        // leaving is the event that commits a text control's value, not the keystroke.
        n += crate::core::events::typing::change_on_leaving(scope, old, timestamp_ms);
        // Unfocus next: a `blur` handler that reads `document.activeElement` sees the body.
        crate::core::events::shared::set_focus_ring(&op_state, None);
        fire(scope, &op_state, old, "blur", to, false, timestamp_ms);
        fire(scope, &op_state, old, "focusout", to, true, timestamp_ms);
        n += 2;
    }
    if let Some(new) = to {
        // A control focused again starts a fresh edit session: its `change` compares against the
        // value it has NOW, never against one a previous session left behind.
        crate::core::events::typing::forget_edit_session(scope, new);
        crate::core::events::shared::set_focus_ring(&op_state, Some(new));
        fire(scope, &op_state, new, "focus", from, false, timestamp_ms);
        fire(scope, &op_state, new, "focusin", from, true, timestamp_ms);
        n += 2;
    } else {
        crate::core::events::shared::set_focus_ring(&op_state, None);
    }
    // ABI 3 (step 12, C3, `on_focus_state`): the ring moved -- tell any plugin watching the focus state. `editable`
    // is whether the newly-focused node takes text input (an IME/typing target); `focused` is whether the page has
    // focus (`document.hasFocus()`'s answer -- the top page document). Reached only when the ring actually moved
    // (the `from == to` guard returned above), and gated on `wants_cursor()`, so with no such plugin nothing runs
    // and the focus path is byte-for-byte what it was (I9/J2).
    // TODO(1021-verify): `focused` reports the running realm's document focus; a window-level blur with no ring
    // move (the OS window losing focus) is not separately hooked here.
    if plugin_host::wants_cursor() {
        let editable = match to {
            Some(new) => {
                let st = op_state.borrow();
                st.try_borrow::<HostDom>().map(|h| takes_text_input(&h.0, new)).unwrap_or(false)
            }
            None => false,
        };
        let focused = crate::core::html::browsing_context::realm_document_focused(scope);
        plugin_host::on_focus_state(focused, editable);
    }
    n
}

/// `el.focus()` / `el.blur()`'s shared body: move the ring and fire the four events. Split out so
/// the two `HTMLElement` rows, the CDP input path and `focus_selector` all move the focus through
/// ONE routine — before P3b each of them wrote `FocusState` directly and none of them dispatched
/// anything.
pub(crate) fn focus_element(scope: &mut v8::PinScope, id: NodeId, timestamp_ms: f64) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let before = op_state.borrow().try_borrow::<FocusState>().and_then(|f| f.0.get());
    let n = move_focus_to(scope, Some(id), timestamp_ms);
    // The caret follows a focus that MOVED here; re-focusing the focused element leaves it.
    let now = op_state.borrow().try_borrow::<FocusState>().and_then(|f| f.0.get());
    if before != Some(id) && now == Some(id) {
        crate::core::editing::selection::caret_on_focus(scope, id);
    }
    n
}

/// `el.blur()`: clear the ring, but only if THIS element holds it (blurring an unfocused element
/// is a no-op, which is what the previous body did and what a browser does).
pub(crate) fn blur_element(scope: &mut v8::PinScope, id: NodeId, timestamp_ms: f64) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let holds = {
        let st = op_state.borrow();
        st.try_borrow::<FocusState>().map(|f| f.0.get() == Some(id)).unwrap_or(false)
    };
    if !holds {
        return 0;
    }
    move_focus_to(scope, None, timestamp_ms)
}

/// Focus whatever a press at the CURRENT hover chain's innermost element would focus — the browser
/// half of `mousedown`'s default action.
///
/// It reads the hover chain rather than hit-testing again, so the element that is focused is
/// exactly the element the pointer is reported to be over (`core/events/hover.rs` committed it on
/// the way in). A press over nothing focusable clears the ring, which is Chrome's answer and not a
/// no-op.
pub fn focus_under_pointer(scope: &mut v8::PinScope, timestamp_ms: f64) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let to = {
        let st = op_state.borrow();
        let Some(dom) = st.try_borrow::<HostDom>().map(|h| &h.0) else { return 0 };
        match dom.hover_chain().last().copied() {
            Some(hit) => focus_target(dom, hit),
            // The pointer is on no box at all. A press there is a press on the document: Chrome
            // blurs. (It is also what a `click_at` into an empty page does, which is why this is
            // not an early return.)
            None => None,
        }
    };
    move_focus_to(scope, to, timestamp_ms)
}
