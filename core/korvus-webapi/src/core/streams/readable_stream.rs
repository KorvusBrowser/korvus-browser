//! `core/streams/readable_stream.rs` — extracted verbatim from bindings.rs by the file-move migration (STEP 1).
//!
//! `readable-stream-piping` (Track 2): added `locked`, `cancel`, `pipeTo`, `pipeThrough`, `tee`,
//! `values` -- the six members Brave 151 exposes that this file did not.
//!
//! The stream is LIVE in the three places it was a snapshot, and has the one state it
//! did not have. Every behaviour below was asked of `google-chrome` 150.0.7871.46 first:
//!
//! * **errored.** `controller.error(e)` did not exist (the controller was a plain two-key object), so a
//!   producer that failed left its reader waiting forever. Chrome: a parked `read()` REJECTS with `e`, a
//!   later `read()` rejects with `e` even when a chunk was queued before the error (the queue is
//!   dropped), `cancel()` rejects with `e`, and a pipe aborts its destination with `e`.
//! * **`pipeTo` forwards what arrives LATER.** It drained what was buffered and resolved, so
//!   `rs.pipeThrough(new TextDecoderStream())` over a stream whose bytes arrive after the pipe began
//!   read nothing. Chrome: `[[true, true], ["a", "b", "close"], false, false]` -- both locked while the
//!   pipe runs, every later chunk written, the sink closed, both unlocked when the source closes.
//! * **`tee` forwards what arrives LATER** to both branches, for the same reason.
//! * **`disturbed`**, the fact `Response.bodyUsed` is: set by the first `read()`/`cancel()`/pipe, never
//!   by `getReader()` alone (Chrome: `r.body.getReader(); r.bodyUsed` is `false`).
//!
//! What the model still does NOT have, stated so nobody reads it as done: a `pull()` source, a queuing
//! strategy (the high-water mark is WebIDL's default 1, counted per chunk), backpressure, and
//! `reader.closed`. Writes into a pipe's destination happen synchronously inside `enqueue`, where
//! Chrome performs them a microtask later; the order of chunks, closes and aborts is Chrome's.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{get_prop, rejected_promise, rejected_type_error, resolved_promise, set_prop, throw_type_error};
use crate::install::live::{getter_row, method_row, set_table_proto};
use crate::install::MemberDef;
use super::readable_stream_default_reader::ReadableStreamDefaultReader; // sibling (moved out of bindings)
use super::writable_stream::WritableStream;

/// Where a stream sends what reaches it once a pipe or a tee has taken it over.
pub(crate) enum Target {
    /// `pipeTo`: the destination `WritableStream`, the pipe's promise, and its two options.
    Pipe { dest: v8::Global<v8::Object>, resolver: v8::Global<v8::PromiseResolver>, prevent_close: bool, prevent_abort: bool },
    /// `tee`: one branch.
    Branch(v8::Global<v8::Object>),
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct ReadableStream {
    chunks: std::cell::RefCell<Vec<v8::Global<v8::Value>>>,
    /// `close()` was requested. With chunks still queued the stream is still readable; it reports done
    /// once the queue drains.
    done: std::cell::Cell<bool>,
    // `true` while a reader is outstanding (`getReader()`/`values()`) and not yet released. Read by
    // `locked`, written by `getReader`/`releaseLock`/`tee`/`values` -- never a stored fact nobody
    // updates (FORBIDDEN: "locked as a stored boolean nobody updates").
    pub(crate) locked: std::cell::Cell<bool>,
    // Parked read() resolvers (WHATWG `[[readRequests]]`): a `read()` on an EMPTY, not-yet-closed
    // stream stores its promise resolver here; `enqueue()` fulfils the oldest directly and `close()`/
    // `cancel()` settle them all as done. Data enqueued AFTER a read began (a Next.js RSC flight stream
    // fed from inline `__next_f` pushes is the canonical case) reaches the reader instead of the read
    // reporting an immediate `{done:true}`, which is what made React's flight client throw
    // "Connection closed."
    read_requests: std::cell::RefCell<Vec<v8::Global<v8::PromiseResolver>>>,
    /// WHATWG `[[storedError]]` once the stream is errored.
    errored: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    /// WHATWG `[[disturbed]]`: a read, a cancel or a pipe has touched this stream.
    disturbed: std::cell::Cell<bool>,
    /// A pipe or tee that owns this stream's future chunks.
    targets: std::cell::RefCell<Vec<Target>>,
    /// The reader currently locked to this stream, so a close or an error can settle its `closed`.
    /// `None` once `releaseLock` runs, which is what makes a released reader's
    /// `closed` keep the release rejection when the stream errors afterwards -- measured.
    reader: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
}

/// The privates a stream's WRAPPER keeps the page's underlying source and the stream's controller under
/// (`new ReadableStream(source)`), which a cancel calls the source's `cancel(reason, controller)` with
/// ([`cancel_stream`]). Heap edges, not `v8::Global`s in the struct: a source whose
/// methods close over the stream would have rooted the stream for ever.
const SOURCE_SLOT: &str = "korvus_rs_source";
const CONTROLLER_SLOT: &str = "korvus_rs_controller";

pub(crate) trait ReadableStreamBehavior {
   fn construct() -> ReadableStream where Self: Sized { stub!("ReadableStream::constructor") }
}

#[webapi(base, own)]
impl ReadableStream {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<ReadableStream, ::deno_error::JsErrorBox> { dispatch!("ReadableStream::constructor", <ReadableStream as ReadableStreamBehavior>::construct()) }
}

impl ReadableStreamBehavior for ReadableStream {
    fn construct() -> ReadableStream {
        ReadableStream {
            chunks: std::cell::RefCell::new(Vec::new()),
            done: std::cell::Cell::new(false),
            locked: std::cell::Cell::new(false),
            read_requests: std::cell::RefCell::new(Vec::new()),
            errored: std::cell::RefCell::new(None),
            disturbed: std::cell::Cell::new(false),
            targets: std::cell::RefCell::new(Vec::new()),
            reader: std::cell::RefCell::new(None),
        }
    }
}

impl ReadableStream {
    /// Is the stream closed (close requested AND nothing left to read)?
    pub(crate) fn is_closed(&self) -> bool {
        self.done.get() && self.chunks.borrow().is_empty()
    }
    pub(crate) fn is_errored(&self) -> bool {
        self.errored.borrow().is_some()
    }
    pub(crate) fn is_locked(&self) -> bool {
        self.locked.get()
    }
    pub(crate) fn is_disturbed(&self) -> bool {
        self.disturbed.get()
    }
    /// The high-water mark (1) minus the queue, WHATWG `ReadableStreamDefaultControllerGetDesiredSize`
    /// with the default count strategy: `None` once errored, 0 once closed.
    fn desired_size(&self) -> Option<f64> {
        if self.is_errored() {
            return None;
        }
        if self.is_closed() {
            return Some(0.0);
        }
        Some(1.0 - self.chunks.borrow().len() as f64)
    }
}

/// The cppgc stream behind `v`, if it is one.
pub(crate) fn as_stream<'a>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<&'a ReadableStream> {
    deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStream>(scope, v).map(|p| unsafe { &*(p.as_ref() as *const ReadableStream) })
}

/// A fresh stream object on this realm's `ReadableStream.prototype`, with nothing queued.
pub(crate) fn new_stream<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::class::mint(scope, <ReadableStream as ReadableStreamBehavior>::construct());
    set_table_proto(scope, obj, "ReadableStream");
    obj
}

