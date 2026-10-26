//! `core/fileapi/file_reader.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::modules::crypto::shared::{crypto_ab, crypto_num};
use crate::binding::protos::{build_fn_data, get_prop, str_val, throw_type_error};
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;
use crate::modules::indexeddb::backend::{idb_queue_microtask};

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// ==================== FileReader (native cppgc EventTarget) ====================
// The 5 read/abort methods as native [native code]. Reads are synchronous (a Blob's bytes are in memory)
// but the observable result + progress events are deferred to a microtask (the spec's async model), and
// fired through the inherited EventTarget.dispatchEvent + the on<type> handler. State (readyState/result/
// error) lives in Rust behind native getters.

fn b64_encode(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct FileReader {
    ready_state: std::cell::Cell<i32>, // 0 EMPTY, 1 LOADING, 2 DONE
    result: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    error: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    pending: std::cell::RefCell<Option<(u8, Vec<u8>, String)>>, // (read kind, bytes, mime) for the deferred drain
}

pub(crate) trait FileReaderBehavior {
   fn construct() -> FileReader where Self: Sized { stub!("FileReader::constructor") }
}

#[webapi(base, own)]
impl FileReader {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<FileReader, ::deno_error::JsErrorBox> { dispatch!("FileReader::constructor", <FileReader as FileReaderBehavior>::construct()) }
}

impl FileReaderBehavior for FileReader {
    fn construct() -> FileReader { FileReader { ready_state: std::cell::Cell::new(0), result: std::cell::RefCell::new(None), error: std::cell::RefCell::new(None), pending: std::cell::RefCell::new(None) } }
}
/// Build the progress-event object the FileReader fires (plain object with type/target/loaded/total, as
/// the JS FileReader did) and dispatchEvent it: the `on<type>` handler is an event handler listener in
/// the same list as addEventListener's, so dispatch runs it in registration order.
/// Fire a TRUSTED `ProgressEvent` `ty` at the reader. It dispatched a plain object with
/// own `type`/`target`/`loaded`/`total` properties -- no `ProgressEvent`, not trusted, a `type` only a
/// property read could find. Chrome 150, measured (tools/realweb/seed/filereader): a `ProgressEvent`,
/// `isTrusted` its one own property, `lengthComputable` true, neither bubbling nor cancelable.
fn fr_fire(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, ty: &str, loaded: usize, total: usize) {
    let Some(ctor) = crate::binding::protos::iface_object(scope, "ProgressEvent").and_then(|c| v8::Local::<v8::Function>::try_from(c).ok()) else { return };
    let init = crate::binding::protos::init_dict(scope);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    crate::binding::protos::set_prop(scope, init, "lengthComputable", t);
    let lv = crypto_num(scope, loaded as f64);
    crate::binding::protos::set_prop(scope, init, "loaded", lv);
    let tv = crypto_num(scope, total as f64);
    crate::binding::protos::set_prop(scope, init, "total", tv);
    let tyv = str_val(scope, ty);
    let Some(ev) = ctor.new_instance(scope, &[tyv, init.into()]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, this, "dispatchEvent", &[evv]);
}
/// Start an async read: capture the Blob's bytes + mime now, go LOADING, drain on a microtask.
fn fr_read(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, kind: u8) {
    let this = args.this();
    if !fr_recv(scope, this.into()) { return; }
    let frh = match deno_core::cppgc::try_unwrap_cppgc_object::<FileReader>(scope, this.into()) { Some(f) => f, None => return };
    let blob = args.get(0);
    // The blob's bytes and type come from the ONE reader over its internal slots
    // (`core/fileapi/blob.rs`), not from an `obj._bytes` own property. A non-Blob argument reads as an
    // empty read, which is what it did before.
    let bytes = crate::core::fileapi::blob::blob_bytes(scope, blob).unwrap_or_default();
    let mime = crate::core::fileapi::blob::blob_type(scope, blob).unwrap_or_default();
    unsafe { frh.as_ref() }.ready_state.set(1); // LOADING
    *unsafe { frh.as_ref() }.result.borrow_mut() = None;
    *unsafe { frh.as_ref() }.error.borrow_mut() = None;
    *unsafe { frh.as_ref() }.pending.borrow_mut() = Some((kind, bytes, mime));
    // Round-trip `this` through a Global so the microtask data has the scope lifetime.
    let this_v: v8::Local<v8::Value> = this.into();
    let this_g = v8::Global::new(scope, this_v);
    let this_s = v8::Local::new(scope, &this_g);
    let fnv = build_fn_data(scope, cb_fr_drain, "", 0, this_s);
    idb_queue_microtask(scope, fnv);
}
fn cb_fr_drain(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let this_g = v8::Global::new(scope, __body_data);
    let this = v8::Local::new(scope, &this_g);
    let frh = match deno_core::cppgc::try_unwrap_cppgc_object::<FileReader>(scope, this) { Some(f) => f, None => return };
    if unsafe { frh.as_ref() }.ready_state.get() != 1 { return; } // aborted before the microtask ran
    let (kind, bytes, mime) = match unsafe { frh.as_ref() }.pending.borrow_mut().take() { Some(p) => p, None => return };
    let total = bytes.len();
    fr_fire(scope, this, "loadstart", 0, total);
    fr_fire(scope, this, "progress", total, total);
    let result: v8::Local<v8::Value> = match kind {
        0 => str_val(scope, &String::from_utf8_lossy(&bytes)),
        1 => crypto_ab(scope, bytes),
        2 => { let s: String = bytes.iter().map(|&b| b as char).collect(); str_val(scope, &s) }
        _ => { let mime = if mime.is_empty() { "application/octet-stream".to_string() } else { mime }; str_val(scope, &format!("data:{};base64,{}", mime, b64_encode(&bytes))) }
    };
    *unsafe { frh.as_ref() }.result.borrow_mut() = Some(v8::Global::new(scope, result));
    unsafe { frh.as_ref() }.ready_state.set(2); // DONE
    fr_fire(scope, this, "load", total, total);
    fr_fire(scope, this, "loadend", total, total);
}
fn cb_fr_read_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { fr_read(scope, &args, 0) }
fn cb_fr_read_array_buffer(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { fr_read(scope, &args, 1) }
fn cb_fr_read_binary_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { fr_read(scope, &args, 2) }
fn cb_fr_read_data_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) { fr_read(scope, &args, 3) }
fn cb_fr_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !fr_recv(scope, this.into()) { return; }
    if let Some(frh) = deno_core::cppgc::try_unwrap_cppgc_object::<FileReader>(scope, this.into()) {
        if unsafe { frh.as_ref() }.ready_state.get() == 1 {
            unsafe { frh.as_ref() }.ready_state.set(2);
            *unsafe { frh.as_ref() }.result.borrow_mut() = None;
            unsafe { frh.as_ref() }.pending.borrow_mut().take();
            fr_fire(scope, this.into(), "abort", 0, 0);
            fr_fire(scope, this.into(), "loadend", 0, 0);
        }
    }
}
fn cb_fr_result(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !fr_recv(scope, args.this().into()) { return; }
    if let Some(frh) = deno_core::cppgc::try_unwrap_cppgc_object::<FileReader>(scope, args.this().into()) {
        match unsafe { frh.as_ref() }.result.borrow().clone() { Some(g) => { let v = v8::Local::new(scope, &g); rv.set(v); } None => { let n = v8::null(scope); rv.set(n.into()); } }
    }
}
fn cb_fr_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !fr_recv(scope, args.this().into()) { return; }
    if let Some(frh) = deno_core::cppgc::try_unwrap_cppgc_object::<FileReader>(scope, args.this().into()) {
        match unsafe { frh.as_ref() }.error.borrow().clone() { Some(g) => { let v = v8::Local::new(scope, &g); rv.set(v); } None => { let n = v8::null(scope); rv.set(n.into()); } }
    }
}

