//! `core/events/shared.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8;
use crate::core::dom::shared::{query_el};
use crate::modules::crypto::shared::{crypto_num};
use crate::core::html::html_dialog_element::{mark_trusted};
use crate::core::dom::element::el_id_of;
use crate::binding::protos::{call_method, global_get, int_val, set_bool, set_prop, str_val};
use crate::core::events::typing::KeyInput;

// THE DOCUMENT-TARGETED MOUSE AND POINTER BUILDERS ARE GONE (FIXER C6, 2026-10-11). `build_trusted_mouse(_full)`
// and `build_trusted_pointer` made every trusted mouse and pointer event with no `view`, `composed` false, `detail`
// 0 and `screenX/Y` copied from `clientX/Y`, and `dispatch_trusted_mouse` / `dispatch_trusted_pointer` dispatched
// them at the DOCUMENT -- so `target` was `#document` and `toElement` null on every move, which is what kept
// a behavioural sensor from recording one. A mouse is `core/events/pointer_input.rs` now: `mouse_input` for a gesture
// (hit test, frames, capture, boundary events, click, focus) and `single_event` for the one-event primitive these
// four entry points keep.

/// ONE trusted mouse event of `kind` at what is under (x, y), with the movement and buttons given. The
/// low-level primitive (`Engine::dispatch_trusted_mouse`); a gesture is `pointer_input::mouse_input`.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_trusted_mouse(scope: &mut v8::PinScope, kind: &str, x: f64, y: f64, mx: f64, my: f64, buttons: i32, button: i32, timestamp_ms: f64) -> bool {
    crate::core::events::pointer_input::single_event(scope, None, kind, x, y, (mx, my), buttons as u16, button, None, timestamp_ms)
}

/// ONE trusted pointer event of `kind` at what is under (x, y) -- see [`dispatch_trusted_mouse`].
#[allow(clippy::too_many_arguments)]
pub fn dispatch_trusted_pointer(scope: &mut v8::PinScope, kind: &str, x: f64, y: f64, mx: f64, my: f64, buttons: i32, button: i32, pressure: f64, timestamp_ms: f64) -> bool {
    crate::core::events::pointer_input::single_event(scope, None, kind, x, y, (mx, my), buttons as u16, button, Some(pressure), timestamp_ms)
}

/// The element `selector` matches, by its arena id.
fn selector_node(scope: &mut v8::PinScope, selector: &str) -> Option<u32> {
    let doc = global_get(scope, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let sel = str_val(scope, selector);
    let elv = call_method(scope, doc.into(), "querySelector", &[sel]).filter(|v| !v.is_null_or_undefined())?;
    crate::binding::protos::unwrap_id(scope, elv)
}

/// Dispatch a genuinely-trusted `PointerEvent` of `kind` ON the element matched by `selector`.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_trusted_pointer_on_selector(scope: &mut v8::PinScope, selector: &str, kind: &str, x: f64, y: f64, buttons: i32, button: i32, pressure: f64, timestamp_ms: f64) -> bool {
    let Some(el) = selector_node(scope, selector) else { return false };
    crate::core::events::pointer_input::single_event(scope, Some(el), kind, x, y, (0.0, 0.0), buttons as u16, button, Some(pressure), timestamp_ms);
    true
}

/// Interaction coupling (P5): dispatch ONE genuinely-trusted mouse event of `kind` ON the element matched
/// by `selector`, at (x,y). The real-time scheduler emits the click graph (mousedown → mouseup → click)
/// as separate time-spaced calls so `event.timeStamp` tracks `performance.now()`. False if no match.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_trusted_mouse_on_selector(scope: &mut v8::PinScope, selector: &str, kind: &str, x: f64, y: f64, buttons: i32, button: i32, timestamp_ms: f64) -> bool {
    let Some(el) = selector_node(scope, selector) else { return false };
    crate::core::events::pointer_input::single_event(scope, Some(el), kind, x, y, (0.0, 0.0), buttons as u16, button, None, timestamp_ms);
    true
}