/// Give a stream made by [`new_stream`] an underlying SOURCE -- an object whose `cancel(reason)`
/// [`cancel_stream`] calls -- the hook the engine's own producers stop on (a network body
/// a page cancels stops being read).
pub(crate) fn set_underlying_source(scope: &mut v8::PinScope, stream: v8::Local<v8::Object>, source: v8::Local<v8::Object>) {
    crate::core::dom::element::priv_set(scope, stream, SOURCE_SLOT, source.into());
}

/// A stream that yields exactly `chunk` (when given) and is already closed -- what a `Response` or
/// `Request` body built over bytes is.
pub(crate) fn new_closed_stream_with<'s>(scope: &mut v8::PinScope<'s, '_>, chunk: Option<v8::Local<v8::Value>>) -> v8::Local<'s, v8::Object> {
    let obj = new_stream(scope);
    if let Some(rs) = as_stream(scope, obj.into()) {
        if let Some(c) = chunk {
            rs.chunks.borrow_mut().push(v8::Global::new(scope, c));
        }
        rs.done.set(true);
    }
    obj
}

/// `{ value, done }` -- the shape every `read()`/`next()` resolves with.
fn read_result<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<v8::Value>, done: bool) -> v8::Local<'s, v8::Value> {
    // Key order is Chrome's: `JSON.stringify(await reader.read())` is `{"done":false,"value":"hi"}`.
    let result = v8::Object::new(scope);
    let doneb: v8::Local<v8::Value> = v8::Boolean::new(scope, done).into();
    set_prop(scope, result, "done", doneb);
    set_prop(scope, result, "value", value);
    result.into()
}

/// The reader currently locked to `rs`, if it is still attached.
fn locked_reader<'a>(scope: &mut v8::PinScope, rs: &ReadableStream) -> Option<&'a ReadableStreamDefaultReader> {
    let g = rs.reader.borrow().clone()?;
    let local = v8::Local::new(scope, &g);
    deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultReader>(scope, local.into())
        .map(|p| unsafe { &*(p.as_ref() as *const ReadableStreamDefaultReader) })
}

/// Fulfil the locked reader's `closed` with `undefined`. Called BEFORE the parked reads settle:
/// Chrome 150 orders `closed` first on close, on error and on releaseLock alike (measured).
fn settle_reader_closed(scope: &mut v8::PinScope, rs: &ReadableStream) {
    if let Some(rd) = locked_reader(scope, rs) {
        rd.closed.resolve(scope);
    }
}

/// Reject the locked reader's `closed` with the stream's stored error, marked handled.
fn reject_reader_closed(scope: &mut v8::PinScope, rs: &ReadableStream, reason: v8::Local<v8::Value>) {
    if let Some(rd) = locked_reader(scope, rs) {
        rd.closed.reject(scope, reason);
    }
}

/// Settle every parked reader with `{ value: undefined, done: true }` and finish every pipe/branch --
/// the tail of a close whose queue is empty. The lists are drained into locals BEFORE anything
/// re-enters JS, so no `RefCell` borrow is held across a `resolve` or a sink call.
fn finish_close(scope: &mut v8::PinScope, rs: &ReadableStream) {
    settle_reader_closed(scope, rs);
    let pending: Vec<v8::Global<v8::PromiseResolver>> = rs.read_requests.borrow_mut().drain(..).collect();
    for resolver_g in pending {
        let resolver = v8::Local::new(scope, &resolver_g);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        let res = read_result(scope, u, true);
        resolver.resolve(scope, res);
    }
    let targets: Vec<Target> = rs.targets.borrow_mut().drain(..).collect();
    // A finished PIPE releases its source; a tee'd or transferred source stays locked, as Chrome's does.
    if targets.iter().any(|t| matches!(t, Target::Pipe { .. })) {
        rs.locked.set(false);
    }
    for t in targets {
        match t {
            Target::Pipe { dest, resolver, prevent_close, .. } => {
                let d = v8::Local::new(scope, &dest);
                if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, d.into()) {
                    let ws = unsafe { ws.as_ref() };
                    if !prevent_close {
                        let _ = super::writable_stream::close_sink(scope, ws);
                    }
                    ws.locked.set(false);
                }
                let r = v8::Local::new(scope, &resolver);
                let u: v8::Local<v8::Value> = v8::undefined(scope).into();
                r.resolve(scope, u);
            }
            Target::Branch(b) => {
                let b = v8::Local::new(scope, &b);
                let _ = rs_close(scope, b.into());
            }
        }
    }
}

/// Hand `chunk` to every pipe and branch that owns this stream. A destination sink that throws ends
/// its pipe: the pipe's promise rejects with what it threw and the destination is released.
fn forward_chunk(scope: &mut v8::PinScope, rs: &ReadableStream, chunk: v8::Local<v8::Value>) {
    // Snapshot the targets as Globals (cheap clones) so a sink that re-enters this stream finds the
    // RefCell free.
    let snapshot: Vec<(usize, bool, v8::Global<v8::Object>)> = rs
        .targets
        .borrow()
        .iter()
        .enumerate()
        .map(|(i, t)| match t {
            Target::Pipe { dest, .. } => (i, true, dest.clone()),
            Target::Branch(b) => (i, false, b.clone()),
        })
        .collect();
    let mut failed: Vec<(usize, v8::Global<v8::Value>)> = Vec::new();
    for (i, is_pipe, obj) in snapshot {
        let o = v8::Local::new(scope, &obj);
        if is_pipe {
            if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, o.into()) {
                if let Err(e) = super::writable_stream::write_chunk(scope, unsafe { ws.as_ref() }, chunk) {
                    failed.push((i, e));
                }
            }
        } else {
            let _ = rs_enqueue(scope, o.into(), chunk);
        }
    }
    let had_pipe_failure = !failed.is_empty();
    for (i, err) in failed.into_iter().rev() {
        let t = {
            let mut ts = rs.targets.borrow_mut();
            if i < ts.len() { Some(ts.remove(i)) } else { None }
        };
        if let Some(Target::Pipe { dest, resolver, .. }) = t {
            let d = v8::Local::new(scope, &dest);
            if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, d.into()) {
                unsafe { ws.as_ref() }.locked.set(false);
            }
            let r = v8::Local::new(scope, &resolver);
            let e = v8::Local::new(scope, &err);
            r.reject(scope, e);
        }
    }
    if had_pipe_failure && rs.targets.borrow().is_empty() {
        rs.locked.set(false);
    }
}

