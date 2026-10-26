//! `core/events/mouse_event.rs` — `MouseEvent` and its two subclasses `PointerEvent` and
//! `WheelEvent`.
//!
//! The three are one file because they are one constructor chain: every `PointerEvent` and every
//! `WheelEvent` runs [`init_mouse_event`], and splitting them would put the same twenty-three
//! coordinate members in two places again — which is what this unit exists to stop. (The precedent
//! is `core/animation/animation_event.rs`, which holds a family for the same reason.)
//!
//! EVERY DEFAULT BELOW WAS MEASURED, not derived: `google-chrome --headless=new` was asked the same
//! expressions this file implements. The ones `host.js` had wrong:
//!
//! | expression                                | host.js | Chrome |
//! |-------------------------------------------|---------|--------|
//! | `new MouseEvent("m",{clientX:5}).offsetX` | 0       | 5      |
//! | `new MouseEvent("m",{clientX:5}).layerX`  | 0       | 5      |
//! | `new MouseEvent("m",{button:-1}).button`  | -1      | 0      |
//! | `new MouseEvent("m",{buttons:-1}).buttons`| -1      | 65535  |
//! | `new MouseEvent("m",{clientX:NaN})`       | 0       | TypeError |
//! | `new MouseEvent("m",{relatedTarget:{}})`  | `{}`    | TypeError |
//! | `new PointerEvent("p").pointerType`       | "mouse" | ""     |
//! | `new PointerEvent("p").isPrimary`         | true    | false  |
//! | `new PointerEvent("p",{buttons:1}).pressure` | 0.5  | 0      |
//! | `new PointerEvent("p",{persistentDeviceId:4}).persistentDeviceId` | 0 | 4 |
//! | `new WheelEvent("w",{deltaY:9}).wheelDeltaY` | -27  | 9      |
//!
//! **`offsetX`/`layerX` are the client coordinates here, and that is not a shortcut.** They are
//! relative to the target's padding box / layer box, and a script-constructed event has no target —
//! so Chrome falls back to the client coordinates, which is exactly what it answers above. An event
//! DISPATCHED on a laid-out element would use that element's box; this engine does not compute that
//! either before or after this unit, and the row is honest about answering the untargeted value.
//!
//! **The `wheelDelta*` rule is the measurement, not Blink's source.** The published Blink
//! constructor negates (`wheelDeltaY = -deltaY`); Brave 151 as measured does not — `{deltaY: 9}`
//! gives `9`, `{deltaY: -9}` gives `-9`, and an explicit `{wheelDeltaY: 4}` wins over both. The rule
//! encoded here is the one that fits all four measured points, and the truth source for this repo is
//! the browser, not the upstream source tree.

use deno_core::v8;

use crate::core::events::ctor::{
    brand, dict_bool, dict_double, dict_int, dict_interface, dict_string, init_dict, require_args, require_new,
};
use crate::core::events::ctor::{arg_string, init_event};
use crate::core::events::event::{
    evt_slot_write, evt_slot_write_null, evt_slot_write_num, install_init_mouse_event, install_modifier_state,
};
use crate::core::events::ui_event::init_ui_event;
use crate::install::MemberDef;

/// A `double` dictionary member read as a double (so NaN and the infinities are the TypeError Chrome raises).
/// `integer` is Blink's `ShouldHaveIntegerCoordinates`: a `MouseEvent` reads its coordinates back FLOORED
/// (`std::floor` in `mouse_event.h`: `{clientX: 5.7}` is `5`, `{clientX: -2.5}` is `-3`), while a `PointerEvent` that
/// is not a `click` / `auxclick` / `contextmenu` keeps the fraction (`pointer_event.h`).
fn coord(scope: &mut v8::PinScope, d: Option<v8::Local<v8::Object>>, key: &str, iface: &str, integer: bool) -> Result<f64, ()> {
    Ok(dict_double(scope, d, key, iface)?.map(|n| if integer { n.floor() } else { n }).unwrap_or(0.0))
}

