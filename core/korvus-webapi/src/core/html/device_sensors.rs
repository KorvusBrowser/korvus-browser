//! `devicemotion`, `deviceorientation` and `deviceorientationabsolute` as Chrome 150 fires them on a device with no
//! motion or orientation sensor -- the persona's desktop (from investigator `testpages` F4: incolumitas'
//! `hc2.js` scores a page whose `devicemotion` listener never hears anything).
//!
//! Blink keeps one controller per type per window (`modules/device_orientation/device_motion_controller.cc`,
//! `device_orientation_controller.cc`, `device_orientation_absolute_controller.cc`, all over
//! `core/frame/device_single_window_event_controller.cc` and `platform_event_controller.cc`). A listener ADDED for its
//! type (`LocalDOMWindow::AddedEventListener`, so `addEventListener` and the first non-null `on<type>` alike, and
//! never a duplicate that adds nothing) starts the controller when the window
//!
//!   * is not detached (`IsContextDestroyed`),
//!   * is a secure context -- the property is `[SecureContext]` but `addEventListener` is not, and
//!   * has `accelerometer` and `gyroscope` enabled by permissions policy (`deviceorientationabsolute` also
//!     `magnetometer`), else the console warning and nothing; and the controller is not already running.
//!
//! Starting creates the type's event pump, which asks the browser for its sensors (`device_sensor_entry.cc`
//! `GetSensor`): `devicemotion` for three at once, `deviceorientationabsolute` for one, and `deviceorientation` for
//! the relative sensor and -- once that reply says there is none -- the absolute one, a second round trip
//! (`device_orientation_event_pump.cc` `DidStartIfPossible`). With every sensor "ready or errored" the pump starts its
//! 60 Hz timer (`device_sensor_event_pump.cc`), and its first tick reads NO data from any sensor and fires: a trusted,
//! non-bubbling event whose every reading is null -- `DeviceMotionEvent` with three sub-objects of nulls and
//! `interval` 16 (`kDefaultPumpDelayMicroseconds / 1000` in integer arithmetic), `DeviceOrientationEvent` with null
//! angles and `absolute` the pump's. The controller sees a NULL event and stops (`IsNullEvent` -> `StopUpdating`), so
//! one start is one event, to every listener of the type the window has at that moment; the next listener added
//! starts it again. Removing the last listener of the type before the tick stops it, and nothing fires.
//!
//! Measured on stock Chrome 150.0.7871.46 (headed, Xvfb, no DevTools; `tests/fixtures/device-sensors/`): the event
//! lands 17 ms after the add with the browser's sensor service warm, ~80 ms in a fresh browser (its first sensor
//! request enumerates the platform's devices). The pump's 16.67 ms is Chromium's; the browser's reply time is not
//! modelled beyond "a reply" ([`queue_reply`]), so this answers as the warm browser does.
//!
//! `DeviceOrientationEvent.requestPermission` / `DeviceMotionEvent.requestPermission` are
//! `[RuntimeEnabled=DeviceOrientationRequestPermission]`, "experimental": absent in Chrome 150, and absent here.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, get_prop, str_val};
use crate::core::events::event::{evt_slot_write, evt_slot_write_num};
use crate::core::events::event_target::EtKey;
use crate::core::html::browsing_context as bc;
use crate::core::intersection_observer::rendering::{interface_proto, queue_reply, queue_task};

/// `DeviceSensorEventPump::kDefaultPumpDelayMicroseconds`: `base::Time::kMicrosecondsPerSecond / 60`.
const PUMP_DELAY_US: i64 = 1_000_000 / 60;
/// `DeviceMotionEventPump`'s `kDefaultPumpDelayMilliseconds`, which every motion event carries as `interval`: the
/// division is Chromium's, in integers -- 16, not 16.67.
const MOTION_INTERVAL_MS: f64 = (PUMP_DELAY_US / 1000) as f64;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Motion,
    Orientation,
    OrientationAbsolute,
}