// ── the table ──────────────────────────────────────────────────────────────
//
// WHAT THIS REPLACES. The `FileReader` block of `dom/08f-crypto.js`: a hand `Object.setPrototypeOf`
// onto `EventTarget.prototype`, nine `Object.defineProperty` sites (two getters, five methods, the
// three constants on BOTH holders) over `op_fr_*` builder round-trips, and `globalThis.FileReader =`
// the raw cppgc class. All twenty rows are the table's now, mounted by THE installer onto a
// prototype it owns and spliced onto the real `EventTarget` chain; the interface object is a native
// constructor over [`cb_fr_ctor`] (`install::live::install_crypto_xhr_interfaces`), so the `FileReader` a
// page makes is the same object the row bodies unwrap and the one the page can name.
//
// `readyState` was the class template's `#[op2] #[getter]`; it is a row body here like `result` and
// `error`, so nothing a page can name is left on the op2 impl.

/// The receiver as a `FileReader`, or the WebIDL `TypeError` a foreign one gets. Every body used to
/// answer `undefined` (or silently do nothing) for any object at all.
fn fr_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<FileReader>(scope, this).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

fn cb_fr_ready_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(frh) = deno_core::cppgc::try_unwrap_cppgc_object::<FileReader>(scope, this) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let n = unsafe { frh.as_ref() }.ready_state.get();
    let v: v8::Local<v8::Value> = v8::Integer::new(scope, n).into();
    rv.set(v);
}