/// The event's own type, from its slot (never a page-visible getter).
fn event_type(scope: &mut v8::PinScope, ev: v8::Local<v8::Object>) -> String {
    crate::core::events::event::evt_slot_read(scope, ev, "type").map(|t| t.to_rust_string_lossy(scope)).unwrap_or_default()
}

/// Blink's `PointerEvent::ShouldHaveIntegerCoordinates`: the three pointer events that are also mouse events.
pub(crate) fn integer_pointer_type(ty: &str) -> bool {
    matches!(ty, "click" | "auxclick" | "contextmenu")
}

/// `UIEvent.which` on a mouse event: Blink's `MouseEvent::which()`, `(unsigned)(button_ + 1)` of the RAW
/// button -- 0 for a move with no button held (`button_` is -1 there), 4294967295 for a raw -2.
pub(crate) fn which_of(raw_button: f64) -> f64 {
    let w = raw_button + 1.0;
    if w < 0.0 { w + 4_294_967_296.0 } else { w }
}

/// The `MouseEvent(type, mouseEventInitDict)` steps after `UIEvent`'s.
pub(crate) fn init_mouse_event(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    dict: Option<v8::Local<v8::Object>>,
    iface: &str,
) -> Result<(), ()> {
    let integer = iface != "PointerEvent" || integer_pointer_type(&event_type(scope, this));
    let client_x = coord(scope, dict, "clientX", iface, integer)?;
    let client_y = coord(scope, dict, "clientY", iface, integer)?;
    for (name, v) in [
        ("screenX", coord(scope, dict, "screenX", iface, integer)?),
        ("screenY", coord(scope, dict, "screenY", iface, integer)?),
        ("clientX", client_x),
        ("clientY", client_y),
        ("movementX", coord(scope, dict, "movementX", iface, true)?),
        ("movementY", coord(scope, dict, "movementY", iface, true)?),
        // pageX/pageY are clientX/clientY plus the scroll offset; nothing here scrolls, and a real
        // browser reports the same for a synthesised event on an unscrolled document.
        ("pageX", client_x),
        ("pageY", client_y),
        // The target-relative pairs. No target -> the client coordinates (measured; see the header).
        ("offsetX", client_x),
        ("offsetY", client_y),
        ("layerX", client_x.floor()),
        ("layerY", client_y.floor()),
    ] {
        evt_slot_write_num(scope, this, name, v);
    }
    // `short button` is kept RAW (Blink's `button_`): the `button` getter answers 0 for a raw -1 and for the five
    // boundary/move types of a MouseEvent, and the raw value for a PointerEvent (`cb_mouse_button`). `unsigned
    // short buttons`: the WebIDL modular conversion, so -1 is 65535 (measured).
    let button = dict_int(scope, dict, "button", 16, true)?.unwrap_or(0.0);
    evt_slot_write_num(scope, this, "button", button);
    let buttons = dict_int(scope, dict, "buttons", 16, false)?.unwrap_or(0.0);
    evt_slot_write_num(scope, this, "buttons", buttons);
    for name in ["ctrlKey", "shiftKey", "altKey", "metaKey"] {
        let b = dict_bool(scope, dict, name);
        let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
        evt_slot_write(scope, this, name, v);
    }
    crate::core::events::event::init_modifier_states(scope, this, dict);
    match dict_interface(scope, dict, "relatedTarget", iface, &format!("{iface}Init"), &["EventTarget"])? {
        Some(v) if !v.is_null() => evt_slot_write(scope, this, "relatedTarget", v),
        _ => evt_slot_write_null(scope, this, "relatedTarget"),
    }
    // `fromElement`/`toElement` are computed from the type, the target and the related target when read
    // (`cb_to_element` / `cb_from_element`). `UIEvent.which` is interface-specific: on a mouse event it is the
    // raw button plus one.
    evt_slot_write_num(scope, this, "which", which_of(button));
    brand(scope, this, "MouseEvent");
    Ok(())
}

