//! `core/streams/readable_stream_default_controller.rs` — made real.
//!
//! The object a `ReadableStream` source's `start()` receives. Until P9e it was a plain object literal
//! with two keys (`enqueue`, `close`) built beside the stream, and this table's four rows were D2 stubs
//! that nothing reached: the earlier ruling kept them so because the stream had no errored state for
//! `error()` to set. It has one now (`readable_stream.rs`), so the ruling's condition is gone and the
//! controller is an instance of this interface.
//!
//! MEASURED on `google-chrome` 150.0.7871.46 (L14's C05 names the missing `error` as a cause):
//!
//! ```text
//! typeof c.error, toString, instanceof, desiredSize   function,[object ReadableStreamDefaultController],true,1
//! own names | prototype names                         |desiredSize,close,enqueue,error,constructor
//! desiredSize: fresh, +1, +2, close                   1,0,-1,-1      errored: null
//! enqueue after close   TypeError  … Cannot enqueue a chunk into a readable stream that is closed or has been requested to be closed
//! close after close     TypeError  … Cannot close a readable stream that has already been requested to be closed
//! enqueue after error   TypeError  … Cannot enqueue a chunk into an errored readable stream
//! close after error     TypeError  … Cannot close an errored readable stream
//! error after close     no throw, no effect
//! ```

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{throw_type_error};
use crate::install::live::{getter_row, method_row, set_table_proto};
use crate::install::MemberDef;

/// The stream a controller drives is kept on its WRAPPER ([`STREAM_SLOT`]), a heap edge the GC traces. It was a
/// `v8::Global` in this struct -- a ROOT -- which was harmless while nothing kept a controller but the page;
/// a stream keeps its controller now, for its source's `cancel(reason, controller)`, and a
/// root back to the stream would have made every such stream immortal.
#[repr(C)]
#[derive(CppgcBase)]
pub struct ReadableStreamDefaultController {
    /// Nothing: cppgc cannot take a zero-sized base type.
    _unit: u8,
}

/// The private the controller's wrapper keeps its stream under.
const STREAM_SLOT: &str = "korvus_rsdc_stream";

pub(crate) trait ReadableStreamDefaultControllerBehavior {
    fn construct() -> ReadableStreamDefaultController where Self: Sized { stub!("ReadableStreamDefaultController::constructor") }
}

#[webapi(base, own)]
impl ReadableStreamDefaultController {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<ReadableStreamDefaultController, ::deno_error::JsErrorBox> {
        dispatch!("ReadableStreamDefaultController::constructor", <ReadableStreamDefaultController as ReadableStreamDefaultControllerBehavior>::construct())
    }
}

impl ReadableStreamDefaultControllerBehavior for ReadableStreamDefaultController {
    fn construct() -> ReadableStreamDefaultController {
        ReadableStreamDefaultController { _unit: 0 }
    }
}


/// A controller over `stream`, on the table's prototype.
pub(crate) fn new_controller<'s>(scope: &mut v8::PinScope<'s, '_>, stream: v8::Local<v8::Object>) -> v8::Local<'s, v8::Object> {
    let c = crate::binding::class::mint(scope, ReadableStreamDefaultController { _unit: 0 });
    set_table_proto(scope, c, "ReadableStreamDefaultController");
    crate::core::dom::element::priv_set(scope, c, STREAM_SLOT, stream.into());
    c
}

/// The stream behind the receiver, or the `Illegal invocation` a foreign receiver gets.
fn this_stream<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Option<v8::Local<'s, v8::Value>> {
    if deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultController>(scope, args.this().into()).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return None;
    }
    crate::core::css::css_style_declaration::priv_get(scope, args.this(), STREAM_SLOT)
}

fn cb_rsdc_desired_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = this_stream(scope, &args) else { return };
    match super::readable_stream::rs_desired_size(scope, s) {
        Some(n) => {
            let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
            rv.set(v);
        }
        None => rv.set_null(),
    }
}

fn cb_rsdc_enqueue(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = this_stream(scope, &args) else { return };
    if let Err(why) = super::readable_stream::rs_enqueue(scope, s, args.get(0)) {
        throw_type_error(scope, &format!("Failed to execute 'enqueue' on 'ReadableStreamDefaultController': {why}"));
    }
}

fn cb_rsdc_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = this_stream(scope, &args) else { return };
    if let Err(why) = super::readable_stream::rs_close(scope, s) {
        throw_type_error(scope, &format!("Failed to execute 'close' on 'ReadableStreamDefaultController': {why}"));
    }
}

fn cb_rsdc_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = this_stream(scope, &args) else { return };
    super::readable_stream::rs_error(scope, s, args.get(0));
}

/// The body every row of [`READABLESTREAMDEFAULTCONTROLLER`] names.
pub fn install_rs_default_controller(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "desiredSize" => getter_row(scope, proto, member, cb_rsdc_desired_size),
        "close" => method_row(scope, proto, member, cb_rsdc_close),
        "enqueue" => method_row(scope, proto, member, cb_rsdc_enqueue),
        "error" => method_row(scope, proto, member, cb_rsdc_error),
        other => debug_assert!(false, "install_rs_default_controller: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `ReadableStreamDefaultController` block of `install/generated_registry.rs`
// (identical to `generated_worker_registry.rs`'s but for `exposed`), in the capture's order.
webapi::interface! {
    "ReadableStreamDefaultController" [interface, both] {
        ctor   "ReadableStreamDefaultController"          len 0 = stub,
        attr   "desiredSize" : Object readonly nullable         = install_rs_default_controller,
        method "close"       : Object len 0                     = install_rs_default_controller,
        method "enqueue"     : Object len 0                     = install_rs_default_controller,
        method "error"       : Object len 0                     = install_rs_default_controller
    }
}
