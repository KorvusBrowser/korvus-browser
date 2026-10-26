//! `core/dom/dom_exception.rs` — `DOMException`.
//!
//! Was `class DOMException extends Error` in `core/korvus-runtime/src/js/host.js` (60 lines, a
//! `WeakMap` of instance state, three `op_nativize` masks and 52 `Object.defineProperty` calls) AND a
//! second, DIFFERENT class in `worker-host.js` (three members, no constants, `name`/`code` written as
//! OWN properties on every instance). It is ONE `interface!` table with a native constructor now, and
//! `Exposed=(Window,Worker)` gives both realms the same one — which is how the worker gains the 25
//! legacy constants, the `code` accessor and a clean instance in the same move.
//!
//! It is the FIRST interface of this unit because it is the error type the rest of it throws: 42 of
//! `host.js`'s lines and every `throw_dom` / `make_dom_exc` call site in `korvus-webapi` reach
//! `globalThis.DOMException` at call time.
//!
//! WHAT CHROME ACTUALLY DOES, measured on `google-chrome --headless=new` 150.0.7871.46 rather than
//! reasoned about (`out.*` below are that probe's keys):
//!
//! * `Object.getOwnPropertyNames(new DOMException("m","AbortError"))` is `[]` and `e.stack` is
//!   `undefined`. The JS class could not reproduce that: `class X extends Error` runs V8's `Error`
//!   constructor, which captures a stack trace and defines an OWN `stack` — so every korvus
//!   DOMException carried one own property a real one does not have. `message` and `name` live in
//!   private slots here and the instance owns nothing.
//! * `Object.getPrototypeOf(DOMException)` is `Function.prototype` (its `.name` reads `""`), NOT
//!   `Error` — WebIDL gives DOMException `%Error.prototype%` for its PROTOTYPE object and leaves the
//!   interface object on `%Function.prototype%`. `extends Error` set both, so the class had a second,
//!   independent chain a page can read with one `getPrototypeOf`. The install below splices only the
//!   prototype.
//! * `DOMException.prototype.name`, `.message` and `.code` all throw `TypeError: Illegal invocation`
//!   — read on the prototype itself, and on any other foreign receiver. This is goal §W9 item 17 for
//!   this interface, and the per-interface brand closes it (`install::webidl`).
//! * `new DOMException()` is `["Error", "", 0]`; `new DOMException("x","NotFoundError")` is
//!   `["NotFoundError", "x", 8]`; `new DOMException("x","Bogus")` is `["Bogus", "x", 0]` — an
//!   unrecognised name keeps its text and answers code 0.
//! * `DOMException("a")` without `new` is "Failed to construct 'DOMException': Please use the 'new'
//!   operator, this DOM object constructor cannot be called as a function." — `webidl::require_new`'s
//!   message verbatim.
//! * every constant is `{writable:false, enumerable:true, configurable:false}` on BOTH the prototype
//!   and the interface object; `code`/`name`/`message` are `{enumerable:true, configurable:true}`
//!   getter-only accessors.

use deno_core::v8;


use crate::binding::protos::{build_fn_data, get_prop, global_get, str_val};
use crate::install::webidl::{brand, branded_getter_row, require_new};
use crate::install::MemberDef;

/// `name` -> legacy `code`. The DOM Level 1 numbering, and the whole reason `code` exists. A name that
/// is not in it answers 0 (measured: `new DOMException("x","Bogus").code` is 0), which is why this is
/// a total match rather than a lookup that can fail.
///
/// It maps 21 names onto 21 of the 25 constants. The other four (`DOMSTRING_SIZE_ERR`,
/// `NO_DATA_ALLOWED_ERR`, `VALIDATION_ERR`, `TYPE_MISMATCH_ERR`) name codes no modern exception name
/// carries; they stay on the interface because the capture records them, and that is the constant
/// list, not this one. Same 21 pairs `host.js`'s `CODES` object carried.
fn code_for(name: &str) -> f64 {
    match name {
        "IndexSizeError" => 1.0,
        "HierarchyRequestError" => 3.0,
        "WrongDocumentError" => 4.0,
        "InvalidCharacterError" => 5.0,
        "NoModificationAllowedError" => 7.0,
        "NotFoundError" => 8.0,
        "NotSupportedError" => 9.0,
        "InUseAttributeError" => 10.0,
        "InvalidStateError" => 11.0,
        "SyntaxError" => 12.0,
        "InvalidModificationError" => 13.0,
        "NamespaceError" => 14.0,
        "InvalidAccessError" => 15.0,
        "SecurityError" => 18.0,
        "NetworkError" => 19.0,
        "AbortError" => 20.0,
        "URLMismatchError" => 21.0,
        "QuotaExceededError" => 22.0,
        "TimeoutError" => 23.0,
        "InvalidNodeTypeError" => 24.0,
        "DataCloneError" => 25.0,
        _ => 0.0,
    }
}

