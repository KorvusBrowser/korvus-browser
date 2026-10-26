//! `modules/url/url_search_params.rs` — `URLSearchParams`, from ONE table.
//!
//! Was: a cppgc class whose constructor reached the page through `dom/00-bootstrap.js`
//! (`globalThis.URLSearchParams = URLSearchParams`), with all fourteen members mounted by
//! `dom/08g-observers.js` over fourteen `op_usp_*` builder round-trips, three `Object.defineProperty`
//! sites and the `KB.__uspBind` bridge slot that `URL.searchParams` used to link a params object to
//! its URL.
//!
//! Now: the [`URLSEARCHPARAMS`] table at the bottom of this file is the one definition site, mounted by
//! `install::live::install_window_url` / `install_worker_url` in BOTH realms (the interface is
//! `[Exposed=(Window,Worker)]` and both captures list it with identical member lists). The ops are
//! gone -- every body is the same Rust callback it already was, now named by a row instead of handed
//! to JS -- and `URL::searchParams` calls [`bind`] directly, so the bridge slot is gone with them.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::protos::throw_type_error;
use crate::core::dom::webidl_iter::{for_each_live_pairs, live_pair_iter};

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hx = |c: u8| (c as char).to_digit(16);
            if let (Some(a), Some(c)) = (hx(b[i + 1]), hx(b[i + 2])) { out.push((a * 16 + c) as u8); i += 3; continue; }
        }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b == b' ' { out.push('+'); }
        else if b == b'*' || b == b'-' || b == b'.' || b == b'_' || b.is_ascii_alphanumeric() { out.push(b as char); }
        else { out.push('%'); out.push_str(&format!("{b:02X}")); }
    }
    out
}
fn usp_parse(s: &str) -> Vec<(String, String)> {
    // application/x-www-form-urlencoded: `+` decodes to space in BOTH the name and the value (the name
    // side was previously left un-decoded, so `new URLSearchParams("a+b=c")` kept the literal `a+b`).
    s.strip_prefix('?').unwrap_or(s).split('&').filter(|kv| !kv.is_empty()).map(|kv| match kv.find('=') {
        Some(i) => (pct_decode(&kv[..i].replace('+', " ")), pct_decode(&kv[i + 1..].replace('+', " "))),
        None => (pct_decode(&kv.replace('+', " ")), String::new()),
    }).collect()
}
fn usp_serialize(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v))).collect::<Vec<_>>().join("&")
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct URLSearchParams {
    p: std::cell::RefCell<Vec<(String, String)>>,
    url: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
}

// ===== URLSearchParams (migrated to scaffold) =====
pub(crate) trait URLSearchParamsBehavior {
    fn construct(_scope: &mut v8::PinScope, _init: v8::Local<v8::Value>) -> URLSearchParams where Self: Sized { stub!("URLSearchParams::constructor") }
}

