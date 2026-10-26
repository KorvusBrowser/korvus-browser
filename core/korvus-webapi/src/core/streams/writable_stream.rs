//! `core/streams/writable_stream.rs` — `readable-stream-piping` (Track 2).
//!
//! Was a shell (`op_noop_method` stubs — see `docs/archive/goal-webapi-max/briefs/readable-stream-piping.md`
//! SEAMS: "check what `WritableStream.getWriter()` actually returns... if the writer is thin, say so
//! and widen this unit"). It was: `getWriter()` returned `undefined`, so `ReadableStream.pipeTo` would
//! have had nowhere real to write. This makes the sink real: the constructor's `sink` argument's
//! `write`/`close`/`abort` are captured as real JS functions and actually called, with the sink object
//! itself as `this` (matching what a real underlying sink's methods receive).
//!
//! Same simplification as `ReadableStream`: no queuing strategy, no backpressure, no `start()` — a
//! synchronous engine has nothing async to model backpressure against, and none of that is what
//! `pipeTo`/`pipeThrough` need to be observably correct.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{get_prop, resolved_promise, throw_type_error};
use crate::install::live::{getter_row, method_row, set_table_proto};
use crate::install::MemberDef;
use super::writable_stream_default_writer::WritableStreamDefaultWriter;

#[repr(C)]
#[derive(CppgcBase)]
pub struct WritableStream {
    // pub(crate): `ReadableStream::pipeTo` (sibling file) reads these directly to drive the sink, and
    // `WritableStreamDefaultWriter` (sibling file) reads them to implement `write`/`close`/`abort`.
    pub(crate) locked: std::cell::Cell<bool>,
    pub(crate) sink: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    pub(crate) write_fn: std::cell::RefCell<Option<v8::Global<v8::Function>>>,
    pub(crate) close_fn: std::cell::RefCell<Option<v8::Global<v8::Function>>>,
    pub(crate) abort_fn: std::cell::RefCell<Option<v8::Global<v8::Function>>>,
    /// `close()` has run: a later write rejects (Chrome: "Cannot write to a CLOSED writable stream").
    pub(crate) closed: std::cell::Cell<bool>,
    /// WHATWG `[[storedError]]`. A sink that throws or rejects ERRORS the stream, and
    /// the error is what every later `write`, the writer's `closed` and its `ready` answer with --
    /// which is the whole of `compression/decompression-bad-chunks`.
    pub(crate) errored: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    /// The writer currently locked to this stream, so a close or an error can settle its `closed`.
    pub(crate) writer: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
}

pub(crate) trait WritableStreamBehavior {
    fn construct() -> WritableStream where Self: Sized { stub!("WritableStream::constructor") }
}

#[webapi(base, own)]
impl WritableStream {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<WritableStream, ::deno_error::JsErrorBox> { dispatch!("WritableStream::constructor", <WritableStream as WritableStreamBehavior>::construct()) }
}

impl WritableStreamBehavior for WritableStream {
    fn construct() -> WritableStream {
        WritableStream {
            locked: std::cell::Cell::new(false),
            sink: std::cell::RefCell::new(None),
            write_fn: std::cell::RefCell::new(None),
            close_fn: std::cell::RefCell::new(None),
            abort_fn: std::cell::RefCell::new(None),
            closed: std::cell::Cell::new(false),
            errored: std::cell::RefCell::new(None),
            writer: std::cell::RefCell::new(None),
        }
    }
}

