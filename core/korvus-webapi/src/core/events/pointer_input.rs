//! `core/events/pointer_input.rs` -- what a real mouse does to a page: Blink 150's input pipeline for one mouse,
//! from the platform event to every DOM event it makes (FIXER C6, 2026-10-11).
//!
//! # Why this exists
//!
//! Every trusted mouse, pointer and wheel event this engine made was dispatched at the DOCUMENT, with `toElement`
//! and `fromElement` null, `view` null, `composed` false, `screenX/Y` equal to `clientX/Y`, `detail` 0 on a click
//! and `which` 1 on a move with no button; only the four `mouse*` boundary events were hit-tested, in the page's
//! document only, and no event ever reached a frame. a behavioural bot sensor's sensor reads `toElement` (else `target`)
//! on every `mousemove` and calls `getAttribute` on it -- a Document has none, the handler stopped, and no korvus
//! session with mouse input was ever validated (lowes, united, fedex, usps: Chrome 14/14, korvus 0/14; swapping
//! only `target` or only `toElement` made korvus pass 3/3). a challenge provider's interactive checkbox lives in a frame the
//! click never reached; a bot-detection script's press-and-hold needs a press that is not also a release.
//!
//! # What it is
//!
//! The parts of Blink's `EventHandler`, `PointerEventManager`, `MouseEventManager`, `PointerEventFactory` and
//! `BoundaryEventDispatcher` (`third_party/blink/renderer/core/input/`, `core/events/`) a mouse reaches, per
//! DOCUMENT as Blink keeps them per frame:
//!
//! * one input is a platform event -- a move, a press or a release of ONE button, with the buttons held after it,
//!   a click count and the modifiers (`MouseInput`, Blink's `WebMouseEvent`); a wheel notch is `WheelInput`;
//! * it is hit-tested in the page's document, and passed on into a same-process frame's document at the frame's
//!   content-box origin (`EventHandler::HandleMouseMoveOrLeaveEvent` / `HandleMousePressEvent` /
//!   `HandleMouseReleaseEvent`, `PassMouse*EventToSubframe`); a press in a frame keeps the mouse there until the
//!   release (`capturing_mouse_events_element_`), and a frame the pointer left gets its own leave first;
//! * in the document it lands in: pointer capture (`ProcessPendingPointerCapture`, got/lostpointercapture), the
//!   POINTER boundary events, then the MOUSE boundary events (`SetElementUnderPointer`, `SetElementUnderMouse`),
//!   then the pointer event and its compatibility mouse event (`SendMousePointerEvent`); a press focuses
//!   (`HandleMouseFocus`); a release clicks the nearest common ancestor of the press and release targets, or the
//!   capture target (`DispatchMouseClickIfNeeded`) -- `click` for the left button, `auxclick` for the others, both
//!   PointerEvents -- and a second click is followed by `dblclick` (`MouseEvent::DispatchEvent`); a right press
//!   sends `contextmenu` right after the press (`WebFrameWidgetImpl::HandleMouseDown`, Linux);
//! * `pointerrawupdate` goes first, for every raw sample, when a listener wants it and the page is a secure
//!   context, from the PAGE's coordinates and with the page's window as `view` even when its target is in a frame
//!   (measured);
//! * every event is built in its target's realm with Blink's fields (`SetMouseEventAttributes`,
//!   `UpdateCommonPointerEventInit`, `SetEventSpecificFields`) and dispatched through the DOM dispatch, which
//!   retargets it out of shadow trees (`event_target::et_path`).
//!
//! # The oracle
//!
//! `core/korvus-runtime/tests/fixtures/mouse-input/page.html`, recorded on stock Chrome 150.0.7871.46 headed on a
//! private Xvfb with REAL X input through XTEST and no DevTools attached, three runs alike; the same input sequence
//! is driven into this engine by `tests/mouse_input_live.rs`. The rows that settled what Blink's source left open:
//!
//! ```text
//! pointerrawupdate  first, before every boundary event of its move; cancelable false; coalesced [itself]
//! pointerover/out   button -1, detail 0, coalesced none; on a press-move: buttons 1, pressure 0.5
//! mouseover/out     button 0 (which 0) with no button held, which 1 while the left button is held
//! pointermove       button -1, which 0; movement = screen delta; coalesced [itself]
//! pointerdown/up    detail 0; mousedown/mouseup detail = the click count; click/auxclick PointerEvents with
//!                   pointerId 1, pointerType "mouse", isPrimary false, persistentDeviceId 0
//! enter/leave       dispatched only at a node with a listener for it, or under a capturing one (no row on a page
//!                   whose listeners are all capturing ones on the window)
//! frames            raw update at the frame's element first; then the page's boundary events at the <iframe>;
//!                   then the frame's own (relatedTarget null), pointermove, mousemove -- the page gets no move
//! screen            screenX - clientX = the content area's screen origin (+ the frame's offset in a frame)
//! ```
//!
//! # Left for later, named
//!
//! Frame-aligned (`requestAnimationFrame`-aligned) delivery of continuous input lives in the runtime's scheduler
//! (`korvus-runtime/src/input.rs` merges a frame's samples into one move with its coalesced list); out-of-process
//! frames (`KORVUS_OOPIF`) get no input; touch is still dispatched at the document; `sourceCapabilities` stays null
//! (no `InputDeviceCapabilities` instance exists); drag and selection by mouse are not modelled.

use std::collections::HashMap;

use deno_core::v8;
use korvus_dom::NodeId;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{set_bool, set_prop, str_val, wrap_cached};
use crate::core::dom::geometry_queries::{frame_laid_out, refresh_frame, refresh_layout_cache, LayoutCache};

/// Blink's `WebPointerProperties::Button::kNoButton`.
pub const NO_BUTTON: i32 = -1;
/// `PointerEventFactory::kMouseId`: "Mouse id is 1 to behave the same as MS Edge for compatibility reasons."
pub const MOUSE_POINTER_ID: i32 = 1;

/// The four modifier keys a mouse event carries (`UIEventWithKeyState::SetFromWebInputEventModifiers`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

/// Which way a mouse input goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Move,
    Down,
    Up,
}

/// One platform mouse event -- Blink's `WebMouseEvent`. Coordinates are the page's viewport (widget) CSS pixels.
#[derive(Clone, Debug)]
pub struct MouseInput {
    pub kind: MouseKind,
    pub x: f64,
    pub y: f64,
    /// The button this event is about (0 left, 1 middle, 2 right, 3 back, 4 forward); for a move, the button held
    /// (left first), or [`NO_BUTTON`] -- what Chrome's X11 builder puts in `WebMouseEvent::button`.
    pub button: i32,
    /// The buttons held once this event has happened (`MouseEvent.buttons`).
    pub buttons: u16,
    /// The platform's click count (`WebMouseEvent::click_count`); 0 for a move.
    pub click_count: i32,
    pub modifiers: Modifiers,
    pub timestamp_ms: f64,
    /// For a move: the raw samples merged into it, oldest first, as `(x, y, timestamp_ms)` -- the move itself is
    /// the last. Empty means the move alone.
    pub coalesced: Vec<(f64, f64, f64)>,
}

impl MouseInput {
    /// A move to `(x, y)` with `buttons` held and nothing coalesced into it.
    pub fn moved(x: f64, y: f64, buttons: u16, timestamp_ms: f64) -> Self {
        MouseInput {
            kind: MouseKind::Move,
            x,
            y,
            button: held_button(buttons),
            buttons,
            click_count: 0,
            modifiers: Modifiers::default(),
            timestamp_ms,
            coalesced: Vec::new(),
        }
    }
}

/// One wheel notch or gesture step -- Blink's `WebMouseWheelEvent`, as the page's `WheelEvent` reports it.
#[derive(Clone, Debug)]
pub struct WheelInput {
    pub x: f64,
    pub y: f64,
    /// `WheelEvent.deltaX` / `deltaY` (positive: right / down) and `deltaMode`.
    pub delta_x: f64,
    pub delta_y: f64,
    pub delta_mode: u32,
    /// The legacy `wheelDeltaX` / `wheelDeltaY`: notches times 120, opposite in sign to the delta.
    pub wheel_delta_x: f64,
    pub wheel_delta_y: f64,
    pub buttons: u16,
    pub modifiers: Modifiers,
    pub timestamp_ms: f64,
}

/// `WebPointerProperties::Buttons` bit of button `b`.
pub fn button_bit(b: i32) -> u16 {
    match b {
        0 => 1,
        1 => 4,
        2 => 2,
        3 => 8,
        4 => 16,
        _ => 0,
    }
}

/// The button a move with `buttons` held reports: left, then middle, then right (Chrome's X11 event builder).
pub fn held_button(buttons: u16) -> i32 {
    if buttons & 1 != 0 {
        0
    } else if buttons & 4 != 0 {
        1
    } else if buttons & 2 != 0 {
        2
    } else {
        NO_BUTTON
    }
}

/// One document's input state -- what Blink keeps per frame in its `MouseEventManager`, `PointerEventManager` and
/// `PointerEventFactory`.
#[derive(Default)]
struct DocInput {
    /// `MouseEventManager::element_under_mouse_` / `PointerEventManager::element_under_pointer_`.
    under_mouse: Option<NodeId>,
    under_pointer: Option<NodeId>,
    /// The factory's last positions, in SCREEN coordinates (what `movementX/Y` is measured from).
    last_position: Option<(f64, f64)>,
    last_raw_position: Option<(f64, f64)>,
    /// `mousedown_element_` and `click_count_` -- what a release clicks.
    mousedown_element: Option<NodeId>,
    click_count: i32,
    mouse_pressed: bool,
    /// Pointer capture of the mouse pointer: the target now, and the one the next event moves it to.
    capture: Option<NodeId>,
    pending_capture: Option<NodeId>,
    /// The frame element a press inside its document went to: the mouse stays there until the release.
    capturing_subframe: Option<NodeId>,
    /// `last_mouse_move_event_subframe_`: the frame element the last move was passed into.
    last_subframe: Option<NodeId>,
    /// `prevent_mouse_event_for_pointer_type_`: a cancelled `pointerdown` suppresses the compatibility mouse
    /// events until the release.
    prevent_mouse: bool,
    /// The factory's `is_active_buttons` for the mouse: what lets `setPointerCapture` take.
    active_buttons: bool,
}

/// The mouse's state over the whole page, per document. Kept in `OpState`; reset when the arena is replaced.
#[derive(Default)]
pub(crate) struct PointerInput {
    arena: u64,
    docs: HashMap<u32, DocInput>,
    /// The frame document that holds the focus (`None`: the page's) -- a press elsewhere moves the WINDOW focus.
    focused_frame: Option<u32>,
    /// The frame element the focus ring was parked on when the focus went into its frame (see `press_focus`).
    parked_on_frame: Option<NodeId>,
    /// For the engine's own input (`platform_button`, `platform_move`): the buttons held, and the last press --
    /// `(timestamp, x, y, button, click count)` -- the platform counts clicks from.
    held: u16,
    last_press: Option<(f64, f64, f64, i32, i32)>,
    /// ABI 3 (step 12, C3): the CSS `cursor` last handed to the plugins through `plugin_host::on_cursor`, so a
    /// move that leaves the cursor unchanged emits nothing. Page-wide, and reset on navigation like the rest.
    last_cursor: Option<String>,
    /// HTML drag and drop ([`drag_pressed`] ... [`dispatch_drag`]).
    drag: DragState,
}

/// A press on a draggable element waiting for the move that starts its drag, and the drag in progress.
#[derive(Default)]
struct DragState {
    /// The draggable element a left press went to, and the page point it was pressed at.
    candidate: Option<(NodeId, f64, f64)>,
    session: Option<DragSession>,
}

