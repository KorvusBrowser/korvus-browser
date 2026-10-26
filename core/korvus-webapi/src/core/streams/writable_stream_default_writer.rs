//! `core/streams/writable_stream_default_writer.rs` — `readable-stream-piping` (Track 2).
//!
//! Only ever constructed internally, by `WritableStream::getWriter()` (`writable_stream.rs`) — the
//! public `new WritableStreamDefaultWriter()` stays illegal, matching the recorded
//! `tests/fixtures/behavior-cases/WritableStreamDefaultWriter.json` case (`throws: "TypeError"`), which
//! predates this file and was captured against the real browser's OWN reason (a required `stream`
//! argument, absent). Same error name, kept deliberately rather than re-verified against a scope this
//! unit does not need: nothing here calls `new WritableStreamDefaultWriter(aRealStream)`.
//!
//! `closed` is a real promise and the sink is deferred. `closed` answered an
//! ALREADY-FULFILLED promise -- so a page waiting for a writable to fail waited forever, and the 27
//! `compression/decompression-bad-chunks` rows that ask `writer.closed` to reject all failed on it.
//! Every transition below was asked of `google-chrome` 150.0.7871.46 first and is recorded line for
//! line in `core/korvus-runtime/tests/fixtures/stream-closed-probe.chrome-150.txt`:
//!
//! ```text
//! live                             closed PENDING, ready fulfilled undefined
//! writer.close()                   closed fulfilled undefined  (ready stays fulfilled)
//! writer.abort(reason)             closed rejected with REASON ITSELF -- `"why"`, not a TypeError
//! a sink write/close that throws   closed rejected with what it threw
//! a sink write that REJECTS        the same, when its promise settles
//! releaseLock(), closed PENDING    closed and ready rejected TypeError "This writable stream writer
//!                                  has been released and cannot be used to monitor the stream's
//!                                  state", SAME promise object
//! releaseLock(), closed SETTLED    the same rejection on a NEW promise object (identity changes)
//! a writer taken from a closed /   already fulfilled / already rejected with the stored error
//!   errored stream
//! after the stream ERRORED         write rejects with the stored error; close rejects TypeError
//!                                  "Cannot close a ERRORED writable stream"; abort FULFILS;
//!                                  ready rejects with the stored error; desiredSize is null
//! after close                      write rejects "Cannot write to a CLOSED writable stream";
//!                                  desiredSize is 0
//! after releaseLock                desiredSize THROWS; write/close/abort reject, one sentence each
//! order against closed             the operation's own promise settles FIRST, both ways
//! ```
//!
//! AND THE SINK RUNS A MICROTASK LATER, not inside `write()`. Measured:
//!
//! ```text
//! chrome   before,after,sink,microtask1,microtask2,task
//! korvus   before,sink,after,microtask1,microtask2,task     (before ST1)
//! ```
//!
//! That one-line difference is what `compression/decompression-extra-input` turns on: a chunk written
//! before the read was issued was enqueued into the readable while nothing was parked there, and the
//! error the SAME sink call then raised cleared the queue before the read arrived. Deferring the sink
//! puts the read in the queue first, so the value reaches it and only the NEXT read sees the error --
//! which is what Chrome does and what the test asserts. `close()` is deferred for the same reason and
//! by the same measurement: `writer.close()` issued in the same turn as a failing `write()` must see
//! the stream ERRORED and reject with the stored error, not run the sink's `close` and invent one.
//!
//! What is NOT deferred, deliberately: `writable_stream::write_chunk` and `close_sink` themselves,
//! which is the path `ReadableStream.pipeTo`/`pipeThrough` drive. A pipe has no writer and no
//! observable write promise; deferring there would reorder every transform in the engine for nothing.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{build_fn_data, rejected_promise, rejected_type_error, resolved_promise};
use super::writable_stream::WritableStream;
use super::Closed;

/// Chrome 150's sentence for each operation on a RELEASED writer. Five sentences, one cause.
const RELEASED_CLOSED: &str =
    "This writable stream writer has been released and cannot be used to monitor the stream's state";
const RELEASED_WRITE: &str =
    "Failed to execute 'write' on 'WritableStreamDefaultWriter': This writable stream writer has been released and cannot be written to";
const RELEASED_CLOSE: &str =
    "Failed to execute 'close' on 'WritableStreamDefaultWriter': This writable stream writer has been released and cannot be closed";
const RELEASED_ABORT: &str =
    "Failed to execute 'abort' on 'WritableStreamDefaultWriter': This writable stream writer has been released and cannot be aborted";