/// Build a genuinely-trusted `KeyboardEvent` (trusted slot + timeStamp) without dispatching, with
/// the two legacy numbers a real key event carries.
///
/// They were both 0 on every event this engine produced, and they are not decoration —
/// `event.keyCode` is what a page written before `key` existed branches on, and `which` (which the
/// `KeyboardEvent` constructor derives from `keyCode`) is what jQuery reports. They differ BETWEEN
/// the members of one keystroke: measured on stock Chrome 150, `keydown`/`keyup` carry the
/// virtual-key code with `charCode` 0, and the `keypress` in between carries the CHARACTER in both
/// (`'a'` is 65/0 then 97/97; `'-'` is 189/0 then 45/45). The caller computes which pair it wants;
/// this only writes them.
///
/// And the rest of the key event -- the modifier state (`shiftKey`..., `getModifierState`),
/// `location`, `repeat`, `composed` and `view` -- from the [`KeyInput`] it stands for. Each was the
/// constructor's default before (no modifiers, location 0, not composed, null view); recorded with real
/// X11 keys on Chrome 150, a trusted key event is composed, its `view` is the window, and Shift's
/// `location` is 1.
pub(crate) fn build_trusted_key_full<'s>(scope: &mut v8::PinScope<'s, '_>, kind: &str, input: &KeyInput, key_code: f64, char_code: f64, timestamp_ms: f64) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, "KeyboardEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", true);
    set_bool(scope, init, "cancelable", true);
    set_bool(scope, init, "composed", true);
    let win: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    set_prop(scope, init, "view", win);
    let k = str_val(scope, &input.key); set_prop(scope, init, "key", k);
    let c = str_val(scope, &input.code); set_prop(scope, init, "code", c);
    let kc = crypto_num(scope, key_code); set_prop(scope, init, "keyCode", kc);
    let cc = crypto_num(scope, char_code); set_prop(scope, init, "charCode", cc);
    let loc = int_val(scope, input.location as i64); set_prop(scope, init, "location", loc);
    if input.repeat {
        set_bool(scope, init, "repeat", true);
    }
    input.mods.write_init(scope, init);
    let ty = str_val(scope, kind);
    let ev = ctor.new_instance(scope, &[ty, init.into()])?;
    mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", timestamp_ms);
    Some(ev)
}

// THE TWO DOCUMENT-TARGETED KEY DISPATCHERS WERE DELETED. `dispatch_trusted_key`
// fired `keydown`/`keyup` on `document`, and `dispatch_trusted_key_on_selector` fired them on a CSS
// selector's match; both are gone. Neither is a thing a browser does: a key event goes to the
// FOCUSED element, and when nothing is focused Chrome targets `<body>` -- never the document, and
// never a selector, because CDP's `Input.*` carry no selector. `core/events/typing.rs::key_at_focus`
// is the one dispatcher now, and it is the same body whether the caller is `Engine::type_text`,
// `Engine::type_into` or the CDP `Input.dispatchKeyEvent` arm.

/// `el.focus()` on the matched element — moves the focus ring and fires `blur`/`focusout` on
/// whatever held it, then `focus`/`focusin` here.
///
/// `None` when the selector matches nothing, `Some(n)` with the events dispatched otherwise — and
/// `Some(0)` is a real answer, not a failure: focusing the already-focused element moves nothing
/// and fires nothing, in this engine and in a browser.
///
/// P3b: it used to WRITE `FocusState` directly and then call the element's own `focus()`, which
/// wrote it a second time and dispatched nothing. One writer now, and it is the routine that fires
/// the four events.
pub fn focus_selector(scope: &mut v8::PinScope, selector: &str, timestamp_ms: f64) -> Option<usize> {
    let el = query_el(scope, selector)?;
    let id = el_id_of(scope, el)?;
    Some(crate::core::events::focus::focus_element(scope, id, timestamp_ms))
}

/// One trusted `WheelEvent` (`deltaMode` 0 = pixel) at the element under (x, y), in its own frame (FIXER C6: it
/// was dispatched at the document). `wheelDelta*` is the notch direction times 120, opposite to the delta, as a
/// wheel notch reports it (measured sign: `deltaY` > 0 with `wheelDeltaY` < 0).
pub fn dispatch_trusted_wheel(scope: &mut v8::PinScope, x: f64, y: f64, delta_x: f64, delta_y: f64, timestamp_ms: f64) -> bool {
    let notch = |d: f64| if d == 0.0 { 0.0 } else { -d.signum() * 120.0 };
    let input = crate::core::events::pointer_input::WheelInput {
        x,
        y,
        delta_x,
        delta_y,
        delta_mode: 0,
        wheel_delta_x: notch(delta_x),
        wheel_delta_y: notch(delta_y),
        buttons: 0,
        modifiers: Default::default(),
        timestamp_ms,
    };
    crate::core::events::pointer_input::wheel_input(scope, &input) > 0
}

