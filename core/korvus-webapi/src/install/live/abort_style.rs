//! Abort, style, blob, fetch and notification.
//!
//! The ten interfaces `dom/08h-abort-attrs.js` used to install with 19 `Object.defineProperty` sites,
//! three `op_nativize` masks and an `op_stub_ctor` interface object. Each is an `interface!` table in
//! its own interface file now (`core/dom/{abort_controller,abstract_range}.rs`,
//! `core/css/css_style_declaration.rs`, `core/fileapi/{blob,file}.rs`,
//! `modules/fetch/{response,request}.rs`, `core/timing/performance_observer.rs`,
//! `modules/notifications/notification.rs`), and this is where THE installer mounts them.
//!
//! Nine of the ten still have a JS CLASS behind them (`core/korvus-runtime/src/js/host.js` and
//! `shared-globals.js`, until W4.b/W4.c ports them), so their prototypes are taken through
//! `installer::overwrite_prototype` -- the shape W5.0b used for `Event` / `EventTarget`. Only
//! `CSSStyleDeclaration`, which never had a class, is `create_interface`'s outright.
//!
//! NO RECEIVER CHECK is added by this lane, deliberately. These ten are JS-class-backed: their
//! instances are ordinary objects with no cppgc brand, and in the WORKER realm six of them have no
//! class at all, so a brand check would turn today's `undefined` into a throw for, say,
//! `new AbortController().signal` inside testharness.js. The bodies are the ones that already existed,
//! answering exactly what they answered before; W4.b/W4.c port the classes and can brand then.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

use super::rows::{parent_of, rechain_orphans};
use crate::binding::protos::global_get;
use crate::install::installer::{self, Flavor};

/// `globalThis.DOMException`: the table's own native constructor and prototype, in either
/// realm.
///
/// FIRST IN THIS LANE, because it is the error type every other interface in it throws --
/// `AbortSignal.abort()`'s default reason, `Blob`'s and `ImageData`'s range errors, `throw_dom` from
/// anywhere in `korvus-webapi`. All of those read `globalThis.DOMException` at CALL time, so the
/// position is about legibility rather than correctness; what the position IS about is the sweep at
/// the end of this lane (see below).
///
/// NOT `build_newable(..., Some("Error"), ...)`, and the difference is measured. That helper splices
/// BOTH halves of the WebIDL inheritance pair -- `proto.[[Prototype]] = Error.prototype` and
/// `ctor.[[Prototype]] = Error` -- which is right for every ordinary child interface and wrong for
/// this one: Chrome 150 answers `Object.getPrototypeOf(DOMException).name === ""`, i.e.
/// `Function.prototype`, while `Object.getPrototypeOf(DOMException.prototype)` is `Error.prototype`.
/// (WebIDL gives DOMException `%Error.prototype%` for its prototype object and leaves the interface
/// object on `%Function.prototype%`.) `class DOMException extends Error` in `host.js` set both, so the
/// engine carried a second chain a page reads with one `getPrototypeOf`. So the prototype is spliced
/// here, alone.
///
/// THE FIVE CHILDREN. `RTCError`, `OverconstrainedError`, `WebTransportError`, `QuotaExceededError`
/// and `WebSocketError` are registry interfaces whose `parent` is `DOMException`. The registry's chain
/// pass runs during the ESM layer and used to find `host.js`'s class there; with the class gone it
/// finds nothing and leaves all five on the bare `Object.prototype` -- the fourth failure mode of a
/// table cutover, and exactly what W4.b hit with `Event`. The `rechain_orphans` sweep this lane
/// already ends with is the repair, and it is why this install belongs in THIS lane rather than
/// anywhere else in the pre-bootstrap block. Asserted by `dom_exception_live`.
fn install_dom_exception_interface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut installer::InstallLedger) {
    use crate::binding::protos::get_prop;
    use crate::core::dom::dom_exception::{cb_dom_exception_ctor, DOMEXCEPTION};
    super::rows::build_newable(scope, global, &DOMEXCEPTION, cb_dom_exception_ctor, None, None, ledger);
    let Some((proto, _)) = parent_of(scope, "DOMException") else { return };
    let Some(error_proto) = global_get(scope, "Error")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|e| get_prop(scope, e, "prototype"))
    else {
        return;
    };
    proto.set_prototype(scope, error_proto);
}