const RELEASED_DESIRED: &str =
    "Failed to read the 'desiredSize' property from 'WritableStreamDefaultWriter': This writable stream writer has been released and cannot be used to get the desiredSize";

#[repr(C)]
#[derive(CppgcBase)]
pub struct WritableStreamDefaultWriter {
    // pub(crate): `WritableStream::get_writer` constructs one; the ops below and `releaseLock` read it.
    pub(crate) stream: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    /// WHATWG `[[closedPromise]]`, settled by the STREAM (`writable_stream.rs`: `ws_error`,
    /// `ws_finish_close`) or by `releaseLock`.
    pub(crate) closed: Closed,
    /// WHATWG `[[readyPromise]]`. Minted already-FULFILLED -- this engine models no queue, so there
    /// is no backpressure for it to gate on -- and rejected by an error or a release.
    pub(crate) ready: Closed,
    /// `releaseLock()` has run: every operation then rejects with its own sentence above.
    pub(crate) released: std::cell::Cell<bool>,
}

#[webapi(base, own)]
impl WritableStreamDefaultWriter {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<WritableStreamDefaultWriter, ::deno_error::JsErrorBox> {
        dispatch_try!(
            "WritableStreamDefaultWriter::constructor",
            Err(::deno_error::JsErrorBox::new("TypeError", "Failed to construct 'WritableStreamDefaultWriter': 1 argument required, but only 0 present."))
        )
    }
}

/// A writer over `stream`, with `closed` armed pending and `ready` armed fulfilled. The ONE mint
/// site, so a writer cannot exist with unarmed slots.
pub(crate) fn new_writer<'s>(scope: &mut v8::PinScope<'s, '_>, stream: v8::Global<v8::Object>) -> v8::Local<'s, v8::Object> {
    let writer = crate::binding::class::mint(
        scope,
        WritableStreamDefaultWriter {
            stream: std::cell::RefCell::new(Some(stream)),
            closed: Closed::empty(),
            ready: Closed::empty(),
            released: std::cell::Cell::new(false),
        },
    );
    if let Some(w) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStreamDefaultWriter>(scope, writer.into()) {
        let w = unsafe { w.as_ref() };
        w.closed.arm(scope);
        w.ready.arm(scope);
        w.ready.resolve(scope);
    }
    writer
}

fn writer_of<'a>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<&'a WritableStreamDefaultWriter> {
    deno_core::cppgc::try_unwrap_cppgc_object::<WritableStreamDefaultWriter>(scope, v)
        .map(|p| unsafe { &*(p.as_ref() as *const WritableStreamDefaultWriter) })
}

fn writer_stream<'s>(scope: &mut v8::PinScope<'s, '_>, w: &WritableStreamDefaultWriter) -> Option<(deno_core::cppgc::UnsafePtr<WritableStream>, v8::Local<'s, v8::Object>)> {
    let g = w.stream.borrow().clone()?;
    let local = v8::Local::new(scope, &g);
    let ws = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, local.into())?;
    Some((ws, local))
}

/// The stream's stored error as a rejected promise, when it has one.
fn rejected_with_stored<'s>(scope: &mut v8::PinScope<'s, '_>, ws: &WritableStream) -> Option<v8::Local<'s, v8::Value>> {
    let e = ws.errored.borrow().clone()?;
    let e = v8::Local::new(scope, &e);
    Some(rejected_promise(scope, e))
}

/// Enqueue `cb` as a microtask carrying `[stream, arg, resolver]`, and answer the resolver's promise.
/// The one shape both `write` and `close` use, so the two cannot drift apart on WHEN the sink runs.
fn defer_sink_call<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    stream: v8::Local<v8::Object>,
    arg: v8::Local<v8::Value>,
) -> v8::Local<'s, v8::Value> {
    let Some(resolver) = v8::PromiseResolver::new(scope) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        return resolved_promise(scope, u);
    };
    let promise = resolver.get_promise(scope);
    let d = v8::Array::new(scope, 3);
    d.set_index(scope, 0, stream.into());
    d.set_index(scope, 1, arg);
    d.set_index(scope, 2, resolver.into());
    let f = build_fn_data(scope, cb, "", 0, d.into());
    scope.enqueue_microtask(f);
    promise.into()
}

