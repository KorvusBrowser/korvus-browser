//! `core/streams/readable_stream_default_reader.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.
//!
//! `closed` has a body. It was `= stub` in the table below and the stub answers `null`,
//! so `reader.closed.then(…)` threw `Cannot read properties of null` instead of settling — and a
//! page that learns a stream ended by waiting on `closed` waited forever. Every transition below was
//! asked of `google-chrome` 150.0.7871.46 first and is recorded line for line in
//! `core/korvus-runtime/tests/fixtures/stream-closed-probe.chrome-150.txt`:
//!
//! ```text
//! live                              PENDING
//! controller.close()                fulfilled undefined   (and PENDING while a chunk is still queued)
//! reader.cancel()                   fulfilled undefined
//! controller.error(e)               rejected e            (whatever e is -- undefined included)
//! releaseLock(), closed PENDING     rejected TypeError "This readable stream reader has been
//!                                   released and cannot be used to monitor the stream's state",
//!                                   SAME promise object
//! releaseLock(), closed SETTLED     the same rejection on a NEW promise object (identity changes)
//! a reader taken from a closed /    already fulfilled / already rejected with the stored error
//!   errored stream
//! order against read()              closed settles FIRST, on close, on error and on releaseLock
//! a read parked at releaseLock      rejected TypeError "Releasing Default reader" -- a DIFFERENT
//!                                   sentence from the one `closed` gets
//! ```

use deno_core::{v8, CppgcBase};
use macros::webapi;

use super::Closed;

/// The sentence Chrome 150 gives every released reader's `closed`, `read()` and `cancel()` — three
/// different sentences that share one cause, so they are named together here rather than spelled
/// out at each use.
pub(crate) const RELEASED_CLOSED: &str =
    "This readable stream reader has been released and cannot be used to monitor the stream's state";
pub(crate) const RELEASED_READ: &str =
    "Failed to execute 'read' on 'ReadableStreamDefaultReader': This readable stream reader has been released and cannot be used to read from its previous owner stream";
pub(crate) const RELEASED_CANCEL: &str =
    "Failed to execute 'cancel' on 'ReadableStreamDefaultReader': This readable stream reader has been released and cannot be used to cancel its previous owner stream";
/// What a read that was PARKED when `releaseLock()` ran rejects with. Chrome does not reuse the
/// sentence above for it (measured).
pub(crate) const RELEASING: &str = "Releasing Default reader";

#[repr(C)]
#[derive(CppgcBase)]
pub struct ReadableStreamDefaultReader {
    // The stream this reader locked. `read()` reads it LIVE (`core::streams::readable_stream`), so the
    // reader keeps no queue or cursor of its own -- that snapshot was the "Connection closed" bug.
    // `releaseLock()` takes this to flip `ReadableStream.prototype.locked` back to `false`; `None` for
    // a reader that has already released (idempotent re-release) or never locked a stream.
    pub(crate) stream: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    /// WHATWG `[[closedPromise]]`. Armed at every mint site (see [`Closed`]); settled by the STREAM
    /// (`readable_stream.rs`: `finish_close`, `rs_error`) or by `releaseLock`.
    pub(crate) closed: Closed,
    /// `releaseLock()` has run. Distinguishes a RELEASED reader -- whose `read()` and `cancel()`
    /// reject with Chrome's two sentences -- from one that never held a stream, which is the
    /// `getReader()` non-stream fallback below and answers `{done:true}` as it always did.
    pub(crate) released: std::cell::Cell<bool>,
}

pub(crate) trait ReadableStreamDefaultReaderBehavior {
   fn construct() -> ReadableStreamDefaultReader where Self: Sized { stub!("ReadableStreamDefaultReader::constructor") }
}

