//! `core/fileapi/file.rs` — `File`.
//!
//! Was a JS `class File extends Blob` in `core/korvus-runtime/src/js/host.js` keeping `_name`,
//! `_lastModified` and `_webkitRelativePath` as OWN properties on every instance. The constructor is
//! native now and the three are v8 privates beside `Blob`'s two, so `Object.getOwnPropertyNames(new
//! File(["a"],"n"))` is `[]` — which is what Chrome answers.
//!
//! Members (Brave-151 baseline order): name, lastModified, lastModifiedDate, webkitRelativePath.
//! `lastModifiedDate` is the legacy Chrome-only accessor (NOT in @webref WebIDL).
//!
//! ONE BEHAVIOUR CHANGED, and Chrome is the reason. `lastModifiedDate` used to answer `null` when
//! `lastModified` was 0, citing MDN's "null if no modification date is known". Measured on
//! `google-chrome --headless=new` 150.0.7871.46: `new File([], "z", {lastModified: 0})
//! .lastModifiedDate.getTime()` is `0` — a real `Date` at the epoch, never null. The engine was asked
//! before the test was.
//!
//! `new File()` and `new File(["a"])` are TypeErrors (`fileBits` and `fileName` are both required, so
//! `File.length` is 2), `fileName` is `ToString`'d, and `lastModified` converts per WebIDL `long long`
//! — ToNumber then truncate toward zero, with NaN and the infinities becoming 0 — defaulting to the
//! construction time.

use deno_core::v8;

use crate::binding::protos::{new_global, str_val, throw_type_error};
use crate::core::fileapi::blob::{blob_parts, blob_property_bag, init_blob, parts_bytes, slot_get, slot_set};
use crate::install::webidl::{brand, branded_getter_row, dict_any, require_args, require_new};
use crate::install::MemberDef;

const NAME_SLOT: &str = "korvus_fileapi_name";
const LM_SLOT: &str = "korvus_fileapi_lastModified";
const PATH_SLOT: &str = "korvus_fileapi_webkitRelativePath";

/// `new File(sequence<BlobPart> fileBits, USVString fileName, optional FilePropertyBag options)`.
///
/// `FilePropertyBag` inherits `BlobPropertyBag`, so the type and the endings come from the SAME
/// reader `Blob`'s constructor uses — one implementation of that dictionary, not two.
pub(crate) fn cb_file_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !require_new(scope, &args, "File") || !require_args(scope, &args, "File", 2) {
        return;
    }
    // The arguments in WebIDL's order (Chrome 150): the parts, then `fileName`, then the
    // `FilePropertyBag` -- `endings`, `type`, `lastModified`, each read ONCE (korvus read the dictionary first, and
    // twice: once for its BlobPropertyBag half and again for `lastModified`).
    let Ok(parts) = blob_parts(scope, args.get(0), "File") else { return };
    // `USVString fileName`: ToString, and a Symbol is Chrome's "Cannot convert a Symbol value to a
    // string" under the constructor's own prefix.
    let name_v = args.get(1);
    if name_v.is_symbol() {
        throw_type_error(scope, "Failed to construct 'File': Cannot convert a Symbol value to a string");
        return;
    }
    let Some(name) = name_v.to_string(scope) else { return };
    let name = name.to_rust_string_lossy(scope);
    let Ok((ty, native, opts)) = blob_property_bag(scope, &args, 2, "File", "FilePropertyBag") else { return };
    let bytes = parts_bytes(parts, native);
    // `long long lastModified`, defaulting to "the current time in milliseconds" (File API): the converted value
    let lm = match dict_any(scope, opts, "lastModified") {
        Some(v) => {
            let n = v.number_value(scope).unwrap_or(0.0);
            if n.is_finite() { n.trunc() } else { 0.0 }
        }
        None => now_ms(),
    };
    let this = args.this();
    init_file(scope, this, bytes, &ty, &name, lm);
    rv.set(this.into());
}