/// One drag: Blink's `DragController` / `DataTransfer` drag data store, and where the drag is.
struct DragSession {
    /// The element whose `dragstart` began it (`None`: data a DevTools client drags in from outside the page).
    source: Option<NodeId>,
    items: Vec<(String, String)>,
    effect_allowed: String,
    /// The element the last `dragenter` / `dragover` went to.
    over: Option<NodeId>,
    /// The last `dragover` was cancelled: a drop there is taken, with this effect.
    accepted: bool,
    drop_effect: String,
    /// A DevTools client took the drag over (`Input.setInterceptDrags`): its moves arrive as `Input.dispatchDragEvent`,
    /// not as mouse moves.
    intercepted: bool,
}

type St = std::rc::Rc<std::cell::RefCell<deno_core::OpState>>;

/// Run `f` over the page-wide state -- made on first use, and started again when the arena was replaced (a
/// navigation): no node id of the old document may name a node of the new one.
fn with_input<R>(st: &St, f: impl FnOnce(&mut PointerInput) -> R) -> R {
    let mut s = st.borrow_mut();
    let arena = s.try_borrow::<HostDom>().map(|h| h.0.instance()).unwrap_or(0);
    if s.try_borrow::<PointerInput>().is_none() {
        s.put(PointerInput::default());
    }
    let p = s.borrow_mut::<PointerInput>();
    if p.arena != arena {
        *p = PointerInput { arena, ..PointerInput::default() };
    }
    f(p)
}

/// Run `f` over the state of document `doc`, creating it on first use.
fn with_doc<R>(st: &St, doc: u32, f: impl FnOnce(&mut DocInput) -> R) -> R {
    with_input(st, |p| f(p.docs.entry(doc).or_default()))
}

/// The page document's arena root.
fn page_root(st: &St) -> Option<u32> {
    st.borrow().try_borrow::<HostDom>().map(|h| h.0.root())
}

/// Is `id` still a node of document `doc` (its shadow-including root)?
fn in_doc(st: &St, id: NodeId, doc: u32) -> bool {
    st.borrow().try_borrow::<HostDom>().is_some_and(|h| (id as usize) < h.0.len() && h.0.shadow_including_root(id) == doc)
}

/// The document a node belongs to (its shadow-including root).
fn doc_of(st: &St, id: NodeId) -> Option<u32> {
    st.borrow().try_borrow::<HostDom>().map(|h| h.0.shadow_including_root(id))
}

// ── geometry ─────────────────────────────────────────────────────────────────────────────────────────

/// Run `f` over document `doc`'s layout, brought up to date first: the page's, or a frame document's.
fn with_layout<R>(st: &St, doc: u32, page: u32, f: impl FnOnce(&page::LaidOut) -> R) -> Option<R> {
    let mut s = st.borrow_mut();
    // the page's own document is laid out in the page's cache; every other -- a frame's, a popup's -- in its own
    let is_page = s.try_borrow::<HostDom>().is_some_and(|h| h.0.root() == doc);
    if doc == page && is_page {
        if !refresh_layout_cache(&mut s) {
            return None;
        }
        match s.try_borrow::<LayoutCache>() {
            Some(LayoutCache(Some((_, out)))) => Some(f(out)),
            _ => None,
        }
    } else {
        if !refresh_frame(&mut s, doc) {
            return None;
        }
        frame_laid_out(&s, doc).map(f)
    }
}

/// The element at client point `(x, y)` of document `doc` -- Blink's hit test, whose text-node results are their
/// parent elements already. `None` outside the viewport.
fn hit(st: &St, doc: u32, page: u32, x: f64, y: f64) -> Option<NodeId> {
    with_layout(st, doc, page, |out| out.element_from_point(x as f32, y as f32)).flatten()
}

/// The client-space origin of frame element `el`'s CONTENT box in its document `doc` -- where the frame's own
/// viewport starts (`LocalFrameView::ConvertFromRootFrame`).
fn frame_origin(st: &St, doc: u32, page: u32, el: NodeId) -> Option<(f64, f64)> {
    with_layout(st, doc, page, |out| {
        let border = out.bounding_rect(el);
        let content = out.content_box(el)?;
        let client = out.viewport_bounding_rect(el);
        Some(((client.x + content.x - border.x) as f64, (client.y + content.y - border.y) as f64))
    })
    .flatten()
}

/// What a press over `el` passes into: the document of the frame `el` is, if it is one shown in this process.
fn subframe_of(st: &St, el: NodeId) -> Option<u32> {
    crate::core::html::browsing_context::frame_content_root(&st.borrow(), el)
}

/// The content area's screen origin: where `clientX/Y` (0, 0) of the page is on the screen.
///
/// Chrome reports `screenX - clientX` = the widget's screen origin (`MouseEvent::SetCoordinatesFromWebPointerProperties`
/// takes `PositionInScreen`; CDP's own input converts the widget point to the screen the same way,
/// `InputHandler::OnWidgetForDispatchMouseEvent`). It is derived from the persona's window geometry the way the
/// browser frame wraps the content: the frame's side borders are equal, `(outerWidth - innerWidth) / 2` each, the
/// toolbar strip is above the content and the bottom border is a side border's width, so the origin is
/// `(screenX + side, screenY + outerHeight - innerHeight - side)`. Measured on stock Chrome 150 (window at 10,10,
/// 1919x1079 outer, 1919x991 inner): `screenY - clientY` = 98 = 10 + 88 - 0 on every event of the page.
fn screen_origin(st: &St) -> (f64, f64) {
    let s = st.borrow();
    let Some(p) = crate::binding::protos::persona_handle(&s) else { return (0.0, 0.0) };
    let g = crate::core::html::window_geometry::window_geometry_of(&p.0.screen());
    let side = ((g[2] - g[0]) / 2.0).max(0.0);
    (g[4] + side, g[5] + (g[3] - g[1]) - side)
}

// ── building and dispatching one event ───────────────────────────────────────────────────────────────

/// Which interface an event is made from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Iface {
    Mouse,
    Pointer,
    Wheel,
    Drag,
}

/// The pointer-specific half of an event (`PointerEventInit`).
#[derive(Clone)]
struct PointerFields {
    pointer_id: i32,
    pointer_type: &'static str,
    is_primary: bool,
    pressure: f64,
    persistent_device_id: i32,
    /// `Some`: this is a `pointermove` / `pointerrawupdate` and these are its coalesced samples, as
    /// `(client x, client y, screen x, screen y, movement x, movement y, timestamp)`.
    coalesced: Option<Vec<[f64; 7]>>,
}

/// Everything one DOM event is made of.
#[derive(Clone)]
struct Spec {
    iface: Iface,
    kind: String,
    bubbles: bool,
    cancelable: bool,
    composed: bool,
    detail: i32,
    client: (f64, f64),
    screen: (f64, f64),
    movement: (f64, f64),
    /// The RAW button (`button_`): -1 for none.
    button: i32,
    buttons: u16,
    modifiers: Modifiers,
    related: Option<NodeId>,
    pointer: Option<PointerFields>,
    wheel: Option<WheelInput>,
    timestamp_ms: f64,
    /// The window whose realm's `view` this is: `None`, the target's own; `Some(doc)`, that document's (a
    /// `pointerrawupdate` carries the page's window into a frame).
    view_doc: Option<u32>,
    /// A drag event's `DataTransfer`: its items, its mode and its effects (made in the target's realm).
    transfer: Option<(Vec<(String, String)>, u8, String, String)>,
}

/// The `PointerEventInit` of a mouse pointer event with `buttons` held (`ConvertIdTypeButtonsEvent`,
/// `GetPointerEventPressure`: a mouse has no force, so 0.5 while a button is held and 0 otherwise).
fn mouse_pointer(buttons: u16, coalesced: Option<Vec<[f64; 7]>>) -> PointerFields {
    PointerFields {
        pointer_id: MOUSE_POINTER_ID,
        pointer_type: "mouse",
        is_primary: true,
        pressure: if buttons != 0 { 0.5 } else { 0.0 },
        persistent_device_id: 1,
        coalesced,
    }
}

/// The page-relative scroll of document `doc` (`MouseEvent::InitCoordinates` adds the layout viewport's scroll
/// offset to `pageX/Y`). A frame's own scroll is not kept here.
fn scroll_of(st: &St, doc: u32, page: u32) -> (f64, f64) {
    if doc != page {
        return (0.0, 0.0);
    }
    crate::core::html::window_geometry::scroll_position(&st.borrow())
}

/// `offsetX/Y` and `layerX/Y` origins of `target` in its document's client space: its PADDING box (Blink
/// `ComputeRelativePosition`: local coordinates less the border; an inline box's are its containing block's,
/// less its own border), and its enclosing positioned box's border box in document coordinates (the layer).
fn relative_origins(st: &St, doc: u32, page: u32, target: NodeId) -> Option<((f64, f64), (f64, f64))> {
    let ancestors: Vec<NodeId> = {
        let s = st.borrow();
        let dom = &s.try_borrow::<HostDom>()?.0;
        let mut v = Vec::new();
        let mut cur = dom.parent(target);
        while let Some(n) = cur {
            if dom.node_type(n) != 1 {
                break;
            }
            v.push(n);
            cur = dom.parent(n);
        }
        v
    };
    with_layout(st, doc, page, |out| {
        let offset = if out.is_inline_box(target) {
            let container = ancestors.iter().copied().find(|a| !out.is_inline_box(*a) && out.padding_box(*a).is_some());
            let (top, left) = out.border_widths(target);
            let base = container.map(|c| out.viewport_bounding_rect(c)).unwrap_or(page::layout::geometry::Rect::ZERO);
            (base.x as f64 + left as f64, base.y as f64 + top as f64)
        } else {
            match out.viewport_padding_box(target) {
                Some(p) => (p.x as f64, p.y as f64),
                None => (0.0, 0.0),
            }
        };
        let positioned = |n: NodeId| {
            out.position_of(n).is_some_and(|p| p != page::layout::style::CssPosition::Static) || out.transforms(n)
        };
        let layer = std::iter::once(target).chain(ancestors.iter().copied()).find(|n| positioned(*n));
        let layer_origin = layer.map(|l| {
            let r = out.bounding_rect(l);
            (r.x as f64, r.y as f64)
        });
        (offset, layer_origin.unwrap_or((0.0, 0.0)))
    })
}

/// `pageX/Y`, `offsetX/Y` and `layerX/Y` of a mouse event about to be dispatched at `target`, from its own
/// `clientX/Y` -- what Blink computes from the target when they are read (`MouseEvent::ComputeRelativePosition`).
/// For an event the engine makes from another one (a label's click on its control).
pub(crate) fn stamp_relative_position(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>, target: NodeId) {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let (Some(page), Some(doc)) = (page_root(&st), doc_of(&st, target)) else { return };
    let cx = crate::core::events::event::evt_slot_read(scope, ev, "clientX").and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let cy = crate::core::events::event::evt_slot_read(scope, ev, "clientY").and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let scroll = scroll_of(&st, doc, page);
    let Some((o, l)) = relative_origins(&st, doc, page, target) else { return };
    use crate::core::events::event::evt_slot_write_num as slot;
    slot(scope, ev, "pageX", cx + scroll.0);
    slot(scope, ev, "pageY", cy + scroll.1);
    slot(scope, ev, "offsetX", (cx - o.0).round());
    slot(scope, ev, "offsetY", (cy - o.1).round());
    slot(scope, ev, "layerX", (cx - l.0).floor());
    slot(scope, ev, "layerY", (cy - l.1).floor());
}