/// `globalThis.AbortSignal` and `globalThis.AbortController`, in either realm.
///
/// `AbortSignal` is NON-NEWABLE: WebIDL declares no constructor and `new AbortSignal()` is a TypeError
/// in Chrome, so `create_interface`'s `Model::Interface` shape is the whole interface object. Its
/// prototype is the installer's now rather than `host.js`'s class's -- W4.c deleted
/// `class AbortSignal extends globalThis.EventTarget`, which was one of the THREE ESM-time `extends
/// EventTarget` declarations W4.b named as the reason `events.js` still exists -- and every signal is
/// minted in Rust onto it (`abort_controller::mint_signal`).
///
/// BOTH halves of the WebIDL inheritance pair are spliced here, unlike `DOMException`: Chrome answers
/// `Object.getPrototypeOf(AbortSignal) === EventTarget` and
/// `Object.getPrototypeOf(AbortSignal.prototype) === EventTarget.prototype` (measured). That is what
/// makes `signal.addEventListener` the inherited native one and `signal instanceof EventTarget` true.
fn install_abort_signal_interface<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut installer::InstallLedger) {
    use crate::core::dom::abort_controller::{cb_abort_controller_ctor, ABORTCONTROLLER, ABORTSIGNAL};
    let et = parent_of(scope, "EventTarget");
    if et.is_none() {
        eprintln!("[korvus] abort/style install: EventTarget is not exposed in this realm");
    }
    let built = installer::create_interface(scope, global, &ABORTSIGNAL, "AbortSignal", et, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] abort/style install gap: {}.{}", g.interface, g.member);
    }
    // `AbortController` has no WebIDL parent: `Object.getPrototypeOf(AbortController)` is
    // `Function.prototype` and its prototype's is `Object.prototype` (measured), which is what
    // `parent: None` gives.
    super::rows::build_newable(scope, global, &ABORTCONTROLLER, cb_abort_controller_ctor, None, None, ledger);
    // `TaskSignal : AbortSignal` and `TaskController : AbortController`, after their
    // parents so both WebIDL splices find them. They were registry shells whose instances failed every
    // inherited member's brand check.
    use crate::core::dom::abort_controller::{cb_task_controller_ctor, TASKCONTROLLER, TASKSIGNAL};
    let asig = parent_of(scope, "AbortSignal");
    let built = installer::create_interface(scope, global, &TASKSIGNAL, "TaskSignal", asig, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] abort/style install gap: {}.{}", g.interface, g.member);
    }
    super::rows::build_newable(scope, global, &TASKCONTROLLER, cb_task_controller_ctor, Some("AbortController"), None, ledger);
}