/// Enqueue one chunk into `stream` -- the body behind a controller's `enqueue(chunk)`. `Err` carries
/// Chrome's reason sentence (the caller prefixes its own "Failed to execute …").
pub(crate) fn rs_enqueue(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>, chunk: v8::Local<v8::Value>) -> Result<(), &'static str> {
    let Some(rs) = as_stream(scope, stream) else { return Ok(()) };
    if rs.is_errored() {
        return Err("Cannot enqueue a chunk into an errored readable stream");
    }
    if rs.done.get() {
        return Err("Cannot enqueue a chunk into a readable stream that is closed or has been requested to be closed");
    }
    if !rs.targets.borrow().is_empty() {
        forward_chunk(scope, rs, chunk);
        return Ok(());
    }
    // A reader already parked a read? Hand it this chunk directly instead of queueing it where an
    // already-resolved (done) read would never see it. Take the resolver out under a scoped borrow
    // so the borrow is dropped before `resolve` re-enters JS.
    let pending = { let mut rr = rs.read_requests.borrow_mut(); if rr.is_empty() { None } else { Some(rr.remove(0)) } };
    match pending {
        Some(resolver_g) => {
            let resolver = v8::Local::new(scope, &resolver_g);
            let res = read_result(scope, chunk, false);
            resolver.resolve(scope, res);
        }
        None => { rs.chunks.borrow_mut().push(v8::Global::new(scope, chunk)); }
    }
    Ok(())
}

/// Close `stream` -- the body behind a controller's `close()`/`terminate()`.
pub(crate) fn rs_close(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>) -> Result<(), &'static str> {
    let Some(rs) = as_stream(scope, stream) else { return Ok(()) };
    if rs.is_errored() {
        return Err("Cannot close an errored readable stream");
    }
    if rs.done.get() {
        return Err("Cannot close a readable stream that has already been requested to be closed");
    }
    rs.done.set(true);
    if rs.chunks.borrow().is_empty() {
        finish_close(scope, rs);
    }
    Ok(())
}

/// Error `stream` with `reason` -- `controller.error(e)`. A closed or already-errored stream ignores
/// it (Chrome: `c.close(); c.error(e)` does not throw and changes nothing).
pub(crate) fn rs_error(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>, reason: v8::Local<v8::Value>) {
    let Some(rs) = as_stream(scope, stream) else { return };
    if rs.is_errored() || rs.is_closed() {
        return;
    }
    *rs.errored.borrow_mut() = Some(v8::Global::new(scope, reason));
    rs.chunks.borrow_mut().clear();
    reject_reader_closed(scope, rs, reason);
    let pending: Vec<v8::Global<v8::PromiseResolver>> = rs.read_requests.borrow_mut().drain(..).collect();
    for resolver_g in pending {
        let resolver = v8::Local::new(scope, &resolver_g);
        resolver.reject(scope, reason);
    }
    let targets: Vec<Target> = rs.targets.borrow_mut().drain(..).collect();
    // A finished PIPE releases its source; a tee'd or transferred source stays locked, as Chrome's does.
    if targets.iter().any(|t| matches!(t, Target::Pipe { .. })) {
        rs.locked.set(false);
    }
    for t in targets {
        match t {
            Target::Pipe { dest, resolver, prevent_abort, .. } => {
                let d = v8::Local::new(scope, &dest);
                if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, d.into()) {
                    let ws = unsafe { ws.as_ref() };
                    if !prevent_abort {
                        let _ = super::writable_stream::abort_sink(scope, ws, reason);
                    }
                    ws.locked.set(false);
                }
                let r = v8::Local::new(scope, &resolver);
                r.reject(scope, reason);
            }
            Target::Branch(b) => {
                let b = v8::Local::new(scope, &b);
                rs_error(scope, b.into(), reason);
            }
        }
    }
}

// ── the controller `start(controller)` receives ─────────────────────────────────────────────────
//
// It was a plain object literal with `enqueue`/`close` (no `error`, no `desiredSize`, and
// `[object Object]` where Chrome answers `[object ReadableStreamDefaultController]`). It is a real
// `ReadableStreamDefaultController` now -- see `readable_stream_default_controller.rs`.

/// The controller for `stream`.
pub(crate) fn make_rs_controller<'s>(scope: &mut v8::PinScope<'s, '_>, stream: v8::Local<v8::Object>) -> v8::Local<'s, v8::Object> {
    super::readable_stream_default_controller::new_controller(scope, stream)
}

/// `desiredSize` of the stream a controller holds, for the controller file.
pub(crate) fn rs_desired_size(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>) -> Option<f64> {
    as_stream(scope, stream).and_then(|rs| rs.desired_size())
}

/// `getReader()` — throws when a reader is already outstanding (WHATWG streams §4.2.4), matching the
/// browser's own guard: a page that checks `locked` before piping must see the right answer, and a
/// second `getReader()` must not silently hand out a second view over the same chunks.
fn cb_rs_get_reader(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(rs_ref) = as_stream(scope, this.into()) else {
        let reader = super::readable_stream_default_reader::new_reader(scope, None);
        set_table_proto(scope, reader, "ReadableStreamDefaultReader");
        rv.set(reader.into());
        return;
    };
    match acquire_reader(scope, this, rs_ref) {
        Some(reader) => rv.set(reader.into()),
        None => throw_type_error(scope, "Failed to execute 'getReader' on 'ReadableStream': ReadableStreamDefaultReader constructor can only accept readable streams that are not yet locked to a reader"),
    }
}

/// Lock `stream` to a fresh reader, or `None` when it is already locked. The reader reads the stream
/// LIVE (`cb_rs_reader_read`), so a chunk enqueued after this still reaches it.
pub(crate) fn acquire_reader<'s>(scope: &mut v8::PinScope<'s, '_>, stream: v8::Local<v8::Object>, rs: &ReadableStream) -> Option<v8::Local<'s, v8::Object>> {
    if rs.locked.get() {
        return None;
    }
    rs.locked.set(true);
    let stream_ref = v8::Global::new(scope, stream);
    let reader = super::readable_stream_default_reader::new_reader(scope, Some(stream_ref));
    set_table_proto(scope, reader, "ReadableStreamDefaultReader");
    *rs.reader.borrow_mut() = Some(v8::Global::new(scope, reader));
    // A reader taken from a stream that has ALREADY finished starts SETTLED, not pending: Chrome
    // answers `fulfilled undefined` on a closed stream and the stored error on an errored one.
    // (The borrow is hoisted out of the scrutinee -- see `start_pipe` for what leaving it there
    // costs.)
    let stored = rs.errored.borrow().clone();
    if let Some(e) = stored {
        let e = v8::Local::new(scope, &e);
        reject_reader_closed(scope, rs, e);
    } else if rs.is_closed() {
        settle_reader_closed(scope, rs);
    }
    Some(reader)
}