/// `(offsetX, offsetY)` and `(layerX, layerY)` of client point `(cx, cy)` against `target`, unrounded -- what the
/// `MouseEvent` getters answer while a listener runs (`core/events/mouse_event.rs`). Blink computes them from the
/// target AS THE LISTENER SEES IT (`ComputeRelativePosition` reads `RawTarget`, re-done after each
/// `ReceivedTarget`): measured, one `pointerrawupdate` read `offsetX/Y` 50,20 in a closed shadow root's listener
/// (its target the inner element) and 60,30 in the window's (its target the host). `None` without a layout.
pub(crate) fn relative_to(scope: &mut v8::PinScope, target: NodeId, cx: f64, cy: f64) -> Option<((f64, f64), (f64, f64))> {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let page = page_root(&st)?;
    let doc = doc_of(&st, target)?;
    let (o, l) = relative_origins(&st, doc, page, target)?;
    Some(((cx - o.0, cy - o.1), (cx - l.0, cy - l.1)))
}

/// The window of document `doc` in the realm it runs in.
fn window_of_doc<'s>(scope: &mut v8::PinScope<'s, '_>, st: &St, doc: u32, page: u32) -> Option<v8::Local<'s, v8::Object>> {
    if doc == page {
        return Some(crate::core::html::browsing_context::main_global(scope));
    }
    let docv = wrap_cached(scope, st, doc);
    crate::core::html::browsing_context::frame_document_window(scope, docv.into())
}

/// Build `spec` in the realm of `target`'s document and dispatch it there. Returns whether nothing cancelled it.
fn fire(scope: &mut v8::PinScope, st: &St, target: NodeId, spec: &Spec) -> bool {
    let Some(page) = page_root(st) else { return true };
    let Some(doc) = doc_of(st, target) else { return true };
    let realm = crate::core::html::browsing_context::realm_of_node(scope, target)
        .unwrap_or_else(|| crate::core::html::browsing_context::main_context(scope));
    let view: Option<v8::Local<v8::Value>> = window_of_doc(scope, st, spec.view_doc.unwrap_or(doc), page).map(Into::into);
    let target_v: v8::Local<v8::Value> = wrap_cached(scope, st, target).into();
    let related_v: Option<v8::Local<v8::Value>> = spec.related.map(|r| wrap_cached(scope, st, r).into());
    let scroll = scroll_of(st, doc, page);
    let origins = relative_origins(st, doc, page, target);
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    let Some(ev) = build(scope, spec, view, related_v) else { return true };
    let integer = spec.iface != Iface::Pointer || crate::core::events::mouse_event::integer_pointer_type(&spec.kind);
    let round = |v: f64| if integer { v.floor() } else { v };
    let (cx, cy) = spec.client;
    let page_xy = (cx + scroll.0, cy + scroll.1);
    let (offset, layer) = match origins {
        Some((o, l)) => ((cx - o.0, cy - o.1), (cx - l.0, cy - l.1)),
        None => (page_xy, page_xy),
    };
    let offset = if integer { (offset.0.round(), offset.1.round()) } else { offset };
    use crate::core::events::event::evt_slot_write_num as slot;
    slot(scope, ev, "pageX", round(page_xy.0));
    slot(scope, ev, "pageY", round(page_xy.1));
    slot(scope, ev, "offsetX", offset.0);
    slot(scope, ev, "offsetY", offset.1);
    slot(scope, ev, "layerX", layer.0.floor());
    slot(scope, ev, "layerY", layer.1.floor());
    if let Some(list) = spec.pointer.as_ref().and_then(|p| p.coalesced.as_ref()) {
        let mut items: Vec<v8::Local<v8::Value>> = Vec::with_capacity(list.len());
        for c in list {
            let mut one = spec.clone();
            one.bubbles = false;
            one.cancelable = false;
            one.client = (c[0], c[1]);
            one.screen = (c[2], c[3]);
            one.movement = (c[4], c[5]);
            one.timestamp_ms = c[6];
            one.related = None;
            if let Some(p) = one.pointer.as_mut() {
                p.coalesced = None;
            }
            if let Some(e) = build(scope, &one, view, None) {
                // Blink sets each coalesced event's target to the dispatch's node (`PointerEvent::DispatchEvent`).
                crate::core::events::event::evt_slot_write(scope, e, "target", target_v);
                slot(scope, e, "pageX", c[0] + scroll.0);
                slot(scope, e, "pageY", c[1] + scroll.1);
                if let Some((o, l)) = origins {
                    slot(scope, e, "offsetX", c[0] - o.0);
                    slot(scope, e, "offsetY", c[1] - o.1);
                    slot(scope, e, "layerX", (c[0] - l.0).floor());
                    slot(scope, e, "layerY", (c[1] - l.1).floor());
                }
                items.push(e.into());
            }
        }
        let arr = v8::Array::new_with_elements(scope, &items);
        crate::core::events::event::evt_slot_write(scope, ev, "_coalesced", arr.into());
    }
    if KEEP_EVENT.with(|k| k.get()) {
        let g = v8::Global::new(scope, v8::Local::<v8::Value>::from(ev));
        LAST_DRAG.with(|l| *l.borrow_mut() = Some(g));
    }
    crate::core::events::event_target::et_dispatch(scope, target_v, ev)
}

/// One number member of an init dictionary.
fn num(scope: &mut v8::PinScope, init: v8::Local<v8::Object>, k: &str, n: f64) {
    let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
    set_prop(scope, init, k, v);
}

/// Make the event `spec` describes, trusted and time-stamped, in the current realm.
fn build<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    spec: &Spec,
    view: Option<v8::Local<v8::Value>>,
    related: Option<v8::Local<v8::Value>>,
) -> Option<v8::Local<'s, v8::Object>> {
    let name = match spec.iface {
        Iface::Mouse => "MouseEvent",
        Iface::Pointer => "PointerEvent",
        Iface::Wheel => "WheelEvent",
        Iface::Drag => "DragEvent",
    };
    let ctor = crate::binding::protos::iface_object(scope, name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", spec.bubbles);
    set_bool(scope, init, "cancelable", spec.cancelable);
    set_bool(scope, init, "composed", spec.composed);
    if let Some(v) = view {
        set_prop(scope, init, "view", v);
    }
    num(scope, init, "detail", spec.detail as f64);
    num(scope, init, "screenX", spec.screen.0);
    num(scope, init, "screenY", spec.screen.1);
    num(scope, init, "clientX", spec.client.0);
    num(scope, init, "clientY", spec.client.1);
    num(scope, init, "movementX", spec.movement.0);
    num(scope, init, "movementY", spec.movement.1);
    num(scope, init, "button", spec.button as f64);
    num(scope, init, "buttons", spec.buttons as f64);
    set_bool(scope, init, "ctrlKey", spec.modifiers.ctrl);
    set_bool(scope, init, "shiftKey", spec.modifiers.shift);
    set_bool(scope, init, "altKey", spec.modifiers.alt);
    set_bool(scope, init, "metaKey", spec.modifiers.meta);
    if let Some(r) = related {
        set_prop(scope, init, "relatedTarget", r);
    }
    if let Some(p) = &spec.pointer {
        num(scope, init, "pointerId", p.pointer_id as f64);
        num(scope, init, "width", 1.0);
        num(scope, init, "height", 1.0);
        num(scope, init, "pressure", p.pressure);
        num(scope, init, "tiltX", 0.0);
        num(scope, init, "tiltY", 0.0);
        num(scope, init, "twist", 0.0);
        num(scope, init, "persistentDeviceId", p.persistent_device_id as f64);
        let t = str_val(scope, p.pointer_type);
        set_prop(scope, init, "pointerType", t);
        set_bool(scope, init, "isPrimary", p.is_primary);
    }
    if let Some((items, mode, allowed, effect)) = &spec.transfer {
        let dt = crate::core::clipboard::data_transfer::dt_mint(scope, items, *mode, allowed, effect);
        set_prop(scope, init, "dataTransfer", dt);
    }
    if let Some(w) = &spec.wheel {
        num(scope, init, "deltaX", w.delta_x);
        num(scope, init, "deltaY", w.delta_y);
        num(scope, init, "deltaZ", 0.0);
        num(scope, init, "deltaMode", w.delta_mode as f64);
        num(scope, init, "wheelDeltaX", w.wheel_delta_x);
        num(scope, init, "wheelDeltaY", w.wheel_delta_y);
    }
    let ty = str_val(scope, &spec.kind);
    let ev = ctor.new_instance(scope, &[ty, init.into()])?;
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", spec.timestamp_ms);
    // A trusted `PointerEvent` that is no click answers `[]` for its two lists unless the input path gave it some.
    Some(ev)
}

// ── boundary events ──────────────────────────────────────────────────────────────────────────────────

/// Blink's flat-tree parent (`FlatTreeTraversal::Parent`): a slotted node's slot, a shadow root's child's host,
/// otherwise the parent -- up to and including the document.
fn flat_parent(dom: &korvus_dom::Dom, id: NodeId) -> Option<NodeId> {
    let parent = dom.parent(id)?;
    if dom.shadow_root_of(parent).is_some() {
        return dom.assigned_slot(id);
    }
    if dom.is_shadow_root(parent) {
        return dom.shadow_host_of(parent);
    }
    Some(parent)
}

/// `node` and its flat-tree ancestors, node first, up to and INCLUDING its document (`BuildAncestorChain`). The
/// document is in it: with real input, stock Chrome 150 fired `pointerenter` and `mouseenter` at a document with
/// a listener for them on the first move into the page (`tests/fixtures/mouse-input`, rows `D`).
fn ancestor_chain(st: &St, node: Option<NodeId>, doc: u32) -> Vec<NodeId> {
    let Some(node) = node else { return Vec::new() };
    if !in_doc(st, node, doc) {
        return Vec::new();
    }
    let s = st.borrow();
    let Some(h) = s.try_borrow::<HostDom>() else { return Vec::new() };
    let dom = &h.0;
    let mut out = Vec::new();
    let mut cur = Some(node);
    let mut budget = dom.len();
    while let Some(n) = cur {
        if budget == 0 {
            break;
        }
        budget -= 1;
        out.push(n);
        cur = flat_parent(dom, n);
    }
    out
}

/// The four types of one boundary family.
struct Family {
    over: &'static str,
    out: &'static str,
    enter: &'static str,
    leave: &'static str,
}
const POINTER_FAMILY: Family = Family { over: "pointerover", out: "pointerout", enter: "pointerenter", leave: "pointerleave" };
const MOUSE_FAMILY: Family = Family { over: "mouseover", out: "mouseout", enter: "mouseenter", leave: "mouseleave" };

/// Has any node of `chain` a CAPTURING listener for `ty`?
fn chain_captures(scope: &mut v8::PinScope, chain: &[NodeId], ty: &str) -> bool {
    chain.iter().any(|a| crate::core::events::event_target::node_has_listener(scope, *a, ty, true))
}