/// Call one of the sink's captured functions (`write`/`close`/`abort`) with the sink object itself as
/// `this` — a default (sink-less) stream has nothing to call, so this is a silent `Ok(None)`, matching
/// the spec's default underlying sink (which does nothing and still "succeeds").
///
/// A sink that THROWS is an `Err` carrying what it threw. It used to be dropped on the floor: a
/// `TextDecoderStream` handed a string (Chrome: the write REJECTS with `TypeError: The provided value is
/// not of type '(ArrayBuffer or ArrayBufferView)'.`) answered a fulfilled write and an exception left
/// pending in the caller's frame.
pub(crate) fn invoke_sink_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ws: &WritableStream,
    which: &std::cell::RefCell<Option<v8::Global<v8::Function>>>,
    call_args: &[v8::Local<v8::Value>],
) -> Result<Option<v8::Global<v8::Value>>, v8::Global<v8::Value>> {
    let Some(f_global) = which.borrow().clone() else { return Ok(None) };
    let f = v8::Local::new(scope, &f_global);
    let this: v8::Local<v8::Value> = match ws.sink.borrow().as_ref() {
        Some(s) => v8::Local::new(scope, s).into(),
        None => v8::undefined(scope).into(),
    };
    v8::tc_scope!(let tc, scope);
    let out = f.call(tc, this, call_args).map(|v| v8::Global::new(tc, v));
    if tc.has_caught() {
        let err = tc.exception().map(|e| v8::Global::new(tc, e));
        tc.reset();
        return Err(err.unwrap_or_else(|| {
            let u: v8::Local<v8::Value> = v8::undefined(tc).into();
            v8::Global::new(tc, u)
        }));
    }
    Ok(out)
}
/// Write one chunk into the sink.
pub(crate) fn write_chunk(scope: &mut v8::PinScope, ws: &WritableStream, chunk: v8::Local<v8::Value>) -> Result<Option<v8::Global<v8::Value>>, v8::Global<v8::Value>> {
    invoke_sink_fn(scope, ws, &ws.write_fn, &[chunk])
}
/// Close the sink (once).
pub(crate) fn close_sink(scope: &mut v8::PinScope, ws: &WritableStream) -> Result<Option<v8::Global<v8::Value>>, v8::Global<v8::Value>> {
    if ws.closed.replace(true) {
        return Ok(None);
    }
    invoke_sink_fn(scope, ws, &ws.close_fn, &[])
}
/// Abort the sink with `reason`. A stream that has already CLOSED or ERRORED has nothing left to
/// abort: Chrome answers a fulfilled promise and does not call the sink at all (measured, both
/// `writer.abort after the stream errored` and `writer.abort after the stream closed`).
pub(crate) fn abort_sink(scope: &mut v8::PinScope, ws: &WritableStream, reason: v8::Local<v8::Value>) -> Result<Option<v8::Global<v8::Value>>, v8::Global<v8::Value>> {
    if ws.errored.borrow().is_some() || ws.closed.get() {
        return Ok(None);
    }
    ws.closed.set(true);
    let r = invoke_sink_fn(scope, ws, &ws.abort_fn, &[reason]);
    // An abort ERRORS the stream with its own reason -- whatever that reason is. Chrome's
    // `writer.closed` then rejects with the string `"why"`, not with a TypeError wrapping it.
    ws_error(scope, ws, reason);
    r
}

// ── the state a `closed` promise reads ─────────────────────────────────────────────

/// The writer currently locked to `ws`, if one is.
pub(crate) fn locked_writer<'a>(scope: &mut v8::PinScope, ws: &WritableStream) -> Option<&'a WritableStreamDefaultWriter> {
    let g = ws.writer.borrow().clone()?;
    let local = v8::Local::new(scope, &g);
    deno_core::cppgc::try_unwrap_cppgc_object::<WritableStreamDefaultWriter>(scope, local.into())
        .map(|p| unsafe { &*(p.as_ref() as *const WritableStreamDefaultWriter) })
}

/// Error `ws` with `reason` — WHATWG `WritableStreamStartErroring` reduced to the two things that
/// are observable here: the stored error every later operation answers with, and the writer's
/// `closed`/`ready`, which both reject with it.
///
/// `ready` is REPLACED rather than rejected in place, because this engine mints it already-fulfilled
/// (it models no queue, so there is no backpressure for `ready` to gate on). Chrome's was pending
/// and rejects in place; the settled VALUE is the same and the identity across an error is the one
/// thing that differs -- stated here rather than hidden, and it is the queue that would have to
/// exist to close it.
pub(crate) fn ws_error(scope: &mut v8::PinScope, ws: &WritableStream, reason: v8::Local<v8::Value>) {
    if ws.errored.borrow().is_some() {
        return;
    }
    *ws.errored.borrow_mut() = Some(v8::Global::new(scope, reason));
    if let Some(w) = locked_writer(scope, ws) {
        w.closed.reject(scope, reason);
        w.ready.reject_or_replace(scope, reason);
    }
}