/// `[stream, arg, resolver]` back out of a deferred call's data.
fn deferred_parts<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<v8::Value>) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Value>, v8::Local<'s, v8::PromiseResolver>)> {
    let d = v8::Local::<v8::Array>::try_from(data).ok()?;
    let stream = v8::Local::<v8::Object>::try_from(d.get_index(scope, 0)?).ok()?;
    let arg = d.get_index(scope, 1)?;
    // SAFETY: slot 2 is the resolver `defer_sink_call` put there, and nothing else reads this array.
    // `PromiseResolver` has no `IsPromiseResolver` predicate to check against.
    let resolver = unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(d.get_index(scope, 2)?) };
    Some((stream, arg, resolver))
}

/// A deferred operation that finds the stream ERRORED rejects with the STORED error rather than
/// running its sink. This is the difference between Chrome's `writer.close()` answer in
/// `decompression-bad-chunks` (the codec's own sentence, because the failing write ran first) and
/// its answer to a `close()` issued after the error was already visible ("Cannot close a ERRORED
/// writable stream") -- both measured, and both fall out of WHERE the check is.
fn errored_before_the_microtask_ran(scope: &mut v8::PinScope, ws: &WritableStream, resolver: v8::Local<v8::PromiseResolver>) -> bool {
    let Some(e) = ws.errored.borrow().clone() else { return false };
    let e = v8::Local::new(scope, &e);
    resolver.reject(scope, e);
    true
}

/// `write(chunk)` — the sink runs a MICROTASK later (see this file's header). A writer whose lock was
/// released, a closed stream and an errored stream each reject with their own sentence, here and now.
fn cb_wsw_write(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(w) = writer_of(scope, args.this().into()) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        rv.set(resolved_promise(scope, u));
        return;
    };
    if w.released.get() {
        rv.set(rejected_type_error(scope, RELEASED_WRITE));
        return;
    }
    let chunk = args.get(0);
    if let Some((ws, stream_obj)) = writer_stream(scope, w) {
        let ws_ref = unsafe { ws.as_ref() };
        if let Some(p) = rejected_with_stored(scope, ws_ref) {
            rv.set(p);
            return;
        }
        // Chrome 150: `await w.close(); await w.write(x)` rejects "Cannot write to a CLOSED writable stream".
        if ws_ref.closed.get() {
            rv.set(rejected_type_error(scope, "Cannot write to a CLOSED writable stream"));
            return;
        }
        let p = defer_sink_call(scope, cb_wsw_deferred_write, stream_obj, chunk);
        rv.set(p);
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

fn cb_wsw_deferred_write(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((stream, chunk, resolver)) = deferred_parts(scope, __body_data) else { return };
    let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, stream.into()) else { return };
    let ws_ref = unsafe { &*(ws.as_ref() as *const WritableStream) };
    if errored_before_the_microtask_ran(scope, ws_ref, resolver) {
        return;
    }
    let r = super::writable_stream::write_chunk(scope, ws_ref, chunk);
    super::writable_stream::settle_sink_call(scope, stream, resolver, r, false);
}

/// `close()` — deferred like `write`, and for the same measured reason.
fn cb_wsw_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(w) = writer_of(scope, args.this().into()) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        rv.set(resolved_promise(scope, u));
        return;
    };
    if w.released.get() {
        rv.set(rejected_type_error(scope, RELEASED_CLOSE));
        return;
    }
    if let Some((ws, stream_obj)) = writer_stream(scope, w) {
        let ws_ref = unsafe { ws.as_ref() };
        if ws_ref.errored.borrow().is_some() {
            rv.set(rejected_type_error(scope, "Cannot close a ERRORED writable stream"));
            return;
        }
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        let p = defer_sink_call(scope, cb_wsw_deferred_close, stream_obj, u);
        rv.set(p);
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

fn cb_wsw_deferred_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((stream, _, resolver)) = deferred_parts(scope, __body_data) else { return };
    let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, stream.into()) else { return };
    let ws_ref = unsafe { &*(ws.as_ref() as *const WritableStream) };
    if errored_before_the_microtask_ran(scope, ws_ref, resolver) {
        return;
    }
    let r = super::writable_stream::close_sink(scope, ws_ref);
    super::writable_stream::settle_sink_call(scope, stream, resolver, r, true);
}

