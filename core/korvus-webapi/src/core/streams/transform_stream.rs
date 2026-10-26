//! `core/streams/transform_stream.rs` — `readable-stream-piping` (Track 2).
//!
//! WHATWG: a `TransformStream` is a `readable`/`writable` pair wired so writing into `writable` (after
//! running the transformer's `transform(chunk, controller)`, default identity) enqueues into
//! `readable`, and closing `writable` runs `flush(controller)` then closes `readable`. Rather than
//! invent a second sink mechanism, the writable side IS a real `WritableStream` (the sibling file) --
//! its constructor JS wrapper hands it a `write`/`close` sink whose bodies drive the paired
//! `ReadableStream` through the SAME `__rsEnqueue`/`__rsClose` globals `ReadableStream` itself uses.
//! This struct only holds the two ends so `.readable`/`.writable` can read them back.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{build_fn_data, get_prop, set_prop, throw_type_error};
use crate::install::live::{getter_row, set_table_proto};
use crate::install::MemberDef;

#[repr(C)]
#[derive(CppgcBase)]
pub struct TransformStream {
    pub(crate) readable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    pub(crate) writable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
}

pub(crate) trait TransformStreamBehavior {
    fn construct() -> TransformStream where Self: Sized { stub!("TransformStream::constructor") }
}

#[webapi(base, own)]
impl TransformStream {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<TransformStream, ::deno_error::JsErrorBox> { dispatch!("TransformStream::constructor", <TransformStream as TransformStreamBehavior>::construct()) }
}

impl TransformStreamBehavior for TransformStream {
    fn construct() -> TransformStream { TransformStream { readable: std::cell::RefCell::new(None), writable: std::cell::RefCell::new(None) } }
}

/// `__tsInit(self, readable, writable)` — the JS constructor wrapper (`08g-observers.js`) builds the
/// real `readable`/`writable` native objects (with the writable's sink already wired to drive the
/// readable) and hands them here to be stored for the two accessors below.
fn ts_init(scope: &mut v8::PinScope, ts_val: v8::Local<v8::Value>, readable: v8::Local<v8::Object>, writable: v8::Local<v8::Object>) {
    let Some(ts) = deno_core::cppgc::try_unwrap_cppgc_object::<TransformStream>(scope, ts_val) else { return };
    let ts_ref = unsafe { ts.as_ref() };
    *ts_ref.readable.borrow_mut() = Some(v8::Global::new(scope, readable));
    *ts_ref.writable.borrow_mut() = Some(v8::Global::new(scope, writable));
}

fn cb_ts_readable_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(ts) = deno_core::cppgc::try_unwrap_cppgc_object::<TransformStream>(scope, args.this().into()) {
        if let Some(g) = unsafe { ts.as_ref() }.readable.borrow().as_ref() {
            rv.set(v8::Local::new(scope, g).into());
            return;
        }
    }
    rv.set_null();
}

fn cb_ts_writable_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(ts) = deno_core::cppgc::try_unwrap_cppgc_object::<TransformStream>(scope, args.this().into()) {
        if let Some(g) = unsafe { ts.as_ref() }.writable.borrow().as_ref() {
            rv.set(v8::Local::new(scope, g).into());
            return;
        }
    }
    rv.set_null();
}


// ── The interface, from ONE table ───────────────────────────────────────────────
//
// `dom/08g-observers.js` mounted `readable`/`writable` over two `op_ts_*` builder round-trips and
// wrapped a 22-line JS arrow in `op_nativize_ctor` so `TransformStream.toString()` would read
// `[native code]`. That arrow is [`cb_ts_ctor`] below, in Rust: the mask, the `KB.__tsInit` bridge slot
// and the two object literals it built (the controller and the writable's sink) are all gone, and the
// wiring is unchanged -- the writable side is a real `WritableStream` whose sink drives the paired
// `ReadableStream` through the SAME enqueue/close bodies `ReadableStream`'s own controller uses.

/// The transform controller: `enqueue`/`terminate` drive `readable`; `error` is the no-op it has
/// always been (the engine models no errored-stream state, and the deleted JS literal's `error` was
/// `() => {}` too -- making it reachable-and-different would be a behaviour change, not a move).
fn cb_tsc_enqueue(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let _ = super::readable_stream::rs_enqueue(scope, __body_data, args.get(0));
}
fn cb_tsc_terminate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let _ = super::readable_stream::rs_close(scope, __body_data);
}
/// `error(e)` errors the readable side -- the stream has an errored state, so this is
/// no longer the `() => {}` it had to be.
fn cb_tsc_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    super::readable_stream::rs_error(scope, __body_data, args.get(0));
}

