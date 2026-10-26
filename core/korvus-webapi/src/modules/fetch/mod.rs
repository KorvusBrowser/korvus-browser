//! `fetch` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod headers;
pub(crate) mod form_data;
pub(crate) mod response;
/// The `fetch` GLOBAL -- one native function, with the Service-Worker interception as a
/// branch inside it rather than a second arrow that replaced the first.
pub mod fetch_global;
pub(crate) mod request;
/// The Body mixin `Response` and `Request` share -- a body is a stream of bytes.
pub(crate) mod body;
