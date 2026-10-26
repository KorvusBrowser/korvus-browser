//! `modules/indexeddb/idb_cursor.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::idb::*;      // IndexedDB backend (Key/KeyRange/…)
use super::*;            // sibling Idb* interfaces (re-exported by mod.rs)
use crate::modules::indexeddb::backend::{IdbBackend, IdbPending, idb_do_delete, idb_do_write, idb_g, idb_l, idb_make_request, idb_pump, idb_recv, idb_structured_clone, idb_throw, idb_throw_type, js_to_key, key_to_js};

#[repr(C)]
#[derive(CppgcBase)]
pub struct IdbCursor {
    pub(crate) req: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) source: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) tx: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pub(crate) entries: std::cell::RefCell<Vec<(Key, Key, v8::Global<v8::Value>)>>, // (key, primaryKey, value)
    pub(crate) pos: std::cell::Cell<i64>,
    /// The spec's "got value flag": true only between the delivery of a record to this cursor's
    /// request and the next `continue`/`continuePrimaryKey`/`advance`. It is the ONE piece of state
    /// that distinguishes "the cursor is being iterated" from "the cursor is sitting on a record",
    /// and without it `continue()` called twice in one `onsuccess` silently iterated twice and
    /// `update()`/`delete()` on an in-flight cursor ran against a stale position. Set in
    /// `cb_idb_drain` (backend.rs), cleared by the three iterating members below.
    pub(crate) got_value: std::cell::Cell<bool>,
    pub(crate) direction: String,
    pub(crate) key_only: bool,
    pub(crate) db_name: String,
    pub(crate) store_name: String,
}

pub(crate) trait IdbCursorBehavior {
   fn construct() -> IdbCursor where Self: Sized { stub!("IdbCursor::constructor") }
   fn direction(&self) -> String { stub!("IdbCursor::direction") }
}

#[webapi(base, own, name = "IDBCursor")]
impl IdbCursor {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<IdbCursor, ::deno_error::JsErrorBox> { dispatch!("IdbCursor::constructor", <IdbCursor as IdbCursorBehavior>::construct()) }
    #[getter]
    #[string]
    fn direction(&self) -> Result<String, ::deno_error::JsErrorBox> { dispatch!("IdbCursor::direction", <IdbCursor as IdbCursorBehavior>::direction(self)) }
}

impl IdbCursorBehavior for IdbCursor {
    fn construct() -> IdbCursor { IdbCursor { req: std::cell::RefCell::new(None), source: std::cell::RefCell::new(None), tx: std::cell::RefCell::new(None), entries: std::cell::RefCell::new(Vec::new()), pos: std::cell::Cell::new(0), got_value: std::cell::Cell::new(false), direction: "next".to_string(), key_only: false, db_name: String::new(), store_name: String::new() } }
    fn direction(&self) -> String { self.direction.clone() }
}

/// Common tail of continue/advance: after `pos` is updated, re-fire the cursor's request with the
/// cursor (or null past the end).
fn idb_cursor_step(scope: &mut v8::PinScope, ch: &IdbCursor, this: v8::Local<v8::Value>) {
    let tx_g = match ch.tx.borrow().clone() { Some(t) => t, None => return };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let valid = { let p = ch.pos.get(); p >= 0 && (p as usize) < ch.entries.borrow().len() };
    let result_val: v8::Local<v8::Value> = if valid { this } else { v8::null(scope).into() };
    let req_g = match ch.req.borrow().clone() { Some(r) => r, None => return };
    let pending = IdbPending { req: req_g, result: Some(idb_g(scope, result_val)), error: None };
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(pending);
    idb_pump(scope, tx_val);
}
// ── the table's bodies (modules/indexeddb/tables.rs) ────────────────────────────────────────────
//
// `direction` was an `#[op2] #[getter]` member of the class template above until the W5.1 L7 cutover;
// an instance now carries the `IDBCursor` (or `IDBCursorWithValue`) TABLE's prototype, so this raw
// callback is the row's body.
pub(crate) fn cb_cursor_direction(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ch = idb_recv!(scope, args.this().into(), IdbCursor);
    let d = unsafe { ch.as_ref() }.direction.clone();
    let v = crate::binding::protos::str_val(scope, &d);
    rv.set(v);
}

