//! `crypto` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod crypto;
pub(crate) mod subtle_crypto;
pub(crate) mod asym_keys;
pub(crate) mod crypto_key;
pub(crate) mod kdf;
pub(crate) mod shared;