#[webapi(base, own)]
impl URLSearchParams {
    #[constructor]
    #[cppgc]
    fn constructor(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Result<URLSearchParams, ::deno_error::JsErrorBox> { dispatch!("URLSearchParams::constructor", <URLSearchParams as URLSearchParamsBehavior>::construct(scope, init)) }
}

/// The WebIDL `(sequence<sequence<USVString>> or record<USVString, USVString> or USVString)` init,
/// as pairs. `Err` means a TypeError has ALREADY been thrown and the caller must return at once
/// without touching V8 again.
///
/// Split out of [`URLSearchParamsBehavior::construct`] so the native constructor
/// callback can see the failure. It used to `throw_type_error` and return a half-built object; the
/// op2 `#[constructor]` wrapper surfaced that pending exception, but a raw constructor callback goes
/// on to call `make_cppgc_object`, and that V8 call SWALLOWED it -- measured: `new URLSearchParams([[1]])`
/// answered a params object instead of Chrome's TypeError.
fn usp_init_pairs(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Result<Vec<(String, String)>, ()> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    if init.is_string() {
        pairs = usp_parse(&init.to_rust_string_lossy(scope));
    } else if let Some(other) = deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, init) {
        pairs = unsafe { other.as_ref() }.p.borrow().clone();
    } else if let Some(items) = match v8::Local::<v8::Object>::try_from(init) {
        // `(sequence<sequence<USVString>> or record<USVString, USVString> or USVString)`: an object's `@@iterator`
        // decides sequence or record, read as WebIDL reads it (Chrome 150) -- any iterable is
        // a sequence (korvus took only an Array, and never asked for `@@iterator`)
        Ok(o) => crate::install::ctor_dicts::sequence_or_record(scope, o, "Failed to construct 'URLSearchParams': ")?,
        Err(_) => None,
    } {
        for pair in items {
            // Each element must itself be a sequence of exactly two strings; anything else is
            // a TypeError (WebIDL sequence<sequence<USVString>>) -- e.g. new URLSearchParams([[1]]).
            match v8::Local::<v8::Array>::try_from(pair) {
                Ok(pa) if pa.length() == 2 => {
                    let k = pa.get_index(scope, 0).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                    let v = pa.get_index(scope, 1).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                    pairs.push((k, v));
                }
                _ => {
                    throw_type_error(scope, "Failed to construct 'URLSearchParams': Each element of the sequence must be a sequence of exactly two elements.");
                    return Err(());
                }
            }
        }
    } else if init.is_object() {
        if let Some(obj) = init.to_object(scope) {
            if let Some(keys) = obj.get_own_property_names(scope, v8::GetPropertyNamesArgs::default()) {
                for i in 0..keys.length() {
                    if let Some(key) = keys.get_index(scope, i) {
                        if let Some(val) = obj.get(scope, key) {
                            pairs.push((key.to_rust_string_lossy(scope), val.to_rust_string_lossy(scope)));
                        }
                    }
                }
            }
        }
    }
    Ok(pairs)
}

impl URLSearchParamsBehavior for URLSearchParams {
    fn construct(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> URLSearchParams {
        let pairs = usp_init_pairs(scope, init).unwrap_or_default();
        URLSearchParams { p: std::cell::RefCell::new(pairs), url: std::cell::RefCell::new(None) }
    }
}

/// Mint an EMPTY params object already on the table's prototype — what `URL::searchParams` returns
/// before its first PULL from the URL's `search`.
///
/// It used to go through `globalThis.URLSearchParams` (a `new_global` call from the getter), so a page
/// that replaced the global changed what `url.searchParams` returned. Chrome's does not; this mints
/// the cppgc object directly.
pub(crate) fn mint_empty<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::class::mint(
        scope,
        URLSearchParams { p: std::cell::RefCell::new(Vec::new()), url: std::cell::RefCell::new(None) },
    );
    crate::install::live::set_table_proto(scope, obj, "URLSearchParams");
    obj
}

/// Bind a `URLSearchParams` to a `URL` (`url.searchParams`) so PULL/NOTIFY sync it.
///
/// Was `KB.__uspBind`, a raw-callback function `dom/08g-observers.js` parked on the bridge so
/// `URL`'s `searchParams` getter could call it from JS. Both sides are Rust now, so it is a plain
/// function call and the bridge slot is deleted (invariant I3).
pub(crate) fn bind(scope: &mut v8::PinScope, usp: v8::Local<v8::Value>, url: v8::Local<v8::Object>) {
    let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, usp) else { return };
    *unsafe { u.as_ref() }.url.borrow_mut() = Some(v8::Global::new(scope, url));
}