/// The index name behind a cursor whose source is an `IDBIndex`, else `None` (a store cursor).
fn idb_cursor_index_name(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<String> {
    let ch = deno_core::cppgc::try_unwrap_cppgc_object::<IdbCursor>(scope, this)?;
    let src = unsafe { ch.as_ref() }.source.borrow().clone()?;
    let v = idb_l(scope, &src);
    let ixh = deno_core::cppgc::try_unwrap_cppgc_object::<crate::modules::indexeddb::idb_index::IdbIndex>(scope, v)?;
    let n = unsafe { ixh.as_ref() }.index_name.borrow().clone();
    Some(n)
}

/// The prologue EVERY iterating or mutating `IDBCursor` member shares, in the order IndexedDB fixes:
///
///   1. transaction not active          -> TransactionInactiveError
///   2. read only                       -> ReadOnlyError            (`write`: update/delete only)
///   3. source / effective object store has been deleted -> InvalidStateError
///
/// The "got value flag" step (4) is `idb_cursor_got_value` and is DELIBERATELY NOT FOLDED IN HERE:
/// `continuePrimaryKey` puts two InvalidAccessError checks of its own between step 3 and it, so a
/// single fused prologue would have to be un-fused for that one member -- and one algorithm split
/// into per-member subsets is the defect this whole goal exists to remove.
///
/// `None` = an exception is already thrown (or the cursor has no transaction) and the caller must
/// return at once. `Some` is the transaction value, which every caller needs next anyway.
fn idb_cursor_prologue<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, write: bool) -> Option<v8::Local<'s, v8::Value>> {
    let ch = deno_core::cppgc::try_unwrap_cppgc_object::<IdbCursor>(scope, this)?;
    let tx_g = unsafe { ch.as_ref() }.tx.borrow().clone()?;
    let db_name = unsafe { ch.as_ref() }.db_name.clone();
    let store_name = unsafe { ch.as_ref() }.store_name.clone();
    let tx_val = idb_l(scope, &tx_g);
    let txh = deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val)?;
    if unsafe { txh.as_ref() }.finished.get() || !unsafe { txh.as_ref() }.active.get() {
        idb_throw(scope, "TransactionInactiveError", "the transaction is not active");
        return None;
    }
    // Step 2 sits AHEAD of the got-value flag, which is what
    // `idbcursor-update-exception-order.any.js` #1 and `idbcursor-delete-exception-order.any.js` #1
    // assert by name: a readonly cursor mid-iteration reports ReadOnlyError, not InvalidStateError.
    if write && unsafe { txh.as_ref() }.mode == "readonly" {
        idb_throw(scope, "ReadOnlyError", "the transaction is read-only");
        return None;
    }
    let index_name = idb_cursor_index_name(scope, this);
    if crate::modules::indexeddb::idb_object_store::idb_source_deleted(scope, &db_name, &store_name, index_name.as_deref()) {
        let what = if index_name.is_some() { "the index has been deleted" } else { "the object store has been deleted" };
        idb_throw(scope, "InvalidStateError", what);
        return None;
    }
    Some(tx_val)
}

/// Step 4 of the prologue above: "if this cursor's got value flag is false, throw InvalidStateError".
/// `false` here means the exception is thrown and the caller must return.
fn idb_cursor_got_value(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    let ok = deno_core::cppgc::try_unwrap_cppgc_object::<IdbCursor>(scope, this)
        .map(|ch| unsafe { ch.as_ref() }.got_value.get())
        .unwrap_or(false);
    if !ok {
        idb_throw(scope, "InvalidStateError", "the cursor is being iterated or has iterated past its last record");
    }
    ok
}

