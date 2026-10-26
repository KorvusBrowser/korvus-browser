//! `modules/fetch/headers.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::core::dom::webidl_iter::{for_each_live_pairs, live_pair_iter};

// ── design notes that came with this code out of bindings.rs (STEP 3) ──
// Native Headers (Fetch) -- op2 class so constructor.name is "Headers" and every method is [native
// code] with correct name/length. State is Fetch's header LIST, names as the page spelled them (no OpState => no op2
// length bug). The union-init constructor takes `scope` (op2 constructors accept it, unlike methods)
// to walk an array/object/Headers. forEach needs scope to call back, so it is a raw-callback method
// installed on the prototype (op_headers_foreach); the rest are scope-free op2 methods.

#[repr(C)]
#[derive(CppgcBase)]
pub struct Headers {
    h: std::cell::RefCell<Vec<(String, String)>>,
    /// Fetch's header GUARD -- five values, not a bool (CONF GUARD1). It decides which names a
    /// mutation of this list may touch; see [`Guard`].
    guard: std::cell::Cell<Guard>,
}

// ── Fetch's header guard ────────────────────────────────────────────────────────────────────────
//
// The guard is a property of the header LIST, fixed when the list is created and never reassigned
// by a page. It has five values, and this engine had one bit -- `immutable: Cell<bool>` -- so every
// other guard was `none` and `new Request("/x", {headers: {cookie: "x"}}).headers.get("cookie")`
// answered `"x"` where Chrome answers `null` (CONF GUARD1, measured before and after).
//
// A dropped name is a SILENT NO-OP, not a throw. Only `immutable` throws. Both measured.

/// Fetch's five header guards.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Guard {
    /// A bare `new Headers()`, and a `new Headers(other)` copy of any list whatever ITS guard
    /// (measured: `new Headers(new Request("/x", {headers: {}}).headers).set("cookie", "x")` sticks).
    #[default]
    None,
    /// A `Request`'s headers.
    Request,
    /// A `no-cors` `Request`'s headers.
    RequestNoCors,
    /// A `Response`'s headers -- the constructor's and `Response.json`'s.
    Response,
    /// A fetched response's headers, `Response.error()`'s and `Response.redirect()`'s. The only
    /// guard that THROWS rather than dropping.
    Immutable,
}

/// THE MEASUREMENT, read once. `include_str!` of the probe's own output file rather than a Rust
/// copy of it: the JSON is the reading, and a second list in this file is exactly the "one spec
/// algorithm implemented as several subsets" defect this goal exists to remove. (The precedent is
/// `core/trusted_types/mod.rs`, which reads `trusted_types_type_map.json` the same way.)
const GUARD_JSON: &str = include_str!("../../../../../tools/sync/data/header_guard.json");

struct GuardData {
    forbidden_request: Vec<String>,
    forbidden_request_prefixes: Vec<String>,
    forbidden_response: Vec<String>,
    cors_safelisted: Vec<String>,
}

fn guard_data() -> &'static GuardData {
    static ONCE: std::sync::OnceLock<GuardData> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(GUARD_JSON).expect("tools/sync/data/header_guard.json is JSON");
        let list = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(|a| a.as_array())
                .unwrap_or_else(|| panic!("header_guard.json has `{k}`"))
                .iter()
                .filter_map(|s| s.as_str().map(str::to_ascii_lowercase))
                .collect()
        };
        GuardData {
            forbidden_request: list("forbidden_request_header_names"),
            forbidden_request_prefixes: list("forbidden_request_header_prefixes"),
            forbidden_response: list("forbidden_response_header_names"),
            cors_safelisted: list("cors_safelisted_request_headers"),
        }
    })
}

/// A "forbidden request-header name": one of the measured names, OR one carrying a measured PREFIX.
///
/// The prefixes are not a shorthand for the list -- `proxy-foo` and `sec-foo` are invented names and
/// both drop, so a fixed list would pass every test that uses a real name and fail every generated
/// one (`header-guard-probe.html`, `_prefix_evidence`).
pub(crate) fn is_forbidden_request_header_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let d = guard_data();
    d.forbidden_request.iter().any(|f| *f == n) || d.forbidden_request_prefixes.iter().any(|p| n.starts_with(p.as_str()))
}