/// `MouseEvent.button`: Blink's `MouseEvent::button()` -- 0 for a raw -1 (no button) and for `mousemove`,
/// `mouseover`, `mouseout`, `mouseenter`, `mouseleave` whatever was held; a `PointerEvent` answers its raw
/// button (`PointerEvent::button()`), so a `pointermove` with nothing pressed reads -1 (measured).
fn cb_mouse_button(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let raw = crate::core::events::event::evt_slot_read(scope, this, "button").and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    if crate::core::events::ctor::has_brand(scope, this.into(), "PointerEvent") {
        rv.set_double(raw);
        return;
    }
    let ty = event_type(scope, this);
    let zero = raw == -1.0 || matches!(ty.as_str(), "mousemove" | "mouseleave" | "mouseenter" | "mouseover" | "mouseout");
    rv.set_double(if zero { 0.0 } else { raw });
}

/// The node `slot` names, when it is one (a window or another target is no `Node`).
fn node_in_slot<'s>(scope: &mut v8::PinScope<'s, '_>, ev: v8::Local<v8::Object>, slot: &str) -> Option<v8::Local<'s, v8::Value>> {
    let v = crate::core::events::event::evt_slot_read(scope, ev, slot).filter(|v| v.is_object())?;
    crate::binding::protos::arg_node_id(scope, v).map(|_| v)
}

/// `MouseEvent.toElement` / `fromElement` (Blink `mouse_event.cc:364-382`, the MSIE extension): on a `mouseout`
/// or `mouseleave` the element the pointer goes TO is the related target and it comes FROM the target; on every
/// other type the other way round. Both are null on a `PointerEvent` (`PointerEvent::toElement` /
/// `fromElement`). They read the dispatch's target as each listener sees it. korvus answered null for both
/// always, and a behavioural sensor reads `toElement` first on every `mousemove` (FIXER C6).
fn mouse_alias(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, to: bool) {
    let this = args.this();
    if crate::core::events::ctor::has_brand(scope, this.into(), "PointerEvent") {
        rv.set_null();
        return;
    }
    let ty = event_type(scope, this);
    let leaving = matches!(ty.as_str(), "mouseout" | "mouseleave");
    let slot = if to == leaving { "relatedTarget" } else { "target" };
    match node_in_slot(scope, this, slot) {
        Some(n) => rv.set(n),
        None => rv.set_null(),
    }
}
fn cb_to_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    mouse_alias(scope, &args, rv, true);
}
fn cb_from_element(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    mouse_alias(scope, &args, rv, false);
}