pub(crate) fn cb_cursor_continue(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this().into();
    let ch = idb_recv!(scope, this, IdbCursor);
    if idb_cursor_prologue(scope, this, false).is_none() { return; }
    if !idb_cursor_got_value(scope, this) { return; }
    let cur_pos = unsafe { ch.as_ref() }.pos.get();
    let key_arg = args.get(0);
    let new_pos = if key_arg.is_undefined() || key_arg.is_null() {
        cur_pos + 1
    } else {
        let target = match js_to_key(scope, key_arg) { Some(k) => k, None => { idb_throw(scope, "DataError", "invalid key"); return; } };
        let prev = unsafe { ch.as_ref() }.direction.starts_with("prev");
        // "If key is less than or equal to this cursor's position and this cursor's direction is
        // next or nextunique, throw a DataError" -- and the mirror for prev/prevunique. THE TARGET
        // WAS NEVER COMPARED WITH THE CURRENT POSITION: the scan below starts at `cur_pos + 1`, so a
        // key behind the cursor simply found nothing and iterated to the end, which is why
        // `cursor.continue(-1)` and `cursor.continue(records[0].pKey)` did not throw.
        let backwards = {
            let e = unsafe { ch.as_ref() }.entries.borrow();
            match e.get(cur_pos as usize) {
                Some(ent) => {
                    let c = target.cmp(&ent.0);
                    if prev { c != std::cmp::Ordering::Less } else { c != std::cmp::Ordering::Greater }
                }
                None => false,
            }
        };
        if backwards { idb_throw(scope, "DataError", "the key is not past the cursor's current position"); return; }
        let e = unsafe { ch.as_ref() }.entries.borrow();
        let mut np = cur_pos + 1;
        while (np as usize) < e.len() {
            let c = e[np as usize].0.cmp(&target);
            let ok = if prev { c != std::cmp::Ordering::Greater } else { c != std::cmp::Ordering::Less };
            if ok { break; }
            np += 1;
        }
        np
    };
    unsafe { ch.as_ref() }.got_value.set(false);
    unsafe { ch.as_ref() }.pos.set(new_pos);
    let cref = unsafe { ch.as_ref() };
    idb_cursor_step(scope, cref, this);
}

