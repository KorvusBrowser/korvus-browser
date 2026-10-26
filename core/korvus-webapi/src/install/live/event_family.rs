//! The EVENT FAMILY: `Event` and the fifteen interfaces that inherit from it, plus `Touch` and
//! `MessageChannel`.
//!
//! WHERE THIS RUNS, and why (the unit brief's failure mode 4 — "an install ORDER change discarding a
//! decorator"). The seventeen used to be created during `host.js`'s ESM evaluation, which is the
//! FIRST thing a realm does, and four later layers then decorated their prototypes:
//!
//!   * `js/generated_event_protos.js` -> `op_install_event_protos` (UIEvent / MouseEvent /
//!     KeyboardEvent members) — DELETED by this unit; those rows are table rows now.
//!   * `js/dom/08g-observers.js` (`initCustomEvent`, two `getModifierState`s, the two PointerEvent
//!     list methods) — DELETED; table rows.
//!   * `js/generated_shells.js`'s registry create pass — it SKIPPED every name already on the global,
//!     which was all of them, and the names leave the registry with the tables anyway.
//!   * `host.js`'s `enumerateProto` sweep, which flipped a JS class's non-enumerable prototype methods
//!     to enumerable — a no-op for a table, whose rows carry the capture's own descriptor flags.
//!
//! So nothing decorates these prototypes any more, and the install runs at the position
//! [`super::html_image::install_window_html_image_element`] already used for `Event` and
//! `EventTarget` (`events::install_event_model`): the POST-bootstrap block of the runtime's context
//! creation. That is the moment `Event.prototype` has been the table's since W5.0b, so this unit
//! moves no existing boundary — it only stops the JS half from existing.
//!
//! WHAT RUNS BETWEEN THE ESM LAYER AND HERE, checked rather than assumed: `BOOTSTRAP` is evaluated in
//! between, and it CONSTRUCTS no event (its one `new Event(type)` is inside a function body that runs
//! at dispatch time, long after this). Every other producer in the tree — `core/events/shared.rs`'s
//! trusted input path, `core/events/report.rs`, the worker receive loop, `BroadcastChannel` delivery,
//! `document.createEvent` — reads the interface object off the global at CALL time.
//!
//! The order WITHIN this function is WebIDL inheritance order: a child's prototype chain is spliced
//! onto a parent that must already exist. `Event` first, then `UIEvent`, then `MouseEvent`, then its
//! two children.

use deno_core::v8;

use super::rows::build_newable;
use crate::install::installer::{self, Flavor};

