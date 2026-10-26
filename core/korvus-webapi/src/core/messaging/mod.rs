//! `messaging` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod message_port;
/// The one `postMessage` transfer list, shared by `MessagePort` and both `Window` bodies.
pub(crate) mod transfer;
pub(crate) use message_port::PortRegistry;
pub use message_port::{PortForward, mp_register_remote}; // re-exported by lib.rs for korvus-runtime (worker.rs)
