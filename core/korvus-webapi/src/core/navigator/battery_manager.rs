//! `core/navigator/battery_manager.rs` -- `BatteryManager` and `navigator.getBattery()` as Blink's
//! `battery_manager.cc` / `navigator_battery.cc` answer them.
//!
//! WHAT WAS HERE BEFORE. `getBattery()` resolved AT ONCE with a fresh object per call that carried its four
//! values, four handlers and an `addEventListener` trio as OWN properties over the interface's stub getters
//! (`persona_objects::build_battery`): `JSON.stringify(manager)` printed them, `onlevelchange = 5` kept the 5, an
//! event dispatched at it reached no listener, and two calls answered two promises and two managers. The challenge
//! frame reads `charging`, `level`, `chargingTime` and `dischargingTime` right after its `getBattery()`, and its
//! flow went on differently from there (live trace, rateyourmusic, 2026-10-09).
//!
//! STOCK CHROME 150, measured (`tests/fixtures/navigator/battery-*`): one `BatteryManager` per navigator and one
//! promise -- every call answers the same promise; the first resolves a task later (after a 0 ms timer queued right
//! after the call, before a 1 ms one: the browser's battery status arrives over IPC); the manager owns nothing
//! (`{}` as JSON), its four values are `BatteryManager.prototype` getters checking their receiver, its handlers are
//! event handler IDL attributes, and it is an `EventTarget`.
//!
//! THE VALUES are the recorded answer of a desktop on mains power, fully charged: `charging` true, `chargingTime`
//! 0, `dischargingTime` Infinity, `level` 1 -- what no persona field carries (see `persona_objects`).

use deno_core::v8;

use crate::install::webidl::{brand, branded_getter_row};
use crate::install::MemberDef;

fn cb_charging(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(true);
}

fn cb_charging_time(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_double(0.0);
}

fn cb_discharging_time(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_double(f64::INFINITY);
}

fn cb_level(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_double(1.0);
}

/// The body every value row of [`BATTERYMANAGER`] names: a branded getter (a foreign receiver is `Illegal
/// invocation`, as Chrome's).
pub fn install_battery_manager(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "charging" => branded_getter_row(scope, proto, member, cb_charging),
        "chargingTime" => branded_getter_row(scope, proto, member, cb_charging_time),
        "dischargingTime" => branded_getter_row(scope, proto, member, cb_discharging_time),
        "level" => branded_getter_row(scope, proto, member, cb_level),
        other => debug_assert!(false, "install_battery_manager: unexpected member {other}"),
    }
}

// Rows from the `BatteryManager` block of `install/generated_registry.rs`, in the capture's order (Chrome 150:
// `charging, chargingTime, dischargingTime, level, onchargingchange, onchargingtimechange, ondischargingtimechange,
// onlevelchange, constructor`).
webapi::interface! {
    "BatteryManager" : "EventTarget" [interface, window, secure] {
        ctor    "BatteryManager"                        len 0 = stub,
        attr    "charging"        : Boolean readonly          = install_battery_manager,
        attr    "chargingTime"    : Double  readonly          = install_battery_manager,
        attr    "dischargingTime" : Double  readonly          = install_battery_manager,
        attr    "level"           : Double  readonly          = install_battery_manager,
        handler "onchargingchange"                            = event_handler,
        handler "onchargingtimechange"                        = event_handler,
        handler "ondischargingtimechange"                     = event_handler,
        handler "onlevelchange"                               = event_handler
    }
}

/// Create the interface at the capture's own-global position of `BatteryManager` (`install::live::ORDERED_HAND`).
pub fn create_battery_manager<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    ledger: &mut crate::install::installer::InstallLedger,
) {
    use crate::install::installer::{self, Flavor};
    let et = crate::install::live::rows::parent_of(scope, "EventTarget");
    let built = installer::create_interface(scope, global, &BATTERYMANAGER, "BatteryManager", et, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] battery-manager install gap: {}.{}", g.interface, g.member);
    }
}

/// The navigator's promise, on a private slot of the Navigator.
const P_BATTERY: &str = "korvus_battery_promise";

/// The promises the browser's battery status has yet to resolve, by task id, with the manager each resolves to.
#[derive(Default)]
struct PendingBattery(std::collections::HashMap<u64, (v8::Global<v8::PromiseResolver>, v8::Global<v8::Object>)>, u64);

/// `navigator.getBattery()`: the navigator's one promise -- made at the first call, resolved a task later with the
/// navigator's one manager.
pub(crate) fn cb_get_battery(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let nav = args.this();
    if let Some(p) = crate::core::css::css_style_declaration::priv_get(scope, nav, P_BATTERY) {
        rv.set(p);
        return;
    }
    let Some(proto) = crate::binding::protos::shell_proto(scope, "BatteryManager") else { return };
    let manager = crate::binding::protos::mint_on(scope, proto.into());
    brand(scope, manager, "BatteryManager");
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let promise = resolver.get_promise(scope);
    crate::core::dom::element::priv_set(scope, nav, P_BATTERY, promise.into());
    let (r, m) = (v8::Global::new(scope, resolver), v8::Global::new(scope, manager));
    let id = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if !st.has::<PendingBattery>() {
            st.put(PendingBattery::default());
        }
        let p = st.borrow_mut::<PendingBattery>();
        p.1 += 1;
        p.0.insert(p.1, (r, m));
        p.1
    };
    // the browser's answer arrives a task later: after a 0 ms timer queued right after the call, before a 1 ms one
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_resolve, id, 1.0);
    rv.set(promise.into());
}

fn cb_resolve(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u64) else { return };
    let pending = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<PendingBattery>().and_then(|p| p.0.remove(&id))
    };
    let Some((r, m)) = pending else { return };
    let (r, m) = (v8::Local::new(scope, &r), v8::Local::new(scope, &m));
    r.resolve(scope, m.into());
}
