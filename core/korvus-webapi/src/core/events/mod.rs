//! `events` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).
//!
//! The event model is Rust end to end -- one listener store and one handler algorithm
//! (`event_target.rs`), error reporting (`report.rs`), `Observable`/`Subscriber`/`when()`
//! (`observable.rs`), and the `Event` / `EventTarget` prototypes as hand tables installed by
//! [`install_event_model`] over the JS classes the runtime still provides.

use deno_core::v8;

pub(crate) mod ctor;
pub(crate) mod event_target;
pub(crate) use event_target::EventListeners;
pub(crate) mod event;
pub(crate) use event::trusted_key;
/// One file per interface (or per constructor CHAIN), each carrying its `interface!` table
/// and the native constructor that fills its slots. These replaced the `host.js` / `worker-host.js`
/// classes, `generated_event_protos`, and the matching `generated_registry` shells.
pub(crate) mod custom_event;
/// `CloseEvent` -- the event a `WebSocket` closes with (`modules/websocket/web_socket.rs`).
pub(crate) mod close_event;
pub(crate) mod error_event;
/// The ServiceWorker realm's event chain -- `ExtendableEvent`, `FetchEvent`,
/// `InstallEvent`. They replaced `worker.rs`'s `SW_PRELUDE`, which hand-decorated a plain `Event`
/// instance with `request` / `respondWith` / `waitUntil` own properties. Installed by
/// `install::live::event_family` in the SERVICE realm only (`[Exposed=ServiceWorker]`).
pub(crate) mod extendable_event;
/// Moving the pointer -- the hover chain plus the four boundary events
/// (`mouseout`/`mouseleave`/`mouseover`/`mouseenter`), in the order stock Chrome 150 fires them.
pub mod hover;
/// FIXER C6: a real mouse -- Blink 150's input pipeline from one platform mouse or wheel event to every DOM event
/// it makes (hit test into frames, pointer capture, boundary events, pointer and compatibility mouse events,
/// click / auxclick / dblclick / contextmenu, focus), each built with Blink's fields in its target's realm.
pub mod pointer_input;
/// Moving the FOCUS ring -- `blur`/`focusout`/`focus`/`focusin`, in the order stock
/// Chrome 150 fires them, and which element a mouse press moves it to.
pub mod focus;
/// Typing -- a keystroke at the focused element and the editing command it runs
/// (`beforeinput` -> the value change -> `input`), measured off stock Chrome 150.
pub mod typing;
/// User activation -- the sticky and the 5-second transient bit that trusted key and pointer
/// input set, `navigator.userActivation` reads and a navigation's `Sec-Fetch-User` follows.
pub mod user_activation;
pub(crate) mod keyboard_event;
pub(crate) mod legacy_events;
pub(crate) mod message_event;
/// `PopStateEvent` and `HashChangeEvent`, which a same-document navigation fires.
pub(crate) mod navigation_events;
pub(crate) mod mouse_event;
pub(crate) mod touch_event;
pub(crate) mod ui_event;
pub(crate) mod observable;
/// The Observable operators and `Observable.from`.
pub(crate) mod observable_ops;
pub(crate) mod report;
pub(crate) mod security_policy_violation_event;
pub(crate) mod shared;

fn ctor<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Function>> {
    crate::binding::protos::global_get(scope, name).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
}

/// Install the event model on the current realm's global (window and worker alike):
///   1. `EventTarget.prototype` -- the table onto the JS `EventTarget` class's prototype
///      (core/korvus-runtime/src/js/events.js), through `installer::overwrite_prototype`: the
///      recorded order (addEventListener, dispatchEvent, removeEventListener, when), `constructor`
///      last, the constructor's length;
///   2. THE EVENT FAMILY -- `Event` and the fifteen interfaces that inherit from it, plus `Touch` and
///      `MessageChannel`, each built from its own table with a NATIVE constructor
///      (`install::live::event_family`). `Exposed` decides which of them a worker gets.
///   3. `Observable` -- a REAL native constructor (`new Observable(cb)`; the callback is required) with
///      a fresh prototype the table is installed onto, replacing the generated shell;
///   4. `Subscriber` -- the non-newable interface, created from its table.
/// Every row goes through the one member mount and into `ledger`. A realm whose `EventTarget` class is
/// missing (a bare runtime) skips that table; a prototype the table cannot take (a non-configurable
/// own key in the way) is reported, never half-installed.
pub fn install_event_model<'s>(scope: &mut v8::PinScope<'s, '_>, ledger: &mut crate::install::installer::InstallLedger, worker: bool) {
    use crate::install::installer::{create_interface, overwrite_prototype, Flavor};
    let global = scope.get_current_context().global(scope);
    // `EventTarget` IS ALREADY NATIVE IN BOTH REALMS WHEN THIS RUNS (the worker and the
    // window), and `events.js` DOES NOT EXIST. `nativize_event_target_ctor` used to sit here, swapping
    // the JS class the worker ESM layer put on the global for a native constructor over the SAME
    // prototype object. Now `install::live::bootstrap::mint_event_target` mints the interface object
    // and its prototype as the first statement of each realm's Rust block -- the slot each JS
    // assignment held -- so there is one `EventTarget` function from the start rather than two in
    // sequence, and every lane that splices a child onto `EventTarget` by name (this table, then the
    // crypto/XHR, abort/style, media and observer installs, and the window's DOM spine) sees the same
    // one. Nothing here is realm-conditional, and now for the same reason in both directions.
    // The bootstrap's mint mounts the table's rows with the interface; a realm whose
    // EventTarget came from elsewhere (a bare runtime) still gets them here.
    if let Some(et) = ctor(scope, "EventTarget") {
        let proto = crate::binding::protos::get_prop(scope, et.into(), "prototype").and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
        let has_rows = proto.is_some_and(|p| {
            let k = crate::binding::protos::member_key(scope, "addEventListener");
            p.has_own_property(scope, k).unwrap_or(false)
        });
        if !has_rows {
            if let Err(e) = overwrite_prototype(scope, et, &event_target::EVENTTARGET, ledger) {
                eprintln!("[korvus] event model: {e}");
            }
        }
    }
    // The WINDOW's event family is NOT built here: it has to exist before the pre-bootstrap lanes
    // that splice onto `Event` (`AnimationEvent`, `TransitionEvent`, `AnimationPlaybackEvent`,
    // `SecurityPolicyViolationEvent`), so it is the second statement of that block --
    // `install::live::install_window_event_family`, called from korvus-runtime. The WORKER has no
    // such block, so its family is built here, where its event model already was.
    if worker {
        crate::install::live::install_worker_event_family(scope, ledger);
    }
    // `Observable`: a real constructor over the table's rows -- `build_newable`, the one builder of that
    // shape, which takes the template step (it was a hand-rolled `build_ctor` + fresh
    // prototype + `overwrite_prototype`, which no template recorded).
    crate::install::live::rows::build_newable(scope, global, &observable::OBSERVABLE, observable::cb_observable_ctor, None, None, ledger);
    create_interface(scope, global, &observable::SUBSCRIBER, "Subscriber", None, None, Flavor::Table, ledger);
}

/// [`install_event_model`] with the context's own ledger -- the worker realm's entry point
/// (core/korvus-runtime/src/worker.rs), beside the DOMPoint/DOMRect reconciles.
pub fn install_worker_event_model<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let mut ledger = crate::install::installer::take_ledger(scope);
    install_event_model(scope, &mut ledger, true);
    crate::install::installer::put_ledger(scope, ledger);
}