/// The exception NAMES `deno_core`'s error builder is taught to build as a `DOMException` — the same
/// list [`code_for`] keys, minus the six that name an ECMAScript builtin whose constructor must be
/// preserved. Read by [`error_builder_names`]; nothing else enumerates it.
const BUILTIN_ERROR_NAMES: &[&str] = &["Error", "RangeError", "ReferenceError", "SyntaxError", "TypeError", "URIError"];

/// Every DOM exception name that should round-trip through `Err(JsErrorBox::new(name, …))`, plus the
/// private `DOMExceptionSyntaxError` alias. The runtime registers them (see
/// `korvus-runtime`'s `register_dom_exception_builders`); the LIST is here, beside the codes, so the
/// two cannot drift.
///
/// `SyntaxError` is not in it and the alias is why: the name belongs to a JS builtin whose
/// constructor `registerErrorBuilder` must not replace, so an op that means the DOM SyntaxError (an
/// invalid selector, an unparsable `DOMMatrix` string) throws under the private class name instead.
/// `assert_throws_dom('SyntaxError', …)` wants a DOMException with code 12; a bare ECMAScript
/// SyntaxError has neither the brand nor the code.
pub fn error_builder_names() -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&'static str, &'static str)> = DOM_EXCEPTION_NAMES
        .iter()
        .filter(|n| !BUILTIN_ERROR_NAMES.contains(n))
        .map(|n| (*n, *n))
        .collect();
    out.push((DOM_SYNTAX_ERROR_CLASS, "SyntaxError"));
    out
}

/// The private class name an op uses when it means the DOM `SyntaxError` rather than the builtin.
pub const DOM_SYNTAX_ERROR_CLASS: &str = "DOMExceptionSyntaxError";

// ── deno_core's error builders ──────────────────────────────────────────────────────────────────

/// One registered builder: `(msg) => new globalThis.DOMException(msg, <the name bound as data>)`.
///
/// The global is read at CALL time, not at registration time, and that is the whole reason this is a
/// native function rather than a Rust closure over the interface object: the registration happens
/// during the extension's ESM evaluation (while `Deno` still exists), and `DOMException` is installed
/// later, by the table. Registering eagerly against a constructor that does not exist yet would
/// register `undefined`.
fn cb_build_dom_exception<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let msg = args.get(0);
    let msg = if msg.is_undefined() { String::new() } else { msg.to_rust_string_lossy(scope) };
    if let Some(e) = crate::binding::protos::make_dom_exc(scope, &msg, &name) {
        rv.set(e);
    }
}

/// Teach `deno_core`'s error builder that these class names are `DOMException`s, so an op returning
/// `Err(JsErrorBox::new("InvalidStateError", …))` throws a real one rather than the `undefined`
/// `buildCustomError` answers for an unregistered class.
///
/// WHY AN OP AT ALL, AND WHY ONLY ONE REALM STILL USES IT. This function replaced a 17-line block in
/// `host.js` and a 29-line block in `worker-host.js`, each carrying its own copy of the exception-name
/// list; [`op_register_dom_exception_builders`] is the JS call site those two blocks became.
///
/// The op's doc used to say the call could not be an install step at all, because
/// `registerErrorBuilder` is reachable only from JS and the WORKER deleted `globalThis.Deno` at the
/// end of its ESM entry point -- before any Rust install of that realm could run. THE SECOND HALF WAS
/// A CONSEQUENCE OF WHERE THE DELETE WAS, not of deno_core: the worker's delete is
/// `binding::hidden::hide_deno`, called from `worker.rs` at the end of the realm's Rust block, and
/// `install::live::install_worker_bootstrap` calls THIS function directly with `Deno` still alive.
/// The first half was never true of this function, which is Rust and reads `registerErrorBuilder` off
/// the live global exactly as the op does.
///
/// THE OP IS GONE AND SO IS ITS LAST CALLER. `host.js`'s
/// `OPS.op_register_dom_exception_builders()` was it; `install::live::install_window_bootstrap` calls
/// this function directly now, in the same slot and for the same reason the worker's does. Both
/// realms register from Rust, and neither goes through an op.
///
/// A name that is already registered makes `registerErrorBuilder` throw; the `try {} catch {}` the two
/// JS blocks wrapped each call in is the `tc_scope!` here.
pub fn register_error_builders(scope: &mut v8::PinScope) {
    let Some(reg) = global_get(scope, "Deno")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|d| get_prop(scope, d, "core"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "registerErrorBuilder"))
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return;
    };
    let recv: v8::Local<v8::Value> = v8::undefined(scope).into();
    for (class, dom_name) in error_builder_names() {
        let data = str_val(scope, dom_name);
        let f = build_fn_data(scope, cb_build_dom_exception, class, 1, data);
        let k = str_val(scope, class);
        v8::tc_scope!(let tc, scope);
        reg.call(tc, recv, &[k, f.into()]);
        tc.reset();
    }
}