/// Build the family from its tables onto `global`. `realm` only names the realm in a diagnostic.
///
/// `Exposed` decides the rows: the eight interfaces the captured WORKER surface lists are `both` in
/// their tables and the rest are `window`, so the worker call below simply skips what a worker has
/// never had. There is no exception list and no second set of definitions — which is the whole point
/// of the unit (invariant I8).
fn install_family<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    worker: bool,
    ledger: &mut installer::InstallLedger,
) {
    use crate::core::events::custom_event::{cb_custom_event_ctor, CUSTOMEVENT};
    use crate::core::events::error_event::{
        cb_error_event_ctor, cb_progress_event_ctor, cb_promise_rejection_event_ctor, ERROREVENT,
        PROGRESSEVENT, PROMISEREJECTIONEVENT,
    };
    use crate::core::events::event::{cb_event_ctor, EVENT};
    use crate::core::events::keyboard_event::{
        cb_focus_event_ctor, cb_input_event_ctor, cb_keyboard_event_ctor, FOCUSEVENT, INPUTEVENT, KEYBOARDEVENT,
    };
    use crate::core::events::legacy_events::{cb_cookie_change_event_ctor, BEFOREUNLOADEVENT, COOKIECHANGEEVENT};
    use crate::core::events::message_event::{
        cb_message_channel_ctor, cb_message_event_ctor, MESSAGECHANNEL, MESSAGEEVENT,
    };
    use crate::core::events::mouse_event::{
        cb_mouse_event_ctor, cb_pointer_event_ctor, cb_wheel_event_ctor, MOUSEEVENT, POINTEREVENT, WHEELEVENT,
    };
    use crate::core::events::touch_event::{cb_touch_ctor, cb_touch_event_ctor, TOUCH, TOUCHEVENT};
    use crate::core::events::ui_event::{cb_ui_event_ctor, UIEVENT};

    // The root. Everything below splices onto it, so it cannot be reordered.
    build_newable(scope, global, &EVENT, cb_event_ctor, None, None, ledger);
    build_newable(scope, global, &CUSTOMEVENT, cb_custom_event_ctor, Some("Event"), None, ledger);
    build_newable(scope, global, &ERROREVENT, cb_error_event_ctor, Some("Event"), None, ledger);
    // `PromiseRejectionEvent`. It was a registry interface with two `stub` rows, so every
    // `unhandledrejection` this engine fired carried `promise === undefined` and
    // `reason === undefined`. Beside `ErrorEvent` because the two are the reporting path's pair.
    build_newable(scope, global, &PROMISEREJECTIONEVENT, cb_promise_rejection_event_ctor, Some("Event"), None, ledger);
    build_newable(scope, global, &PROGRESSEVENT, cb_progress_event_ctor, Some("Event"), None, ledger);
    build_newable(scope, global, &MESSAGEEVENT, cb_message_event_ctor, Some("Event"), None, ledger);
    build_newable(scope, global, &MESSAGECHANNEL, cb_message_channel_ctor, None, None, ledger);
    if worker {
        // The `[Exposed=ServiceWorker]` chain, and ONLY in a ServiceWorker realm.
        //
        // The `interface!` exposure vocabulary is window|worker|both and has no `service` word, so
        // the three tables say `worker` -- the truthful half, since none of them is a window
        // interface -- and the realm gate is HERE. That is not a new mechanism: the derived global
        // scopes themselves are picked exactly this way (`install_worker_global_scope` switches
        // `SharedWorkerGlobalScope` / `ServiceWorkerGlobalScope` / `DedicatedWorkerGlobalScope` on
        // the same `scope_kind` answer), and it asks the kernel the same question through the same
        // function rather than inventing a second one.
        //
        // A DEDICATED worker must not gain a name from this: `tests/worker_surface.rs` diffs that
        // realm's globals against the capture and both `korvus-only interfaces` and `GLOBAL-TELLS`
        // are 0, so an unconditional install here would fail the gate rather than pass quietly.
        if crate::core::workers::worker_global_scope::scope_kind(scope) == "service" {
            use crate::core::events::extendable_event::{
                cb_extendable_event_ctor, cb_fetch_event_ctor, cb_install_event_ctor, EXTENDABLEEVENT, FETCHEVENT,
                INSTALLEVENT,
            };
            // WebIDL inheritance order again: the parent has to exist before a child splices onto it.
            build_newable(scope, global, &EXTENDABLEEVENT, cb_extendable_event_ctor, Some("Event"), None, ledger);
            build_newable(scope, global, &FETCHEVENT, cb_fetch_event_ctor, Some("ExtendableEvent"), None, ledger);
            build_newable(scope, global, &INSTALLEVENT, cb_install_event_ctor, Some("ExtendableEvent"), None, ledger);
            // `self.clients`' interfaces, the same gate.
            crate::core::workers::service_worker::install_sw_client_interfaces(scope, global, ledger);
        }
        return;
    }
    // Window-only from here: the captured worker surface lists none of these.
    build_newable(scope, global, &COOKIECHANGEEVENT, cb_cookie_change_event_ctor, Some("Event"), None, ledger);
    // `BeforeUnloadEvent` is `Model::Interface`: `new BeforeUnloadEvent()` throws "Illegal
    // constructor" (measured on Chrome), and `document.createEvent("BeforeUnloadEvent")` mints it in
    // Rust instead. `create_interface` builds exactly that shape.
    let event_parent = super::rows::parent_of(scope, "Event");
    let built = installer::create_interface(
        scope,
        global,
        &BEFOREUNLOADEVENT,
        "BeforeUnloadEvent",
        event_parent,
        None,
        Flavor::Table,
        ledger,
    );
    for g in &built.gaps {
        eprintln!("[korvus] event family install gap: {}.{}", g.interface, g.member);
    }
    build_newable(scope, global, &UIEVENT, cb_ui_event_ctor, Some("Event"), None, ledger);
    build_newable(scope, global, &MOUSEEVENT, cb_mouse_event_ctor, Some("UIEvent"), None, ledger);
    build_newable(scope, global, &POINTEREVENT, cb_pointer_event_ctor, Some("MouseEvent"), None, ledger);
    build_newable(scope, global, &WHEELEVENT, cb_wheel_event_ctor, Some("MouseEvent"), None, ledger);
    build_newable(scope, global, &KEYBOARDEVENT, cb_keyboard_event_ctor, Some("UIEvent"), None, ledger);
    build_newable(scope, global, &INPUTEVENT, cb_input_event_ctor, Some("UIEvent"), None, ledger);
    build_newable(scope, global, &FOCUSEVENT, cb_focus_event_ctor, Some("UIEvent"), None, ledger);
    build_newable(scope, global, &TOUCHEVENT, cb_touch_event_ctor, Some("UIEvent"), None, ledger);
    build_newable(scope, global, &TOUCH, cb_touch_ctor, None, None, ledger);
}

