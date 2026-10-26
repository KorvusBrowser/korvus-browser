//! `workers` Web-API interfaces.
//!
//! `Worker` and `SharedWorker` as `interface!` tables with NATIVE
//! constructors, replacing `core/korvus-runtime/src/js/host.js`'s `class Worker extends EventTarget`
//! and `class SharedWorker extends EventTarget` — two of the three ESM-time `extends EventTarget`
//! declarations W4.b named as the reason `events.js` still exists.
//!
//! THE RECEIVE LOOPS LIVE HERE TOO, and their SHAPE is unchanged from the JS they replace: arm the
//! async op, UNREF its promise so a parked receive does not by itself keep the page event loop alive,
//! handle one frame, re-arm. What changed is that every step is a native function rather than an
//! `async` JS closure — see [`worker::arm_pump`].

/// The three ServiceWorker interfaces -- `ServiceWorker`, `ServiceWorkerRegistration` and
/// `ServiceWorkerContainer` -- closing the REMAINS W4.c named and deleting the two transitional ops.
pub(crate) mod service_worker;
/// What a ServiceWorker ISOLATE does at start -- the SW half of the `korvus-sw-fetch`
/// transport and the install -> activate lifecycle. Both were `worker.rs`'s `SW_PRELUDE` JS string.
pub mod service_worker_realm;
pub(crate) mod shared_worker;
pub(crate) mod worker;
pub(crate) mod worker_global_scope;