// Clone out the URL Global (if bound) without holding the cppgc Ptr across scope use.
fn usp_url(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<v8::Global<v8::Object>> {
    let usp = deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, this)?;
    let u = unsafe { usp.as_ref() }.url.borrow().clone();
    u
}
fn usp_pull(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) {
    let url = match usp_url(scope, this) { Some(u) => u, None => return };
    let url_local = v8::Local::new(scope, &url);
    let key = match v8::String::new(scope, "search") { Some(k) => k, None => return };
    let search = match url_local.get(scope, key.into()) { Some(s) => s.to_rust_string_lossy(scope), None => return };
    let pairs = usp_parse(&search);
    if let Some(usp) = deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, this) {
        *unsafe { usp.as_ref() }.p.borrow_mut() = pairs;
    }
}
fn usp_notify(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) {
    let url = match usp_url(scope, this) { Some(u) => u, None => return };
    let pairs = match usp_pairs(scope, this) { Some(p) => p, None => return };
    let s = usp_serialize(&pairs);
    let val = if s.is_empty() { String::new() } else { format!("?{s}") };
    let url_local = v8::Local::new(scope, &url);
    let key = match v8::String::new(scope, "search") { Some(k) => k, None => return };
    let vs = match v8::String::new(scope, &val) { Some(v) => v, None => return };
    url_local.set(scope, key.into(), vs.into());
}
fn usp_pairs(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<Vec<(String, String)>> {
    let usp = deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, this)?;
    let p = unsafe { usp.as_ref() }.p.borrow().clone();
    Some(p)
}
/// The list an iteration reads at each step (`webidl_iter::live_pairs`): the owning URL's query first.
pub(crate) fn usp_current(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<Vec<(String, String)>> {
    deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, this)?;
    usp_pull(scope, this);
    usp_pairs(scope, this)
}
fn usp_set(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, pairs: Vec<(String, String)>) {
    if let Some(usp) = deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, this) {
        *unsafe { usp.as_ref() }.p.borrow_mut() = pairs;
    }
}

/// The WebIDL receiver check every member shares: a call whose `this` is not a `URLSearchParams`
/// throws Chrome's `TypeError: Illegal invocation` (measured against headless Chrome 150 over
/// loopback for `get`, `size` and the rest) instead of the silent `undefined` the `None` arm used to
/// return. `false` means an exception is pending and the caller must return at once.
fn usp_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<URLSearchParams>(scope, this).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

// The WebIDL arity of each member (how many arguments are NOT optional) is the `required` column of
// the table at the bottom of this file; `install::live::method_row` wraps the callback in the shared
// arity guard from it, so no body repeats the check. What each body DOES repeat is the receiver
// check above -- it has to run after the guard and before any state is touched.
macro_rules! usp_read {
    ($cb:ident, $name:literal, |$pairs:ident, $args:ident, $scope:ident, $rv:ident| $body:block) => {
        fn $cb($scope: &mut v8::PinScope, $args: v8::FunctionCallbackArguments, mut $rv: v8::ReturnValue) {
            let _ = $name;
            let this = $args.this().into();
            if !usp_recv($scope, this) { return; }
            usp_pull($scope, this);
            let $pairs = match usp_pairs($scope, this) { Some(p) => p, None => return };
            $body
        }
    };
}
macro_rules! usp_write {
    ($cb:ident, $name:literal, |$pairs:ident, $args:ident, $scope:ident| $body:block) => {
        fn $cb($scope: &mut v8::PinScope, $args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
            let _ = $name;
            let this = $args.this().into();
            if !usp_recv($scope, this) { return; }
            usp_pull($scope, this);
            let mut $pairs = match usp_pairs($scope, this) { Some(p) => p, None => return };
            $body
            usp_set($scope, this, $pairs);
            usp_notify($scope, this);
        }
    };
}