/// `BoundaryEventDispatcher::SendBoundaryEvents`: `out` at the element left, `leave` up its exclusive ancestors
/// (innermost first), `over` at the element entered, `enter` down its exclusive ancestors (outermost first). An
/// enter or leave goes only to a node with a listener for it, unless a node of its chain has a CAPTURING one
/// (the "contrived scenarios" note in `boundary_event_dispatcher.cc`; observable: a page whose only listeners
/// capture at the window sees no enter/leave at all).
fn send_boundary(scope: &mut v8::PinScope, st: &St, doc: u32, family: &Family, exited: Option<NodeId>, entered: Option<NodeId>, base: &Spec) -> usize {
    let exited = exited.filter(|e| in_doc(st, *e, doc));
    if exited == entered {
        return 0;
    }
    let mut n = 0;
    let one = |kind: &str, related: Option<NodeId>| {
        let enter_or_leave = kind.ends_with("enter") || kind.ends_with("leave");
        let mut s = base.clone();
        s.kind = kind.to_string();
        s.related = related;
        s.bubbles = !enter_or_leave;
        s.cancelable = !enter_or_leave;
        s.composed = !enter_or_leave;
        s.detail = 0;
        s.movement = (0.0, 0.0);
        if let Some(p) = s.pointer.as_mut() {
            p.coalesced = None;
        }
        s
    };
    if let Some(e) = exited {
        fire(scope, st, e, &one(family.out, entered));
        n += 1;
    }
    let exited_chain = ancestor_chain(st, exited, doc);
    let entered_chain = ancestor_chain(st, entered, doc);
    let (mut ei, mut ni) = (exited_chain.len(), entered_chain.len());
    while ei > 0 && ni > 0 && exited_chain[ei - 1] == entered_chain[ni - 1] {
        ei -= 1;
        ni -= 1;
    }
    let exited_capturing = chain_captures(scope, &exited_chain, family.leave);
    for &node in &exited_chain[..ei] {
        if exited_capturing || crate::core::events::event_target::node_has_listener(scope, node, family.leave, false) {
            fire(scope, st, node, &one(family.leave, entered));
            n += 1;
        }
    }
    if let Some(e) = entered.filter(|e| in_doc(st, *e, doc)) {
        fire(scope, st, e, &one(family.over, exited));
        n += 1;
    }
    // Located AFTER the leaves: a leave handler may have added a capturing enter listener.
    let entered_capturing = chain_captures(scope, &entered_chain, family.enter);
    for &node in entered_chain[..ni].iter().rev() {
        if entered_capturing || crate::core::events::event_target::node_has_listener(scope, node, family.enter, false) {
            fire(scope, st, node, &one(family.enter, exited));
            n += 1;
        }
    }
    n
}

// ── the per-document steps ───────────────────────────────────────────────────────────────────────────

/// One input as one document sees it: the point in its client space, and everything else as the platform said.
struct At<'a> {
    doc: u32,
    page: u32,
    x: f64,
    y: f64,
    input: &'a MouseInput,
    origin: (f64, f64),
}

impl At<'_> {
    fn screen(&self) -> (f64, f64) {
        (self.origin.0 + self.input.x, self.origin.1 + self.input.y)
    }

    /// The base of every event this input makes in this document.
    fn spec(&self, iface: Iface, kind: &str, button: i32, pointer: Option<PointerFields>) -> Spec {
        Spec {
            iface,
            kind: kind.to_string(),
            bubbles: true,
            cancelable: true,
            composed: true,
            detail: 0,
            client: (self.x, self.y),
            screen: self.screen(),
            movement: (0.0, 0.0),
            button,
            buttons: self.input.buttons,
            modifiers: self.input.modifiers,
            related: None,
            pointer,
            wheel: None,
            timestamp_ms: self.input.timestamp_ms,
            view_doc: None,
            transfer: None,
        }
    }
}

/// `GetMouseEventTarget`: the frame element a press went into, else the mouse's PENDING capture target (a mouse
/// event goes to the element that is about to capture it), else what is under the point. A press is never
/// captured.
fn mouse_target(st: &St, at: &At, press: bool) -> Option<NodeId> {
    if !press {
        let (sub, capture) = with_doc(st, at.doc, |d| (d.capturing_subframe, d.pending_capture));
        if let Some(f) = sub.filter(|f| in_doc(st, *f, at.doc) && subframe_of(st, *f).is_some()) {
            return Some(f);
        }
        if let Some(c) = capture.filter(|c| in_doc(st, *c, at.doc)) {
            return Some(c);
        }
    }
    hit(st, at.doc, at.page, at.x, at.y)
}

/// `ProcessPendingPointerCapture`: when the pending capture differs from the current one, `lostpointercapture` at
/// the old target (its document if it left the tree) and `gotpointercapture` at the new one.
fn process_pending_capture(scope: &mut v8::PinScope, st: &St, at: &At, base: &Spec) -> usize {
    let (current, pending) = with_doc(st, at.doc, |d| (d.capture, d.pending_capture));
    if current == pending {
        return 0;
    }
    let mut n = 0;
    let capture_event = |kind: &str| {
        let mut s = base.clone();
        s.kind = kind.to_string();
        s.cancelable = false;
        s.movement = (0.0, 0.0);
        s.related = None;
        if let Some(p) = s.pointer.as_mut() {
            p.coalesced = None;
        }
        s
    };
    if let Some(old) = current {
        with_doc(st, at.doc, |d| d.capture = None);
        let target = if in_doc(st, old, at.doc) { old } else { at.doc };
        fire(scope, st, target, &capture_event("lostpointercapture"));
        n += 1;
    }
    if let Some(new) = pending.filter(|p| in_doc(st, *p, at.doc)) {
        set_under_pointer(scope, st, at, Some(new), base);
        fire(scope, st, new, &capture_event("gotpointercapture"));
        n += 1;
        if in_doc(st, new, at.doc) {
            with_doc(st, at.doc, |d| d.capture = Some(new));
        } else {
            fire(scope, st, at.doc, &capture_event("lostpointercapture"));
            n += 1;
        }
    }
    n
}

/// `SetElementUnderPointer`: the pointer boundary events, from the pointer event `base`.
fn set_under_pointer(scope: &mut v8::PinScope, st: &St, at: &At, target: Option<NodeId>, base: &Spec) -> usize {
    let exited = with_doc(st, at.doc, |d| std::mem::replace(&mut d.under_pointer, target));
    send_boundary(scope, st, at.doc, &POINTER_FAMILY, exited, target, base)
}

/// `SetElementUnderMouse`: the mouse boundary events, from the platform mouse event.
fn set_under_mouse(scope: &mut v8::PinScope, st: &St, at: &At, target: Option<NodeId>) -> usize {
    let exited = with_doc(st, at.doc, |d| std::mem::replace(&mut d.under_mouse, target));
    let base = at.spec(Iface::Mouse, "mouseover", at.input.button, None);
    send_boundary(scope, st, at.doc, &MOUSE_FAMILY, exited, target, &base)
}

/// The pointer event a mouse input makes in this document (`PointerEventFactory::Create`): its type -- a
/// press or release while other buttons stay held is a `pointermove` ("chorded buttons") -- its button (-1 for a
/// move), and its movement from this document's last position, which it then moves on.
fn pointer_event(st: &St, at: &At) -> Spec {
    let input = at.input;
    let mut kind = match input.kind {
        MouseKind::Move => "pointermove",
        MouseKind::Down => "pointerdown",
        MouseKind::Up => "pointerup",
    };
    let chorded = match input.kind {
        MouseKind::Down => input.buttons & !button_bit(input.button) != 0,
        MouseKind::Up => input.buttons != 0,
        MouseKind::Move => false,
    };
    if chorded {
        kind = "pointermove";
    }
    let button = if input.kind == MouseKind::Move { NO_BUTTON } else { input.button };
    let screen = at.screen();
    let last = with_doc(st, at.doc, |d| d.last_position.replace(screen));
    let movement = if kind == "pointermove" {
        let last = last.unwrap_or(screen);
        (screen.0.trunc() - last.0.trunc(), screen.1.trunc() - last.1.trunc())
    } else {
        (0.0, 0.0)
    };
    let coalesced = (kind == "pointermove").then(|| coalesced_samples(at, last.unwrap_or(screen)));
    let mut spec = at.spec(Iface::Pointer, kind, button, Some(mouse_pointer(input.buttons, coalesced)));
    spec.movement = movement;
    spec
}

/// The coalesced list of a move: one entry per raw sample merged into it, each with its movement from the one
/// before (`PointerEventFactory::CreateEventSequence`); a move alone carries itself.
fn coalesced_samples(at: &At, mut last: (f64, f64)) -> Vec<[f64; 7]> {
    let samples: Vec<(f64, f64, f64)> = if at.input.coalesced.is_empty() {
        vec![(at.input.x, at.input.y, at.input.timestamp_ms)]
    } else {
        at.input.coalesced.clone()
    };
    let (dx, dy) = (at.x - at.input.x, at.y - at.input.y);
    samples
        .into_iter()
        .map(|(x, y, t)| {
            let screen = (at.origin.0 + x, at.origin.1 + y);
            let m = (screen.0.trunc() - last.0.trunc(), screen.1.trunc() - last.1.trunc());
            last = screen;
            [x + dx, y + dy, screen.0, screen.1, m.0, m.1, t]
        })
        .collect()
}

/// `PointerEventManager::SendMousePointerEvent` in one document: capture, the two boundary families, the pointer
/// event, its compatibility mouse event, and for a release the click. Returns the events dispatched and whether
/// the pointer and mouse events went uncancelled.
fn send_mouse_pointer(scope: &mut v8::PinScope, st: &St, at: &At, target: Option<NodeId>) -> (usize, bool) {
    let input = at.input;
    let mut n = 0;
    // The mouse event's movement is measured from the position BEFORE the pointer event moves it on.
    let last_mouse = with_doc(st, at.doc, |d| d.last_position);
    let pointer = pointer_event(st, at);
    if input.kind == MouseKind::Down && input.buttons != 0 {
        with_doc(st, at.doc, |d| d.active_buttons = true);
    }
    // "This is for when the mouse is released outside of the page."
    if input.kind == MouseKind::Move && input.buttons == 0 {
        with_doc(st, at.doc, |d| {
            d.pending_capture = None;
            d.prevent_mouse = false;
        });
    }
    // ProcessCaptureAndPositionOfPointerEvent
    n += process_pending_capture(scope, st, at, &pointer);
    let capture = with_doc(st, at.doc, |d| d.capture).filter(|c| in_doc(st, *c, at.doc));
    let effective = capture.or(target);
    n += set_under_pointer(scope, st, at, effective, &pointer);
    n += set_under_mouse(scope, st, at, effective);
    let mut handled = false;
    if let Some(t) = effective {
        handled |= !fire(scope, st, t, &pointer);
        n += 1;
    }
    if handled && pointer.kind == "pointerdown" {
        with_doc(st, at.doc, |d| d.prevent_mouse = true);
    }
    let send_compat = !with_doc(st, at.doc, |d| d.prevent_mouse);
    let consider_click = input.kind == MouseKind::Up;
    let mouse_target = with_doc(st, at.doc, |d| d.under_mouse).filter(|m| in_doc(st, *m, at.doc));
    if send_compat {
        if let Some(m) = mouse_target {
            let kind = match input.kind {
                MouseKind::Move => "mousemove",
                MouseKind::Down => "mousedown",
                MouseKind::Up => "mouseup",
            };
            let mut spec = at.spec(Iface::Mouse, kind, input.button, None);
            if input.kind == MouseKind::Move {
                let screen = at.screen();
                let last = last_mouse.unwrap_or(screen);
                spec.movement = (screen.0.trunc() - last.0.trunc(), screen.1.trunc() - last.1.trunc());
            } else {
                spec.detail = with_doc(st, at.doc, |d| d.click_count);
            }
            handled |= !fire(scope, st, m, &spec);
            n += 1;
        }
    }
    let captured_click_target = if consider_click { capture } else { None };
    let mut release_target = target;
    if pointer.kind == "pointerup" {
        with_doc(st, at.doc, |d| {
            d.pending_capture = None;
            d.prevent_mouse = false;
            d.active_buttons = false;
        });
        // A capture being released: the element now under the pointer is hit-tested again.
        if capture.is_some() {
            release_target = hit(st, at.doc, at.page, at.x, at.y);
        }
    }
    if consider_click {
        if let Some(m) = mouse_target {
            n += process_pending_capture(scope, st, at, &pointer);
            n += click_if_needed(scope, st, at, m, captured_click_target);
        }
    }
    if pointer.kind == "pointerup" {
        // If a click was dispatched above, this only moves the element under the pointer and mouse.
        n += process_pending_capture(scope, st, at, &pointer);
        let capture = with_doc(st, at.doc, |d| d.capture).filter(|c| in_doc(st, *c, at.doc));
        let effective = capture.or(release_target);
        n += set_under_pointer(scope, st, at, effective, &pointer);
        n += set_under_mouse(scope, st, at, effective);
    }
    (n, !handled)
}

