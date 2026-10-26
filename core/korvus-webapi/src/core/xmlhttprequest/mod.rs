//! `xmlhttprequest` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod mime_sniff;
pub(crate) mod xml_http_request;