/// One `read()` against `stream`: the promise the reader hands back.
pub(crate) fn read_from<'s>(scope: &mut v8::PinScope<'s, '_>, rs: &ReadableStream) -> v8::Local<'s, v8::Value> {
    rs.disturbed.set(true);
    let next = { let mut c = rs.chunks.borrow_mut(); if c.is_empty() { None } else { Some(c.remove(0)) } };
    if let Some(chunk_g) = next {
        let v = v8::Local::new(scope, &chunk_g);
        let res = read_result(scope, v, false);
        let p = resolved_promise(scope, res);
        // The read that took the last chunk of a closed stream is what closes it.
        if rs.done.get() && rs.chunks.borrow().is_empty() {
            finish_close(scope, rs);
        }
        return p;
    }
    if let Some(e) = rs.errored.borrow().clone() {
        let e = v8::Local::new(scope, &e);
        return rejected_promise(scope, e);
    }
    if rs.done.get() {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        let res = read_result(scope, u, true);
        return resolved_promise(scope, res);
    }
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    rs.read_requests.borrow_mut().push(v8::Global::new(scope, resolver));
    promise.into()
}

pub(crate) fn cb_rs_reader_read(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(rd) = deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultReader>(scope, args.this().into()) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        let res = read_result(scope, u, true);
        rv.set(resolved_promise(scope, res));
        return;
    };
    let rd = unsafe { rd.as_ref() };
    if rd.released.get() {
        rv.set(rejected_type_error(scope, super::readable_stream_default_reader::RELEASED_READ));
        return;
    }
    let stream_g = rd.stream.borrow().clone();
    if let Some(sg) = stream_g {
        let stream_local = v8::Local::new(scope, &sg);
        if let Some(rs) = as_stream(scope, stream_local.into()) {
            let p = read_from(scope, rs);
            rv.set(p);
            return;
        }
    }
    // Detached reader (released, or `getReader()`'s non-stream fallback): nothing to read, report done.
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let res = read_result(scope, u, true);
    rv.set(resolved_promise(scope, res));
}
/// `releaseLock()` — the other half of `getReader()`'s guard: flips the source stream's `locked` back
/// to `false` so a subsequent `getReader()` (or `pipeTo`, which acquires its own reader) can succeed.
/// Idempotent: a reader with no stored stream (already released, or never locked one) is a no-op, as
/// the spec allows.
pub(crate) fn cb_rs_reader_release(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(rd) = deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultReader>(scope, args.this().into()) else { return };
    release_reader(scope, unsafe { rd.as_ref() });
}

/// WHATWG ReadableStreamDefaultReaderRelease: `releaseLock()`'s body, and what an async iterator does when
/// its iteration ends ([`cb_rs_iter_next`], [`cb_rs_iter_return`]).
fn release_reader(scope: &mut v8::PinScope, rd_ref: &ReadableStreamDefaultReader) {
    let stream_global = rd_ref.stream.borrow_mut().take();
    let Some(g) = stream_global else { return };
    rd_ref.released.set(true);
    let stream_local = v8::Local::new(scope, &g);
    let Some(rs) = as_stream(scope, stream_local.into()) else { return };
    rs.locked.set(false);
    *rs.reader.borrow_mut() = None;
    // `closed` FIRST, then the parked reads -- Chrome's order, and the two carry DIFFERENT sentences.
    super::release_slots(scope, super::readable_stream_default_reader::RELEASED_CLOSED, &[&rd_ref.closed]);
    let pending: Vec<v8::Global<v8::PromiseResolver>> = rs.read_requests.borrow_mut().drain(..).collect();
    if !pending.is_empty() {
        let Some(m) = v8::String::new(scope, super::readable_stream_default_reader::RELEASING) else { return };
        let e = v8::Exception::type_error(scope, m);
        for resolver_g in pending {
            let resolver = v8::Local::new(scope, &resolver_g);
            resolver.reject(scope, e);
        }
    }
}

/// Cancel `rs` with `reason`: an errored stream rejects with its error (Chrome: `rej ce`); otherwise the
/// queue is dropped, every parked read settles done, and -- unless the stream had already CLOSED (close
/// requested and nothing left to read) -- the UNDERLYING SOURCE's `cancel` runs, WHATWG ReadableStreamCancel:
/// the promise settles when the source's does, with `undefined`, and a source `cancel` that throws rejects
/// it. Chrome 150.0.7871.46 calls it from `rs.cancel(r)`, `reader.cancel(r)` and an async iterator's
/// `return()`, with `(reason, controller)` -- the stream's `ReadableStreamDefaultController` second
/// (measured). It was never called here.
pub(crate) fn cancel_stream<'s>(scope: &mut v8::PinScope<'s, '_>, stream: v8::Local<v8::Object>, rs: &ReadableStream, reason: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    rs.disturbed.set(true);
    if let Some(e) = rs.errored.borrow().clone() {
        let e = v8::Local::new(scope, &e);
        return rejected_promise(scope, e);
    }
    let was_closed = rs.is_closed();
    rs.chunks.borrow_mut().clear();
    rs.done.set(true);
    finish_close(scope, rs);
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    if was_closed {
        return resolved_promise(scope, u);
    }
    let src = crate::core::css::css_style_declaration::priv_get(scope, stream, SOURCE_SLOT).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let controller = crate::core::css::css_style_declaration::priv_get(scope, stream, CONTROLLER_SLOT).unwrap_or(u);
    let Some(src) = src else { return resolved_promise(scope, u) };
    let Some(cancel) = get_prop(scope, src, "cancel").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return resolved_promise(scope, u);
    };
    v8::tc_scope!(let tc, scope);
    let Some(r) = cancel.call(tc, src.into(), &[reason, controller]) else {
        let e = tc.exception().unwrap_or_else(|| v8::undefined(tc).into());
        tc.reset();
        return rejected_promise(tc, e);
    };
    // `undefined` once the source's result settles: a template's function, the same in every call
    let settled = resolved_promise(tc, r);
    let to_undefined = crate::binding::protos::template_fn(tc, cb_to_undefined, "", 1, None);
    match v8::Local::<v8::Promise>::try_from(settled).ok().and_then(|p| p.then(tc, to_undefined)) {
        Some(p) => p.into(),
        None => resolved_promise(tc, u),
    }
}

/// A promise reaction that answers `undefined` ([`cancel_stream`]).
fn cb_to_undefined(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_undefined();
}