impl Kind {
    fn of(ty: &str) -> Option<Kind> {
        match ty {
            "devicemotion" => Some(Kind::Motion),
            "deviceorientation" => Some(Kind::Orientation),
            "deviceorientationabsolute" => Some(Kind::OrientationAbsolute),
            _ => None,
        }
    }
    fn event_type(self) -> &'static str {
        match self {
            Kind::Motion => "devicemotion",
            Kind::Orientation => "deviceorientation",
            Kind::OrientationAbsolute => "deviceorientationabsolute",
        }
    }
    fn interface(self) -> &'static str {
        match self {
            Kind::Motion => "DeviceMotionEvent",
            Kind::Orientation | Kind::OrientationAbsolute => "DeviceOrientationEvent",
        }
    }
    /// The permissions-policy features `CheckPolicyFeatures` asks for.
    fn features(self) -> &'static [&'static str] {
        match self {
            Kind::Motion | Kind::Orientation => &["accelerometer", "gyroscope"],
            Kind::OrientationAbsolute => &["accelerometer", "gyroscope", "magnetometer"],
        }
    }
    /// The browser round trips before every sensor is ready or errored: the relative orientation pump falls back
    /// to the absolute sensor only after the relative one's reply.
    fn round_trips(self) -> u32 {
        match self {
            Kind::Orientation => 2,
            Kind::Motion | Kind::OrientationAbsolute => 1,
        }
    }
}

/// One started pump: its window and the round trips still to come before its timer starts.
struct Pump {
    key: EtKey,
    kind: Kind,
    window: v8::Global<v8::Object>,
    round_trips: u32,
}

/// The running controllers (`is_active_`), by window and type, each naming its pump; and the pumps whose task is
/// still to run. A stopped controller's pump is dropped when its task runs and finds itself no longer current.
#[derive(Default)]
pub(crate) struct DeviceSensors {
    active: std::collections::HashMap<(EtKey, Kind), u64>,
    pumps: std::collections::HashMap<u64, Pump>,
    next: u64,
}

fn with<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut DeviceSensors) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if !st.has::<DeviceSensors>() {
        st.put(DeviceSensors::default());
    }
    f(st.borrow_mut::<DeviceSensors>())
}

/// `DidAddEventListener`: a listener of `ty` was ADDED to the target `this` stores under `key`.
pub(crate) fn did_add_listener(scope: &mut v8::PinScope, key: EtKey, this: v8::Local<v8::Value>, ty: &str) {
    let Some(kind) = Kind::of(ty) else { return };
    if !matches!(key, EtKey::Window | EtKey::Object(_)) || !bc::has_document(scope) {
        return;
    }
    let Some((ctx, window)) = window_target(scope, key, this) else { return };
    // "The window could be detached, e.g. if it is the `contentWindow` of an <iframe> that has been removed".
    if ctx.is_some_and(|id| !bc::context_fully_active(scope, id)) {
        return;
    }
    if !secure_context(scope, ctx) {
        return;
    }
    // Blink logs "The <type> events are blocked by permissions policy. ..." to the console here; this engine has no
    // DevTools console for an engine warning to reach.
    if !kind.features().iter().all(|f| policy_enabled(scope, ctx, window, f)) {
        return;
    }
    start_updating(scope, key, kind, window);
}

/// `DidRemoveEventListener` / `DidRemoveAllEventListeners`: the target `key` has no listener of `ty` left.
pub(crate) fn did_remove_last_listener(scope: &mut v8::PinScope, key: EtKey, ty: &str) {
    let Some(kind) = Kind::of(ty) else { return };
    with(scope, |s| s.active.remove(&(key, kind)));
}

/// Every listener of the target `key` is gone (a frame's window given a new document).
pub(crate) fn did_remove_all_listeners(scope: &mut v8::PinScope, key: EtKey) {
    with(scope, |s| s.active.retain(|(k, _), _| *k != key));
}

/// The window a listener was added to, and its browsing context (`None` = the page's). A frame's BARE
/// `addEventListener` arrives with the page's window as `this` and means the frame's (`event_target::et_key_add`).
fn window_target<'s>(scope: &mut v8::PinScope<'s, '_>, key: EtKey, this: v8::Local<v8::Value>) -> Option<(Option<u32>, v8::Local<'s, v8::Object>)> {
    let main = bc::main_global(scope);
    if key == EtKey::Window {
        return Some((None, main));
    }
    let obj = v8::Local::<v8::Object>::try_from(this).ok()?;
    let window = if this.strict_equals(main.into()) { bc::running_frame_window(scope)? } else { v8::Local::new(scope, obj) };
    let id = bc::ctx_of_window(scope, window)?;
    (id != u32::MAX).then_some((Some(id), window))
}

