//! `core/navigator/` -- the `Navigator` / `WorkerNavigator` interface and the objects it hands back.
//!
//! WHY IT MOVED HERE (invariant I9). The table was authored in `korvus-runtime`
//! (`src/navigator_table.rs`) because its scalar getters read the persona out of the runtime's own
//! `HostPersona` slot in `OpState` -- a runtime concept a Web API file could not name. It can now:
//! the persona is a SERVICE on the kernel `Context` (`spec::PersonaHandle`, provided by
//! `PersonaPlugin` in the window and, since this track, in the worker isolate too), and a service is
//! exactly the seam I9 asks a table body to use. So the Web API definition lives in the Web API
//! crate and the kernel provides the algorithm service, in both realms.
//!
//! WHERE each realm MOUNTS it is `install/live/navigator.rs`, like every other domain.

/// The device / capability singletons and the hidden-constructor objects.
/// Measured answers for the promise members pages ask first.
pub mod answers;
pub mod devices;
/// Credential Management without a credential store.
pub mod credentials;
/// APIs answered by the running document's focus / top-levelness.
pub mod context_apis;
/// `Geolocation.getCurrentPosition`, resolved from a loaded plugin (`plugin_host::geolocation`, C5).
pub mod geolocation;
/// `BatteryManager` and `navigator.getBattery()`: one manager and one promise per navigator.
pub mod battery_manager;
/// `navigator.keyboard`: `Keyboard` and `KeyboardLayoutMap`, the map from the persona.
pub mod keyboard;
/// The sixteen `Navigator` methods.
pub mod methods;
/// `navigator.storageBuckets` and its `StorageBucket`s.
pub mod storage_buckets;
/// The `Navigator` table, its persona-backed scalar getters, and the one lazy-object row body.
pub mod navigator;
/// `NetworkInformation` and `navigator.connection`, from the persona.
pub mod network_information;
/// `userAgentData`, `permissions` and `mediaDevices`.
pub mod persona_objects;
// `service_worker.rs` IS GONE. It held the ONE seam W4.a left open: `host.js`'s
// service-worker subsystem handed its container BUILDER over through `op_service_worker_source`, and
// this module called it on first read of `navigator.serviceWorker`. The container is a Rust mint on
// a table prototype now (`crate::core::workers::service_worker::build_container`), so there is no
// builder to hand over, no op, and no seam -- the `serviceWorker` row reaches the three tables the
// same way every other object-valued row reaches its interface.
