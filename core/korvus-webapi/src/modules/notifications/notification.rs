//! `modules/notifications/notification.rs` -- the `Notification` table.
//!
//! Korvus has no OS notification surface, so this interface is honest about that: the CLASS in
//! `core/korvus-runtime/src/js/host.js` carries the constructor's options into the instance's private
//! SLOTS and nothing is ever delivered. Every row below says so -- the twenty attributes read those
//! slots (`kits::slot_get`), the four `on*` handlers are the ONE event-handler store, and `close`, the
//! two static attributes and `requestPermission` are honest D2 `stub` rows. `close` was an
//! `op_noop_method` installed from `js/dom/08h-abort-attrs.js`; it is still a stub, now a counted one.
//! No notification delivery is invented here.
//!
//! The three STATICS are real, and they moved here with the interface: `requestPermission` was
//! `korvus-runtime`'s `op_notification_request_permission` installed from host.js by hand, and
//! `permission` / `maxActions` were two `Object.defineProperty` DATA properties where the capture
//! records native ACCESSORS. All three are rows now, with the descriptors `surface.json` gives them.
//!
//! What changes hands is the PROTOTYPE, which is `installer::overwrite_prototype`'s in the window and
//! `installer::create_interface`'s in the worker (which has no `Notification` class).

use deno_core::v8;
use crate::binding::protos::{int_val, str_val};
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;

/// `Notification.requestPermission(cb)` -- the request opens a prompt, and Brave 151 leaves the promise
/// pending behind it while `Notification.permission` stays "default" (measured
/// 2026-10-01). This resolved "default" at once, as no browser with a person at it does. The legacy
/// callback is called when the promise settles, so it is not called either.
fn cb_request_permission(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = crate::binding::protos::pending_promise(scope);
    rv.set(p);
}

/// `Notification.permission` as Blink answers it (`Notification::permission`, notification.cc): an insecure
/// context is "denied" without asking; otherwise the stored status, which in Korvus is never granted (ASK,
/// "default") -- except that "permission can only be requested from top-level frames and same-origin iframes":
/// with ASK, a context with no frame (a worker) or a frame CROSS-ORIGIN to the outermost main frame answers
/// "denied". Measured: stock Chrome 150 in a cross-origin iframe says "denied" -- the
/// an interactive-widget challenge challenge frame reads it there -- where this said "default".
fn cb_permission(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let href = crate::core::html::window::href(scope);
    let denied = !crate::core::html::window::potentially_trustworthy(&href)
        || !crate::core::html::browsing_context::has_document(scope)
        || crate::core::html::browsing_context::realm_cross_origin_to_top(scope);
    let v = str_val(scope, if denied { "denied" } else { "default" });
    rv.set(v);
}

/// `Notification.maxActions` -- the recorded Chrome value.
fn cb_max_actions(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = int_val(scope, 2);
    rv.set(v);
}

/// The body the three implemented rows of [`NOTIFICATION`] name -- all three statics, so `holder` is
/// the interface object.
pub fn install_notification(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "permission" => getter_row(scope, holder, member, cb_permission),
        "maxActions" => getter_row(scope, holder, member, cb_max_actions),
        "requestPermission" => method_row(scope, holder, member, cb_request_permission),
        other => debug_assert!(false, "install_notification: unexpected member {other}"),
    }
}

// ── the constructor ──────────────────────────────────────────────────────────────────

/// `new Notification(title, options)` -- native, replacing `host.js`'s
/// `class Notification extends EventTarget`. Everything it carries goes into the private slots the
/// twenty rows above read; a real `Notification` owns no properties at all, which is what the capture
/// records and what the class (which called `super()` and so acquired `events.js`'s own
/// `__listeners` property) could not give.
///
/// Measured against `google-chrome --headless=new`, and wrong in the class:
///   * `new Notification()` is a TypeError ("1 argument required") -- the class accepted it.
///   * `new Notification("t", {dir: "nope"})` is a TypeError -- `dir` is a WebIDL enum
///     (`auto` / `ltr` / `rtl`), and the class stored whatever string it was handed.
///   * `new Notification(5).title` is the STRING `"5"`.
pub(crate) fn cb_notification_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    use crate::core::events::ctor::{
        arg_string, brand, dict_any, dict_bool, dict_enum, dict_string, init_dict, require_args, require_new,
    };
    use crate::core::events::event::{evt_slot_write, evt_slot_write_num};

    if !require_new(scope, &args, "Notification") || !require_args(scope, &args, "Notification", 1) {
        return;
    }
    let Some(title) = arg_string(scope, &args, 0, "Notification") else { return };
    let Ok(opts) = init_dict(scope, &args, 1, "Notification", "NotificationOptions") else { return };
    let this = args.this();
    let tv = str_val(scope, &title);
    evt_slot_write(scope, this, "title", tv);
    for name in ["body", "icon", "badge", "image", "tag", "lang"] {
        let s = match dict_string(scope, opts, name) { Ok(v) => v.unwrap_or_default(), Err(_) => return };
        let v = str_val(scope, &s);
        evt_slot_write(scope, this, name, v);
    }
    let dir = match dict_enum(scope, opts, "dir", "Notification", "NotificationOptions", "NotificationDirection", &["auto", "ltr", "rtl"]) {
        Ok(v) => v.unwrap_or_else(|| "auto".to_string()),
        Err(_) => return,
    };
    let dv = str_val(scope, &dir);
    evt_slot_write(scope, this, "dir", dv);
    for name in ["silent", "requireInteraction", "renotify"] {
        let b = dict_bool(scope, opts, name);
        let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
        evt_slot_write(scope, this, name, v);
    }
    match dict_any(scope, opts, "data") {
        Some(v) if !v.is_null() => evt_slot_write(scope, this, "data", v),
        _ => crate::core::events::event::evt_slot_write_null(scope, this, "data"),
    }
    // `actions` and `vibrate` are FrozenArrays this engine never fills: it shows no notification and
    // vibrates nothing, and an empty frozen list is what Chrome hands back for a page-constructed one.
    for name in ["actions", "vibrate"] {
        let arr = v8::Array::new(scope, 0);
        let o: v8::Local<v8::Object> = arr.into();
        let _ = o.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
        let v: v8::Local<v8::Value> = arr.into();
        evt_slot_write(scope, this, name, v);
    }
    // `timestamp` is a DOMTimeStamp: epoch milliseconds, the same `Date.now()` the class read.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs_f64() * 1000.0).floor())
        .unwrap_or(0.0);
    evt_slot_write_num(scope, this, "timestamp", now);
    brand(scope, this, "Notification");
    rv.set(this.into());
}