/// Blink's `cors::IsForbiddenRequestHeader(name, value)` (`platform/loader/cors/cors.cc:142`, over
/// `net::HttpUtil::IsSafeHeader`, `net/http/http_util.cc:326-412`): a forbidden NAME, or one of the method-override
/// names whose value lists -- comma-separated, LWS-trimmed -- a forbidden method (`CONNECT`, `TRACE`, `TRACK`). The
/// value is judged as given (Blink normalizes it first; trimming does not change a token). XHR's `setRequestHeader`
/// and a `request`-guarded header list both ask it.
///
/// Chromium 152's list no longer names `access-control-request-private-network` (Chromium 150's did): a page may set
/// it, and it goes on the wire as written.
pub(crate) fn is_forbidden_request_header(name: &str, value: &str) -> bool {
    if is_forbidden_request_header_name(name) {
        return true;
    }
    let overrides = ["x-http-method", "x-http-method-override", "x-method-override"];
    overrides.iter().any(|o| name.eq_ignore_ascii_case(o))
        && value
            .split(',')
            .map(|m| m.trim_matches(|c| c == ' ' || c == '\t'))
            .any(|m| ["connect", "trace", "track"].iter().any(|f| m.eq_ignore_ascii_case(f)))
}

/// Fetch's "normalize" of a header value (Blink's `FetchUtils::NormalizeHeaderValue`): leading and trailing HTTP
/// whitespace -- tab, LF, CR, space -- removed. Chrome 150 sends XHR's `setRequestHeader('X-B', '  v  ')` and
/// `fetch(u, {headers: {'X-B': '  v  '}})` as `X-B: v`; korvus sent `v  `.
pub(crate) fn normalize_header_value(v: &str) -> String {
    v.trim_matches(|c| matches!(c, '\t' | '\n' | '\r' | ' ')).to_string()
}

/// Blink's `IsValidHTTPToken`: a header NAME -- non-empty, every character an RFC 7230 `tchar`.
pub(crate) fn is_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) && name.is_ascii()
}

/// Blink's `IsValidHTTPHeaderValue`: Latin-1, and no CR, LF or NUL.
pub(crate) fn is_header_value(value: &str) -> bool {
    value.chars().all(|c| (c as u32) <= 0xFF && !matches!(c, '\r' | '\n' | '\0'))
}

/// A "forbidden response-header name". Note the asymmetry a copied list gets wrong: `set-cookie` is
/// on BOTH this list and the request one, `set-cookie2` only on this one.
fn is_forbidden_response_header_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    guard_data().forbidden_response.iter().any(|f| *f == n)
}

/// Is `name` one of the four CORS-safelisted request-header NAMES, ignoring its value? Only
/// `delete` asks this: it has no value to judge, where `append`/`set` go through the value-aware
/// [`super::fetch_global::no_cors_safelisted`].
fn is_cors_safelisted_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    guard_data().cors_safelisted.iter().any(|f| *f == n)
}

/// THE guard check -- ONE copy, shared by `append`, `set` and `delete`.
///
/// `Err(())` is "throw", `Ok(false)` is "drop this mutation silently", `Ok(true)` is "go ahead".
/// `value` is `None` for `delete`, which has none.
///
/// The `request-no-cors` arm calls `fetch_global`'s `no_cors_safelisted` rather than carrying its
/// own copy of the safelist. That function was already the wire path's filter; it is now also the
/// guard's, so the two cannot drift. Measured (CONF GUARD1, widened probe case A): the safelist IS
/// value-aware -- `content-type: application/json`, a 200-byte `accept` and `accept-language: (`
/// all DROP under `request-no-cors`, while `content-type: text/plain` sticks.
fn guard_allows(guard: Guard, name: &str, value: Option<&str>) -> Result<bool, ()> {
    match guard {
        Guard::Immutable => Err(()),
        Guard::None => Ok(true),
        // value-aware, as Blink's `cors::IsForbiddenRequestHeader` is (`delete` asks with the empty value)
        Guard::Request => Ok(!is_forbidden_request_header(name, value.unwrap_or(""))),
        Guard::RequestNoCors => Ok(match value {
            Some(v) => super::fetch_global::no_cors_safelisted(name, v),
            None => is_cors_safelisted_name(name),
        }),
        Guard::Response => Ok(!is_forbidden_response_header_name(name)),
    }
}