pub(crate) fn cb_rs_reader_cancel(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(rd) = deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultReader>(scope, args.this().into()) {
        if unsafe { rd.as_ref() }.released.get() {
            rv.set(rejected_type_error(scope, super::readable_stream_default_reader::RELEASED_CANCEL));
            return;
        }
        let g = unsafe { rd.as_ref() }.stream.borrow().clone();
        if let Some(g) = g {
            let stream_local = v8::Local::new(scope, &g);
            if let Some(rs) = as_stream(scope, stream_local.into()) {
                let p = cancel_stream(scope, stream_local, rs, args.get(0));
                rv.set(p);
                return;
            }
        }
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let p = resolved_promise(scope, u); rv.set(p);
}

/// `get locked()` — whatever `getReader()`/`releaseLock()` (and `tee`/`values`/a pipe, which acquire
/// their own reader internally) last left it as.
fn cb_rs_locked_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let locked = as_stream(scope, args.this().into()).map(|rs| rs.locked.get()).unwrap_or(false);
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, locked).into();
    rv.set(v);
}

/// `cancel(reason)` — WHATWG §4.2.4: reject with a TypeError while locked (only the reader's own
/// `cancel()` may cancel a locked stream), otherwise discard the queue and settle as done.
fn cb_rs_cancel(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(rs_ref) = as_stream(scope, args.this().into()) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        rv.set(resolved_promise(scope, u));
        return;
    };
    if rs_ref.locked.get() {
        rv.set(rejected_type_error(scope, "Failed to execute 'cancel' on 'ReadableStream': Cannot cancel a locked stream"));
        return;
    }
    let p = cancel_stream(scope, args.this(), rs_ref, args.get(0));
    rv.set(p);
}

/// `tee()` — WHATWG §4.2.4: locks `this` and returns two branches that each see the WHOLE sequence
/// independently: what is queued now is copied into both, and anything enqueued later is forwarded to
/// both (Chrome: a chunk enqueued 5 ms after `tee()` reaches both readers).
pub(crate) fn tee_stream<'s>(scope: &mut v8::PinScope<'s, '_>, rs_ref: &ReadableStream) -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>) {
    rs_ref.locked.set(true);
    let chunks: Vec<v8::Global<v8::Value>> = rs_ref.chunks.borrow_mut().drain(..).collect();
    let branch_a = new_stream(scope);
    let branch_b = new_stream(scope);
    for b in [branch_a, branch_b] {
        if let Some(br) = as_stream(scope, b.into()) {
            *br.chunks.borrow_mut() = chunks.clone();
        }
    }
    if let Some(e) = rs_ref.errored.borrow().clone() {
        let e = v8::Local::new(scope, &e);
        rs_error(scope, branch_a.into(), e);
        rs_error(scope, branch_b.into(), e);
    } else if rs_ref.done.get() {
        let _ = rs_close(scope, branch_a.into());
        let _ = rs_close(scope, branch_b.into());
    } else {
        rs_ref.disturbed.set(true);
        let ga = v8::Global::new(scope, branch_a);
        let gb = v8::Global::new(scope, branch_b);
        let mut t = rs_ref.targets.borrow_mut();
        t.push(Target::Branch(ga));
        t.push(Target::Branch(gb));
    }
    (branch_a, branch_b)
}

/// Move `rs`'s bytes into a NEW stream that now owns them, leaving `rs` disturbed and locked -- Fetch's
/// "take" of a body when `new Request(request)` re-wraps it (Chrome: the source's `bodyUsed` turns
/// `true`, the new request reads the bytes). What is queued moves now; what arrives later is forwarded.
pub(crate) fn transfer_stream<'s>(scope: &mut v8::PinScope<'s, '_>, rs: &ReadableStream) -> v8::Local<'s, v8::Object> {
    let out = new_stream(scope);
    rs.locked.set(true);
    rs.disturbed.set(true);
    let chunks: Vec<v8::Global<v8::Value>> = rs.chunks.borrow_mut().drain(..).collect();
    if let Some(o) = as_stream(scope, out.into()) {
        *o.chunks.borrow_mut() = chunks;
    }
    if let Some(e) = rs.errored.borrow().clone() {
        let e = v8::Local::new(scope, &e);
        rs_error(scope, out.into(), e);
    } else if rs.done.get() {
        let _ = rs_close(scope, out.into());
    } else {
        let g = v8::Global::new(scope, out);
        rs.targets.borrow_mut().push(Target::Branch(g));
    }
    out
}

fn cb_rs_tee(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(rs_ref) = as_stream(scope, args.this().into()) else {
        throw_type_error(scope, "Failed to execute 'tee' on 'ReadableStream': Illegal invocation");
        return;
    };
    if rs_ref.locked.get() {
        throw_type_error(scope, "Failed to execute 'tee' on 'ReadableStream': ReadableStreamDefaultReader constructor can only accept readable streams that are not yet locked to a reader");
        return;
    }
    let (branch_a, branch_b) = tee_stream(scope, rs_ref);
    let pair = v8::Array::new(scope, 2);
    pair.set_index(scope, 0, branch_a.into());
    pair.set_index(scope, 1, branch_b.into());
    rv.set(pair.into());
}

/// `[Symbol.asyncIterator]`/`values(options)` — WHATWG streams' async-iterator mixin. Acquires a reader
/// (same lock discipline as `getReader()`) and returns it as the ITERATOR: a real
/// `ReadableStreamDefaultReader` internally (its cppgc layout and `read()`), whose prototype is the realm's
/// ONE `ReadableStream AsyncIterator` prototype ([`async_iter_proto`]).
///
/// Chrome 150.0.7871.46, measured, which this answers: the iterator has no own keys; its
/// prototype holds `next` and `return` (enumerable, writable, configurable, length 0) and the
/// `ReadableStream AsyncIterator` tag, and inherits `%AsyncIteratorPrototype%` (so its `@@asyncIterator`
/// is V8's); two iterators share it; `next`/`return` on anything else reject "Failed to execute 'next' on
/// 'AsyncIterator_ReadableStream': Illegal invocation"; the iteration's end (done or an error) releases the
/// lock; `return(v)` cancels the stream with `v` -- unless `preventCancel` -- releases the lock and answers
/// `{done: true, value: v}`, then and every later time; an options value that is neither an object nor
/// null/undefined is a TypeError. Each iterator used to get a fresh prototype of its own, with a `next` and
/// an `@@asyncIterator` built for it, no `return` and no tag.
fn cb_rs_values(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(rs_ref) = as_stream(scope, this.into()) else {
        throw_type_error(scope, "Failed to execute 'values' on 'ReadableStream': Illegal invocation");
        return;
    };
    // WebIDL's `ReadableStreamIteratorOptions`, converted before the operation's steps run
    let opts = args.get(0);
    let prevent_cancel = if opts.is_null_or_undefined() {
        false
    } else if let Ok(o) = v8::Local::<v8::Object>::try_from(opts) {
        let Some(k) = v8::String::new(scope, "preventCancel") else { return };
        let Some(v) = o.get(scope, k.into()) else { return };
        v.boolean_value(scope)
    } else {
        throw_type_error(scope, "Failed to execute 'values' on 'ReadableStream': The provided value is not of type 'ReadableStreamIteratorOptions'.");
        return;
    };
    if rs_ref.locked.get() {
        throw_type_error(scope, "Failed to execute 'values' on 'ReadableStream': ReadableStreamDefaultReader constructor can only accept readable streams that are not yet locked to a reader");
        return;
    }
    let Some(proto) = async_iter_proto(scope) else { return };
    rs_ref.locked.set(true);
    let stream_ref = v8::Global::new(scope, this);
    let iter = super::readable_stream_default_reader::new_reader(scope, Some(stream_ref));
    iter.set_prototype(scope, proto.into());
    let pc: v8::Local<v8::Value> = v8::Boolean::new(scope, prevent_cancel).into();
    crate::core::dom::element::priv_set(scope, iter, ITER_PREVENT_CANCEL, pc);
    rv.set(iter.into());
}