/// This lane's ten interfaces in the WINDOW. Called from `korvus-runtime/src/lib.rs` at context
/// creation, beside [`super::install_window_indexeddb`] -- AFTER the whole ESM layer, so it is the last
/// writer of these ten prototypes and nothing re-orders them behind it. The interfaces left both
/// generated registries with the tables, so this is their only source. (They left
/// `generated_order.rs` too, back when there was one; that second table is deleted.)
pub fn install_window_abort_style<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::css::css_style_declaration::CSSSTYLEDECLARATION;
    use crate::core::dom::abstract_range::ABSTRACTRANGE;
    use crate::core::fileapi::blob::{cb_blob_ctor, BLOB};
    use crate::core::fileapi::file::{cb_file_ctor, FILE};
    use crate::core::html::image_data::{cb_image_data_ctor, IMAGEDATA};
    use crate::modules::fetch::request::{cb_request_ctor, REQUEST};
    use crate::modules::fetch::response::{cb_response_ctor, RESPONSE};
    use crate::modules::notifications::notification::{cb_notification_ctor, NOTIFICATION};

    let mut ledger = installer::take_ledger(scope);
    install_dom_exception_interface(scope, global, &mut ledger);
    install_abort_signal_interface(scope, global, &mut ledger);
    // The one interface of this lane with no class behind it: interface object AND prototype are the
    // installer's. `element::cb_el_style` and `css_style_declaration::computed_style_decl` read the
    // prototype off the global at call time (the second was `op_computed_style_decl` until
    // that op was deleted), so every `el.style` / `getComputedStyle(el)` follows this by construction.
    let built = installer::create_interface(scope, global, &CSSSTYLEDECLARATION, "CSSStyleDeclaration", None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] abort/style install gap: {}.{}", g.interface, g.member);
    }
    // `Blob` and `File` are NATIVE constructors now, so their interface objects AND
    // prototypes are the table's rather than a JS class's. Parent before child, so `File`'s WebIDL
    // splice (prototype AND constructor -- Chrome has `Object.getPrototypeOf(File) === Blob`) finds
    // the target it needs.
    super::rows::build_newable(scope, global, &BLOB, cb_blob_ctor, None, None, &mut ledger);
    super::rows::build_newable(scope, global, &FILE, cb_file_ctor, Some("Blob"), None, &mut ledger);
    // `Response` and `Request` are NATIVE constructors now -- `shared-globals.js`'s
    // `class Response` and `host.js`'s `class Request` are gone -- so their interface objects and
    // prototypes are both the table's. Neither inherits anything (`Object.getPrototypeOf(Response)` is
    // `Function.prototype`, measured).
    super::rows::build_newable(scope, global, &RESPONSE, cb_response_ctor, None, None, &mut ledger);
    super::rows::build_newable(scope, global, &REQUEST, cb_request_ctor, None, None, &mut ledger);
    // `ImageData`, the last of `host.js`'s slot-backed classes.
    super::rows::build_newable(scope, global, &IMAGEDATA, cb_image_data_ctor, None, None, &mut ledger);
    // ...and the two GLOBALS this lane's types serve.
    //
    // `fetch` AFTER `Request`/`Response`, because its body mints a `Response` off the global, and
    // after `postshells` (the first statement of this whole block), which is where the `WINQ` arity
    // pass used to run: `fetch` is a promise-returning operation and every one of its input failures is
    // a REJECTION, which a generic arity guard cannot express, so it had to be installed behind the
    // pass AND stamp itself out of it. `WINQ` is deleted; the ordering is kept because the rest
    // of this block's reasons (a `Response` minted off the global) are unchanged.
    //
    // `structuredClone` has to move WITH `ImageData` above -- see `modules/structured_clone.rs`: a JS
    // clone builds its copy with `Object.create(getPrototypeOf(v))`, which carries the prototype and
    // not the BRAND, so every branded row on the copy threw. Nothing in JS can stamp a brand, and
    // nothing in JS should be able to.
    crate::modules::fetch::fetch_global::install_fetch(scope, global);
    crate::modules::structured_clone::install_structured_clone(scope, global);
    // ...and the last two globals `shared-globals.js` owned, which go with the file.
    crate::modules::base64::install_base64(scope, global);
    crate::modules::console_ns::install_console(scope, global, true);
    // `Cache` and `CacheStorage` are `interface!` tables -- they left the generated
    // registry with the tables, so this is their only source in this realm. The `caches` global was
    // created by the registry walk at its capture position; this call gives it its
    // prototype, which is the first moment `CacheStorage.prototype` exists.
    crate::modules::cache_storage::cache::install_cache_api(scope, global, true);
    // `Notification` has no class behind it any more (`host.js`'s
    // `class Notification extends EventTarget`): its interface object is a NATIVE constructor and its
    // prototype is built here, spliced onto the real `EventTarget.prototype`.
    super::rows::build_newable(scope, global, &NOTIFICATION, cb_notification_ctor, Some("EventTarget"), None, &mut ledger);
    // `AbstractRange` is CREATED here now, not taken over. It was `host.js`'s
    // `class AbstractRange { get collapsed() { … } }` and this call was `take_prototype`: the table
    // owned the five rows, the JS class owned the IDENTITY -- so `String(AbstractRange)` printed JS
    // source and `new AbstractRange()` CONSTRUCTED one, where Chrome answers
    // `TypeError: Failed to construct 'AbstractRange': Illegal constructor` (measured with
    // `google-chrome --headless=new`). A `[interface, …]` table is exactly the non-newable interface
    // object `create_interface` builds, so there is nothing left to take a prototype FROM. Its two
    // children (`Range`, `StaticRange`) are built by `install_window_ranges`, later in the same block.
    let built = installer::create_interface(scope, global, &ABSTRACTRANGE, "AbstractRange", None, None, installer::Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] abstract range install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
    rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
    // ...and the HAND TABLES that named a parent of this lane's before it existed. W4.b built this
    // mechanism for `Event` and it is `DOMException`'s for exactly the same reason: lane L17's
    // `GPUPipelineError` is `build_newable(..., Some("DOMException"), ...)` during the webapi
    // webapi bootstrap (from `wire_canvas_oracle`), which is the first statement of this block, so
    // its edge is REMEMBERED rather than dropped and spliced here. `rechain_orphans` cannot reach it:
    // that sweep walks the generated REGISTRY and `GPUPipelineError` is a hand table that left it.
    // Found by `webgpu_live` failing on `p instanceof DOMException` -- the third failure mode of a
    // table cutover, caught by the gate that was written for the last one.
    super::rows::drain_pending_chains(scope, global);
}