thread_local! {
    /// The element the current touch started on: Blink keeps a touch point's target for the rest of its life
    /// (`TouchEventManager::touch_attribute_map_`), so its `touchmove` and `touchend` go there wherever the finger is.
    static TOUCH_TARGET: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Construct a genuinely-trusted `TouchEvent` of `kind` (touchstart/touchmove/touchend) with one `Touch`
/// (elliptical radiusX/Y + force) and dispatch it at the element the touch began on -- hit-tested at its start, in
/// its own frame (it went to `document`, so an element's `ontouchstart` never ran). `touches`/`targetTouches` are
/// empty on touchend (finger lifted); `changedTouches` always carries the touch.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_trusted_touch(scope: &mut v8::PinScope, kind: &str, x: f64, y: f64, force: f64, radius_x: f64, radius_y: f64, timestamp_ms: f64) -> bool {
    let hit = crate::core::events::pointer_input::touch_target(scope, x, y);
    let target_id = match kind {
        "touchstart" => {
            let t = hit.map(|h| h.0);
            TOUCH_TARGET.with(|c| c.set(t));
            t
        }
        _ => TOUCH_TARGET.with(|c| c.get()).or(hit.map(|h| h.0)),
    };
    if matches!(kind, "touchend" | "touchcancel") {
        TOUCH_TARGET.with(|c| c.set(None));
    }
    let (cx_v, cy_v, sx_v, sy_v) = match hit {
        Some((_, lx, ly, sx, sy)) => (lx, ly, sx, sy),
        None => (x, y, x, y),
    };
    let doc = match target_id.and_then(|id| v8::Local::<v8::Object>::try_from(crate::page::node_object(scope, id)).ok()) {
        Some(el) => el,
        None => match global_get(scope, "document").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) { Some(d) => d, None => return false },
    };
    let touch_ctor = match crate::binding::protos::iface_object(scope, "Touch").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) { Some(c) => c, None => return false };
    let te_ctor = match crate::binding::protos::iface_object(scope, "TouchEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) { Some(c) => c, None => return false };
    let ti = v8::Object::new(scope);
    // `identifier` and `target` are REQUIRED members of `TouchInit` (Chrome: `new Touch({})` throws
    // "Failed to read the 'identifier' property"), and the native `Touch` constructor enforces that
    // where the deleted JS class defaulted them silently. One finger, targeted at the document -- the
    // element under the point is what `dispatchEvent` below decides, exactly as before.
    let id = int_val(scope, 1);
    set_prop(scope, ti, "identifier", id);
    let docv: v8::Local<v8::Value> = doc.into();
    set_prop(scope, ti, "target", docv);
    let cx = crypto_num(scope, cx_v); set_prop(scope, ti, "clientX", cx);
    let cy = crypto_num(scope, cy_v); set_prop(scope, ti, "clientY", cy);
    let sx = crypto_num(scope, sx_v); set_prop(scope, ti, "screenX", sx);
    let sy = crypto_num(scope, sy_v); set_prop(scope, ti, "screenY", sy);
    let pgx = crypto_num(scope, cx_v); set_prop(scope, ti, "pageX", pgx);
    let pgy = crypto_num(scope, cy_v); set_prop(scope, ti, "pageY", pgy);
    let rx = crypto_num(scope, radius_x); set_prop(scope, ti, "radiusX", rx);
    let ry = crypto_num(scope, radius_y); set_prop(scope, ti, "radiusY", ry);
    let fo = crypto_num(scope, force); set_prop(scope, ti, "force", fo);
    let touch = match touch_ctor.new_instance(scope, &[ti.into()]) { Some(t) => t, None => return false };
    let touchv: v8::Local<v8::Value> = touch.into();
    let active = kind != "touchend";
    let touches = if active {
        let a = v8::Array::new(scope, 1); a.set_index(scope, 0, touchv); a
    } else {
        v8::Array::new(scope, 0)
    };
    let changed = { let a = v8::Array::new(scope, 1); a.set_index(scope, 0, touchv); a };
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "bubbles", true);
    set_bool(scope, init, "cancelable", true);
    set_prop(scope, init, "touches", touches.into());
    set_prop(scope, init, "targetTouches", touches.into());
    set_prop(scope, init, "changedTouches", changed.into());
    let ty = str_val(scope, kind);
    let ev = match te_ctor.new_instance(scope, &[ty, init.into()]) { Some(e) => e, None => return false };
    mark_trusted(scope, ev);
    crate::core::events::event::evt_slot_write_num(scope, ev, "timeStamp", timestamp_ms);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, doc.into(), "dispatchEvent", &[evv]);
    true
}

/// The page document's node id in the arena-keyed stores (a nested browsing context's document uses
/// its own root id, so the two never share a key).
pub(crate) const ON_DOCUMENT_ID: u32 = u32::MAX;

/// Focus tracking: the currently-focused element's node id (`None` = default, i.e. body). `el.focus()`
/// sets it; `document.activeElement` reads it. Cell so op2 getters (which take `&OpState`) can write.
#[derive(Default)]
pub(crate) struct FocusState(pub(crate) std::cell::Cell<Option<u32>>);

/// A press is moving the focus ring (`pointer_input::press_focus`): what it focuses shows its focus only when it is a
/// text field ([`set_focus_ring`]).
#[derive(Default)]
pub(crate) struct FocusByPointer(pub(crate) bool);

/// Move the page's focus ring to `to` and mirror it into the DOM, whose `:focus`, `:focus-within` and `:focus-visible`
/// read it. The focus is VISIBLE as Chrome's heuristic has it: a text field always, anything else unless a press put
/// it there.
pub(crate) fn set_focus_ring(op_state: &std::cell::RefCell<deno_core::OpState>, to: Option<u32>) {
    let mut st = op_state.borrow_mut();
    if let Some(f) = st.try_borrow::<FocusState>() {
        f.0.set(to);
    }
    let by_pointer = st.try_borrow::<FocusByPointer>().is_some_and(|p| p.0);
    if let Some(h) = st.try_borrow_mut::<crate::binding::host_dom::HostDom>() {
        let focus = to.map(|n| (n, !by_pointer || crate::core::events::focus::takes_text_input(&h.0, n)));
        h.0.set_focus(focus);
    }
}