/// The realm slot its `ReadableStream AsyncIterator` prototype is kept under ([`async_iter_proto`]).
const ASYNC_ITER_PROTO: &str = "korvus_rs_async_iter_proto";
/// A `values()` iterator's `preventCancel` -- on every such iterator, which is how `next`/`return` know
/// their receiver -- and whether its iteration is over.
const ITER_PREVENT_CANCEL: &str = "korvus_rs_iter_prevent_cancel";
const ITER_FINISHED: &str = "korvus_rs_iter_finished";

/// This realm's `ReadableStream AsyncIterator` prototype (one per realm, made on its first `values()`):
/// `next` and `return` from their templates, the tag, and `%AsyncIteratorPrototype%` -- V8's intrinsic,
/// read off a one-property object template -- as its prototype.
fn async_iter_proto<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(p) = crate::binding::protos::realm_slot(scope, ASYNC_ITER_PROTO).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(p);
    }
    let aip = async_iterator_prototype(scope)?;
    let proto = crate::binding::protos::mint_on(scope, aip.into());
    let next = crate::binding::protos::template_fn(scope, cb_rs_iter_next, "next", 0, None);
    let ret = crate::binding::protos::template_fn(scope, cb_rs_iter_return, "return", 0, None);
    for (name, f) in [("next", next), ("return", ret)] {
        let nk = v8::String::new(scope, name)?;
        let mut d = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
        d.set_enumerable(true);
        d.set_configurable(true);
        proto.define_property(scope, nk.into(), &d);
    }
    let tag: v8::Local<v8::Value> = v8::String::new(scope, "ReadableStream AsyncIterator")?.into();
    let mut td = v8::PropertyDescriptor::new_from_value_writable(tag, false);
    td.set_enumerable(false);
    td.set_configurable(true);
    let tk = v8::Symbol::get_to_string_tag(scope);
    proto.define_property(scope, tk.into(), &td);
    crate::binding::protos::set_realm_slot(scope, ASYNC_ITER_PROTO, proto.into());
    Some(proto)
}

/// This realm's `%AsyncIteratorPrototype%` -- V8's intrinsic, read off a one-property object template, the one
/// way the embedder API hands it out. Every async iterator prototype this engine makes inherits it (this
/// file's, and `FileSystemDirectoryHandle`'s in `core/fileapi/opfs.rs`), so the read is made in one place.
pub(crate) fn async_iterator_prototype<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let ot = v8::ObjectTemplate::new(scope);
    let k = v8::String::new(scope, "p")?;
    ot.set_intrinsic_data_property(k.into(), v8::Intrinsic::AsyncIteratorPrototype, v8::PropertyAttribute::NONE);
    let holder = ot.new_instance(scope)?;
    holder.get(scope, k.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// The reader a `values()` iterator is, and its `preventCancel`; `None` for any other receiver.
fn as_iter<'a>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<(&'a ReadableStreamDefaultReader, bool)> {
    let pc = crate::core::css::css_style_declaration::priv_get(scope, this, ITER_PREVENT_CANCEL)?;
    let pc = pc.is_true();
    let rd = deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultReader>(scope, this.into())?;
    Some((unsafe { &*(rd.as_ref() as *const ReadableStreamDefaultReader) }, pc))
}

fn iter_finished(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    crate::core::css::css_style_declaration::priv_get(scope, this, ITER_FINISHED).is_some_and(|v| v.is_true())
}

/// The iteration is over: the lock goes, and every later `next()` answers done.
fn finish_iter(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, rd: &ReadableStreamDefaultReader) {
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    crate::core::dom::element::priv_set(scope, this, ITER_FINISHED, t);
    release_reader(scope, rd);
}

/// `next()`: the reader's read; a read that ends the iteration (done, or the stream's error) releases the
/// lock -- at once when it has already settled, else when it does ([`cb_rs_iter_settled`]).
fn cb_rs_iter_next<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some((rd, _)) = as_iter(scope, this) else {
        rv.set(iter_illegal(scope, "next"));
        return;
    };
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let stream = rd.stream.borrow().clone();
    let Some(rs) = stream.filter(|_| !iter_finished(scope, this)).and_then(|g| {
        let l = v8::Local::new(scope, &g);
        as_stream(scope, l.into())
    }) else {
        let res = read_result(scope, u, true);
        rv.set(resolved_promise(scope, res));
        return;
    };
    let p = read_from(scope, rs);
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(p) else { rv.set(p); return };
    match promise.state() {
        v8::PromiseState::Fulfilled => {
            let r = promise.result(scope);
            if result_done(scope, r) {
                finish_iter(scope, this, rd);
            }
            rv.set(p);
        }
        v8::PromiseState::Rejected => {
            finish_iter(scope, this, rd);
            rv.set(p);
        }
        v8::PromiseState::Pending => {
            let ok = crate::binding::protos::build_fn_data(scope, cb_rs_iter_settled, "", 1, this.into());
            let err = crate::binding::protos::build_fn_data(scope, cb_rs_iter_failed, "", 1, this.into());
            match promise.then2(scope, ok, err) {
                Some(p2) => rv.set(p2.into()),
                None => rv.set(p),
            }
        }
    }
}

/// What `next`/`return` answer a receiver that is not a `values()` iterator: Chrome's rejection.
fn iter_illegal<'s>(scope: &mut v8::PinScope<'s, '_>, op: &str) -> v8::Local<'s, v8::Value> {
    rejected_type_error(scope, &format!("Failed to execute '{op}' on 'AsyncIterator_ReadableStream': Illegal invocation"))
}

/// Whether a read's `{done, value}` says done.
fn result_done(scope: &mut v8::PinScope, r: v8::Local<v8::Value>) -> bool {
    v8::Local::<v8::Object>::try_from(r).ok().and_then(|o| get_prop(scope, o, "done")).is_some_and(|v| v.is_true())
}

/// A parked `next()`'s read fulfilled (data: the iterator): done ends the iteration; the result passes on.
fn cb_rs_iter_settled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let r = args.get(0);
    if result_done(scope, r) {
        if let Ok(this) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) {
            if let Some((rd, _)) = as_iter(scope, this) {
                finish_iter(scope, this, rd);
            }
        }
    }
    rv.set(r);
}