/// `abort(reason)` — errors the stream with `reason` and calls the sink's `abort` (if any). An
/// already-errored stream answers a fulfilled promise and does not run the sink again (measured).
fn cb_wsw_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(w) = writer_of(scope, args.this().into()) else {
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        rv.set(resolved_promise(scope, u));
        return;
    };
    if w.released.get() {
        rv.set(rejected_type_error(scope, RELEASED_ABORT));
        return;
    }
    let reason = args.get(0);
    if let Some((ws, _)) = writer_stream(scope, w) {
        let ws_ref = unsafe { ws.as_ref() };
        let r = super::writable_stream::abort_sink(scope, ws_ref, reason);
        let p = super::writable_stream::sink_result_promise(scope, r);
        rv.set(p);
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

/// `releaseLock()` — the writer half of `ReadableStreamDefaultReader.releaseLock`: flips the stream's
/// `locked` back to `false`, forgets the stream, and rejects `closed` and `ready` with the release
/// sentence (idempotent re-release is a no-op).
fn cb_wsw_release(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(w) = writer_of(scope, args.this().into()) else { return };
    let Some(g) = w.stream.borrow_mut().take() else { return };
    w.released.set(true);
    let local = v8::Local::new(scope, &g);
    if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, local.into()) {
        let ws_ref = unsafe { ws.as_ref() };
        ws_ref.locked.set(false);
        *ws_ref.writer.borrow_mut() = None;
    }
    super::release_slots(scope, RELEASED_CLOSED, &[&w.closed, &w.ready]);
}

/// `get closed()` — the promise the stream's own transitions settle.
fn cb_wsw_closed_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(w) = writer_of(scope, args.this().into()) else { return };
    let p = w.closed.get(scope);
    rv.set(p);
}

/// `get ready()` — fulfilled while the stream is live (no queue is modeled, so there is no
/// backpressure to gate on), rejected once the stream errors or the lock is released.
fn cb_wsw_ready_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(w) = writer_of(scope, args.this().into()) else { return };
    let p = w.ready.get(scope);
    rv.set(p);
}

/// `get desiredSize()` — `1` for a live writer, `0` once the stream closed, `null` once it errored,
/// and a THROW once the lock is released. No queue is modeled, so the live value cannot reflect real
/// backpressure -- it reflects only "can a page still write".
fn cb_wsw_desired_size_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(w) = writer_of(scope, args.this().into()) else { return };
    if w.released.get() {
        crate::binding::protos::throw_type_error(scope, RELEASED_DESIRED);
        return;
    }
    let Some((ws, _)) = writer_stream(scope, w) else {
        rv.set_null();
        return;
    };
    let ws_ref = unsafe { ws.as_ref() };
    if ws_ref.errored.borrow().is_some() {
        rv.set_null();
        return;
    }
    let n = if ws_ref.closed.get() { 0.0 } else { 1.0 };
    let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
    rv.set(v);
}


// ── The interface, from ONE table ───────────────────────────────────────────────
//
// `dom/08g-observers.js` mounted all seven members over seven `op_wsw_*` builder round-trips onto the
// cppgc class template's own prototype. They are rows now; the bodies are unchanged, and every one is
// the same Rust callback it already was.
//
// Not newable in the capture (`"newable": false`, ctor length 1), so the interface object is the
// installer's "Illegal constructor" one -- a TypeError either way, which is what
// `tests/fixtures/behavior-cases/WritableStreamDefaultWriter.json` records. `WritableStream::getWriter`
// is still the only thing that mints one, and it re-prototypes onto this table's prototype.

/// The body every row of [`WRITABLESTREAMDEFAULTWRITER`] names.
pub fn install_ws_default_writer(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{getter_row, method_row};
    match member.name {
        "closed" => getter_row(scope, proto, member, cb_wsw_closed_getter),
        "desiredSize" => getter_row(scope, proto, member, cb_wsw_desired_size_getter),
        "ready" => getter_row(scope, proto, member, cb_wsw_ready_getter),
        "abort" => method_row(scope, proto, member, cb_wsw_abort),
        "close" => method_row(scope, proto, member, cb_wsw_close),
        "releaseLock" => method_row(scope, proto, member, cb_wsw_release),
        "write" => method_row(scope, proto, member, cb_wsw_write),
        other => debug_assert!(false, "install_ws_default_writer: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `WritableStreamDefaultWriter` block of `install/generated_registry.rs`.
webapi::interface! {
    "WritableStreamDefaultWriter" [value, both] {
        ctor   "WritableStreamDefaultWriter"          len 1 required 1 = stub,
        attr   "closed"      : Promise readonly             = install_ws_default_writer,
        attr   "desiredSize" : Object readonly nullable     = install_ws_default_writer,
        attr   "ready"       : Promise readonly             = install_ws_default_writer,
        method "abort"       : Promise len 0                = install_ws_default_writer,
        method "close"       : Promise len 0                = install_ws_default_writer,
        method "releaseLock" : Object len 0                 = install_ws_default_writer,
        method "write"       : Promise len 0                = install_ws_default_writer
    }
}