/// `new FileReader()` -- the `ctor` row's body. The class template's `#[constructor]` is unreachable
/// once the table owns the interface object, so the construction lives here: mint the cppgc value,
/// then re-prototype it to `new.target.prototype`, which is the table's prototype for a plain `new`
/// and a subclass's for `class R extends FileReader {}`.
pub fn cb_fr_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("FileReader"));
        return;
    }
    let obj = crate::binding::class::mint(scope, <FileReader as FileReaderBehavior>::construct());
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => crate::install::live::set_table_proto(scope, obj, "FileReader"),
    }
    rv.set(obj.into());
}

/// The body every non-constant row of [`FILEREADER`] names.
pub fn install_file_reader(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "readyState" => getter_row(scope, proto, member, cb_fr_ready_state),
        "result" => getter_row(scope, proto, member, cb_fr_result),
        "error" => getter_row(scope, proto, member, cb_fr_error),
        "abort" => method_row(scope, proto, member, cb_fr_abort),
        "readAsArrayBuffer" => method_row(scope, proto, member, cb_fr_read_array_buffer),
        "readAsBinaryString" => method_row(scope, proto, member, cb_fr_read_binary_string),
        "readAsDataURL" => method_row(scope, proto, member, cb_fr_read_data_url),
        "readAsText" => method_row(scope, proto, member, cb_fr_read_text),
        other => debug_assert!(false, "install_file_reader: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `FileReader` blocks of `install/generated_registry.rs` +
// `generated_worker_registry.rs` (identical member lists in both realms), in the capture's order:
// the three attributes, the six handlers, the three constants on the prototype, the five methods,
// then the same three constants on the interface object. `ty` is `Object` on every method row as the
// registry has it (only `Undefined` would change behaviour, by making the arity guard force an
// undefined return). `[value, both]`: the capture records `newable: true` and the bundle's oracle
// `new FileReader()` -> `[object FileReader]`, in the window and in a worker alike.
webapi::interface! {
    "FileReader" : "EventTarget" [value, both] {
        ctor            "FileReader"                             len 0 = stub,
        attr            "readyState"         : Object readonly           = install_file_reader,
        attr            "result"             : Object readonly nullable  = install_file_reader,
        attr            "error"              : Object readonly nullable  = install_file_reader,
        handler         "onloadstart"                                    = event_handler,
        handler         "onprogress"                                     = event_handler,
        handler         "onload"                                         = event_handler,
        handler         "onabort"                                        = event_handler,
        handler         "onerror"                                        = event_handler,
        handler         "onloadend"                                      = event_handler,
        constant        "EMPTY"              : Integer                   = 0,
        constant        "LOADING"            : Integer                   = 1,
        constant        "DONE"               : Integer                   = 2,
        method          "abort"              : Object len 0              = install_file_reader,
        method          "readAsArrayBuffer"  : Object len 1 required 1    = install_file_reader,
        method          "readAsBinaryString" : Object len 1 required 1    = install_file_reader,
        method          "readAsDataURL"      : Object len 1 required 1    = install_file_reader,
        method          "readAsText"         : Object len 1 required 1    = install_file_reader,
        static_constant "EMPTY"              : Integer                   = 0,
        static_constant "LOADING"            : Integer                   = 1,
        static_constant "DONE"               : Integer                   = 2
    }
}