fn make_ts_controller<'s>(scope: &mut v8::PinScope<'s, '_>, readable: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Object> {
    let c = v8::Object::new(scope);
    let enq = build_fn_data(scope, cb_tsc_enqueue, "enqueue", 1, readable);
    let term = build_fn_data(scope, cb_tsc_terminate, "terminate", 0, readable);
    let err = build_fn_data(scope, cb_tsc_error, "error", 1, readable);
    set_prop(scope, c, "enqueue", enq.into());
    set_prop(scope, c, "terminate", term.into());
    set_prop(scope, c, "error", err.into());
    c
}

/// The sink handed to the writable half. Its bound data is `[readable, transformer]`, so both
/// functions can reach the paired readable and the page's transformer without a bridge slot.
fn sink_parts(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<(v8::Global<v8::Value>, v8::Global<v8::Value>)> {
    let arr = v8::Local::<v8::Array>::try_from(data).ok()?;
    let readable = arr.get_index(scope, 0)?;
    let transformer = arr.get_index(scope, 1)?;
    Some((v8::Global::new(scope, readable), v8::Global::new(scope, transformer)))
}

fn cb_ts_sink_write(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((readable_g, transformer_g)) = sink_parts(scope, __body_data) else { return };
    let readable = v8::Local::new(scope, &readable_g);
    let transformer = v8::Local::new(scope, &transformer_g);
    let chunk = args.get(0);
    let transform = v8::Local::<v8::Object>::try_from(transformer)
        .ok()
        .and_then(|t| get_prop(scope, t, "transform"))
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    match transform {
        Some(f) => {
            let controller = make_ts_controller(scope, readable);
            let cv: v8::Local<v8::Value> = controller.into();
            f.call(scope, transformer, &[chunk, cv]);
        }
        None => {
            let _ = super::readable_stream::rs_enqueue(scope, readable, chunk);
        }
    }
}

fn cb_ts_sink_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((readable_g, transformer_g)) = sink_parts(scope, __body_data) else { return };
    let readable = v8::Local::new(scope, &readable_g);
    let transformer = v8::Local::new(scope, &transformer_g);
    let flush = v8::Local::<v8::Object>::try_from(transformer)
        .ok()
        .and_then(|t| get_prop(scope, t, "flush"))
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    if let Some(f) = flush {
        let controller = make_ts_controller(scope, readable);
        let cv: v8::Local<v8::Value> = controller.into();
        f.call(scope, transformer, &[cv]);
    }
    let _ = super::readable_stream::rs_close(scope, readable);
}

/// `new TransformStream(transformer)` — the identity transform by default, which is what
/// `pipeThrough` needs; a `transform`/`flush` on the transformer runs through the controller above.
pub fn cb_ts_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("TransformStream"));
        return;
    }
    let obj = crate::binding::class::mint(scope, <TransformStream as TransformStreamBehavior>::construct());
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => {
            obj.set_prototype(scope, p.into());
        }
        None => set_table_proto(scope, obj, "TransformStream"),
    }

    let readable = crate::binding::class::mint(
        scope,
        <super::readable_stream::ReadableStream as super::readable_stream::ReadableStreamBehavior>::construct(),
    );
    set_table_proto(scope, readable, "ReadableStream");
    let rg = v8::Global::new(scope, readable);
    let rv_local: v8::Local<v8::Value> = v8::Local::new(scope, &rg).into();

    let parts = v8::Array::new(scope, 2);
    parts.set_index(scope, 0, rv_local);
    parts.set_index(scope, 1, args.get(0));
    let pg = v8::Global::new(scope, parts);
    let pv: v8::Local<v8::Value> = v8::Local::new(scope, &pg).into();

    let sink = v8::Object::new(scope);
    let w = build_fn_data(scope, cb_ts_sink_write, "write", 1, pv);
    let c = build_fn_data(scope, cb_ts_sink_close, "close", 0, pv);
    set_prop(scope, sink, "write", w.into());
    set_prop(scope, sink, "close", c.into());

    let writable = crate::binding::class::mint(
        scope,
        <super::writable_stream::WritableStream as super::writable_stream::WritableStreamBehavior>::construct(),
    );
    set_table_proto(scope, writable, "WritableStream");
    super::writable_stream::ws_init(scope, writable.into(), sink.into());

    ts_init(scope, obj.into(), readable, writable);
    rv.set(obj.into());
}

/// The body every row of [`TRANSFORMSTREAM`] names.
pub fn install_transform_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "readable" => getter_row(scope, proto, member, cb_ts_readable_getter),
        "writable" => getter_row(scope, proto, member, cb_ts_writable_getter),
        other => debug_assert!(false, "install_transform_stream: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `TransformStream` block of `install/generated_registry.rs`.
webapi::interface! {
    "TransformStream" [value, both] {
        ctor "TransformStream"             len 0 = stub,
        attr "readable" : Object readonly        = install_transform_stream,
        attr "writable" : Object readonly        = install_transform_stream
    }
}