/// `ParentForClickEvent`: the flat-tree parent.
fn common_ancestor(st: &St, a: NodeId, b: NodeId) -> Option<NodeId> {
    let s = st.borrow();
    let dom = &s.try_borrow::<HostDom>()?.0;
    let mut chain_a = Vec::new();
    let mut cur = Some(a);
    while let Some(n) = cur {
        chain_a.push(n);
        cur = flat_parent(dom, n);
    }
    let mut cur = Some(b);
    while let Some(n) = cur {
        if chain_a.contains(&n) {
            return Some(n);
        }
        cur = flat_parent(dom, n);
    }
    None
}

/// `MouseEventManager::DispatchMouseClickIfNeeded`, then `MouseEvent::DispatchEvent`'s `dblclick`.
fn click_if_needed(scope: &mut v8::PinScope, st: &St, at: &At, release: NodeId, captured: Option<NodeId>) -> usize {
    let (count, down) = with_doc(st, at.doc, |d| (d.click_count, d.mousedown_element));
    let Some(down) = down.filter(|d| in_doc(st, *d, at.doc)) else { return 0 };
    if count <= 0 {
        return 0;
    }
    let Some(target) = captured.or_else(|| common_ancestor(st, release, down)) else { return 0 };
    let left = at.input.button == 0;
    let kind = if left { "click" } else { "auxclick" };
    // A PointerEvent with the mouse's id and type but NOT primary and no device id: `SetMouseEventAttributes`
    // plus `setPointerId` / `setPointerType` only (measured: isPrimary false, persistentDeviceId 0).
    let pointer = PointerFields { pointer_id: MOUSE_POINTER_ID, pointer_type: "mouse", is_primary: false, pressure: 0.0, persistent_device_id: 0, coalesced: None };
    let mut spec = at.spec(Iface::Pointer, kind, at.input.button, Some(pointer));
    spec.detail = count;
    fire(scope, st, target, &spec);
    let mut n = 1;
    if left && count == 2 {
        let mut dbl = at.spec(Iface::Mouse, "dblclick", at.input.button, None);
        dbl.detail = 2;
        fire(scope, st, target, &dbl);
        n += 1;
    }
    n
}

/// `HandleMouseFocus` after an uncancelled press: the nearest focusable ancestor-or-self of the hit element takes
/// the focus, or the ring is cleared; a press in another FRAME also moves the window focus -- `blur` at the window
/// that had it, `focus` at the frame's (measured: a press in a frame fired the page's `blur`/`focusout` on its
/// focused element, then `blur` on the page's window, `focus` on the frame's, and the page's
/// `document.activeElement` became the `<iframe>`).
fn press_focus(scope: &mut v8::PinScope, st: &St, at: &At, hit_el: NodeId, frame_el: Option<NodeId>) -> usize {
    let ts = at.input.timestamp_ms;
    let to = {
        let s = st.borrow();
        let Some(h) = s.try_borrow::<HostDom>() else { return 0 };
        crate::core::events::focus::focus_target(&h.0, hit_el)
    };
    let new_frame = (at.doc != at.page).then_some(at.doc);
    let (old_frame, parked) = with_input(st, |p| (p.focused_frame, p.parked_on_frame.take()));
    if let Some(f) = parked {
        // The ring was parked on the frame element silently; it leaves it the same way.
        let parked_here = st.borrow().try_borrow::<crate::core::events::shared::FocusState>().is_some_and(|fs| fs.0.get() == Some(f));
        if parked_here {
            crate::core::events::shared::set_focus_ring(st, None);
        }
    }
    // a press focuses: its focus shows only on a text field (`:focus-visible`)
    st.borrow_mut().put(crate::core::events::shared::FocusByPointer(true));
    let mut n = crate::core::events::focus::move_focus_to(scope, to, ts);
    st.borrow_mut().put(crate::core::events::shared::FocusByPointer(false));
    // The <iframe> becomes the page's `activeElement` BEFORE the window focus moves: measured, the page window's
    // `blur` and the frame window's `focus` already read `document.activeElement === iframe`.
    if to.is_none() {
        if let Some(f) = frame_el {
            st.borrow_mut().put(crate::core::events::shared::FocusByPointer(true));
            crate::core::events::shared::set_focus_ring(st, Some(f));
            st.borrow_mut().put(crate::core::events::shared::FocusByPointer(false));
            with_input(st, |p| p.parked_on_frame = Some(f));
        }
    }
    if old_frame != new_frame {
        let old_doc = old_frame.unwrap_or(at.page);
        n += fire_window_focus(scope, st, old_doc, at.page, "blur", ts);
        n += fire_window_focus(scope, st, at.doc, at.page, "focus", ts);
        with_input(st, |p| p.focused_frame = new_frame);
    }
    n
}

/// A window's own `focus` / `blur`: a plain `Event`, neither bubbling nor cancelable nor composed.
fn fire_window_focus(scope: &mut v8::PinScope, st: &St, doc: u32, page: u32, kind: &str, ts: f64) -> usize {
    let realm = if doc == page {
        crate::core::html::browsing_context::main_context(scope)
    } else {
        match crate::core::html::browsing_context::realm_of_node(scope, doc) {
            Some(r) => r,
            None => return 0,
        }
    };
    let Some(win) = window_of_doc(scope, st, doc, page) else { return 0 };
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return 0 };
    let init = crate::binding::protos::init_dict(scope);
    let ty = str_val(scope, kind);
    let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) else { return 0 };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", ts);
    crate::core::events::event_target::et_dispatch(scope, win.into(), ev);
    1
}

/// USER ACTIVATION (left for C7's shared function, see the coordinator's note): Blink notifies the frame's user
/// activation (`LocalFrame::NotifyUserActivation(kInteraction)`) on a trusted mouse press before the press is
/// dispatched (`EventHandler::HandleMousePressEvent`). Activation is one state for the page (C7's
/// `user_activation`), so the frame's document is not needed yet.
fn notify_user_activation(scope: &mut v8::PinScope, _doc: u32) {
    super::user_activation::notify_user_activation(scope);
}

/// `HandleMouseMoveOrLeaveEvent` in one document, passed on into a frame the pointer is over.
fn move_in(scope: &mut v8::PinScope, st: &St, at: &At) -> usize {
    let mut n = 0;
    let element = mouse_target(st, at, false);
    let sub = element.and_then(|e| subframe_of(st, e).map(|d| (e, d)));
    // "We want mouseouts to happen first, from the inside out": the frame the pointer was in gets its leave.
    let last = with_doc(st, at.doc, |d| d.last_subframe);
    if let Some(last) = last.filter(|l| Some(*l) != sub.map(|s| s.0)) {
        n += leave_frame(scope, st, at, last);
    }
    if let Some((frame_el, sub_doc)) = sub {
        // Over a frame: this document's boundary events go to the <iframe>, and the move goes on inside.
        let pointer = pointer_event(st, at);
        n += process_pending_capture(scope, st, at, &pointer);
        n += set_under_pointer(scope, st, at, Some(frame_el), &pointer);
        n += set_under_mouse(scope, st, at, Some(frame_el));
        with_doc(st, at.doc, |d| d.last_subframe = Some(frame_el));
        if let Some((ox, oy)) = frame_origin(st, at.doc, at.page, frame_el) {
            let inner = At { doc: sub_doc, page: at.page, x: at.x - ox, y: at.y - oy, input: at.input, origin: at.origin };
            n += move_in(scope, st, &inner);
        }
        return n;
    }
    with_doc(st, at.doc, |d| d.last_subframe = None);
    n + send_mouse_pointer(scope, st, at, element).0
}

/// A frame the pointer left: Blink's `HandleMouseLeaveEvent` there -- its own boundary events to nothing (at the
/// frame-relative point), and its last position forgotten.
fn leave_frame(scope: &mut v8::PinScope, st: &St, at: &At, frame_el: NodeId) -> usize {
    with_doc(st, at.doc, |d| d.last_subframe = None);
    let Some(sub_doc) = subframe_of(st, frame_el) else { return 0 };
    let Some((ox, oy)) = frame_origin(st, at.doc, at.page, frame_el) else { return 0 };
    let inner = At { doc: sub_doc, page: at.page, x: at.x - ox, y: at.y - oy, input: at.input, origin: at.origin };
    let mut n = 0;
    if let Some(last) = with_doc(st, sub_doc, |d| d.last_subframe) {
        n += leave_frame(scope, st, &inner, last);
    }
    let pointer = pointer_event(st, &inner);
    n += set_under_pointer(scope, st, &inner, None, &pointer);
    n += set_under_mouse(scope, st, &inner, None);
    with_doc(st, sub_doc, |d| {
        d.last_position = None;
        d.last_raw_position = None;
    });
    n
}

/// `HandleMousePressEvent` in one document.
fn press_in(scope: &mut v8::PinScope, st: &St, at: &At, frame_el: Option<NodeId>) -> usize {
    with_doc(st, at.doc, |d| {
        d.capturing_subframe = None;
        d.mouse_pressed = true;
    });
    let Some(element) = mouse_target(st, at, true) else {
        with_doc(st, at.doc, |d| d.click_count = 0);
        return 0;
    };
    if let Some(sub_doc) = subframe_of(st, element) {
        let mut n = 0;
        if let Some((ox, oy)) = frame_origin(st, at.doc, at.page, element) {
            let inner = At { doc: sub_doc, page: at.page, x: at.x - ox, y: at.y - oy, input: at.input, origin: at.origin };
            n += press_in(scope, st, &inner, Some(element));
        }
        if with_doc(st, sub_doc, |d| d.mouse_pressed) {
            with_doc(st, at.doc, |d| d.capturing_subframe = Some(element));
        }
        with_doc(st, at.doc, |d| d.click_count = 0);
        return n;
    }
    // USER-ACTIVATION HOOK (C7): `LocalFrame::NotifyUserActivation(frame_, kInteraction)` comes here, before the
    // pointerdown is dispatched.
    notify_user_activation(scope, at.doc);
    with_doc(st, at.doc, |d| {
        d.click_count = at.input.click_count;
        d.mousedown_element = Some(element);
    });
    let (mut n, uncancelled) = send_mouse_pointer(scope, st, at, Some(element));
    if uncancelled {
        n += press_focus(scope, st, at, element, frame_el);
    }
    n
}

/// `HandleMouseReleaseEvent` in one document.
fn release_in(scope: &mut v8::PinScope, st: &St, at: &At) -> usize {
    with_doc(st, at.doc, |d| d.mouse_pressed = false);
    let element = mouse_target(st, at, false);
    with_doc(st, at.doc, |d| d.capturing_subframe = None);
    if let Some((frame_el, sub_doc)) = element.and_then(|e| subframe_of(st, e).map(|d| (e, d))) {
        if let Some((ox, oy)) = frame_origin(st, at.doc, at.page, frame_el) {
            let inner = At { doc: sub_doc, page: at.page, x: at.x - ox, y: at.y - oy, input: at.input, origin: at.origin };
            return release_in(scope, st, &inner);
        }
        return 0;
    }
    send_mouse_pointer(scope, st, at, element).0
}

/// The deepest document and element under `(x, y)` -- following the frames and the pointer capture -- and the
/// point in that document. What a `pointerrawupdate` and a `contextmenu` are aimed at.
fn deepest(st: &St, page: u32, x: f64, y: f64, honour_capture: bool) -> Option<(u32, NodeId, f64, f64)> {
    let (mut doc, mut x, mut y) = (page, x, y);
    for _ in 0..16 {
        let at_capture = if honour_capture { with_doc(st, doc, |d| d.pending_capture.or(d.capture)).filter(|c| in_doc(st, *c, doc)) } else { None };
        let el = match at_capture {
            Some(c) => return Some((doc, c, x, y)),
            None => hit(st, doc, page, x, y)?,
        };
        match subframe_of(st, el) {
            Some(sub) => {
                let (ox, oy) = frame_origin(st, doc, page, el)?;
                doc = sub;
                x -= ox;
                y -= oy;
            }
            None => return Some((doc, el, x, y)),
        }
    }
    None
}

