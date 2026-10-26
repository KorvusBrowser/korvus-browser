//! `fileapi` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod file;
pub(crate) mod file_reader;
pub(crate) mod file_list;
pub(crate) use file_list::FileList;
pub(crate) mod blob;
pub(crate) mod opfs;
pub(crate) mod opfs_store;
pub(crate) mod opfs_timing;