/// `ExecutionContext::IsSecureContext()` for the window of `ctx`: its own document's URL potentially trustworthy
/// (an `about:` document is its creator's, which the walk reaches as an ancestor) and no insecure ancestor
/// (`LocalDOMWindow::HasInsecureContextInAncestors`). Chrome 150, measured: an `http://127.0.0.1` frame inside an
/// `http://<LAN address>` page is not secure and hears nothing.
fn secure_context(scope: &mut v8::PinScope, ctx: Option<u32>) -> bool {
    use crate::core::html::window::potentially_trustworthy;
    let trustworthy = |url: &str| url.is_empty() || url.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("about:")) || potentially_trustworthy(url);
    let mut cur = ctx;
    // bounded by the frame tree's depth
    for _ in 0..64 {
        match cur {
            Some(id) => {
                let url = bc::frame_context_url(scope, id);
                if !trustworthy(&url) {
                    return false;
                }
                match bc::frame_parent(scope, id) {
                    Some(parent) => cur = parent,
                    None => return true, // a popup: a top-level document
                }
            }
            None => {
                let (url, ancestors) = {
                    let op_state = deno_core::JsRuntime::op_state_from(scope);
                    let st = op_state.borrow();
                    let url = st.try_borrow::<crate::core::dom::document::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default();
                    let ancestors = crate::core::html::remote_frame::embedding(&st).map(|e| e.ancestor_origins).unwrap_or_default();
                    (url, ancestors)
                };
                // A remote frame's document: its ancestors live in other engines, their origins handed over.
                return trustworthy(&url) && ancestors.iter().all(|o| potentially_trustworthy(o));
            }
        }
    }
    false
}

/// `LocalDOMWindow::IsFeatureEnabled(feature)` for the window of `ctx`. A sandboxed frame's origin is opaque, so a
/// `'self'` default never admits it (Chrome 150, measured: an `allow-scripts` sandbox hears nothing).
fn policy_enabled(scope: &mut v8::PinScope, ctx: Option<u32>, window: v8::Local<v8::Object>, feature: &str) -> bool {
    match ctx {
        Some(id) if bc::realm_is_sandboxed(scope, window.into()) => crate::core::feature_policy::inherited_enabled(scope, feature, id, "null"),
        _ => crate::core::feature_policy::enabled_in(scope, feature, ctx),
    }
}

/// `PlatformEventController::StartUpdating`: unless running already, start the type's pump -- its sensor requests
/// go to the browser now.
fn start_updating(scope: &mut v8::PinScope, key: EtKey, kind: Kind, window: v8::Local<v8::Object>) {
    let window = v8::Global::new(scope, window);
    let id = with(scope, |s| {
        if s.active.contains_key(&(key, kind)) {
            return None;
        }
        s.next += 1;
        let id = s.next;
        s.active.insert((key, kind), id);
        s.pumps.insert(id, Pump { key, kind, window, round_trips: kind.round_trips() });
        Some(id)
    });
    if let Some(id) = id {
        let d: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
        let f = build_fn_data(scope, cb_sensor_reply, "", 0, d);
        queue_reply(scope, f, 0.0);
    }
}