/// The WINDOW realm's event family, from the runtime's pre-bootstrap block (SECOND statement, right
/// after `install_window_postshells`).
///
/// THE POSITION IS MEASURED, not chosen. `Event` used to exist from `host.js`'s ESM evaluation, which
/// is before everything, and four later lanes in that same block splice a child onto it:
/// `AnimationEvent`, `TransitionEvent` and `AnimationPlaybackEvent` (L5) and
/// `SecurityPolicyViolationEvent` (L1). Built at the position `install_event_model` occupies -- AFTER
/// the bootstrap -- the family left all four on the bare `Object.prototype`, and `anim_live` and
/// `small_elements_live` said so in the same run: `AnimationEvent:` where the capture records
/// `AnimationEvent:Event`, and `new SecurityPolicyViolationEvent("securitypolicyviolation").type`
/// reading `undefined`. Here, every later lane finds the parent it always found.
///
/// It is after `install_window_postshells` and not before it because that lane is the webapi
/// extension's ESM epilogue -- the arity and constant-seal passes over the REGISTRY (there was an
/// enumeration-order pass too until it and its table were deleted) -- and the nineteen names
/// leave the registry with their tables, so the two cannot interact in either direction.
pub fn install_window_event_family_now<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = installer::take_ledger(scope);
    install_family(scope, global, false, &mut ledger);
    installer::put_ledger(scope, ledger);
    // `rechain_orphans` LAST, for the same reason six other lanes end with it -- and here it matters
    // more than anywhere else, because `Event` is the parent of more registry interfaces than any
    // other name in the tree. A registry child of a name this lane took OUT of the registry is left
    // on the bare `Object.prototype` by the registry's own chain pass, which ran before these tables
    // existed. Two such children are installed EARLIER than this lane, during the webapi extension's
    // ESM evaluation, and so cannot be repaired by anything before it: `OfflineAudioCompletionEvent`
    // and `GPUUncapturedErrorEvent`, both mounted by `wire_canvas_oracle` (lane L17). Found by
    // their own live gates failing on this merge -- `OfflineAudioCompletionEvent -> Event` answered
    // `false|false`, and `new GPUUncapturedErrorEvent(...)` produced no `type`.
    //
    // Derived from the registry, never a list, and deliberately narrow: it repairs only a child whose
    // prototype is still the empty root, so running it again here cannot re-point anything a lane
    // deliberately chained itself.
    super::rows::rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
    // ...and the hand tables that named `Event` before it existed (see `drain_pending_chains`).
    super::rows::drain_pending_chains(scope, global);
}

/// The WORKER realm's, from `core::events::install_worker_event_model`.
pub fn install_worker_event_family<'s>(scope: &mut v8::PinScope<'s, '_>, ledger: &mut installer::InstallLedger) {
    let global = scope.get_current_context().global(scope);
    install_family(scope, global, true, ledger);
    // The worker's half of the repair above, against the WORKER registry. Symmetric with
    // `abort_style`, `media_rtc` and `performance`, each of which rechains in both realms.
    super::rows::rechain_orphans(scope, global, crate::install::generated_registry::WORKER_ORDER);
    super::rows::drain_pending_chains(scope, global);
}
