//! `timing` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

/// The performance TIMELINE: `Performance` and the entry / L1-navigation-timing / `EventCounts`
/// interfaces around it.
pub(crate) mod performance;
/// `PerformanceResourceTiming`: the entries a realm's fetches add to its timeline.
pub(crate) mod resource_timing;
/// Long animation frames: `long-animation-frame` entries as Blink's `AnimationFrameTimingMonitor` makes them.
pub(crate) mod loaf;
pub(crate) mod server_timing; // PerformanceServerTiming and the Server-Timing header
pub(crate) mod performance_observer;