/// A parked `next()`'s read rejected (the stream errored): the iteration ends and the rejection passes on.
fn cb_rs_iter_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Ok(this) = v8::Local::<v8::Object>::try_from(crate::binding::protos::body_data(scope, &args)) {
        if let Some((rd, _)) = as_iter(scope, this) {
            finish_iter(scope, this, rd);
        }
    }
    scope.throw_exception(args.get(0));
}

/// `return(value)`: cancel the stream with `value` unless `preventCancel`, release the lock, and answer
/// `{done: true, value}` once the cancel has settled -- and at once on an iteration already over.
fn cb_rs_iter_return(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some((rd, prevent_cancel)) = as_iter(scope, this) else {
        rv.set(iter_illegal(scope, "return"));
        return;
    };
    let value = args.get(0);
    let done = read_result(scope, value, true);
    let stream = rd.stream.borrow().clone();
    let Some(sg) = stream.filter(|_| !iter_finished(scope, this)) else {
        rv.set(resolved_promise(scope, done));
        return;
    };
    let stream_local = v8::Local::new(scope, &sg);
    let cancelled = match (prevent_cancel, as_stream(scope, stream_local.into())) {
        (false, Some(rs)) => Some(cancel_stream(scope, stream_local, rs, value)),
        _ => None,
    };
    finish_iter(scope, this, rd);
    let Some(p) = cancelled.and_then(|p| v8::Local::<v8::Promise>::try_from(p).ok()) else {
        rv.set(resolved_promise(scope, done));
        return;
    };
    match p.state() {
        v8::PromiseState::Fulfilled => rv.set(resolved_promise(scope, done)),
        v8::PromiseState::Rejected => rv.set(p.into()),
        v8::PromiseState::Pending => {
            let to_done = crate::binding::protos::build_fn_data(scope, cb_rs_iter_done, "", 1, done);
            match p.then(scope, to_done) {
                Some(p2) => rv.set(p2.into()),
                None => rv.set(p.into()),
            }
        }
    }
}

/// A `return()`'s cancel settled (data: the `{done: true, value}` it answers).
fn cb_rs_iter_done(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = crate::binding::protos::body_data(scope, &args);
    rv.set(d);
}

/// `pipeTo(destination, options)` — lock both, write every queued chunk into the destination's sink,
/// then either finish at once (the source is already closed or errored) or hand the source's future
/// chunks to the destination and settle when the source closes or errors.
///
/// Argument failures REJECT rather than throw -- checked against real Brave (`new
/// ReadableStream().pipeTo()` returns a rejected promise, not a synchronous throw), which is the
/// general WebIDL rule for a Promise-returning operation. `pipeThrough`, whose return type is a
/// `ReadableStream` and not a promise, does not get this treatment.
fn cb_rs_pipe_to(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if as_stream(scope, this.into()).is_none() {
        throw_type_error(scope, "Failed to execute 'pipeTo' on 'ReadableStream': Illegal invocation");
        return;
    }
    // The argument count is NOT checked here any more -- `method_row`'s shared `make_arity_guard`
    // answers it, with the same sentence, now that this row is typed `Promise` (TY2). A hand-written
    // copy of a rule the installer already applies is the defect this goal removes; the RECEIVER
    // check above stays, because it catches what the installer's chain check deliberately lets
    // through (`Object.create(ReadableStream.prototype)` -- see `install::receiver`'s header).
    match start_pipe(scope, this, args.get(0), args.get(1)) {
        Ok(p) => rv.set(p.into()),
        Err(msg) => rv.set(rejected_type_error(scope, &format!("Failed to execute 'pipeTo' on 'ReadableStream': {msg}"))),
    }
}

/// Start a pipe from `this` into `dest_val`. `Err` is an ARGUMENT failure (not a stream, locked) --
/// which `pipeTo` rejects with and `pipeThrough` throws; a failure once the pipe runs (a sink that
/// throws, a source that errors) settles the returned promise instead.
fn start_pipe<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Object>,
    dest_val: v8::Local<v8::Value>,
    opts: v8::Local<v8::Value>,
) -> Result<v8::Local<'s, v8::Promise>, &'static str> {
    let Some(rs_ref) = as_stream(scope, this.into()) else { return Err("Illegal invocation") };
    let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, dest_val) else {
        return Err("parameter 1 is not of type 'WritableStream'.");
    };
    let ws_ref = unsafe { &*(ws.as_ref() as *const WritableStream) };
    if rs_ref.locked.get() || ws_ref.locked.get() {
        return Err("Cannot pipe a locked stream");
    }
    let (mut prevent_close, mut prevent_abort) = (false, false);
    if let Ok(o) = v8::Local::<v8::Object>::try_from(opts) {
        if let Some(pc) = get_prop(scope, o, "preventClose") { prevent_close = pc.boolean_value(scope); }
        if let Some(pa) = get_prop(scope, o, "preventAbort") { prevent_abort = pa.boolean_value(scope); }
    }
    let Ok(dest_obj) = v8::Local::<v8::Object>::try_from(dest_val) else { return Err("parameter 1 is not of type 'WritableStream'.") };
    rs_ref.locked.set(true);
    ws_ref.locked.set(true);
    rs_ref.disturbed.set(true);
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    let promise = resolver.get_promise(scope);
    rs_ref.targets.borrow_mut().push(Target::Pipe {
        dest: v8::Global::new(scope, dest_obj),
        resolver: v8::Global::new(scope, resolver),
        prevent_close,
        prevent_abort,
    });
    // What is already queued goes first, through the same forwarding a later enqueue takes.
    let queued: Vec<v8::Global<v8::Value>> = rs_ref.chunks.borrow_mut().drain(..).collect();
    for g in queued {
        if rs_ref.targets.borrow().is_empty() {
            break;
        }
        let c = v8::Local::new(scope, &g);
        forward_chunk(scope, rs_ref, c);
    }
    if !rs_ref.targets.borrow().is_empty() {
        // The `borrow()` is TAKEN OUT of the `if let` scrutinee on purpose: a temporary there lives
        // until the end of the whole `if let`, so `errored.borrow_mut()` two lines down panicked
        // `RefCell already borrowed` and ABORTED the process. Measured on master before this lane
        // touched anything -- `streams/piping/multiple-propagation.any.js` is the tenth file of the
        // `streams` tier and killed the whole test binary there, which is why the tier had no
        // number at all. A shared-machinery abort, not a wrong answer.
        let stored = rs_ref.errored.borrow().clone();
        if let Some(e) = stored {
            // An errored source has no queue; its error goes to the destination now.
            *rs_ref.errored.borrow_mut() = None;
            let e = v8::Local::new(scope, &e);
            rs_error(scope, this.into(), e);
        } else if rs_ref.done.get() {
            finish_close(scope, rs_ref);
        }
    }
    Ok(promise)
}

