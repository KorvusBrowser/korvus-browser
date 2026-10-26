//! `intersection_observer` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).
//!
//! `rendering` is the frame both geometry observers are computed in.

pub(crate) mod intersection_observer;
pub(crate) mod rendering;
pub(crate) use intersection_observer::{IntersectionObserver};