/// The sink's `close` succeeded: the writer's `closed` fulfils.
pub(crate) fn ws_finish_close(scope: &mut v8::PinScope, ws: &WritableStream) {
    if let Some(w) = locked_writer(scope, ws) {
        w.closed.resolve(scope);
    }
}

/// Settle `resolver` with the outcome of one sink call, and move the STREAM's state with it. The
/// operation's own promise settles FIRST and the stream's `closed` after it -- Chrome's order for
/// both a throwing write (`write-rej,closed-rej`) and a clean close (`close,closed`), measured.
///
/// `finish_close` is the close variant: it fulfils the writer's `closed` when the sink succeeded.
pub(crate) fn settle_sink_call(
    scope: &mut v8::PinScope,
    stream: v8::Local<v8::Object>,
    resolver: v8::Local<v8::PromiseResolver>,
    r: Result<Option<v8::Global<v8::Value>>, v8::Global<v8::Value>>,
    finish_close: bool,
) {
    let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, stream.into()) else { return };
    let ws_ref = unsafe { &*(ws.as_ref() as *const WritableStream) };
    match r {
        Err(e) => {
            let e = v8::Local::new(scope, &e);
            resolver.reject(scope, e);
            ws_error(scope, ws_ref, e);
        }
        Ok(Some(v)) => {
            let v = v8::Local::new(scope, &v);
            // An ASYNC sink settles the operation when ITS promise settles, and a rejection there
            // errors the stream exactly as a synchronous throw does (measured: `writer.closed after
            // a sink write rejects`).
            if let Ok(p) = v8::Local::<v8::Promise>::try_from(v) {
                let d = v8::Array::new(scope, 3);
                d.set_index(scope, 0, stream.into());
                d.set_index(scope, 1, resolver.into());
                let fc = v8::Boolean::new(scope, finish_close);
                d.set_index(scope, 2, fc.into());
                let ok = crate::binding::protos::build_fn_data(scope, cb_sink_settled_ok, "", 1, d.into());
                let err = crate::binding::protos::build_fn_data(scope, cb_sink_settled_err, "", 1, d.into());
                p.then2(scope, ok, err);
                return;
            }
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            resolver.resolve(scope, u);
            if finish_close {
                ws_finish_close(scope, ws_ref);
            }
        }
        Ok(None) => {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            resolver.resolve(scope, u);
            if finish_close {
                ws_finish_close(scope, ws_ref);
            }
        }
    }
}

/// `data = [stream, resolver, finishClose]` — the two halves of an async sink's own promise.
fn sink_settled_parts<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<v8::Value>) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::PromiseResolver>, bool)> {
    let d = v8::Local::<v8::Array>::try_from(data).ok()?;
    let stream = v8::Local::<v8::Object>::try_from(d.get_index(scope, 0)?).ok()?;
    // SAFETY: slot 1 is the resolver `settle_sink_call` put there two lines above, and nothing else
    // reads this array. `PromiseResolver` has no `IsPromiseResolver` predicate to check against.
    let resolver = unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(d.get_index(scope, 1)?) };
    let finish_close = d.get_index(scope, 2)?.boolean_value(scope);
    Some((stream, resolver, finish_close))
}

fn cb_sink_settled_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((stream, resolver, finish_close)) = sink_settled_parts(scope, __body_data) else { return };
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    resolver.resolve(scope, u);
    if finish_close {
        if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, stream.into()) {
            ws_finish_close(scope, unsafe { &*(ws.as_ref() as *const WritableStream) });
        }
    }
}

fn cb_sink_settled_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((stream, resolver, _)) = sink_settled_parts(scope, __body_data) else { return };
    let e = args.get(0);
    resolver.reject(scope, e);
    if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, stream.into()) {
        ws_error(scope, unsafe { &*(ws.as_ref() as *const WritableStream) }, e);
    }
}