/// `offsetX/Y` and `layerX/Y`: against the event's target as the running listener sees it, computed when read
/// (Blink `MouseEvent::ComputeRelativePosition`, redone for each target the dispatch hands it) -- the padding box
/// of the target for the offset, its enclosing positioned box for the layer. With no target, or no layout, the
/// value the event was made with: the client point for a script-constructed event (measured, see the header).
/// A `MouseEvent` (and a click-type `PointerEvent`) rounds the offset; any other `PointerEvent` keeps the fraction;
/// the layer is floored.
fn relative_member(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, which: usize) {
    let this = args.this();
    let slot = ["offsetX", "offsetY", "layerX", "layerY"][which];
    let stored = crate::core::events::event::evt_slot_read(scope, this, slot).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let target = node_in_slot(scope, this, "target").and_then(|t| crate::binding::protos::unwrap_id(scope, t));
    let Some(target) = target else {
        rv.set_double(stored);
        return;
    };
    let read = |scope: &mut v8::PinScope, k: &str| crate::core::events::event::evt_slot_read(scope, this, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let (cx, cy) = (read(scope, "clientX"), read(scope, "clientY"));
    let Some((offset, layer)) = crate::core::events::pointer_input::relative_to(scope, target, cx, cy) else {
        rv.set_double(stored);
        return;
    };
    let integer = !crate::core::events::ctor::has_brand(scope, this.into(), "PointerEvent") || integer_pointer_type(&event_type(scope, this));
    let v = match which {
        0 => offset.0,
        1 => offset.1,
        2 => layer.0.floor(),
        _ => layer.1.floor(),
    };
    rv.set_double(if which < 2 && integer { v.round() } else { v });
}
fn cb_offset_x(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    relative_member(scope, &args, rv, 0);
}
fn cb_offset_y(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    relative_member(scope, &args, rv, 1);
}
fn cb_layer_x(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    relative_member(scope, &args, rv, 2);
}
fn cb_layer_y(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    relative_member(scope, &args, rv, 3);
}

/// The `MouseEvent` table's computed rows: `button`, `toElement`, `fromElement`, `offsetX/Y`, `layerX/Y`.
pub fn install_mouse_computed(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let name = format!("get {}", member.name);
    let g = match member.name {
        "button" => crate::binding::protos::build_fn(scope, cb_mouse_button, &name, 0),
        "toElement" => crate::binding::protos::build_fn(scope, cb_to_element, &name, 0),
        "offsetX" => crate::binding::protos::build_fn(scope, cb_offset_x, &name, 0),
        "offsetY" => crate::binding::protos::build_fn(scope, cb_offset_y, &name, 0),
        "layerX" => crate::binding::protos::build_fn(scope, cb_layer_x, &name, 0),
        "layerY" => crate::binding::protos::build_fn(scope, cb_layer_y, &name, 0),
        _ => crate::binding::protos::build_fn(scope, cb_from_element, &name, 0),
    };
    let gg = crate::core::events::ctor::brand_accessor(scope, "MouseEvent", &name, 0, g);
    crate::install::kits::define_accessor(scope, proto, member, gg, None);
}

pub(crate) fn cb_mouse_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "MouseEvent") || !require_args(scope, &args, "MouseEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "MouseEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "MouseEvent", "MouseEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() || init_mouse_event(scope, this, dict, "MouseEvent").is_err() {
        return;
    }
    rv.set(this.into());
}

// ── PointerEvent ────────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_pointer_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "PointerEvent") || !require_args(scope, &args, "PointerEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "PointerEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "PointerEvent", "PointerEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() || init_mouse_event(scope, this, dict, "PointerEvent").is_err() {
        return;
    }
    if init_pointer_event(scope, this, dict).is_err() {
        return;
    }
    rv.set(this.into());
}

pub(crate) fn init_pointer_event(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, dict: Option<v8::Local<v8::Object>>) -> Result<(), ()> {
    let i = "PointerEvent";
    for (name, default) in [("pressure", 0.0), ("tangentialPressure", 0.0), ("tiltX", 0.0), ("tiltY", 0.0), ("twist", 0.0), ("azimuthAngle", 0.0)] {
        let v = dict_double(scope, dict, name, i)?.unwrap_or(default);
        evt_slot_write_num(scope, this, name, v);
    }
    // `width`/`height` default to 1 (the one-pixel contact box a mouse reports).
    for name in ["width", "height"] {
        let v = dict_double(scope, dict, name, i)?.unwrap_or(1.0);
        evt_slot_write_num(scope, this, name, v);
    }
    // `altitudeAngle` defaults to pi/2 -- a pointer perpendicular to the surface.
    let alt = dict_double(scope, dict, "altitudeAngle", i)?.unwrap_or(std::f64::consts::FRAC_PI_2);
    evt_slot_write_num(scope, this, "altitudeAngle", alt);
    let id = dict_int(scope, dict, "pointerId", 32, true)?.unwrap_or(0.0);
    evt_slot_write_num(scope, this, "pointerId", id);
    let dev = dict_int(scope, dict, "persistentDeviceId", 32, true)?.unwrap_or(0.0);
    evt_slot_write_num(scope, this, "persistentDeviceId", dev);
    // `pointerType` is the EMPTY STRING by default, not "mouse" (measured), and `isPrimary` is
    // false -- a constructed event describes no real device.
    let pt = dict_string(scope, dict, "pointerType")?.unwrap_or_default();
    let ptv = crate::binding::protos::str_val(scope, &pt);
    evt_slot_write(scope, this, "pointerType", ptv);
    let primary = dict_bool(scope, dict, "isPrimary");
    let pv: v8::Local<v8::Value> = v8::Boolean::new(scope, primary).into();
    evt_slot_write(scope, this, "isPrimary", pv);
    brand(scope, this, "PointerEvent");
    Ok(())
}