// THIS FILE DECLARES NO OPS. `op_register_dom_exception_builders` was a three-line
// wrapper around [`register_error_builders`] for one JS caller, and that caller is gone.

/// The keys of [`code_for`], in the order `host.js`'s `CODES` object literal listed them (the order
/// decided which builders got registered, and it is kept so the registration is the same set).
const DOM_EXCEPTION_NAMES: &[&str] = &[
    "IndexSizeError",
    "HierarchyRequestError",
    "WrongDocumentError",
    "InvalidCharacterError",
    "NoModificationAllowedError",
    "NotFoundError",
    "NotSupportedError",
    "InUseAttributeError",
    "InvalidStateError",
    "SyntaxError",
    "InvalidModificationError",
    "NamespaceError",
    "InvalidAccessError",
    "SecurityError",
    "NetworkError",
    "AbortError",
    "URLMismatchError",
    "QuotaExceededError",
    "TimeoutError",
    "InvalidNodeTypeError",
    "DataCloneError",
];

// ── the instance's two slots ────────────────────────────────────────────────────────────────────
//
// `message` and `name` are WebIDL readonly attributes, so they are v8 Privates and the prototype's
// native accessors read them back. Page JS can neither enumerate them (they are invisible to
// `Object.getOwnPropertyNames` and to `JSON.stringify`, which is why `JSON.stringify(e)` is `"{}"` in
// Chrome and now here) nor forge them onto a plain object.

fn slot_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_domexc_{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

fn slot_read(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> String {
    let key = slot_key(scope, name);
    this.get_private(scope, key)
        .filter(|v| !v.is_undefined())
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default()
}

fn slot_write(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, value: &str) {
    let key = slot_key(scope, name);
    let v = str_val(scope, value);
    this.set_private(scope, key, v);
}

/// The `DOMException(optional DOMString message = "", optional DOMString name = "Error")` steps.
/// Both arguments are optional, so `length` is 0 and there is no arity guard — only the `new` guard.
pub(crate) fn cb_dom_exception_ctor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    if !require_new(scope, &args, "DOMException") {
        return;
    }
    let this = args.this();
    // WebIDL `DOMString` with a default: an ABSENT argument takes the default, and `undefined` is
    // absent for an optional argument -- `new DOMException(undefined, undefined)` is `["Error", ""]`,
    // not `["undefined", "undefined"]`. Everything else is `ToString`, so `new DOMException(null)`
    // really does carry the message "null" (Chrome agrees).
    let message = arg_or(scope, &args, 0, "");
    let name = arg_or(scope, &args, 1, "Error");
    init_dom_exception(scope, this, &message, &name);
    rv.set(this.into());
}

fn arg_or(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, idx: i32, default: &str) -> String {
    let v = args.get(idx);
    if v.is_undefined() {
        return default.to_string();
    }
    match v.to_string(scope) {
        Some(s) => s.to_rust_string_lossy(scope),
        None => default.to_string(),
    }
}

/// Fill a DOMException's slots and stamp its brand. The ONE entry point, so a Rust mint site cannot
/// build a half-initialised one.
pub(crate) fn init_dom_exception(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, message: &str, name: &str) {
    slot_write(scope, this, "message", message);
    slot_write(scope, this, "name", name);
    brand(scope, this, "DOMException");
}

fn cb_dom_exception_code(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = slot_read(scope, args.this(), "name");
    rv.set(v8::Number::new(scope, code_for(&name)).into());
}

fn cb_dom_exception_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = slot_read(scope, args.this(), "name");
    rv.set(str_val(scope, &n));
}

fn cb_dom_exception_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let m = slot_read(scope, args.this(), "message");
    rv.set(str_val(scope, &m));
}