/// `pointerrawupdate` for one raw sample: at the deepest element (or the capture target), with the PAGE's client
/// coordinates and window, its movement measured from the page's last raw position (measured: a raw update at a
/// frame's element reported `view === top window` and the page's `clientX/Y`).
fn raw_update(scope: &mut v8::PinScope, st: &St, page: u32, input: &MouseInput, sample: (f64, f64, f64), origin: (f64, f64)) -> usize {
    let Some((_, target, _, _)) = deepest(st, page, sample.0, sample.1, true) else { return 0 };
    let screen = (origin.0 + sample.0, origin.1 + sample.1);
    let last = with_doc(st, page, |d| d.last_raw_position.replace(screen)).unwrap_or(screen);
    let movement = (screen.0.trunc() - last.0.trunc(), screen.1.trunc() - last.1.trunc());
    let one = [sample.0, sample.1, screen.0, screen.1, movement.0, movement.1, sample.2];
    let spec = Spec {
        iface: Iface::Pointer,
        kind: "pointerrawupdate".to_string(),
        bubbles: true,
        cancelable: false,
        composed: true,
        detail: 0,
        client: (sample.0, sample.1),
        screen,
        movement,
        button: NO_BUTTON,
        buttons: input.buttons,
        modifiers: input.modifiers,
        related: None,
        pointer: Some(mouse_pointer(input.buttons, Some(vec![one]))),
        wheel: None,
        timestamp_ms: sample.2,
        view_doc: Some(page),
        transfer: None,
    };
    fire(scope, st, target, &spec);
    1
}

/// Commit `:hover` to the chain under the mouse: in each document the input passed through, the element under the
/// mouse and its flat-tree element ancestors, outermost document first. Committed BEFORE the first event (hover.rs
/// measured it: a `mouseout` handler already sees the new chain).
fn commit_hover(st: &St, page: u32, x: f64, y: f64) {
    let mut chain: Vec<NodeId> = Vec::new();
    let (mut doc, mut x, mut y) = (page, x, y);
    for _ in 0..16 {
        let capture = with_doc(st, doc, |d| d.capturing_subframe.or(d.pending_capture)).filter(|c| in_doc(st, *c, doc));
        let Some(el) = capture.or_else(|| hit(st, doc, page, x, y)) else { break };
        let mut local: Vec<NodeId> = ancestor_chain(st, Some(el), doc);
        {
            let s = st.borrow();
            if let Some(h) = s.try_borrow::<HostDom>() {
                local.retain(|n| h.0.node_type(*n) == 1);
            }
        }
        local.reverse();
        chain.extend(local);
        match subframe_of(st, el).zip(frame_origin(st, doc, page, el)) {
            Some((sub, (ox, oy))) => {
                doc = sub;
                x -= ox;
                y -= oy;
            }
            None => break,
        }
    }
    let mut s = st.borrow_mut();
    if let Some(h) = s.try_borrow_mut::<HostDom>() {
        h.0.set_hover_chain(chain);
    }
}

/// ABI 3 (step 12, C3, `on_cursor`): tell the plugins the CSS cursor of the element under the pointer, but only
/// when it CHANGED since the last report (Chrome recomputes the cursor at the end of a move -- `EventHandler::
/// UpdateCursor` -- so this runs after `move_in` has committed the hover chain and run its handlers).
///
/// The shape is the element's *computed* `cursor` property (`css_computed_one`): a UA rule (`input` -> "text",
/// `<button>` -> "default") or an author `cursor:` declaration is reported as written. korvus has no used-value
/// cursor resolution of its own, so an element whose computed `cursor` is `auto` is reported as "auto" -- a bare
/// link reads "auto", NOT "pointer" (Chrome's pointer-over-link is `SelectCursor`'s used value, not a UA rule).
/// TODO(1021-verify): fold in a `SelectCursor`-style `auto` -> pointer/text resolution if the plugins need it.
///
/// Nothing here runs -- no hit test, no style read -- unless a plugin fills `on_cursor`/`on_focus_state`
/// (`plugin_host::wants_cursor`), so with no such plugin the move path is byte-for-byte what it was (I9/J2).
fn update_cursor(st: &St, page: u32, x: f64, y: f64) {
    if !plugin_host::wants_cursor() {
        return;
    }
    // The element the pointer reports being over, honouring pointer capture -- the box whose cursor Chrome shows.
    let Some((_, el, _, _)) = deepest(st, page, x, y, true) else { return };
    let shape = {
        let mut s = st.borrow_mut();
        crate::core::css::cssom::css_computed_one(&mut s, el, "cursor")
    };
    // An unknown / absent computed value reads as the initial `cursor`, which is `auto`.
    let shape = if shape.is_empty() { "auto".to_string() } else { shape };
    let changed = with_input(st, |p| {
        if p.last_cursor.as_deref() == Some(shape.as_str()) {
            false
        } else {
            p.last_cursor = Some(shape.clone());
            true
        }
    });
    if changed {
        plugin_host::on_cursor(&shape);
    }
}

// ── the entry points ─────────────────────────────────────────────────────────────────────────────────

/// Is the page a secure context (`pointerrawupdate` is `[SecureContext]`, Blink's
/// `PointerRawUpdateOnlyInSecureContext`)?
fn page_is_secure(scope: &mut v8::PinScope) -> bool {
    let main = crate::core::html::browsing_context::main_context(scope);
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, main);
    let href = crate::core::html::window::href(scope);
    crate::core::html::window::potentially_trustworthy(&href)
}

/// Deliver one platform mouse event to the page, as Blink 150 does. Returns how many DOM events it dispatched.
pub fn mouse_input(scope: &mut v8::PinScope, input: &MouseInput) -> usize {
    mouse_input_at(scope, input, None)
}

/// [`mouse_input`] into the window whose document is `top` (`None`: the page's) -- a popup this process shows, which
/// a DevTools client drives as a page of its own.
pub fn mouse_input_at(scope: &mut v8::PinScope, input: &MouseInput, top: Option<u32>) -> usize {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let Some(page) = top.or_else(|| page_root(&st)) else { return 0 };
    // A press or release with no button is swallowed (`kHandledSuppressed`).
    if input.kind != MouseKind::Move && input.button == NO_BUTTON {
        return 0;
    }
    let origin = screen_origin(&st);
    let mut n = 0;
    let at = At { doc: page, page, x: input.x, y: input.y, input, origin };
    match input.kind {
        MouseKind::Move => {
            // A drag this page runs itself: the mouse moves the drag, and no mouse event is dispatched while it does.
            if with_input(&st, |p| p.drag.session.as_ref().is_some_and(|s| !s.intercepted)) {
                return drag_over(scope, &st, page, input.x, input.y, input.buttons, input.modifiers, input.timestamp_ms);
            }
            if crate::core::events::event_target::any_listener(scope, "pointerrawupdate") && page_is_secure(scope) {
                let samples: Vec<(f64, f64, f64)> =
                    if input.coalesced.is_empty() { vec![(input.x, input.y, input.timestamp_ms)] } else { input.coalesced.clone() };
                for s in samples {
                    n += raw_update(scope, &st, page, input, s, origin);
                }
            }
            commit_hover(&st, page, input.x, input.y);
            n += move_in(scope, &st, &at);
            // ABI 3 (step 12, C3): the cursor is recomputed at the end of a move, after the hover chain and its
            // handlers have run (no-op unless a plugin watches it).
            update_cursor(&st, page, input.x, input.y);
            // the move that carries a pressed draggable past the drag threshold starts its drag
            if input.buttons & 1 != 0 {
                n += drag_start_if_moved(scope, &st, page, input);
            }
        }
        MouseKind::Down => {
            commit_hover(&st, page, input.x, input.y);
            n += press_in(scope, &st, &at, None);
            if input.button == 0 {
                drag_pressed(&st, page, input.x, input.y);
            }
            // Linux shows the context menu on the press: `contextmenu` goes to the element under the pointer in
            // its own frame, right after the press (`WebFrameWidgetImpl::HandleMouseDown` -> `MouseContextMenu`).
            if input.button == 2 {
                if let Some((doc, el, x, y)) = deepest(&st, page, input.x, input.y, false) {
                    with_doc(&st, doc, |d| d.mouse_pressed = false);
                    let inner = At { doc, page, x, y, input, origin };
                    let pointer = PointerFields { pointer_id: MOUSE_POINTER_ID, pointer_type: "mouse", is_primary: false, pressure: 0.0, persistent_device_id: 0, coalesced: None };
                    let spec = inner.spec(Iface::Pointer, "contextmenu", input.button, Some(pointer));
                    fire(scope, &st, el, &spec);
                    n += 1;
                }
            }
        }
        MouseKind::Up => {
            with_input(&st, |p| p.drag.candidate = None);
            // the release ends a drag this page runs itself: its drop, and no mouseup (Blink cancels the press)
            if with_input(&st, |p| p.drag.session.as_ref().is_some_and(|s| !s.intercepted)) {
                with_doc(&st, page, |d| d.mouse_pressed = false);
                return drag_drop(scope, &st, page, input.x, input.y, input.modifiers, input.timestamp_ms);
            }
            n += release_in(scope, &st, &at);
            commit_hover(&st, page, input.x, input.y);
        }
    }
    n
}

// ── drag and drop ────────────────────────────────────────────────────────────────────────────────────
//
// "drag and selection by mouse are not modelled" was this file's own gap: a press on a `draggable` element and a move
// past the threshold started nothing, so no page could be dragged in (Playwright's `dragTo` timed out; Puppeteer's
// `drag` the same). Blink's: a left press on a draggable element (`draggable=true`, a link, an image -- not one with
// `draggable=false`), then a move more than 4 px away while the button is held, dispatches `dragstart` at it with a
// read/write `DataTransfer`; if nobody cancels it the drag begins. From then on the mouse moves the drag --
// `dragenter` at a new element under it, `dragleave` at the one it left, `dragover` at the one it is over, each with
// a protected `DataTransfer` -- no mouse event is dispatched, and the release drops: `drop` at the element under it
// with a read-only `DataTransfer` when its last `dragover` was cancelled (else its `dragleave`), then `dragend` at the
// source. A DevTools client that intercepts drags (`Input.setInterceptDrags`) is handed the drag data store instead
// (`Input.dragIntercepted`) and drives the rest with `Input.dispatchDragEvent` ([`dispatch_drag`]).

/// `kDragThresholdX` / `kDragThresholdY` (`mouse_event_manager.cc`).
const DRAG_THRESHOLD: f64 = 4.0;

static INTERCEPT_DRAGS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static INTERCEPTED: std::sync::Mutex<Option<(Vec<(String, String)>, u32)>> = std::sync::Mutex::new(None);