/// Put `headers` (a `Headers` instance) under `guard`, and drop whatever the guard forbids.
///
/// Fetch fills a guarded header list by APPENDING each entry through the guard, so a guard applied
/// after the fill must drop the same entries the guarded fill would never have taken. So this does
/// not `retain` -- it REPLAYS the fill, appending each entry through the same rule in order, which
/// matters because `append`'s `request-no-cors` check reads the entries already kept (see
/// [`combined_value`]). This engine builds the list first (`new Headers(init)`) and stamps the guard
/// here, so the filtering is not optional: without it, `new Request("/x", {headers: {cookie: "x"}})`
/// would keep the cookie and only LATER mutations would be filtered. Measured (widened probe case
/// C): Chrome drops it at fill time.
///
/// `immutable` is the exception and must not filter: a fetched response's header list legitimately
/// holds `set-cookie`, and it is exactly the list this guard is stamped onto.
pub(crate) fn set_guard(scope: &mut v8::PinScope, headers: v8::Local<v8::Value>, guard: Guard) {
    let Some(hdr) = deno_core::cppgc::try_unwrap_cppgc_object::<Headers>(scope, headers) else { return };
    let hdr = unsafe { hdr.as_ref() };
    if guard != Guard::Immutable {
        let old = std::mem::take(&mut *hdr.h.borrow_mut());
        let mut kept: Vec<(String, String)> = Vec::with_capacity(old.len());
        for (k, v) in old {
            let judged = match guard {
                Guard::RequestNoCors => combined_value(&kept, &k, &v),
                _ => v.clone(),
            };
            if guard_allows(guard, &k, Some(&judged)) == Ok(true) {
                kept.push((k, v));
            }
        }
        *hdr.h.borrow_mut() = kept;
    }
    hdr.guard.set(guard);
}

/// Fetch's "temporary value" for an `append` under `request-no-cors`: what `get(name)` WOULD answer
/// once `value` is appended -- the values already in `list` for `name`, then `value`, joined with
/// `", "`. The guard judges THAT, not the new value alone.
///
/// Measured on Chrome 150.0.7871.46 (`header-guard-probe.html`, CASES J), with controls:
///
/// ```text
///   append accept = 100 bytes, then another 100      -> get() is 100 bytes: the SECOND was dropped
///   append accept = 120 "s", "", 120 "s"             -> 122 bytes, not 244
///   append accept-language = "en", then "("          -> "en"
///   append content-type = "text/plain", then "application/json" -> "text/plain"
///   CONTROL two 10-byte appends                      -> 22 bytes, both kept
///   CONTROL the same pair under guard `none`         -> 202 bytes
///   CONTROL `set` twice                              -> 100 bytes; `set` REPLACES, so it never combines
/// ```
///
/// Without it `headers-no-cors.any.js` fails its seven "cannot have <name> set to <long>, <long>"
/// subtests: each value passes the 128-byte cap alone and the pair does not.
fn combined_value(list: &[(String, String)], name: &str, value: &str) -> String {
    let mut out = String::new();
    for (_, v) in list.iter().filter(|x| x.0.eq_ignore_ascii_case(name)) {
        out.push_str(v);
        out.push_str(", ");
    }
    out.push_str(value);
    out
}

/// The guard `headers` is under, or `none` for anything that is not a `Headers`.
pub(crate) fn guard_of(scope: &mut v8::PinScope, headers: v8::Local<v8::Value>) -> Guard {
    deno_core::cppgc::try_unwrap_cppgc_object::<Headers>(scope, headers).map(|h| unsafe { h.as_ref() }.guard.get()).unwrap_or(Guard::None)
}