usp_read!(cb_usp_get, "get", |pairs, args, scope, rv| {
    let k = args.get(0).to_rust_string_lossy(scope);
    match pairs.iter().find(|x| x.0 == k) { Some((_, v)) => { let s = v8::String::new(scope, v).unwrap(); rv.set(s.into()); } None => rv.set_null() }
});
usp_read!(cb_usp_get_all, "getAll", |pairs, args, scope, rv| {
    let k = args.get(0).to_rust_string_lossy(scope);
    let vals: Vec<&String> = pairs.iter().filter(|x| x.0 == k).map(|x| &x.1).collect();
    let arr = v8::Array::new(scope, vals.len() as i32);
    for (i, v) in vals.iter().enumerate() { let s = v8::String::new(scope, v).unwrap(); arr.set_index(scope, i as u32, s.into()); }
    rv.set(arr.into());
});
usp_read!(cb_usp_has, "has", |pairs, args, scope, rv| {
    let k = args.get(0).to_rust_string_lossy(scope);
    // has(name, value): if a value is given, match BOTH name and value (WHATWG 2023 addition).
    if args.length() >= 2 && !args.get(1).is_undefined() {
        let v = args.get(1).to_rust_string_lossy(scope);
        rv.set_bool(pairs.iter().any(|x| x.0 == k && x.1 == v));
    } else {
        rv.set_bool(pairs.iter().any(|x| x.0 == k));
    }
});
usp_read!(cb_usp_size, "size", |pairs, _args, _scope, rv| { rv.set_uint32(pairs.len() as u32); });
usp_read!(cb_usp_to_string, "toString", |pairs, _args, scope, rv| {
    let s = usp_serialize(&pairs); let v = v8::String::new(scope, &s).unwrap(); rv.set(v.into());
});
// The iterators walk the list as it stands at each `next()` (`webidl_iter::live_pairs`), as Chrome 150's do.
fn usp_iter<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue, what: u32) {
    let this = args.this().into();
    if !usp_recv(scope, this) { return; }
    let it = live_pair_iter(scope, "URLSearchParams", this, what);
    rv.set(it);
}
fn cb_usp_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue) { usp_iter(scope, &args, rv, 0) }
fn cb_usp_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue) { usp_iter(scope, &args, rv, 1) }
fn cb_usp_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue) { usp_iter(scope, &args, rv, 2) }

usp_write!(cb_usp_append, "append", |pairs, args, scope| {
    let k = args.get(0).to_rust_string_lossy(scope); let v = args.get(1).to_rust_string_lossy(scope);
    pairs.push((k, v));
});
usp_write!(cb_usp_delete, "delete", |pairs, args, scope| {
    let k = args.get(0).to_rust_string_lossy(scope);
    // delete(name, value): if a value is given, remove only entries matching BOTH name and value.
    if args.length() >= 2 && !args.get(1).is_undefined() {
        let v = args.get(1).to_rust_string_lossy(scope);
        pairs.retain(|x| !(x.0 == k && x.1 == v));
    } else {
        pairs.retain(|x| x.0 != k);
    }
});
usp_write!(cb_usp_set, "set", |pairs, args, scope| {
    let k = args.get(0).to_rust_string_lossy(scope); let v = args.get(1).to_rust_string_lossy(scope);
    // keep only the first occurrence of k, then overwrite it (or append if none)
    let mut seen = false;
    pairs.retain(|x| if x.0 == k { if seen { false } else { seen = true; true } } else { true });
    if let Some(e) = pairs.iter_mut().find(|x| x.0 == k) { e.1 = v; } else { pairs.push((k, v)); }
});
usp_write!(cb_usp_sort, "sort", |pairs, _args, _scope| {
    // URLSearchParams.sort compares names by CODE UNITS (UTF-16), not Unicode scalar values: an astral
    // char (surrogate pair, lead unit 0xD800-0xDBFF) sorts below U+E000..U+FFFF. Rust's str Ord is by
    // scalar value, so compare the UTF-16 encodings instead. sort_by is stable (equal names keep order).
    pairs.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
});

fn cb_usp_foreach<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !usp_recv(scope, args.this().into()) { return; }
    for_each_live_pairs(scope, &args, "URLSearchParams");
}

// ── The interface, from ONE table ───────────────────────────────────────────────