/// `getCoalescedEvents` / `getPredictedEvents`: a NEW array of the events the trusted input path stored on this
/// one (`_coalesced` / `_predicted`, written by `core/events/pointer_input.rs`), empty for every other event.
/// Chrome 150: a trusted `pointermove` or `pointerrawupdate` carries itself as its one coalesced event when nothing
/// was merged into it, every other pointer event none; nothing is predicted for a mouse (measured,
/// `tests/fixtures/mouse-input`). A script-constructed event answers empty (its init's lists are not kept here).
fn pointer_list(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, mut rv: v8::ReturnValue, slot: &str) {
    let items: Vec<v8::Local<v8::Value>> = match crate::core::events::event::evt_slot_read(scope, args.this(), slot)
        .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
    {
        Some(a) => (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect(),
        None => Vec::new(),
    };
    let out = v8::Array::new_with_elements(scope, &items);
    rv.set(out.into());
}
fn cb_pointer_coalesced(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    pointer_list(scope, &args, rv, "_coalesced");
}
fn cb_pointer_predicted(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    pointer_list(scope, &args, rv, "_predicted");
}

pub fn install_pointer_event(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    if member.name == "getCoalescedEvents" {
        crate::core::events::event::evt_branded_method(scope, proto, member, cb_pointer_coalesced);
    } else {
        crate::core::events::event::evt_branded_method(scope, proto, member, cb_pointer_predicted);
    }
}

// ── WheelEvent ──────────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_wheel_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "WheelEvent") || !require_args(scope, &args, "WheelEvent", 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, "WheelEvent") else { return };
    let Ok(dict) = init_dict(scope, &args, 1, "WheelEvent", "WheelEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    if init_ui_event(scope, this, dict).is_err() || init_mouse_event(scope, this, dict, "WheelEvent").is_err() {
        return;
    }
    let i = "WheelEvent";
    let dx = match dict_double(scope, dict, "deltaX", i) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    let dy = match dict_double(scope, dict, "deltaY", i) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    let dz = match dict_double(scope, dict, "deltaZ", i) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    let dm = match dict_int(scope, dict, "deltaMode", 32, false) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    for (n, v) in [("deltaX", dx), ("deltaY", dy), ("deltaZ", dz), ("deltaMode", dm)] {
        evt_slot_write_num(scope, this, n, v);
    }
    // The three legacy `wheelDelta*` members: the init's own value when it names a non-zero one,
    // otherwise the matching delta (measured -- see the file header).
    let wdx = match dict_int(scope, dict, "wheelDeltaX", 32, true) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    let wdy = match dict_int(scope, dict, "wheelDeltaY", 32, true) { Ok(v) => v.unwrap_or(0.0), Err(_) => return };
    let wheel_x = if wdx != 0.0 { wdx } else { dx };
    let wheel_y = if wdy != 0.0 { wdy } else { dy };
    for (n, v) in [("wheelDeltaX", wheel_x), ("wheelDeltaY", wheel_y), ("wheelDelta", wheel_y)] {
        evt_slot_write_num(scope, this, n, v);
    }
    // `momentum` is Chromium's legacy inertial-scroll flag; a constructed event has none.
    let mv: v8::Local<v8::Value> = v8::Boolean::new(scope, false).into();
    evt_slot_write(scope, this, "momentum", mv);
    brand(scope, this, "WheelEvent");
    rv.set(this.into());
}

// ── the three tables ────────────────────────────────────────────────────────────────────────────
//
// Names, order, kinds and descriptor flags are the `install/generated_registry.rs` blocks they
// replace, VERBATIM, plus the `EVENT_PROTOS` row set for `MouseEvent` (the generated layer this unit
// deletes). `x`/`y` carry `cfg` because they are ALIASES: the capture records them as their own
// accessors reading `clientX`/`clientY`, which is what the kit's `cfg` expresses.