/// The promise a writer/stream operation answers: rejected with what the sink threw, the sink's own
/// returned promise when it returned one (an async sink settles the write), else fulfilled.
pub(crate) fn sink_result_promise<'s>(scope: &mut v8::PinScope<'s, '_>, r: Result<Option<v8::Global<v8::Value>>, v8::Global<v8::Value>>) -> v8::Local<'s, v8::Value> {
    match r {
        Err(e) => {
            let e = v8::Local::new(scope, &e);
            crate::binding::protos::rejected_promise(scope, e)
        }
        Ok(Some(v)) => {
            let v = v8::Local::new(scope, &v);
            if v.is_promise() {
                if let Some(resolver) = v8::PromiseResolver::new(scope) {
                    resolver.resolve(scope, v);
                    return resolver.get_promise(scope).into();
                }
            }
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            resolved_promise(scope, u)
        }
        Ok(None) => {
            let u: v8::Local<v8::Value> = v8::undefined(scope).into();
            resolved_promise(scope, u)
        }
    }
}

/// A fresh stream with no sink yet, on `WritableStream.prototype` -- a subclass (`FileSystemWritableFileStream`)
/// puts its own prototype on it and [`ws_init`]s its sink.
pub(crate) fn mint_stream<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::class::mint(scope, <WritableStream as WritableStreamBehavior>::construct());
    set_table_proto(scope, obj, "WritableStream");
    obj
}

/// The stream `v` is, if it is one.
pub(crate) fn as_writable<'a>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<&'a WritableStream> {
    deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, v).map(|p| unsafe { &*(p.as_ref() as *const WritableStream) })
}

/// `__wsInit(self, sink)` — the JS constructor wrapper's other half (`08g-observers.js`): pulls the
/// sink's `write`/`close`/`abort` off whatever object the page passed to `new WritableStream(sink)`,
/// the same "capture the callables, ignore the rest" shape `ReadableStream`'s `__rsEnqueue`/`__rsClose`
/// already use for `start(controller)`.
pub(crate) fn ws_init(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>, sink: v8::Local<v8::Value>) {
    let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, stream) else { return };
    let ws_ref = unsafe { ws.as_ref() };
    let Ok(sink_obj) = v8::Local::<v8::Object>::try_from(sink) else { return };
    *ws_ref.sink.borrow_mut() = Some(v8::Global::new(scope, sink_obj));
    if let Some(f) = get_prop(scope, sink_obj, "write").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        *ws_ref.write_fn.borrow_mut() = Some(v8::Global::new(scope, f));
    }
    if let Some(f) = get_prop(scope, sink_obj, "close").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        *ws_ref.close_fn.borrow_mut() = Some(v8::Global::new(scope, f));
    }
    if let Some(f) = get_prop(scope, sink_obj, "abort").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        *ws_ref.abort_fn.borrow_mut() = Some(v8::Global::new(scope, f));
    }
}

/// `get locked()`.
fn cb_ws_locked_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let locked = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, args.this().into())
        .map(|ws| unsafe { ws.as_ref() }.locked.get())
        .unwrap_or(false);
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, locked).into();
    rv.set(v);
}

/// `getWriter()` — throws while already locked (WHATWG §4.4.4), same guard as
/// `ReadableStream.getReader`.
fn cb_ws_get_writer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, this.into()) else {
        throw_type_error(scope, "Failed to execute 'getWriter' on 'WritableStream': Illegal invocation");
        return;
    };
    let ws_ref = unsafe { ws.as_ref() };
    if ws_ref.locked.get() {
        throw_type_error(scope, "Failed to execute 'getWriter' on 'WritableStream': WritableStreamDefaultWriter constructor can only accept writable streams that are not yet locked to a writer");
        return;
    }
    ws_ref.locked.set(true);
    let stream_ref = v8::Global::new(scope, this);
    let writer = super::writable_stream_default_writer::new_writer(scope, stream_ref);
    set_table_proto(scope, writer, "WritableStreamDefaultWriter");
    *ws_ref.writer.borrow_mut() = Some(v8::Global::new(scope, writer));
    // A writer taken from a stream that has ALREADY finished starts SETTLED: Chrome answers
    // `fulfilled undefined` on a closed stream and the stored error on an errored one (measured).
    let stored = ws_ref.errored.borrow().clone();
    if let Some(e) = stored {
        let e = v8::Local::new(scope, &e);
        if let Some(w) = locked_writer(scope, ws_ref) {
            w.closed.reject(scope, e);
            w.ready.reject_or_replace(scope, e);
        }
    } else if ws_ref.closed.get() {
        ws_finish_close(scope, ws_ref);
    }
    rv.set(writer.into());
}