#[webapi(base, own)]
impl ReadableStreamDefaultReader {
    // `new ReadableStreamDefaultReader(stream)` (the Chrome oracle ratchet): the argument is
    // required and must be an unlocked ReadableStream -- `new ReadableStreamDefaultReader()` constructed
    // a reader over nothing where Chrome throws. A reader made here locks the stream and reads it; the
    // stream's own reader slot is `getReader()`'s (this one's `closed` does not follow the stream).
    #[constructor]
    #[cppgc]
    fn constructor(scope: &mut v8::PinScope, stream: v8::Local<v8::Value>) -> Result<ReadableStreamDefaultReader, ::deno_error::JsErrorBox> {
        let Some(rs) = super::readable_stream::as_stream(scope, stream) else {
            let msg = if stream.is_undefined() {
                "Failed to construct 'ReadableStreamDefaultReader': 1 argument required, but only 0 present."
            } else {
                "Failed to construct 'ReadableStreamDefaultReader': parameter 1 is not of type 'ReadableStream'."
            };
            return Err(::deno_error::JsErrorBox::new("TypeError", msg));
        };
        if rs.is_locked() {
            return Err(::deno_error::JsErrorBox::new("TypeError", "Failed to construct 'ReadableStreamDefaultReader': ReadableStreamDefaultReader constructor can only accept readable streams that are not yet locked to a reader"));
        }
        rs.locked.set(true);
        let obj = v8::Local::<v8::Object>::try_from(stream).map_err(|_| ::deno_error::JsErrorBox::new("TypeError", "Illegal invocation"))?;
        let r = <ReadableStreamDefaultReader as ReadableStreamDefaultReaderBehavior>::construct();
        *r.stream.borrow_mut() = Some(v8::Global::new(scope, obj));
        Ok(r)
    }
}

impl ReadableStreamDefaultReaderBehavior for ReadableStreamDefaultReader {
    fn construct() -> ReadableStreamDefaultReader {
        ReadableStreamDefaultReader {
            stream: std::cell::RefCell::new(None),
            closed: Closed::empty(),
            released: std::cell::Cell::new(false),
        }
    }
}

/// A reader over `stream` (or over nothing), with its `closed` armed. The ONE mint site every other
/// one goes through, so a reader cannot exist with an unarmed slot.
pub(crate) fn new_reader<'s>(scope: &mut v8::PinScope<'s, '_>, stream: Option<v8::Global<v8::Object>>) -> v8::Local<'s, v8::Object> {
    let reader = crate::binding::class::mint(
        scope,
        ReadableStreamDefaultReader {
            stream: std::cell::RefCell::new(stream),
            closed: Closed::empty(),
            released: std::cell::Cell::new(false),
        },
    );
    if let Some(rd) = deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultReader>(scope, reader.into()) {
        unsafe { rd.as_ref() }.closed.arm(scope);
    }
    reader
}

/// `get closed()`.
fn cb_rs_reader_closed_getter(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(rd) = deno_core::cppgc::try_unwrap_cppgc_object::<ReadableStreamDefaultReader>(scope, args.this().into()) else { return };
    let p = unsafe { rd.as_ref() }.closed.get(scope);
    rv.set(p);
}


// ── The interface, from ONE table ───────────────────────────────────────────────
//
// `dom/08g-observers.js` mounted `read`/`releaseLock`/`cancel` over three `op_rs_reader_*` builder
// round-trips onto the cppgc class template's prototype and left `closed` to the registry shell. The
// three bodies live in the sibling `readable_stream.rs` (they read the stream the reader locked), so
// this table names them there; `closed` is the fourth row and its body is in THIS file, because the
// promise belongs to the reader and only the stream's transitions settle it.

/// The body every row of [`READABLESTREAMDEFAULTREADER`] names.
pub fn install_rs_default_reader(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use super::readable_stream::{cb_rs_reader_cancel, cb_rs_reader_read, cb_rs_reader_release};
    use crate::install::live::{getter_row, method_row};
    match member.name {
        "read" => method_row(scope, proto, member, cb_rs_reader_read),
        "releaseLock" => method_row(scope, proto, member, cb_rs_reader_release),
        "closed" => getter_row(scope, proto, member, cb_rs_reader_closed_getter),
        "cancel" => method_row(scope, proto, member, cb_rs_reader_cancel),
        other => debug_assert!(false, "install_rs_default_reader: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `ReadableStreamDefaultReader` block of `install/generated_registry.rs`.
webapi::interface! {
    "ReadableStreamDefaultReader" [value, both] {
        ctor   "ReadableStreamDefaultReader"    len 1 required 1 = stub,
        method "read"        : Promise len 0          = install_rs_default_reader,
        method "releaseLock" : Object len 0           = install_rs_default_reader,
        attr   "closed"      : Promise readonly      = install_rs_default_reader,
        method "cancel"      : Promise len 0          = install_rs_default_reader
    }
}