webapi::interface! {
    "MouseEvent" : "UIEvent" [value, window] {
        ctor   "MouseEvent"                             len 1 = stub,
        attr   "screenX"          : Any readonly              = evt_get,
        attr   "screenY"          : Any readonly              = evt_get,
        attr   "clientX"          : Any readonly              = evt_get,
        attr   "clientY"          : Any readonly              = evt_get,
        attr   "ctrlKey"          : Any readonly              = evt_get,
        attr   "shiftKey"         : Any readonly              = evt_get,
        attr   "altKey"           : Any readonly              = evt_get,
        attr   "metaKey"          : Any readonly              = evt_get,
        attr   "button"           : Any readonly              = install_mouse_computed,
        attr   "buttons"          : Any readonly              = evt_get,
        attr   "relatedTarget"    : Any readonly nullable     = evt_get_null,
        attr   "pageX"            : Any readonly              = evt_get,
        attr   "pageY"            : Any readonly              = evt_get,
        attr   "x"                : Any readonly cfg "clientX" = evt_get,
        attr   "y"                : Any readonly cfg "clientY" = evt_get,
        attr   "offsetX"          : Any readonly              = install_mouse_computed,
        attr   "offsetY"          : Any readonly              = install_mouse_computed,
        attr   "movementX"        : Any readonly              = evt_get,
        attr   "movementY"        : Any readonly              = evt_get,
        attr   "fromElement"      : Any readonly nullable     = install_mouse_computed,
        attr   "toElement"        : Any readonly nullable     = install_mouse_computed,
        attr   "layerX"           : Any readonly              = install_mouse_computed,
        attr   "layerY"           : Any readonly              = install_mouse_computed,
        method "getModifierState" : Any len 1 required 1      = install_modifier_state,
        method "initMouseEvent"   : Any len 1 required 1      = install_init_mouse_event
    }
}

webapi::interface! {
    "PointerEvent" : "MouseEvent" [value, window] {
        ctor   "PointerEvent"                           len 1 = stub,
        attr   "pointerId"          : Any readonly            = evt_get,
        attr   "width"              : Any readonly            = evt_get,
        attr   "height"             : Any readonly            = evt_get,
        attr   "pressure"           : Any readonly            = evt_get,
        attr   "tiltX"              : Any readonly            = evt_get,
        attr   "tiltY"              : Any readonly            = evt_get,
        attr   "azimuthAngle"       : Any readonly            = evt_get,
        attr   "altitudeAngle"      : Any readonly            = evt_get,
        attr   "tangentialPressure" : Any readonly            = evt_get,
        attr   "twist"              : Any readonly            = evt_get,
        attr   "pointerType"        : Any readonly            = evt_get,
        attr   "isPrimary"          : Any readonly            = evt_get,
        method "getPredictedEvents" : Any len 0               = install_pointer_event,
        attr   "persistentDeviceId" : Any readonly            = evt_get,
        method "getCoalescedEvents" : Any len 0               = install_pointer_event
    }
}

webapi::interface! {
    "WheelEvent" : "MouseEvent" [value, window] {
        ctor            "WheelEvent"                    len 1 = stub,
        attr            "deltaX"          : Any readonly      = evt_get,
        attr            "deltaY"          : Any readonly      = evt_get,
        attr            "deltaZ"          : Any readonly      = evt_get,
        attr            "deltaMode"       : Any readonly      = evt_get,
        attr            "wheelDeltaX"     : Any readonly      = evt_get,
        attr            "wheelDeltaY"     : Any readonly      = evt_get,
        attr            "wheelDelta"      : Any readonly      = evt_get,
        constant        "DOM_DELTA_PIXEL" : Any = 0,
        constant        "DOM_DELTA_LINE"  : Any = 1,
        constant        "DOM_DELTA_PAGE"  : Any = 2,
        attr            "momentum"        : Any readonly      = evt_get,
        static_constant "DOM_DELTA_PIXEL" : Any = 0,
        static_constant "DOM_DELTA_LINE"  : Any = 1,
        static_constant "DOM_DELTA_PAGE"  : Any = 2
    }
}
