//! `indexeddb` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod idb_factory;
pub(crate) mod idb_database;
pub(crate) use idb_database::IdbDatabase;
pub(crate) mod idb_transaction;
pub(crate) use idb_transaction::IdbTransaction;
pub(crate) mod idb_object_store;
pub(crate) use idb_object_store::IdbObjectStore;
pub(crate) mod idb_index;
pub(crate) use idb_index::IdbIndex;
pub(crate) mod idb_cursor;
pub(crate) use idb_cursor::IdbCursor;
pub(crate) mod idb_key_range;
pub(crate) use idb_key_range::IdbKeyRange;
pub(crate) mod idb_request;
pub(crate) use idb_request::IdbRequest;
pub(crate) mod backend;
/// Committed databases kept past their realm and shared with the context's other pages.
pub(crate) mod sync;
/// The twelve IndexedDB interfaces as `interface!` tables -- the ONE definition site for
/// each (invariant I1), installed into both realms by THE installer (`install::live`).
pub(crate) mod tables;