// `continuePrimaryKey(key, primaryKey)` -- in the capture, absent here. It is `continue()` with a
// two-part target: the entries already carry `(key, primaryKey, value)`, so the scan is the same walk
// with a lexicographic compare on the pair instead of on the key alone.
//
// The spec's two guards are the point of the member, so both are here: it is INDEX-cursor-only and
// direction-only ("next"/"prev", not the unique variants), and both arguments are required. Recorded
// from Brave: `.length` is 2.
pub(crate) fn cb_cursor_continue_primary_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this().into();
    let ch = idb_recv!(scope, this, IdbCursor);
    if !crate::binding::protos::require_args(scope, &args, 2, "IDBCursor", "continuePrimaryKey") { return; }
    if idb_cursor_prologue(scope, this, false).is_none() { return; }
    // "If the cursor's source is not an index, throw an InvalidAccessError." A store cursor's key IS
    // its primary key, so the two-part target has no meaning there. BOTH InvalidAccessError steps sit
    // between "has been deleted" and "got value flag" -- the one place this member's order differs
    // from `continue`'s, and the reason the prologue above stops short of the got-value step.
    {
        let src = unsafe { ch.as_ref() }.source.borrow().clone();
        let is_index = match src {
            Some(g) => {
                let v = idb_l(scope, &g);
                deno_core::cppgc::try_unwrap_cppgc_object::<crate::modules::indexeddb::idb_index::IdbIndex>(scope, v).is_some()
            }
            None => false,
        };
        if !is_index { idb_throw(scope, "InvalidAccessError", "the cursor's source is not an index"); return; }
        let dir = unsafe { ch.as_ref() }.direction.clone();
        if dir != "next" && dir != "prev" { idb_throw(scope, "InvalidAccessError", "the cursor's direction must be next or prev"); return; }
    }
    if !idb_cursor_got_value(scope, this) { return; }
    let Some(target_key) = js_to_key(scope, args.get(0)) else { idb_throw(scope, "DataError", "invalid key"); return };
    let Some(target_pk) = js_to_key(scope, args.get(1)) else { idb_throw(scope, "DataError", "invalid primary key"); return };
    let cur_pos = unsafe { ch.as_ref() }.pos.get();
    let prev = unsafe { ch.as_ref() }.direction.starts_with("prev");
    // The same missing comparison `continue` had, on the PAIR: for "next", (key, primaryKey) must be
    // strictly past (position, object store position); for "prev", strictly before it.
    let backwards = {
        let e = unsafe { ch.as_ref() }.entries.borrow();
        match e.get(cur_pos as usize) {
            Some(ent) => {
                let c = target_key.cmp(&ent.0).then_with(|| target_pk.cmp(&ent.1));
                if prev { c != std::cmp::Ordering::Less } else { c != std::cmp::Ordering::Greater }
            }
            None => false,
        }
    };
    if backwards { idb_throw(scope, "DataError", "the key is not past the cursor's current position"); return; }
    let new_pos = {
        let e = unsafe { ch.as_ref() }.entries.borrow();
        let mut np = cur_pos + 1;
        while (np as usize) < e.len() {
            let ent = &e[np as usize];
            let c = ent.0.cmp(&target_key).then_with(|| ent.1.cmp(&target_pk));
            let ok = if prev { c != std::cmp::Ordering::Greater } else { c != std::cmp::Ordering::Less };
            if ok { break; }
            np += 1;
        }
        np
    };
    unsafe { ch.as_ref() }.got_value.set(false);
    unsafe { ch.as_ref() }.pos.set(new_pos);
    let cref = unsafe { ch.as_ref() };
    idb_cursor_step(scope, cref, this);
}
pub(crate) fn cb_cursor_advance(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this().into();
    let ch = idb_recv!(scope, this, IdbCursor);
    // `void advance([EnforceRange] unsigned long count)`, then "if count is 0, throw a TypeError".
    // This read `.number_value().unwrap_or(0.0)` and rejected on `count < 1.0` -- a comparison that
    // is FALSE for NaN and for +Infinity, so `advance({})`, `advance(undefined)`, `advance(NaN)` and
    // `advance(Infinity)` did not throw at all.
    let count = match crate::modules::indexeddb::backend::idb_enforce_range_u32(scope, args.get(0)) { Ok(c) => c, Err(()) => return };
    if count == 0 { idb_throw_type(scope, "advance count must be >= 1"); return; }
    if idb_cursor_prologue(scope, this, false).is_none() { return; }
    if !idb_cursor_got_value(scope, this) { return; }
    let cur_pos = unsafe { ch.as_ref() }.pos.get();
    unsafe { ch.as_ref() }.got_value.set(false);
    unsafe { ch.as_ref() }.pos.set(cur_pos + count as i64);
    let cref = unsafe { ch.as_ref() };
    idb_cursor_step(scope, cref, this);
}
/// Current (db, store, primaryKey) of a live cursor position, or None if exhausted.
fn idb_cursor_here(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<(String, String, v8::Global<v8::Value>, Key)> {
    let ch = deno_core::cppgc::try_unwrap_cppgc_object::<IdbCursor>(scope, this)?;
    let r = unsafe { ch.as_ref() };
    let pos = r.pos.get();
    let e = r.entries.borrow();
    if pos < 0 || (pos as usize) >= e.len() { return None; }
    let tx = r.tx.borrow().clone()?;
    Some((r.db_name.clone(), r.store_name.clone(), tx, e[pos as usize].1.clone()))
}
pub(crate) fn cb_cursor_update(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this().into();
    let ch = idb_recv!(scope, this, IdbCursor);
    // THE ORDER WAS INSIDE OUT. `idb_cursor_here` ran first, so an exhausted-or-iterating cursor on a
    // READONLY transaction answered InvalidStateError where every browser answers ReadOnlyError
    // (`idbcursor-update-exception-order.any.js`, "#1"). The prologue is the spec's order and is
    // shared with continue/advance/delete.
    if idb_cursor_prologue(scope, this, true).is_none() { return; }
    if !idb_cursor_got_value(scope, this) { return; }
    // "If this cursor's key only flag is true, throw an InvalidStateError." Missing entirely:
    // `openKeyCursor()` handed back a cursor whose `update()` wrote a record.
    if unsafe { ch.as_ref() }.key_only { idb_throw(scope, "InvalidStateError", "the cursor is a key-only cursor"); return; }
    let (db_name, store_name, tx_g, pk) = match idb_cursor_here(scope, this) { Some(x) => x, None => { idb_throw(scope, "InvalidStateError", "cursor is not positioned"); return; } };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let store_val = idb_structured_clone(scope, args.get(0));
    let source = idb_g(scope, this);
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);
    let write_res = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        idb_do_write(scope, st.borrow_mut::<IdbBackend>(), &db_name, &store_name, pk.clone(), store_val, false)
    };
    let pending = match write_res {
        Ok(undo) => { unsafe { txh.as_ref() }.undo.borrow_mut().push(undo); let kv = key_to_js(scope, &pk); IdbPending { req: idb_g(scope, req), result: Some(idb_g(scope, kv)), error: None } }
        Err((n, m)) => IdbPending { req: idb_g(scope, req), result: None, error: Some((n, m)) },
    };
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(pending);
    idb_pump(scope, tx_val);
    rv.set(req);
}
pub(crate) fn cb_cursor_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this().into();
    let ch = idb_recv!(scope, this, IdbCursor);
    // Same inversion as `update` above, same fix: the shared prologue, then got value, then key only.
    if idb_cursor_prologue(scope, this, true).is_none() { return; }
    if !idb_cursor_got_value(scope, this) { return; }
    if unsafe { ch.as_ref() }.key_only { idb_throw(scope, "InvalidStateError", "the cursor is a key-only cursor"); return; }
    let (db_name, store_name, tx_g, pk) = match idb_cursor_here(scope, this) { Some(x) => x, None => { idb_throw(scope, "InvalidStateError", "cursor is not positioned"); return; } };
    let tx_val = idb_l(scope, &tx_g);
    let txh = match deno_core::cppgc::try_unwrap_cppgc_object::<IdbTransaction>(scope, tx_val) { Some(t) => t, None => return };
    let undos = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        idb_do_delete(scope, st.borrow_mut::<IdbBackend>(), &db_name, &store_name, &KeyRange::only(pk))
    };
    for u in undos { unsafe { txh.as_ref() }.undo.borrow_mut().push(u); }
    let source = idb_g(scope, this);
    let txg = idb_g(scope, tx_val);
    let req = idb_make_request(scope, Some(source), Some(txg), false);
    unsafe { txh.as_ref() }.pending.borrow_mut().push_back(IdbPending { req: idb_g(scope, req), result: None, error: None });
    idb_pump(scope, tx_val);
    rv.set(req);
}
/// One of `key` / `primaryKey` / `value` at the cursor's current position. The RECEIVER check is the
/// caller's (each of the three getters enters through `idb_recv!`), so `None` here means only
/// "exhausted", which reads as `undefined` -- Chrome's answer for a cursor past its last entry.
fn idb_cursor_entry_field<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, which: u8) -> Option<v8::Local<'s, v8::Value>> {
    let ch = deno_core::cppgc::try_unwrap_cppgc_object::<IdbCursor>(scope, this)?;
    let r = unsafe { ch.as_ref() };
    let pos = r.pos.get();
    let e = r.entries.borrow();
    if pos < 0 || (pos as usize) >= e.len() { return None; }
    let entry = &e[pos as usize];
    Some(match which {
        0 => key_to_js(scope, &entry.0),
        1 => key_to_js(scope, &entry.1),
        2 => if r.key_only { return None; } else { idb_l(scope, &entry.2) },
        _ => return None,
    })
}
pub(crate) fn cb_cursor_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbCursor);
    match idb_cursor_entry_field(scope, args.this().into(), 0) { Some(v) => rv.set(v), None => { let u = v8::undefined(scope); rv.set(u.into()); } }
}
pub(crate) fn cb_cursor_primary_key(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbCursor);
    match idb_cursor_entry_field(scope, args.this().into(), 1) { Some(v) => rv.set(v), None => { let u = v8::undefined(scope); rv.set(u.into()); } }
}
pub(crate) fn cb_cursor_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = idb_recv!(scope, args.this().into(), IdbCursor);
    match idb_cursor_entry_field(scope, args.this().into(), 2) { Some(v) => rv.set(v), None => { let u = v8::undefined(scope); rv.set(u.into()); } }
}
pub(crate) fn cb_cursor_source(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ch = idb_recv!(scope, args.this().into(), IdbCursor);
    let s = unsafe { ch.as_ref() }.source.borrow().clone();
    if let Some(s) = s { let v = idb_l(scope, &s); rv.set(v); }
}
pub(crate) fn cb_cursor_request(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ch = idb_recv!(scope, args.this().into(), IdbCursor);
    let r = unsafe { ch.as_ref() }.req.borrow().clone();
    if let Some(r) = r { let v = idb_l(scope, &r); rv.set(v); }
}