/// `abort(reason)` — calls the sink's `abort` (if any) and fulfils. Allowed even while locked: a page
/// aborting through the stream rather than its writer is still a real WHATWG path.
fn cb_ws_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, args.this().into()) {
        let ws_ref = unsafe { ws.as_ref() };
        if ws_ref.locked.get() {
            rv.set(crate::binding::protos::rejected_type_error(scope, "Failed to execute 'abort' on 'WritableStream': Cannot abort a locked stream"));
            return;
        }
        let reason = args.get(0);
        let r = abort_sink(scope, ws_ref, reason);
        let p = sink_result_promise(scope, r);
        rv.set(p);
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}

/// `close()` — calls the sink's `close` (if any) and fulfils.
fn cb_ws_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(ws) = deno_core::cppgc::try_unwrap_cppgc_object::<WritableStream>(scope, args.this().into()) {
        let ws_ref = unsafe { ws.as_ref() };
        // A stream a writer holds cannot be closed or aborted THROUGH the stream -- Chrome rejects
        // both with a sentence naming the lock, the same guard `getWriter` already has (measured).
        if ws_ref.locked.get() {
            rv.set(crate::binding::protos::rejected_type_error(scope, "Failed to execute 'close' on 'WritableStream': Cannot close a locked stream"));
            return;
        }
        // A stream that has errored answers with its error, and one already closed is a TypeError (Chrome 150:
        // "Cannot close a CLOSED writable stream", measured on a FileSystemWritableFileStream).
        if let Some(e) = ws_ref.errored.borrow().clone() {
            let e = v8::Local::new(scope, &e);
            let Some(r) = v8::PromiseResolver::new(scope) else { return };
            r.reject(scope, e);
            rv.set(r.get_promise(scope).into());
            return;
        }
        if ws_ref.closed.get() {
            rv.set(crate::binding::protos::rejected_type_error(scope, "Cannot close a CLOSED writable stream"));
            return;
        }
        let r = close_sink(scope, ws_ref);
        let p = sink_result_promise(scope, r);
        rv.set(p);
        return;
    }
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    rv.set(resolved_promise(scope, u));
}


// ── The interface, from ONE table ───────────────────────────────────────────────
//
// `dom/08g-observers.js` mounted four members over four `op_ws_*` builder round-trips and wrapped a
// JS arrow (`(sink) => { const self = new NativeWritableStream(); KB.__wsInit(self, sink); return self; }`)
// in `op_nativize_ctor` so `WritableStream.toString()` would read `[native code]`. The table below is
// those four members and [`cb_ws_ctor`] is that arrow as a real Rust constructor -- the mask and the
// `KB.__wsInit` bridge slot both go.

/// `new WritableStream(underlyingSink)` — mints the cppgc stream and captures the sink's
/// `write`/`close`/`abort`, exactly as the masked JS body did.
pub fn cb_ws_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("WritableStream"));
        return;
    }
    let obj = crate::binding::class::mint(scope, <WritableStream as WritableStreamBehavior>::construct());
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => {
            obj.set_prototype(scope, p.into());
        }
        None => set_table_proto(scope, obj, "WritableStream"),
    }
    ws_init(scope, obj.into(), args.get(0));
    rv.set(obj.into());
}

/// The body every row of [`WRITABLESTREAM`] names.
pub fn install_writable_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "locked" => getter_row(scope, proto, member, cb_ws_locked_getter),
        "abort" => method_row(scope, proto, member, cb_ws_abort),
        "close" => method_row(scope, proto, member, cb_ws_close),
        "getWriter" => method_row(scope, proto, member, cb_ws_get_writer),
        other => debug_assert!(false, "install_writable_stream: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `WritableStream` block of `install/generated_registry.rs`.
webapi::interface! {
    "WritableStream" [value, both] {
        ctor   "WritableStream"                  len 0 = stub,
        attr   "locked"    : Object readonly           = install_writable_stream,
        method "abort"     : Promise len 0             = install_writable_stream,
        method "close"     : Promise len 0             = install_writable_stream,
        method "getWriter" : Object len 0              = install_writable_stream
    }
}