/// The body the three accessor rows name. Each is mounted behind the brand, so a foreign receiver
/// (`DOMException.prototype.name`, `{}`, an `Error`) throws `Illegal invocation` the way Chrome does
/// rather than answering the shape default.
pub fn install_dom_exception(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "code" => branded_getter_row(scope, proto, member, cb_dom_exception_code),
        "name" => branded_getter_row(scope, proto, member, cb_dom_exception_name),
        "message" => branded_getter_row(scope, proto, member, cb_dom_exception_message),
        other => debug_assert!(false, "install_dom_exception: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `DOMException` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: `code`, `name`, `message`, the 25 legacy
// constants on the prototype, then the same 25 on the interface object. `[value, both]`:
// `surface.json` and `worker.surface.json` both record `newable: true`, `length: 0`,
// `toStringTag: "DOMException"` and the prototype chain `DOMException.prototype -> Error.prototype`.
webapi::interface! {
    "DOMException" : "Error" [value, both] {
        ctor "DOMException" len 0 = stub,
        attr "code"    : Integer readonly   = install_dom_exception,
        attr "name"    : DomString readonly = install_dom_exception,
        attr "message" : DomString readonly = install_dom_exception,

        constant "INDEX_SIZE_ERR"              : Integer = 1,
        constant "DOMSTRING_SIZE_ERR"          : Integer = 2,
        constant "HIERARCHY_REQUEST_ERR"       : Integer = 3,
        constant "WRONG_DOCUMENT_ERR"          : Integer = 4,
        constant "INVALID_CHARACTER_ERR"       : Integer = 5,
        constant "NO_DATA_ALLOWED_ERR"         : Integer = 6,
        constant "NO_MODIFICATION_ALLOWED_ERR" : Integer = 7,
        constant "NOT_FOUND_ERR"               : Integer = 8,
        constant "NOT_SUPPORTED_ERR"           : Integer = 9,
        constant "INUSE_ATTRIBUTE_ERR"         : Integer = 10,
        constant "INVALID_STATE_ERR"           : Integer = 11,
        constant "SYNTAX_ERR"                  : Integer = 12,
        constant "INVALID_MODIFICATION_ERR"    : Integer = 13,
        constant "NAMESPACE_ERR"               : Integer = 14,
        constant "INVALID_ACCESS_ERR"          : Integer = 15,
        constant "VALIDATION_ERR"              : Integer = 16,
        constant "TYPE_MISMATCH_ERR"           : Integer = 17,
        constant "SECURITY_ERR"                : Integer = 18,
        constant "NETWORK_ERR"                 : Integer = 19,
        constant "ABORT_ERR"                   : Integer = 20,
        constant "URL_MISMATCH_ERR"            : Integer = 21,
        constant "QUOTA_EXCEEDED_ERR"          : Integer = 22,
        constant "TIMEOUT_ERR"                 : Integer = 23,
        constant "INVALID_NODE_TYPE_ERR"       : Integer = 24,
        constant "DATA_CLONE_ERR"              : Integer = 25,

        static_constant "INDEX_SIZE_ERR"              : Integer = 1,
        static_constant "DOMSTRING_SIZE_ERR"          : Integer = 2,
        static_constant "HIERARCHY_REQUEST_ERR"       : Integer = 3,
        static_constant "WRONG_DOCUMENT_ERR"          : Integer = 4,
        static_constant "INVALID_CHARACTER_ERR"       : Integer = 5,
        static_constant "NO_DATA_ALLOWED_ERR"         : Integer = 6,
        static_constant "NO_MODIFICATION_ALLOWED_ERR" : Integer = 7,
        static_constant "NOT_FOUND_ERR"               : Integer = 8,
        static_constant "NOT_SUPPORTED_ERR"           : Integer = 9,
        static_constant "INUSE_ATTRIBUTE_ERR"         : Integer = 10,
        static_constant "INVALID_STATE_ERR"           : Integer = 11,
        static_constant "SYNTAX_ERR"                  : Integer = 12,
        static_constant "INVALID_MODIFICATION_ERR"    : Integer = 13,
        static_constant "NAMESPACE_ERR"               : Integer = 14,
        static_constant "INVALID_ACCESS_ERR"          : Integer = 15,
        static_constant "VALIDATION_ERR"              : Integer = 16,
        static_constant "TYPE_MISMATCH_ERR"           : Integer = 17,
        static_constant "SECURITY_ERR"                : Integer = 18,
        static_constant "NETWORK_ERR"                 : Integer = 19,
        static_constant "ABORT_ERR"                   : Integer = 20,
        static_constant "URL_MISMATCH_ERR"            : Integer = 21,
        static_constant "QUOTA_EXCEEDED_ERR"          : Integer = 22,
        static_constant "TIMEOUT_ERR"                 : Integer = 23,
        static_constant "INVALID_NODE_TYPE_ERR"       : Integer = 24,
        static_constant "DATA_CLONE_ERR"              : Integer = 25
    }
}