/// `Input.setInterceptDrags`: a drag the page starts is the client's.
pub fn set_intercept_drags(on: bool) {
    INTERCEPT_DRAGS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The drag a page started since the last call while the client intercepts drags: its data store's string items and
/// Chrome's `dragOperationsMask` (copy 1, link 2, move 16), for `Input.dragIntercepted`.
pub fn take_drag_intercepted() -> Option<(Vec<(String, String)>, u32)> {
    INTERCEPTED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// `effectAllowed` as a drag operations mask.
fn operations_mask(allowed: &str) -> u32 {
    match allowed {
        "none" => 0,
        "copy" => 1,
        "link" => 2,
        "move" => 16,
        "copyLink" => 3,
        "copyMove" => 17,
        "linkMove" => 18,
        _ => 19,
    }
}

/// The `effectAllowed` a client's operations mask stands for.
fn allowed_of_mask(mask: u32) -> &'static str {
    match (mask & 1 != 0, mask & 2 != 0, mask & 16 != 0) {
        (false, false, false) => "none",
        (true, false, false) => "copy",
        (false, true, false) => "link",
        (false, false, true) => "move",
        (true, true, false) => "copyLink",
        (true, false, true) => "copyMove",
        (false, true, true) => "linkMove",
        (true, true, true) => "all",
    }
}

/// The `dropEffect` a drag event starts with (HTML 6.11.5): what `effectAllowed` allows, preferring copy, link, move.
fn initial_drop_effect(allowed: &str) -> &'static str {
    match allowed {
        "none" => "none",
        "move" | "linkMove" => "move",
        "link" | "copyLink" if allowed == "link" => "link",
        _ => "copy",
    }
}

/// Is `el` draggable (HTML `draggable`): its attribute when it says `true` or `false`, else a link with an `href` or an
/// image.
fn draggable(dom: &korvus_dom::Dom, el: NodeId) -> bool {
    if dom.node_type(el) != 1 {
        return false;
    }
    match dom.get_attribute(el, "draggable").map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "true" => true,
        Some(v) if v == "false" => false,
        _ => match dom.tag_name(el).map(str::to_ascii_lowercase).as_deref() {
            Some("img") => true,
            Some("a") => dom.get_attribute(el, "href").is_some(),
            _ => false,
        },
    }
}

/// A left press: the draggable element it went to (the hit element or its nearest draggable ancestor in its document),
/// kept until the move that starts its drag or the release.
fn drag_pressed(st: &St, page: u32, x: f64, y: f64) {
    let Some((doc, el, _, _)) = deepest(st, page, x, y, false) else { return };
    let found = {
        let s = st.borrow();
        let Some(h) = s.try_borrow::<HostDom>() else { return };
        let mut n = Some(el);
        let mut found = None;
        while let Some(id) = n {
            if id == doc {
                break;
            }
            if draggable(&h.0, id) {
                found = Some(id);
                break;
            }
            n = flat_parent(&h.0, id);
        }
        found
    };
    with_input(st, |p| p.drag.candidate = found.map(|e| (e, x, y)));
}

/// A drag event of `kind` at `target`, its `DataTransfer` of `items` in `mode`: whether nobody cancelled it, and the
/// `dropEffect` and data store the page left.
#[allow(clippy::too_many_arguments)]
fn fire_drag(
    scope: &mut v8::PinScope,
    st: &St,
    page: u32,
    target: NodeId,
    kind: &str,
    x: f64,
    y: f64,
    session: &DragSession,
    mode: u8,
    buttons: u16,
    modifiers: Modifiers,
    timestamp_ms: f64,
) -> (bool, String, Option<(Vec<(String, String)>, String, String)>) {
    let (cx, cy) = match deepest(st, page, x, y, false) {
        Some((_, _, lx, ly)) => (lx, ly),
        None => (x, y),
    };
    let origin = screen_origin(st);
    let cancelable = !matches!(kind, "dragleave" | "dragend");
    let effect = initial_drop_effect(&session.effect_allowed).to_string();
    let spec = Spec {
        iface: Iface::Drag,
        kind: kind.to_string(),
        bubbles: true,
        cancelable,
        composed: true,
        detail: 0,
        client: (cx, cy),
        screen: (origin.0 + x, origin.1 + y),
        movement: (0.0, 0.0),
        button: 0,
        buttons,
        modifiers,
        related: None,
        pointer: None,
        wheel: None,
        timestamp_ms,
        view_doc: None,
        transfer: Some((session.items.clone(), mode, session.effect_allowed.clone(), effect.clone())),
    };
    let realm = crate::core::html::browsing_context::realm_of_node(scope, target).unwrap_or_else(|| crate::core::html::browsing_context::main_context(scope));
    // the event the page sees, kept to read its `DataTransfer` back after the dispatch
    LAST_DRAG.with(|l| l.borrow_mut().take());
    let go = fire_keeping(scope, st, target, &spec);
    let after = LAST_DRAG.with(|l| l.borrow_mut().take()).and_then(|g| {
        let scope = &mut v8::ContextScope::new(scope, realm);
        let ev = v8::Local::new(scope, &g);
        let ev = v8::Local::<v8::Object>::try_from(ev).ok()?;
        let dt = crate::core::events::event::evt_slot_read(scope, ev, "dataTransfer").or_else(|| crate::binding::protos::get_prop(scope, ev, "dataTransfer"))?;
        crate::core::clipboard::data_transfer::dt_read(scope, dt)
    });
    let effect = after.as_ref().map(|a| a.2.clone()).unwrap_or(effect);
    (go, effect, after)
}

thread_local! {
    /// The drag event [`fire_keeping`] dispatched last.
    static LAST_DRAG: std::cell::RefCell<Option<v8::Global<v8::Value>>> = const { std::cell::RefCell::new(None) };
}

/// Drop [`LAST_DRAG`]: a handle into the realm being torn down ([`crate::release_realm_handles`]).
pub(crate) fn forget_drag() {
    LAST_DRAG.with(|l| l.borrow_mut().take());
}

/// [`fire`], keeping the event it built ([`LAST_DRAG`]) so its `DataTransfer` can be read back.
fn fire_keeping(scope: &mut v8::PinScope, st: &St, target: NodeId, spec: &Spec) -> bool {
    KEEP_EVENT.with(|k| k.set(true));
    let go = fire(scope, st, target, spec);
    KEEP_EVENT.with(|k| k.set(false));
    go
}

