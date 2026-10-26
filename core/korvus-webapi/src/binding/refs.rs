//! THE NATIVE POINTERS THIS BINARY HANDS V8 (the context snapshot, slice 1).
//!
//! A snapshot of a realm can carry a pointer into this binary -- a callback, an accessor, an interceptor, a static
//! `v8::External` -- only as an index into the EXTERNAL-REFERENCE table its isolate was made with, on the side that
//! writes the snapshot and on every side that reads it, in the same order; and V8 will not write a snapshot holding a
//! pointer it does not know. So every site that gives V8 such a pointer passes it through here first: [`f`] (a
//! `FunctionCallback`), [`g`] (an accessor getter), [`c`] (any C callback already converted, `builder_raw` and the
//! interceptors' `*_raw`), [`ext`] (an `External`'s pointer). While a run RECORDS ([`start_recording`]) each one is
//! kept as its offset from the binary's anchor -- the identity a blueprint already names a callback by
//! (`templates::put_cb`) -- in first-seen order, so a later process of the SAME binary (its build id) rebuilds the
//! table from [`offsets`]. A pointer outside the binary (a heap `External`) is not one this table can hold: it is
//! counted ([`foreign`]) so a snapshot run can refuse.
//!
//! Every other run pays one relaxed load per site: nothing is recorded unless a run asked.
//!
//! The gate's lint (`scripts/`, slice 1) fails a new site that hands V8 a pointer without one of these.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use deno_core::v8;

static RECORDING: AtomicBool = AtomicBool::new(false);
static FOREIGN: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Seen {
    set: std::collections::HashSet<i64>,
    order: Vec<i64>,
    /// where a pointer OUTSIDE the binary was handed to V8 ([`ext`]'s caller): what a snapshot run names when it refuses
    foreign_sites: std::collections::BTreeSet<String>,
    /// where each recorded pointer was first handed over ([`site_of`])
    sites: std::collections::HashMap<i64, &'static std::panic::Location<'static>>,
}

fn seen() -> &'static Mutex<Seen> {
    static S: OnceLock<Mutex<Seen>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Record every pointer handed to V8 from now on (the run whose list a snapshot will be made with).
pub fn start_recording() {
    RECORDING.store(true, Ordering::Relaxed);
}

/// Whether this run records.
pub fn recording() -> bool {
    RECORDING.load(Ordering::Relaxed)
}

/// The recorded pointers, as offsets from the binary's anchor, in first-seen order.
pub fn offsets() -> Vec<i64> {
    seen().lock().map(|s| s.order.clone()).unwrap_or_default()
}

/// The pointers `offsets` (a recorded table, [`offsets`]) name in THIS process: the same binary, whose anchor moved
/// with it. `None` if one falls outside the binary (a table of another build).
pub fn pointers(offsets: &[i64]) -> Option<Vec<usize>> {
    let a = crate::install::templates::anchor() as i64;
    offsets
        .iter()
        .map(|o| {
            let p = (a + o) as usize;
            crate::install::templates::in_image(p).then_some(p)
        })
        .collect()
}

/// How many pointers outside the binary were handed to V8 while recording: a snapshot of such a heap cannot be made.
pub fn foreign() -> usize {
    FOREIGN.load(Ordering::Relaxed)
}

/// Where those were handed over (`file:line` of each [`ext`] call that gave one), for the refusal's message.
pub fn foreign_sites() -> Vec<String> {
    seen().lock().map(|s| s.foreign_sites.iter().cloned().collect()).unwrap_or_default()
}

#[track_caller]
fn note(p: usize) {
    if !crate::install::templates::in_image(p) {
        FOREIGN.fetch_add(1, Ordering::Relaxed);
        let at = std::panic::Location::caller();
        if let Ok(mut s) = seen().lock() {
            s.foreign_sites.insert(format!("{}:{}", at.file(), at.line()));
        }
        return;
    }
    let off = p as i64 - crate::install::templates::anchor() as i64;
    if let Ok(mut s) = seen().lock() {
        if s.set.insert(off) {
            s.order.push(off);
            s.sites.insert(off, std::panic::Location::caller());
        }
    }
}

/// Where the recorded pointer at `off` ([`offsets`]) was first handed to V8: `file:line` of its `refs::` call.
pub fn site_of(off: i64) -> Option<String> {
    let s = seen().lock().ok()?;
    s.sites.get(&off).map(|l| format!("{}:{}", l.file(), l.line()))
}

/// Any C callback, already converted (`builder_raw`, the interceptors' `*_raw`): noted while recording, returned as is.
#[inline]
#[track_caller]
pub fn c<C: Copy>(cb: C) -> C {
    // `C` must be an already-converted `extern "C" fn` POINTER (usize-sized): the `transmute_copy` below reads
    // exactly that many bytes from it. A zero-sized fn ITEM slips past inference here -- it did, at
    // `templates.rs` (goal 1022), and because a release build strips `debug_assert!` the UB surfaced only as a
    // snapshot-maker panic. Check the size at COMPILE time, for every instantiation, so a fn item is a build
    // error here (coerce it, e.g. `as v8::FunctionCallback`), never a run-time read past the end of a 0-byte value.
    struct PtrSized<T>(std::marker::PhantomData<T>);
    impl<T> PtrSized<T> {
        const CHECK: () = assert!(
            std::mem::size_of::<T>() == std::mem::size_of::<usize>(),
            "refs::c needs an extern \"C\" fn POINTER (usize-sized), not a fn item -- coerce it, e.g. `as v8::FunctionCallback`",
        );
    }
    let () = PtrSized::<C>::CHECK;
    if RECORDING.load(Ordering::Relaxed) {
        // SAFETY: the compile-time assert above proves `C` is usize-sized -- an `extern "C" fn` pointer.
        note(unsafe { std::mem::transmute_copy::<C, usize>(&cb) });
    }
    cb
}

/// A `FunctionCallback` given as a Rust fn (`Function::builder`, `FunctionTemplate::builder`): the C shim V8 receives,
/// noted while recording; the fn returned as is.
#[inline]
#[track_caller]
pub fn f<F: v8::MapFnTo<v8::FunctionCallback> + Copy>(cb: F) -> F {
    if RECORDING.load(Ordering::Relaxed) {
        note(cb.map_fn_to() as usize);
    }
    cb
}

/// An accessor getter given as a Rust fn (`set_lazy_data_property_with_data`, `set_accessor`), as [`f`].
#[inline]
#[track_caller]
pub fn g<F: v8::MapFnTo<v8::AccessorNameGetterCallback> + Copy>(cb: F) -> F {
    if RECORDING.load(Ordering::Relaxed) {
        note(cb.map_fn_to() as usize);
    }
    cb
}

/// An `External`'s pointer: noted while recording (a static inside the binary), returned as is. A heap pointer is
/// counted, with this call's site ([`foreign_sites`]).
#[inline]
#[track_caller]
pub fn ext<T>(p: *mut T) -> *mut T {
    if RECORDING.load(Ordering::Relaxed) {
        note(p as usize);
    }
    p
}