/// The pump `id` when its controller still runs it; a stopped one is dropped (`Stop()` cancelled its timer, and a
/// reply arriving after it starts nothing).
fn current_pump(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u64> {
    let id = crate::binding::protos::body_data(scope, args).number_value(scope)? as u64;
    with(scope, |s| {
        let pump = s.pumps.get(&id)?;
        if s.active.get(&(pump.key, pump.kind)) != Some(&id) {
            s.pumps.remove(&id);
            return None;
        }
        Some(id)
    })
}

/// The browser's reply to the pump's sensor requests: no sensor. After the last one, the 60 Hz timer starts.
fn cb_sensor_reply(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = current_pump(scope, &args) else { return };
    let more = with(scope, |s| {
        let pump = s.pumps.get_mut(&id)?;
        pump.round_trips = pump.round_trips.saturating_sub(1);
        Some(pump.round_trips > 0)
    });
    let d: v8::Local<v8::Value> = v8::Number::new(scope, id as f64).into();
    match more {
        Some(true) => {
            let f = build_fn_data(scope, cb_sensor_reply, "", 0, d);
            queue_reply(scope, f, 0.0);
        }
        Some(false) => {
            let f = build_fn_data(scope, cb_pump_tick, "", 0, d);
            queue_task(scope, f, PUMP_DELAY_US as f64 / 1000.0);
        }
        None => {}
    }
}

/// The pump's first tick: no reading from any sensor, so the null event, to every listener of the type -- and the
/// controller, seeing a null event, stops (`DeviceSingleWindowEventController::DispatchDeviceEvent`).
fn cb_pump_tick(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = current_pump(scope, &args) else { return };
    let Some(pump) = with(scope, |s| s.pumps.remove(&id)) else { return };
    let window = v8::Local::new(scope, &pump.window);
    let detached = bc::ctx_of_window(scope, window).filter(|&c| c != u32::MAX).is_some_and(|c| !bc::context_fully_active(scope, c));
    if !detached {
        // The event is the window's realm's (a frame's own `DeviceMotionEvent`), whichever realm queued the task.
        let realm = window.get_creation_context(scope);
        match realm {
            Some(realm) => {
                let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
                dispatch_null_event(scope, &pump.window, pump.kind);
            }
            None => dispatch_null_event(scope, &pump.window, pump.kind),
        }
    }
    // `IsNullEvent` -> `StopUpdating`: whatever runs now stops -- a start a listener made during the dispatch after
    // removing the type's last listener included, as in Blink.
    with(scope, |s| s.active.remove(&(pump.key, pump.kind)));
}

fn dispatch_null_event(scope: &mut v8::PinScope, window: &v8::Global<v8::Object>, kind: Kind) {
    let window = v8::Local::new(scope, window);
    let Some(ctor) = crate::binding::protos::iface_object(scope, kind.interface()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return;
    };
    let t = str_val(scope, kind.event_type());
    // `DeviceMotionEvent::Create(type, data)` / `DeviceOrientationEvent::Create`: bubbles and cancelable false.
    let Some(ev) = ctor.new_instance(scope, &[t]) else { return };
    match kind {
        Kind::Motion => {
            // `DeviceMotionEventPump::GetDataFromSharedMemory` with no reading: each sub-object made of NaNs, which
            // its getters answer as null.
            for (slot, iface) in [
                ("acceleration", "DeviceMotionEventAcceleration"),
                ("accelerationIncludingGravity", "DeviceMotionEventAcceleration"),
                ("rotationRate", "DeviceMotionEventRotationRate"),
            ] {
                let Some(o) = reading(scope, iface, None) else { return };
                evt_slot_write(scope, ev, slot, o.into());
            }
            evt_slot_write_num(scope, ev, "interval", MOTION_INTERVAL_MS);
        }
        // `DeviceOrientationEventPump::GetDataFromSharedMemory`: no angle, `absolute` the pump's own.
        Kind::Orientation => {}
        Kind::OrientationAbsolute => {
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            evt_slot_write(scope, ev, "absolute", t);
        }
    }
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    crate::core::events::event_target::et_dispatch(scope, window.into(), ev);
}

/// The members of a `DeviceMotionEventAcceleration` / `DeviceMotionEventRotationRate`.
fn reading_members(iface: &str) -> &'static [&'static str] {
    if iface == "DeviceMotionEventRotationRate" { &["alpha", "beta", "gamma"] } else { &["x", "y", "z"] }
}

/// A `DeviceMotionEventAcceleration` / `DeviceMotionEventRotationRate`, each member the dictionary's number or null
/// (`Create(init)`: an absent member is NaN, which the getter answers as null); every member null with no dictionary.
fn reading<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, init: Option<v8::Local<v8::Object>>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = interface_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    for m in reading_members(iface) {
        let v = init.and_then(|d| get_prop(scope, d, *m)).filter(|v| v.is_number());
        let v: v8::Local<v8::Value> = match v {
            Some(n) if !n.number_value(scope).unwrap_or(f64::NAN).is_nan() => n,
            _ => v8::null(scope).into(),
        };
        evt_slot_write(scope, o, m, v);
    }
    crate::install::webidl::brand(scope, o, iface);
    Some(o)
}

/// `new DeviceMotionEvent(type, init)`'s three sub-objects (`DeviceMotionData::Create(init)`): a member the
/// dictionary has is a NEW `DeviceMotionEventAcceleration` / `DeviceMotionEventRotationRate` made from it; an absent
/// one stays null. `None` for every other member, which the shell constructor fills as it always has.
pub(crate) fn motion_init_member<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, member: &str, given: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    if iface != "DeviceMotionEvent" {
        return None;
    }
    let sub = match member {
        "acceleration" | "accelerationIncludingGravity" => "DeviceMotionEventAcceleration",
        "rotationRate" => "DeviceMotionEventRotationRate",
        _ => return None,
    };
    let init = v8::Local::<v8::Object>::try_from(given).ok();
    reading(scope, sub, init).map(Into::into)
}