/// The TypeError a mutation of an immutable header list throws, in Chrome's words (measured:
/// `(await fetch(u)).headers.set('x', '1')`).
fn immutable_error(member: &str) -> ::deno_error::JsErrorBox {
    ::deno_error::JsErrorBox::type_error(format!("Failed to execute '{member}' on 'Headers': Headers are immutable"))
}

pub(crate) trait HeadersBehavior {
   fn construct(_scope: &mut v8::PinScope, _init: v8::Local<v8::Value>) -> Headers where Self: Sized { stub!("Headers::constructor") }
   fn append(&self, _k: String, _v: String)  { stub!("Headers::append") }
   fn delete_(&self, _k: String)  { stub!("Headers::delete") }
   fn get(&self, _k: String) -> Option<String> { stub!("Headers::get") }
   fn get_set_cookie(&self) -> Vec<String> { stub!("Headers::getSetCookie") }
   fn has(&self, _k: String) -> bool { stub!("Headers::has") }
   fn set(&self, _k: String, _v: String)  { stub!("Headers::set") }
}

// Declaration order == prototype install order == the Brave-151 baseline enumeration order
// (append, delete, get, getSetCookie, has, set); entries/forEach/keys/values are installed after the
// class by dom.js (they need scope), completing the baseline order (entries, forEach, keys, values).
#[webapi(base, own)]
impl Headers {
    #[constructor]
    #[cppgc]
    fn constructor(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Result<Headers, ::deno_error::JsErrorBox> { dispatch!("Headers::constructor", <Headers as HeadersBehavior>::construct(scope, init)) }
    #[fast]
    // `append` judges the COMBINED value under `request-no-cors` -- see [`combined_value`]. `set`
    // does not, because `set` REPLACES; both measured.
    fn append(&self, #[string] k: String, #[string] v: String) -> Result<(), ::deno_error::JsErrorBox> { let v = normalize_header_value(&v); let judged = match self.guard.get() { Guard::RequestNoCors => combined_value(&self.h.borrow(), &k, &v), _ => v.clone() }; match guard_allows(self.guard.get(), &k, Some(&judged)) { Err(()) => return Err(immutable_error("append")), Ok(false) => return Ok(()), Ok(true) => {} } dispatch!("Headers::append", <Headers as HeadersBehavior>::append(self, k, v)) }
    #[fast]
    #[rename("delete")]
    fn delete_(&self, #[string] k: String) -> Result<(), ::deno_error::JsErrorBox> { match guard_allows(self.guard.get(), &k, None) { Err(()) => return Err(immutable_error("delete")), Ok(false) => return Ok(()), Ok(true) => {} } dispatch!("Headers::delete", <Headers as HeadersBehavior>::delete_(self, k)) }
    #[string]
    fn get(&self, #[string] k: String) -> Result<Option<String>, ::deno_error::JsErrorBox> { dispatch!("Headers::get", <Headers as HeadersBehavior>::get(self, k)) }
    #[serde]
    fn get_set_cookie(&self) -> Result<Vec<String>, ::deno_error::JsErrorBox> { dispatch!("Headers::getSetCookie", <Headers as HeadersBehavior>::get_set_cookie(self)) }
    #[fast]
    fn has(&self, #[string] k: String) -> Result<bool, ::deno_error::JsErrorBox> { dispatch!("Headers::has", <Headers as HeadersBehavior>::has(self, k)) }
    #[fast]
    fn set(&self, #[string] k: String, #[string] v: String) -> Result<(), ::deno_error::JsErrorBox> { let v = normalize_header_value(&v); match guard_allows(self.guard.get(), &k, Some(&v)) { Err(()) => return Err(immutable_error("set")), Ok(false) => return Ok(()), Ok(true) => {} } dispatch!("Headers::set", <Headers as HeadersBehavior>::set(self, k, v)) }
}

impl HeadersBehavior for Headers {
    // A `new Headers(...)` is ALWAYS guard `none`, whatever it was built from -- measured (widened
    // probe case I3): `new Headers(new Request("/x", {headers: {}}).headers).set("cookie", "x")`
    // sticks. The callers that need another guard stamp it with `set_guard` right after.
    fn construct(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Headers { let hdr = Headers { h: std::cell::RefCell::new(Vec::new()), guard: std::cell::Cell::new(Guard::None) };
        // `HeadersInit` is `(sequence<sequence<ByteString>> or record<ByteString, ByteString>)`: an object's
        // `@@iterator` decides which, read as WebIDL reads it (Chrome 150) -- any iterable is a
        // sequence of pairs, anything else a record (korvus took only an Array, and never asked for `@@iterator`).
        // A Headers is iterable like any other: its `@@iterator` yields Fetch's "sort and combine" -- each name once,
        // LOWERCASE, its values joined -- which is the list the copy is filled from (Blink's generated union
        // conversion, `ScriptIterator::FromIterable`). So `fetch(u, {headers: h})` with `h.append('Accept', 'a/b')`
        // goes out as `accept: a/b` in Chrome 150; korvus copied `h`'s own list and its
        // spelling. A copy the ENGINE makes (`Request.clone()`, a Request's inherited headers) is [`headers_clone`].
        let seq = match v8::Local::<v8::Object>::try_from(init) {
            Ok(o) => crate::install::ctor_dicts::sequence_or_record(scope, o, "Failed to construct 'Headers': ").ok().flatten(),
            _ => None,
        };
        if let Some(items) = seq {
            for pair in items {
                if let Ok(pa) = v8::Local::<v8::Array>::try_from(pair) {
                    let k = pa.get_index(scope, 0).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                    let v = pa.get_index(scope, 1).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                    <Headers as HeadersBehavior>::append(&hdr, k, v);
                }
            }
        } else if init.is_object() {
            if let Some(obj) = init.to_object(scope) {
                if let Some(keys) = obj.get_own_property_names(scope, v8::GetPropertyNamesArgs::default()) {
                    for i in 0..keys.length() {
                        if let Some(key) = keys.get_index(scope, i) {
                            if let Some(val) = obj.get(scope, key) {
                                let (k, v) = (key.to_rust_string_lossy(scope), val.to_rust_string_lossy(scope));
                                <Headers as HeadersBehavior>::append(&hdr, k, v);
                            }
                        }
                    }
                }
            }
        }
        hdr }
    // THE LIST KEEPS THE PAGE'S SPELLING (Fetch "append": a name the list already holds takes that header's
    // spelling), and every lookup ignores case. Chrome 150 sends `fetch(u, {headers: {'X-Foo': '1', 'x-bar': '2'}})`
    // as `X-Foo` and `x-bar` on an HTTP/1.1 wire; korvus lowercased both. An iteration still
    // yields lowercase names ([`headers_pairs`]).
    fn append(&self, k: String, v: String)  {
        // Fetch "append" step 1: the value normalized ([`normalize_header_value`]).
        let v = normalize_header_value(&v);
        let mut b = self.h.borrow_mut();
        let k = b.iter().find(|x| x.0.eq_ignore_ascii_case(&k)).map(|x| x.0.clone()).unwrap_or(k);
        b.push((k, v)); }
    fn delete_(&self, k: String)  { self.h.borrow_mut().retain(|x| !x.0.eq_ignore_ascii_case(&k)); }
    fn get(&self, k: String) -> Option<String> {
        let vs: Vec<String> = self.h.borrow().iter().filter(|x| x.0.eq_ignore_ascii_case(&k)).map(|x| x.1.clone()).collect();
        if vs.is_empty() { None } else { Some(vs.join(", ")) } }
    // Per the Fetch spec, set-cookie is NOT combined by `get` (it stays on the forbidden-combine list);
    // getSetCookie surfaces each individual value instead.
    fn get_set_cookie(&self) -> Vec<String> { self.h.borrow().iter().filter(|x| x.0.eq_ignore_ascii_case("set-cookie")).map(|x| x.1.clone()).collect() }
    fn has(&self, k: String) -> bool { self.h.borrow().iter().any(|x| x.0.eq_ignore_ascii_case(&k)) }
    // Fetch "set": the first header of that name takes the value -- and keeps its spelling -- and the others go.
    fn set(&self, k: String, v: String)  {
        let v = normalize_header_value(&v);
        let mut b = self.h.borrow_mut();
        match b.iter().position(|x| x.0.eq_ignore_ascii_case(&k)) {
            Some(i) => {
                b[i].1 = v;
                let name = b[i].0.clone();
                let mut seen = false;
                b.retain(|x| {
                    if !x.0.eq_ignore_ascii_case(&name) {
                        return true;
                    }
                    let first = !seen;
                    seen = true;
                    first
                });
            }
            None => b.push((k, v)),
        } }
}

/// The pairs an iteration of `this` visits: Fetch's "sort and combine". Names sorted,
/// every name's values joined with ", " -- except `set-cookie`, whose values stay separate entries.
/// Measured: `[...new Headers([['b','1'],['a','2'],['A','3'],['set-cookie','x'],['set-cookie','y']])]`
/// is `a: "2, 3"`, `b: "1"`, `set-cookie: "x"`, `set-cookie: "y"`; this engine answered the insertion
/// order with `a` twice.
pub(crate) fn headers_pairs(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<Vec<(String, String)>> {
    let hdr = deno_core::cppgc::try_unwrap_cppgc_object::<Headers>(scope, this)?;
    let pairs = unsafe { hdr.as_ref() }.h.borrow().iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect();
    Some(sort_and_combine(pairs))
}

/// The list a REQUEST carries onto the wire: each name once, as the page first spelled it, its values joined
/// with ", " in list order (Blink's `HTTPHeaderMap`, which keeps a header's first spelling). Not sorted: the
/// transport puts the names in Blink's map order.
pub(crate) fn headers_wire_pairs(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<Vec<(String, String)>> {
    let hdr = deno_core::cppgc::try_unwrap_cppgc_object::<Headers>(scope, this)?;
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in unsafe { hdr.as_ref() }.h.borrow().iter() {
        match out.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            Some((_, joined)) => {
                joined.push_str(", ");
                joined.push_str(v);
            }
            None => out.push((k.clone(), v.clone())),
        }
    }
    Some(out)
}

/// A copy of `headers`' list the ENGINE makes -- Blink's `Headers::Clone` / `FetchHeaderList::Clone`: every entry as it
/// is, spelling and order kept, its guard carried; no page code runs. `Request.clone()`, `Response.clone()` and a
/// `Request` built from another without `init.headers` copy this way (`new Headers(h)` iterates `h` instead).
pub(crate) fn headers_clone<'s>(scope: &mut v8::PinScope<'s, '_>, headers: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    let src = deno_core::cppgc::try_unwrap_cppgc_object::<Headers>(scope, headers)?;
    let (list, guard) = {
        let src = unsafe { src.as_ref() };
        (src.h.borrow().clone(), src.guard.get())
    };
    let copy: v8::Local<v8::Value> = crate::binding::protos::new_global(scope, "Headers", &[])?.into();
    let dst = deno_core::cppgc::try_unwrap_cppgc_object::<Headers>(scope, copy)?;
    let dst = unsafe { dst.as_ref() };
    *dst.h.borrow_mut() = list;
    dst.guard.set(guard);
    Some(copy)
}

fn sort_and_combine(mut pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<(String, String)> = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        match out.last_mut() {
            Some((lk, lv)) if *lk == k && k != "set-cookie" => {
                lv.push_str(", ");
                lv.push_str(&v);
            }
            _ => out.push((k, v)),
        }
    }
    out
}

fn cb_headers_foreach<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    for_each_live_pairs(scope, &args, "Headers");
}

fn cb_headers_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let it = live_pair_iter(scope, "Headers", args.this().into(), 0);
    rv.set(it);
}

fn cb_headers_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let it = live_pair_iter(scope, "Headers", args.this().into(), 1);
    rv.set(it);
}

fn cb_headers_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let it = live_pair_iter(scope, "Headers", args.this().into(), 2);
    rv.set(it);
}


/// The four rows of [`HEADERS`] the class template cannot realise, plus the `@@iterator` alias.
///
/// `#[op2] impl` gives a method no scope, and all four of these need one -- `forEach` to call back,
/// the other three to build an Array Iterator. They were an ESM pass behind four ops
/// (`op_headers_entries` and friends, deleted with it); they are rows now, mounted in table order after
/// the six generated ones, which is the capture's order. (Once the op2 class template
/// realised the six and `installer::verify_native`, deleted then, mounted these four after them.)
pub fn install_headers(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::rows::method_row;
    match member.name {
        "entries" => method_row(scope, proto, member, cb_headers_entries),
        "forEach" => method_row(scope, proto, member, cb_headers_foreach),
        "keys" => method_row(scope, proto, member, cb_headers_keys),
        "values" => method_row(scope, proto, member, cb_headers_values),
        // A Fetch `iterable<>` declaration's `@@iterator` IS its `entries` -- the SAME function
        // object (Chrome 151, measured: `Headers.prototype[Symbol.iterator] === Headers.prototype.entries`),
        // non-enumerable, writable, configurable. The row is LAST so `entries` is already mounted
        // when this reads it back -- and reading it back is what makes the two the same object even
        // though `entries` went through the installer's receiver check.
        "@@iterator" => {
            let k: v8::Local<v8::Value> = crate::binding::protos::member_key(scope, "entries").into();
            if let Some(f) = proto.get(scope, k) {
                crate::install::kits::define_value(scope, proto, member, f, true);
            }
        }
        other => debug_assert!(false, "install_headers: unexpected member {other}"),
    }
}

// ── One-table definition ─────────────────────────────────────────────────────────
//
// Rows VERBATIM from the `Headers` block of `install/generated_registry.rs` (the capture's order,
// webref's types/lengths/required counts), plus the `@@iterator` row the surface dump records as
// `iterable: true` but cannot list among the string members.
//
// SIX rows are `cppgc` -- the members `#[webapi(own)]` generated from the `impl` above (`class Headers`),
// mounted like any row: `required` is the WebIDL arity guard (`headers.get()` once looked up a header
// literally named "undefined" and answered null, where Chrome throws) and a Rust `()` is a real
// `undefined`. (Once an op2 class template realised them and `installer::verify_native`,
// deleted then, finished each from its row.) FOUR are
// `Install`, because they need a scope the template cannot give them.
//
// `[value, both]`: `Headers` is `[Exposed=(Window,Worker)]` and one table serves both realms (goal
// invariant I8). The WINDOW takes the class template through `js/dom/00-bootstrap.js`; the WORKER had
// no assignment at all, so its `Headers` was a registry SHELL -- `new Headers(); h.append("a","1");
// h.get("a")` answered **null** and `[...new Headers([["a","1"]])]` threw "not iterable". The install
// (`install::live::fetch_values`) puts the same class template on whichever realm's global lacks it.
webapi::interface! {
    "Headers" [value, both, class Headers] {
        ctor          "Headers"                             len 0 = stub,
        method        "append"       : Undefined len 2 required 2 = cppgc,
        method        "delete"       : Undefined len 1 required 1 = cppgc,
        method        "get"          : DomString len 1 required 1 nullable = cppgc,
        method        "getSetCookie" : Sequence  len 0            = cppgc,
        method        "has"          : Boolean   len 1 required 1 = cppgc,
        method        "set"          : Undefined len 2 required 2 = cppgc,
        method        "entries"      : Any       len 0            = install_headers,
        method        "forEach"      : Any       len 1            = install_headers,
        method        "keys"         : Any       len 0            = install_headers,
        method        "values"       : Any       len 0            = install_headers,
        method        "@@iterator"   : Any       len 0 hidden     = install_headers
    }
}