/// The engine's own `stream.pipeThrough({writable, readable})` (`Body::textStream`'s): the realm's pipe, never a
/// page-replaceable `pipeThrough`. `None` when the pipe cannot start (a locked stream).
pub(crate) fn pipe_through_pair<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<'s, v8::Object>,
    writable: v8::Local<'s, v8::Value>,
    readable: v8::Local<'s, v8::Value>,
) -> Option<v8::Local<'s, v8::Value>> {
    let undefined: v8::Local<v8::Value> = v8::undefined(scope).into();
    let promise = start_pipe(scope, this, writable, undefined).ok()?;
    promise.mark_as_handled();
    Some(readable)
}

/// `pipeThrough({writable, readable})` — WHATWG §4.2.4: `pipeTo(transform.writable)` then return
/// `transform.readable`. The pipe's own promise is marked handled, as the spec's is: a failure shows
/// up on the readable side, not as an unhandled rejection.
fn cb_rs_pipe_through(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let transform = args.get(0);
    let Ok(transform_obj) = v8::Local::<v8::Object>::try_from(transform) else {
        throw_type_error(scope, "Failed to execute 'pipeThrough' on 'ReadableStream': parameter 1 is not of type 'ReadableWritablePair'.");
        return;
    };
    let Some(writable) = get_prop(scope, transform_obj, "writable") else {
        throw_type_error(scope, "Failed to execute 'pipeThrough' on 'ReadableStream': parameter 1 is not of type 'ReadableWritablePair'.");
        return;
    };
    let Some(readable) = get_prop(scope, transform_obj, "readable") else {
        throw_type_error(scope, "Failed to execute 'pipeThrough' on 'ReadableStream': parameter 1 is not of type 'ReadableWritablePair'.");
        return;
    };
    let this = args.this();
    // Chrome 150 names the dictionary member for a non-stream `writable`, and the lock sentence as
    // pipeTo does: both measured.
    if deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, writable).is_none() {
        throw_type_error(scope, "Failed to execute 'pipeThrough' on 'ReadableStream': Failed to read the 'writable' property from 'ReadableWritablePair': Failed to convert value to 'WritableStream'.");
        return;
    }
    // pipeTo's own argument checks, THROWN here (pipeThrough returns a stream, not a promise). A
    // failure after the pipe starts shows up on the readable side, and the pipe's promise is marked
    // handled as the spec's is.
    let promise = match start_pipe(scope, this, writable, args.get(1)) {
        Ok(p) => p,
        Err(msg) => {
            throw_type_error(scope, &format!("Failed to execute 'pipeThrough' on 'ReadableStream': {msg}"));
            return;
        }
    };
    promise.mark_as_handled();
    rv.set(readable);
}


// ── The interface, from ONE table ───────────────────────────────────────────────

/// `new ReadableStream(underlyingSource)` — mints the cppgc stream and runs `source.start(controller)`
/// synchronously. A `start` that throws errors the stream with what it threw, and the constructor
/// rethrows it (WHATWG `SetUpReadableStreamDefaultController`).
pub fn cb_rs_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("ReadableStream"));
        return;
    }
    let obj = crate::binding::class::mint(scope, <ReadableStream as ReadableStreamBehavior>::construct());
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => {
            obj.set_prototype(scope, p.into());
        }
        None => set_table_proto(scope, obj, "ReadableStream"),
    }
    let src = args.get(0);
    if let Ok(src_obj) = v8::Local::<v8::Object>::try_from(src) {
        let controller = make_rs_controller(scope, obj);
        crate::core::dom::element::priv_set(scope, obj, SOURCE_SLOT, src_obj.into());
        crate::core::dom::element::priv_set(scope, obj, CONTROLLER_SLOT, controller.into());
        if let Some(start) = get_prop(scope, src_obj, "start").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
            let cv: v8::Local<v8::Value> = controller.into();
            if start.call(scope, src, &[cv]).is_none() {
                return;
            }
        }
    }
    rv.set(obj.into());
}

/// The body every implemented row of [`READABLESTREAM`] names.
pub fn install_readable_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "locked" => getter_row(scope, proto, member, cb_rs_locked_getter),
        "cancel" => method_row(scope, proto, member, cb_rs_cancel),
        "getReader" => method_row(scope, proto, member, cb_rs_get_reader),
        "pipeThrough" => method_row(scope, proto, member, cb_rs_pipe_through),
        // `pipeTo` TAKES THE SHARED GUARD LIKE EVERY OTHER ROW. It was carved out of `method_row`
        // because "the guard would pre-empt the rejection with a throw" -- true when this row was
        // typed `Object`, and false since TY2 typed it `Promise`: `make_arity_guard` is handed
        // `member.ty` and `cb_arity_guard` asks `TypeTag::rejects_rather_than_throws`, so the shared
        // guard now rejects the same sentence `cb_rs_pipe_to` used to hand-write. The carve-out was
        // a mistyped row wearing a special case.
        "pipeTo" => method_row(scope, proto, member, cb_rs_pipe_to),
        "tee" => method_row(scope, proto, member, cb_rs_tee),
        "values" => method_row(scope, proto, member, cb_rs_values),
        other => debug_assert!(false, "install_readable_stream: unexpected member {other}"),
    }
}

/// `ReadableStream.prototype[Symbol.asyncIterator]` — the one thing the table cannot say.
///
/// The `interface!` grammar has no symbol-keyed row, and the capture records `"iterable": false` for
/// this interface (Brave 151 ships `values()` but not the `@@asyncIterator` alias), so a row would be a
/// structural diff even if the grammar had one. The deleted JS installed it and `for await (const c of
/// stream)` is load-bearing on real sites, so the install function puts it there from Rust.
///
/// IT IS `values` ITSELF, the same function object, as WebIDL's async iterable declaration makes it:
/// `ReadableStream.prototype[Symbol.asyncIterator] === ReadableStream.prototype.values` is `true` in Chrome
/// 150.0.7871.46 (measured). It was a second function over the same body, which no
/// template made either.
pub fn install_rs_async_iterator(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>) {
    let Some(f) = get_prop(scope, proto, "values").filter(|v| v.is_function()) else { return };
    let sym = v8::Symbol::get_async_iterator(scope);
    let mut d = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
    d.set_enumerable(false);
    d.set_configurable(true);
    proto.define_property(scope, sym.into(), &d);
}

// Rows VERBATIM from the `ReadableStream` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s but for `exposed`), in the capture's order.
webapi::interface! {
    "ReadableStream" [value, both] {
        ctor   "ReadableStream"                       len 0 = stub,
        attr   "locked"      : Object readonly              = install_readable_stream,
        method "cancel"      : Promise len 0                = install_readable_stream,
        method "getReader"   : Object len 0                 = install_readable_stream,
        method "pipeThrough" : Object len 1 required 1      = install_readable_stream,
        method "pipeTo"      : Promise len 1 required 1     = install_readable_stream,
        method "tee"         : Object len 0                 = install_readable_stream,
        method "values"      : Object len 0                 = install_readable_stream
    }
}