/// `new URLSearchParams(init)` — the real Rust constructor the table's `ctor` row names.
///
/// The op2 class template's own constructor used to BE the global (`dom/00-bootstrap.js` assigned it),
/// which is why `URLSearchParams.length` read 2 (deno_ops counts the `scope` and `init` parameters)
/// where Chrome reads 0, and why the prototype enumerated `constructor` FIRST instead of last. This
/// callback is the constructor `install::live::build_newable` builds, so both come from the table.
/// The body is unchanged: [`URLSearchParamsBehavior::construct`], the same one the op2 `#[constructor]`
/// dispatches to.
pub fn cb_usp_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::binding::protos::get_prop;
    use crate::install::live::set_table_proto;
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("URLSearchParams"));
        return;
    }
    // The init parse runs FIRST and its TypeError returns immediately: `make_cppgc_object` is a V8
    // call, and a V8 call made with an exception already pending swallows it.
    let Ok(pairs) = usp_init_pairs(scope, args.get(0)) else { return };
    let obj = crate::binding::class::mint(
        scope,
        URLSearchParams { p: std::cell::RefCell::new(pairs), url: std::cell::RefCell::new(None) },
    );
    // A subclass (`class X extends URLSearchParams`) must land on ITS prototype; the plain call lands
    // on the table's, never on the cppgc class template's (the brief's failure mode 1).
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => set_table_proto(scope, obj, "URLSearchParams"),
    }
    rv.set(obj.into());
}

/// `URLSearchParams.prototype[Symbol.iterator]` — the one member no row can carry.
///
/// WebIDL's `iterable<USVString, USVString>` makes `@@iterator` the SAME function object as
/// `entries` (Chrome: `URLSearchParams.prototype[Symbol.iterator] === URLSearchParams.prototype.entries`,
/// measured), so it cannot be a row: a row builds its own function, with its own name. It is also
/// absent from the capture's `members` list (the dumper reads `getOwnPropertyNames`, which skips
/// symbols) and recorded instead as `"iterable": true`. So it is aliased here, right after the rows
/// are in, exactly as `dom/08g-observers.js` did -- same value, same `{writable, non-enumerable,
/// configurable}` descriptor.
pub fn install_usp_iterator(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>) {
    use crate::binding::protos::get_prop;
    let Some(entries) = get_prop(scope, proto, "entries") else { return };
    let key = v8::Symbol::get_iterator(scope);
    let mut d = v8::PropertyDescriptor::new_from_value_writable(entries, true);
    d.set_enumerable(false);
    d.set_configurable(true);
    proto.define_property(scope, key.into(), &d);
}

/// The body every row of [`URLSEARCHPARAMS`] names.
pub fn install_url_search_params(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{getter_row, method_row};
    match member.name {
        "size" => getter_row(scope, proto, member, cb_usp_size),
        "append" => method_row(scope, proto, member, cb_usp_append),
        "delete" => method_row(scope, proto, member, cb_usp_delete),
        "get" => method_row(scope, proto, member, cb_usp_get),
        "getAll" => method_row(scope, proto, member, cb_usp_get_all),
        "has" => method_row(scope, proto, member, cb_usp_has),
        "set" => method_row(scope, proto, member, cb_usp_set),
        "sort" => method_row(scope, proto, member, cb_usp_sort),
        "toString" => method_row(scope, proto, member, cb_usp_to_string),
        "entries" => method_row(scope, proto, member, cb_usp_entries),
        "forEach" => method_row(scope, proto, member, cb_usp_foreach),
        "keys" => method_row(scope, proto, member, cb_usp_keys),
        "values" => method_row(scope, proto, member, cb_usp_values),
        other => debug_assert!(false, "install_url_search_params: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `URLSearchParams` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s but for `exposed`), in the capture's order.
webapi::interface! {
    "URLSearchParams" [value, both] {
        ctor   "URLSearchParams"          len 0             = stub,
        attr   "size"     : Object readonly                 = install_url_search_params,
        method "append"   : Object len 2 required 2         = install_url_search_params,
        method "delete"   : Object len 1 required 1         = install_url_search_params,
        method "get"      : Object len 1 required 1 nullable = install_url_search_params,
        method "getAll"   : Object len 1 required 1         = install_url_search_params,
        method "has"      : Object len 1 required 1         = install_url_search_params,
        method "set"      : Object len 2 required 2         = install_url_search_params,
        method "sort"     : Object len 0                    = install_url_search_params,
        method "toString" : Object len 0                    = install_url_search_params,
        method "entries"  : Object len 0                    = install_url_search_params,
        method "forEach"  : Object len 1                    = install_url_search_params,
        method "keys"     : Object len 0                    = install_url_search_params,
        method "values"   : Object len 0                    = install_url_search_params
    }
}