thread_local! {
    static KEEP_EVENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The move that may start a drag: past the threshold from a pressed draggable, `dragstart` at it; if nobody cancels it
/// the drag begins -- the client's when it intercepts drags, else this page's own.
fn drag_start_if_moved(scope: &mut v8::PinScope, st: &St, page: u32, input: &MouseInput) -> usize {
    let Some((source, px, py)) = with_input(st, |p| p.drag.candidate) else { return 0 };
    if (input.x - px).abs() <= DRAG_THRESHOLD && (input.y - py).abs() <= DRAG_THRESHOLD {
        return 0;
    }
    with_input(st, |p| p.drag.candidate = None);
    if !in_doc(st, source, doc_of(st, source).unwrap_or(page)) {
        return 0;
    }
    let mut session = DragSession {
        source: Some(source),
        items: Vec::new(),
        effect_allowed: "uninitialized".to_string(),
        over: None,
        accepted: false,
        drop_effect: "none".to_string(),
        intercepted: INTERCEPT_DRAGS.load(std::sync::atomic::Ordering::Relaxed),
    };
    let (go, _, after) = fire_drag(scope, st, page, source, "dragstart", px, py, &session, crate::core::clipboard::data_transfer::DT_READ_WRITE, input.buttons, input.modifiers, input.timestamp_ms);
    if !go {
        return 1;
    }
    if let Some((items, allowed, _)) = after {
        session.items = items;
        session.effect_allowed = allowed;
    }
    // the press ends here: Blink hands the mouse to the drag
    with_doc(st, page, |d| d.mouse_pressed = false);
    if session.intercepted {
        *INTERCEPTED.lock().unwrap_or_else(|e| e.into_inner()) = Some((session.items.clone(), operations_mask(&session.effect_allowed)));
    }
    let intercepted = session.intercepted;
    with_input(st, |p| p.drag.session = Some(session));
    if !intercepted {
        // the drag's first position is over its own source
        return 1 + drag_over(scope, st, page, input.x, input.y, input.buttons, input.modifiers, input.timestamp_ms);
    }
    1
}

/// The drag at page point `(x, y)`: `dragenter` at a new element under it and `dragleave` at the one it left, then
/// `dragover` -- whose cancellation is what lets a drop land there.
#[allow(clippy::too_many_arguments)]
fn drag_over(scope: &mut v8::PinScope, st: &St, page: u32, x: f64, y: f64, buttons: u16, modifiers: Modifiers, ts: f64) -> usize {
    let Some(mut session) = with_input(st, |p| p.drag.session.take()) else { return 0 };
    let n = drag_over_in(scope, st, page, x, y, buttons, modifiers, ts, &mut session);
    with_input(st, |p| p.drag.session = Some(session));
    n
}

#[allow(clippy::too_many_arguments)]
fn drag_over_in(scope: &mut v8::PinScope, st: &St, page: u32, x: f64, y: f64, buttons: u16, modifiers: Modifiers, ts: f64, session: &mut DragSession) -> usize {
    let mut n = 0;
    let target = deepest(st, page, x, y, false).map(|t| t.1);
    let protected = crate::core::clipboard::data_transfer::DT_PROTECTED;
    if target != session.over {
        if let Some(t) = target {
            fire_drag(scope, st, page, t, "dragenter", x, y, session, protected, buttons, modifiers, ts);
            n += 1;
        }
        if let Some(old) = session.over {
            fire_drag(scope, st, page, old, "dragleave", x, y, session, protected, buttons, modifiers, ts);
            n += 1;
        }
        session.over = target;
    }
    session.accepted = false;
    if let Some(t) = target {
        let (go, effect, _) = fire_drag(scope, st, page, t, "dragover", x, y, session, protected, buttons, modifiers, ts);
        n += 1;
        session.accepted = !go && effect != "none";
        session.drop_effect = if session.accepted { effect } else { "none".to_string() };
    }
    n
}

/// The drop at page point `(x, y)`: `drop` where the last `dragover` was cancelled (else `dragleave` there), then
/// `dragend` at the source; the drag is over. `cancel`: no drop at all (`dragCancel`).
fn drag_drop(scope: &mut v8::PinScope, st: &St, page: u32, x: f64, y: f64, modifiers: Modifiers, ts: f64) -> usize {
    drag_end(scope, st, page, x, y, modifiers, ts, false)
}

#[allow(clippy::too_many_arguments)]
fn drag_end(scope: &mut v8::PinScope, st: &St, page: u32, x: f64, y: f64, modifiers: Modifiers, ts: f64, cancel: bool) -> usize {
    let Some(mut session) = with_input(st, |p| p.drag.session.take()) else { return 0 };
    let mut n = 0;
    // the drop lands where the drag is: a release elsewhere moves it there first
    if !cancel && deepest(st, page, x, y, false).map(|t| t.1) != session.over {
        n += drag_over_in(scope, st, page, x, y, 0, modifiers, ts, &mut session);
    }
    let mut effect = "none".to_string();
    match (session.over, session.accepted && !cancel) {
        (Some(t), true) => {
            let (go, e, _) = fire_drag(scope, st, page, t, "drop", x, y, &session, crate::core::clipboard::data_transfer::DT_READ_ONLY, 0, modifiers, ts);
            n += 1;
            // a drop nobody cancelled is the browser's to perform; a page's own drop is the page's
            effect = if go { session.drop_effect.clone() } else { e };
        }
        (Some(t), false) => {
            fire_drag(scope, st, page, t, "dragleave", x, y, &session, crate::core::clipboard::data_transfer::DT_PROTECTED, 0, modifiers, ts);
            n += 1;
        }
        (None, _) => {}
    }
    if let Some(src) = session.source.filter(|s| doc_of(st, *s).is_some()) {
        session.drop_effect = effect;
        fire_drag(scope, st, page, src, "dragend", x, y, &session, crate::core::clipboard::data_transfer::DT_PROTECTED, 0, modifiers, ts);
        n += 1;
    }
    n
}

/// `Input.dispatchDragEvent`: a DevTools client moving a drag -- the one it intercepted, or data it brings from outside
/// the page -- to page point `(x, y)`: `dragEnter` / `dragOver` move it, `drop` drops it, `dragCancel` ends it with no
/// drop. Returns how many DOM events were dispatched.
pub fn dispatch_drag(scope: &mut v8::PinScope, kind: &str, x: f64, y: f64, items: Vec<(String, String)>, mask: u32, modifiers: Modifiers, timestamp_ms: f64) -> usize {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let Some(page) = page_root(&st) else { return 0 };
    let open = with_input(&st, |p| p.drag.session.is_some());
    if !open {
        with_input(&st, |p| {
            p.drag.session = Some(DragSession {
                source: None,
                items: items.clone(),
                effect_allowed: allowed_of_mask(mask).to_string(),
                over: None,
                accepted: false,
                drop_effect: "none".to_string(),
                intercepted: true,
            })
        });
    }
    match kind {
        "dragEnter" | "dragOver" => drag_over(scope, &st, page, x, y, 1, modifiers, timestamp_ms),
        "drop" => drag_drop(scope, &st, page, x, y, modifiers, timestamp_ms),
        // `dragCancel`: a `dragleave` where it was and the source's `dragend`, with no drop
        _ => drag_end(scope, &st, page, x, y, modifiers, timestamp_ms, true),
    }
}

/// The client-space origin of frame element `el`'s content box in its own document's viewport -- where the frame's
/// viewport starts there (a DevTools quad of a node in the frame is its frame-local box moved by it).
pub(crate) fn frame_element_origin(scope: &mut v8::PinScope, el: NodeId) -> Option<(f64, f64)> {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let page = page_root(&st)?;
    let doc = doc_of(&st, el)?;
    frame_origin(&st, doc, page, el)
}

/// Where a finger at page point `(x, y)` lands: the element under it in its own frame, the point in that frame, and the
/// screen point -- what a `Touch` names (`TouchEventManager::HandleTouchPoint` hit-tests each new touch point).
pub(crate) fn touch_target(scope: &mut v8::PinScope, x: f64, y: f64) -> Option<(NodeId, f64, f64, f64, f64)> {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let page = page_root(&st)?;
    let (_, el, lx, ly) = deepest(&st, page, x, y, false)?;
    let origin = screen_origin(&st);
    Some((el, lx, ly, origin.0 + x, origin.1 + y))
}

/// Deliver one wheel event: a `WheelEvent` at the element under the pointer in its own frame
/// (`MouseWheelEventManager::HandleWheelEvent`), cancelable only when some listener is not passive.
pub fn wheel_input(scope: &mut v8::PinScope, input: &WheelInput) -> usize {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let Some(page) = page_root(&st) else { return 0 };
    let origin = screen_origin(&st);
    let Some((doc, el, x, y)) = deepest(&st, page, input.x, input.y, false) else { return 0 };
    let cancelable = crate::core::events::event_target::any_blocking_listener(scope, &["wheel", "mousewheel"]);
    let as_mouse = MouseInput {
        kind: MouseKind::Move,
        x: input.x,
        y: input.y,
        button: NO_BUTTON,
        buttons: input.buttons,
        click_count: 0,
        modifiers: input.modifiers,
        timestamp_ms: input.timestamp_ms,
        coalesced: Vec::new(),
    };
    let at = At { doc, page, x, y, input: &as_mouse, origin };
    let mut spec = at.spec(Iface::Wheel, "wheel", NO_BUTTON, None);
    spec.cancelable = cancelable;
    spec.wheel = Some(input.clone());
    // its default action, unless a listener cancelled it: scroll the chain under the pointer (`element::wheel_scroll`)
    if fire(scope, &st, el, &spec) {
        crate::core::dom::element::wheel_scroll(scope, el, input.delta_x, input.delta_y);
    }
    1
}

/// `Element.setPointerCapture(pointerId)`: `Err(())` for a pointer id no pointer has (the caller throws
/// `NotFoundError` "No active pointer with the given id is found."); the mouse (id 1) is always active, and its
/// capture is only taken while one of its buttons is down (`PointerEventManager::SetPointerCapture`).
pub(crate) fn set_pointer_capture(state: &St, el: NodeId, pointer_id: i32) -> Result<(), ()> {
    if pointer_id != MOUSE_POINTER_ID {
        return Err(());
    }
    let Some(doc) = doc_of(state, el) else { return Ok(()) };
    with_doc(state, doc, |d| {
        if d.active_buttons {
            d.pending_capture = Some(el);
        }
    });
    Ok(())
}

/// `Element.releasePointerCapture(pointerId)`: only the element that will get the next event can release.
pub(crate) fn release_pointer_capture(state: &St, el: NodeId, pointer_id: i32) -> Result<(), ()> {
    if pointer_id != MOUSE_POINTER_ID {
        return Err(());
    }
    let Some(doc) = doc_of(state, el) else { return Ok(()) };
    with_doc(state, doc, |d| {
        if d.pending_capture == Some(el) {
            d.pending_capture = None;
        }
    });
    Ok(())
}

/// `Element.hasPointerCapture(pointerId)`: the PENDING capture is the answer (`HasPointerCapture`).
pub(crate) fn has_pointer_capture(state: &St, el: NodeId, pointer_id: i32) -> bool {
    if pointer_id != MOUSE_POINTER_ID {
        return false;
    }
    let Some(doc) = doc_of(state, el) else { return false };
    with_doc(state, doc, |d| d.pending_capture == Some(el))
}

/// ONE trusted mouse-family event of `kind` -- the low-level primitive behind `Engine::dispatch_trusted_mouse` and
/// the behaviour layer's selector path: at `node`, or at the element under `(x, y)` (the page's document when the
/// point is on no box), with the movement, buttons and button given and Blink's fields for everything else
/// (`view`, `composed`, the screen origin, `detail` 1 on a press, release or click, the pointer fields of the
/// mouse). Not a gesture: no boundary event, no click, no focus -- `mouse_input` is the gesture.
#[allow(clippy::too_many_arguments)]
pub fn single_event(
    scope: &mut v8::PinScope,
    node: Option<NodeId>,
    kind: &str,
    x: f64,
    y: f64,
    movement: (f64, f64),
    buttons: u16,
    button: i32,
    pressure: Option<f64>,
    timestamp_ms: f64,
) -> bool {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let Some(page) = page_root(&st) else { return false };
    let origin = screen_origin(&st);
    let (target, doc, cx, cy) = match node {
        Some(n) => (n, doc_of(&st, n).unwrap_or(page), x, y),
        None => match deepest(&st, page, x, y, false) {
            Some((doc, el, cx, cy)) => (el, doc, cx, cy),
            None => (page, page, x, y),
        },
    };
    let input = MouseInput {
        kind: MouseKind::Move,
        x,
        y,
        button,
        buttons,
        click_count: 0,
        modifiers: Modifiers::default(),
        timestamp_ms,
        coalesced: Vec::new(),
    };
    let at = At { doc, page, x: cx, y: cy, input: &input, origin };
    let enter_or_leave = kind.ends_with("enter") || kind.ends_with("leave");
    let mouse_click = matches!(kind, "click" | "auxclick" | "contextmenu");
    let pointer = kind.starts_with("pointer") || kind.ends_with("pointercapture");
    let mut spec = if mouse_click {
        let p = PointerFields { pointer_id: MOUSE_POINTER_ID, pointer_type: "mouse", is_primary: false, pressure: 0.0, persistent_device_id: 0, coalesced: None };
        at.spec(Iface::Pointer, kind, button, Some(p))
    } else if pointer {
        let mut p = mouse_pointer(buttons, (kind == "pointermove" || kind == "pointerrawupdate").then(|| vec![[cx, cy, origin.0 + x, origin.1 + y, movement.0, movement.1, timestamp_ms]]));
        if let Some(pr) = pressure {
            p.pressure = pr;
        }
        let b = if matches!(kind, "pointerdown" | "pointerup") { button } else { NO_BUTTON };
        at.spec(Iface::Pointer, kind, b, Some(p))
    } else {
        at.spec(Iface::Mouse, kind, button, None)
    };
    spec.bubbles = !enter_or_leave;
    spec.cancelable = !enter_or_leave && kind != "pointerrawupdate" && !kind.ends_with("pointercapture");
    spec.composed = !enter_or_leave;
    spec.movement = movement;
    if matches!(kind, "mousedown" | "mouseup" | "click" | "auxclick" | "dblclick") {
        spec.detail = 1;
    }
    fire(scope, &st, target, &spec)
}

/// The client-space centre of the element `selector` matches in the page's document, when it has a box whose
/// centre is inside the viewport -- where a person would click it.
pub fn element_center(scope: &mut v8::PinScope, selector: &str) -> Option<(f64, f64)> {
    let el = crate::core::dom::shared::query_el(scope, selector)?;
    let id = crate::binding::protos::unwrap_id(scope, el.into())?;
    let st = deno_core::JsRuntime::op_state_from(scope);
    let page = page_root(&st)?;
    if doc_of(&st, id)? != page {
        return None;
    }
    let (vw, vh) = crate::core::dom::geometry_queries::viewport_client_size(scope);
    let r = with_layout(&st, page, page, |out| out.viewport_bounding_rect(id))?;
    if r.width <= 0.0 || r.height <= 0.0 {
        return None;
    }
    let (cx, cy) = ((r.x + r.width / 2.0).floor() as f64, (r.y + r.height / 2.0).floor() as f64);
    (cx >= 0.0 && cy >= 0.0 && cx < vw && cy < vh).then_some((cx, cy))
}

/// The display's frame interval in ms (the persona's refresh rate) -- what continuous input is aligned to.
pub fn frame_interval_ms(scope: &mut v8::PinScope) -> f64 {
    crate::core::intersection_observer::rendering::frame_interval_ms(scope)
}

/// A press or release of `button` as the PLATFORM reports it, for input the engine makes itself (the behaviour
/// layer): the buttons held after it, and the click count Chrome's `ui::MouseEvent::GetRepeatCount` gives -- one
/// more than the last press of the same button when it came within `kDoubleClickTimeMs` (500 ms) and 2 px
/// (`kDoubleClickWidth / 2`) of it, at most 3; a release repeats its press's count. CDP input brings its own
/// `buttons` and `clickCount` and goes to [`mouse_input`] directly, as Chrome takes them from the client.
pub fn platform_button(scope: &mut v8::PinScope, kind: MouseKind, x: f64, y: f64, button: i32, timestamp_ms: f64) -> usize {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let bit = button_bit(button);
    let (buttons, click_count) = with_input(&st, |p| match kind {
        MouseKind::Down => {
            p.held |= bit;
            let count = match p.last_press {
                Some((t, lx, ly, lb, lc))
                    if lb == button && timestamp_ms > t && timestamp_ms - t <= 500.0 && (x - lx).abs() <= 2.0 && (y - ly).abs() <= 2.0 =>
                {
                    (lc + 1).min(3)
                }
                _ => 1,
            };
            p.last_press = Some((timestamp_ms, x, y, button, count));
            (p.held, count)
        }
        MouseKind::Up => {
            p.held &= !bit;
            let count = match p.last_press {
                Some((_, _, _, lb, lc)) if lb == button => lc,
                _ => 1,
            };
            (p.held, count)
        }
        MouseKind::Move => (p.held, 0),
    });
    let input = MouseInput { kind, x, y, button, buttons, click_count, modifiers: Modifiers::default(), timestamp_ms, coalesced: Vec::new() };
    mouse_input(scope, &input)
}

/// A move of the engine's own pointer to `(x, y)` with whatever it holds, carrying the raw `samples` merged into
/// it (`(x, y, timestamp_ms)`, oldest first, the move itself last; empty for the move alone).
pub fn platform_move(scope: &mut v8::PinScope, x: f64, y: f64, samples: Vec<(f64, f64, f64)>, timestamp_ms: f64) -> usize {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let held = with_input(&st, |p| p.held);
    let mut input = MouseInput::moved(x, y, held, timestamp_ms);
    input.coalesced = samples;
    mouse_input(scope, &input)
}