/// Write a file's state -- the blob's two slots, the name, `lastModified`, the empty relative path --
/// and stamp both brands. Shared by the constructor and [`mint_file`].
fn init_file(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, bytes: Vec<u8>, ty: &str, name: &str, lm: f64) {
    init_blob(scope, this, bytes, ty);
    let n = str_val(scope, name);
    slot_set(scope, this, NAME_SLOT, n);
    let l: v8::Local<v8::Value> = v8::Number::new(scope, lm).into();
    slot_set(scope, this, LM_SLOT, l);
    // "" for a constructed file: only a file picked through `<input webkitdirectory>` carries the
    // selected-directory-relative path.
    let p = str_val(scope, "");
    slot_set(scope, this, PATH_SLOT, p);
    brand(scope, this, "File");
}

/// A `File`'s name, for the engine's own readers (a form's submission). `None` for anything but a file.
pub(crate) fn file_name(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    if !crate::install::webidl::has_brand(scope, v, "File") {
        return None;
    }
    Some(slot_get(scope, o, NAME_SLOT)?.to_rust_string_lossy(scope))
}

/// Mint a `File` from Rust on THIS realm's `File.prototype`, without running the constructor -- a file
/// rebuilt from a structured clone. The constructor converts `sequence<BlobPart>` with
/// `Array.from`, which is JS, and V8's ValueDeserializer forbids JS while it rebuilds a worker's message.
pub(crate) fn mint_file<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>, ty: &str, name: &str, lm: f64) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, "File")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| crate::binding::protos::get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    init_file(scope, obj, bytes, ty, name, lm);
    Some(obj)
}

/// Wall-clock milliseconds since the epoch — the default `lastModified`. `SystemTime` rather than the
/// realm's monotonic `PerfClock`: File API says "the current time", which is a `Date` value.
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

// File.name: the constructor-stringified fileName (WebIDL readonly attribute -> prototype accessor).
fn cb_file_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = slot_get(scope, args.this(), NAME_SLOT).unwrap_or_else(|| str_val(scope, ""));
    rv.set(n);
}

// File.lastModified: WebIDL `long long`, so a double and NOT `int_val` (the i32 truncation would
// corrupt epoch-ms values ~1.7e12).
fn cb_file_last_modified(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = slot_get(scope, args.this(), LM_SLOT).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let num = v8::Number::new(scope, n);
    rv.set(num.into());
}

// File.lastModifiedDate (legacy, Chrome-only): `new Date(lastModified)`, INCLUDING at 0 (measured —
// see the module docs; this used to answer null there).
fn cb_file_last_modified_date(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let msec = slot_get(scope, args.this(), LM_SLOT).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let num = v8::Number::new(scope, msec);
    match new_global(scope, "Date", &[num.into()]) {
        Some(d) => rv.set(d.into()),
        None => { let n = v8::null(scope); rv.set(n.into()); }
    }
}

// File.webkitRelativePath (File and Directory Entries API): "" for script-constructed files.
fn cb_file_webkit_relative_path(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let p = slot_get(scope, args.this(), PATH_SLOT).unwrap_or_else(|| str_val(scope, ""));
    rv.set(p);
}

/// The body every row of [`FILE`] names, each behind the `File` brand.
pub fn install_file(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "name" => branded_getter_row(scope, proto, member, cb_file_name),
        "lastModified" => branded_getter_row(scope, proto, member, cb_file_last_modified),
        "lastModifiedDate" => branded_getter_row(scope, proto, member, cb_file_last_modified_date),
        "webkitRelativePath" => branded_getter_row(scope, proto, member, cb_file_webkit_relative_path),
        other => debug_assert!(false, "install_file: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `File` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: name, lastModified, lastModifiedDate,
// webkitRelativePath. `[value, both]` with parent `Blob`, and the WebIDL constructor length 2 the
// capture records.
webapi::interface! {
    "File" : "Blob" [value, both] {
        ctor "File"                             len 2 = stub,
        attr "name"               : Object readonly   = install_file,
        attr "lastModified"       : Object readonly   = install_file,
        attr "lastModifiedDate"   : Object readonly   = install_file,
        attr "webkitRelativePath" : Object readonly   = install_file
    }
}