// `WORKER_NOTIFICATION_MEMBERS` WAS HERE: the worker capture's twenty-two names. Its
// one fact -- `requestPermission` is an `[Exposed=Window]` static, which is why the worker capture
// lists two statics and the window's three -- is the `window` word on that row now.

// Rows VERBATIM from the `Notification` block of `install/generated_registry.rs`, in the capture's
// order: the four handlers, fifteen slot-backed attributes (`silent` is the nullable one), `close`,
// `image`, then the three statics. ONE row deviates from the registry's data, and only in its IDL
// return TYPE: `close` is `undefined close()` in webref (gen/webref/notifications.idl:37), where the
// registry -- which is generated from the capture, and a capture cannot see a return type -- carries
// the generic `Object`. It matters because the D2 stub derives its empty answer from that type:
// `Object` would make `n.close()` return `null`, and both Chrome and the `op_noop_method` this row
// replaces return `undefined`. `[value, window]` with parent `EventTarget` and WebIDL length 1;
// the worker exposure is the selector above, walked by `install::live::install_worker_abort_style`.
webapi::interface! {
    "Notification" : "EventTarget" [value, both] {
        ctor          "Notification"                          len 1 = stub,
        handler       "onclick"                                     = evt_handler,
        handler       "onshow"                                      = evt_handler,
        handler       "onerror"                                     = evt_handler,
        handler       "onclose"                                     = evt_handler,
        attr          "title"              : Object readonly        = evt_get,
        attr          "dir"                : Object readonly        = evt_get,
        attr          "lang"               : Object readonly        = evt_get,
        attr          "body"               : Object readonly        = evt_get,
        attr          "tag"                : Object readonly        = evt_get,
        attr          "icon"               : Object readonly        = evt_get,
        attr          "badge"              : Object readonly        = evt_get,
        attr          "vibrate"            : Object readonly        = evt_get,
        attr          "timestamp"          : Object readonly        = evt_get,
        attr          "renotify"           : Object readonly        = evt_get,
        attr          "silent"             : Object readonly nullable = evt_get_null,
        attr          "requireInteraction" : Object readonly        = evt_get,
        attr          "data"               : Object readonly        = evt_get,
        attr          "actions"            : Object readonly        = evt_get,
        method        "close"              : Undefined len 0        = stub,
        attr          "image"              : Object readonly        = evt_get,
        // EDGE'S ONE, after `image`, which is where Edge 151 puts it -- and it is the only row in
        // this table that appears in BOTH realms' Edge captures, so it closes a window gap and a
        // worker gap with one line (the `both` exposure above already covers that).
        //
        // MEASURED on Microsoft Edge 151.0.0.0, 2026-09-13: a getter-only accessor named
        // `get scenario`, length 0, enumerable and configurable, with a real receiver check
        // (`Notification.prototype.scenario` throws `TypeError: Illegal invocation`). Its VALUE on a
        // constructed notification could not be read without granting the permission prompt, which
        // this rig does not click (the external-traffic rule), so the row is a D2 stub: a counted
        // "no body" rather than a value nobody measured. `DomString` because every neighbouring
        // Notification string attribute is one and the member is an enum in Edge's IDL.
        attr          "scenario"           : DomString readonly     = stub,
        static_attr   "permission"         : Object readonly        = install_notification,
        static_attr   "maxActions"         : Object readonly        = install_notification,
        // THE ROW SAYS WHICH REALM. This fact used to live in a hand-written
        // per-realm selector beside the table -- a copy of one capture's member list. The list a
        // realm installs is the ACTIVE capture's now (`install::installer::join`), so the fact
        // belongs where WebIDL puts it, on the row: BOTH bundles agree, in both realms, and a
        // realm with no capture to read (a bare worker runtime) is right by this word alone.
        static_method "requestPermission"  : Object window len 0    = install_notification
    }
}