/// The eight of this lane's interfaces the WORKER capture lists, from the SAME tables (invariant I8),
/// called from `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_crypto_xhr`].
///
/// `CSSStyleDeclaration` and `AbstractRange` are absent: `worker.surface.json` has neither, and the
/// bundle is the denominator. Of the eight, only `Response` (shared-globals.js) and `PerformanceEntry`
/// (worker-host.js) have a class in this realm; the other six had shape-only worker SHELLS, which the
/// tables replace outright -- so those six are `create_interface`'s here where the window takes an
/// existing prototype. `Notification` walks the worker selector, because `requestPermission` is a
/// `[Exposed=Window]` static the worker capture does not record.
pub fn install_worker_abort_style<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::fileapi::blob::{cb_blob_ctor, BLOB};
    use crate::core::fileapi::file::{cb_file_ctor, FILE};
    use crate::core::html::image_data::{cb_image_data_ctor, IMAGEDATA};
    use crate::modules::fetch::request::{cb_request_ctor, REQUEST};
    use crate::modules::fetch::response::{cb_response_ctor, RESPONSE};
    use crate::modules::notifications::notification::{cb_notification_ctor, NOTIFICATION};

    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    // The SAME table the window builds (invariant I8). The worker's `DOMException` was a SECOND,
    // different JS class in `worker-host.js`: three members, no legacy constants, and `name`/`code`
    // written as OWN properties on every instance where the worker capture records prototype
    // accessors. One definition now, and the worker gains the 25 constants and the `code` accessor
    // without a line being written for it.
    install_dom_exception_interface(scope, global, &mut ledger);
    // (A generic `build` closure over `installer::create_interface` stood here, with
    // `let et = parent_of(scope, "EventTarget")` feeding it and a `let _ = et;` at the end of the
    // function silencing the binding it no longer reached. Nothing called the closure: every
    // interface below is built by a named helper -- `build_newable`, `install_dom_exception_interface`,
    // `install_abort_signal_interface`, `install_cache_api` -- each of which reports its own gaps.
    // All three lines deleted.)
    // The SAME native constructors the window builds. `create_interface`'s `Model::Value`
    // placeholder used to make `new Blob(["a"]).size` `undefined` in a worker -- the interface existed,
    // the constructor was a no-op, and every row read a slot nothing had written.
    super::rows::build_newable(scope, global, &BLOB, cb_blob_ctor, None, None, &mut ledger);
    super::rows::build_newable(scope, global, &FILE, cb_file_ctor, Some("Blob"), None, &mut ledger);
    // The SAME shape the window builds -- the non-newable `AbortSignal` interface object
    // spliced onto `EventTarget` both ways, and `AbortController`'s native constructor. The worker used
    // to get `create_interface`'s `Model::Value` no-op here, so `new AbortController().signal` in a
    // worker was `undefined`: testharness.js's `fetch` cases could not abort at all.
    install_abort_signal_interface(scope, global, &mut ledger);
    super::rows::build_newable(scope, global, &REQUEST, cb_request_ctor, None, None, &mut ledger);
    // The worker's `Notification` is the SAME native constructor as the window's, walking the worker
    // selector (`requestPermission` is `[Exposed=Window]`). It used to be `create_interface`'s no-op
    // placeholder constructor, so `new Notification("t")` in a worker built an empty object.
    // NO HAND SELECTOR ANY MORE. The realm's member list used to be written down
    // here, copied out of one capture; `install::installer::join` reads it off the ACTIVE
    // capture instead, so the list is derived and a bundle swap moves it.
    super::rows::build_newable(scope, global, &NOTIFICATION, cb_notification_ctor, Some("EventTarget"), None, &mut ledger);
    super::rows::build_newable(scope, global, &RESPONSE, cb_response_ctor, None, None, &mut ledger);
    // `ImageData` is `[Exposed=(Window,Worker)]` -- the worker capture lists it -- and it is the SAME
    // table with the same native constructor here.
    super::rows::build_newable(scope, global, &IMAGEDATA, cb_image_data_ctor, None, None, &mut ledger);
    // `fetch`, `structuredClone`, `atob` and `btoa` ARE `WorkerGlobalScope` ROWS, and
    // therefore NOT INSTALLED HERE AT ALL. The bundle's worker surface records all four as
    // enumerable+configurable METHODS of `WorkerGlobalScope` -- `Object.getOwnPropertyNames(self)` in
    // a real worker does not contain them -- and the worker global INHERITS them through
    // `DedicatedWorkerGlobalScope.prototype -> WorkerGlobalScope.prototype`.
    //
    // This file used to mount the four onto that prototype itself, over the top of the registry
    // shell's four stubs: the right holder, the right bodies, and TWO WRITERS for every one of the
    // four members. `core/workers/worker_global_scope.rs` names the same four Rust bodies from its own
    // rows, so there is one writer again and the install order stops mattering.
    //
    // THE CACHE API is the other half of that move: `Cache` and `CacheStorage` are tables now, and
    // this is where both realms build them. In the worker it also publishes the `caches` singleton the
    // `WorkerGlobalScope` row exposes.
    crate::modules::cache_storage::cache::install_cache_api(scope, global, false);
    // `console` STAYS an own global: the capture lists it in the worker's `globals`, not on the
    // prototype. The bundle is the denominator, per member.
    crate::modules::console_ns::install_console(scope, global, false);
    installer::put_ledger(scope, ledger);
    rechain_orphans(scope, global, crate::install::generated_registry::WORKER_ORDER);
    // The worker's half of the hand-table splice above (`GPUPipelineError` is `[Exposed=(Window,
    // Worker)]`, and `install_worker_webgpu` builds it during this realm's ESM too).
    super::rows::drain_pending_chains(scope, global);
}
