//! `broadcastchannel` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod broadcast_channel;
pub(crate) use broadcast_channel::BcRegistry;
pub use broadcast_channel::BcForward; // re-exported by lib.rs for korvus-runtime (worker.rs)
pub(crate) mod serialize;
