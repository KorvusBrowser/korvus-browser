//! `modules/cookie_store/cookie_store.rs` — the `CookieStore` interface: `get`, `getAll`, `set`, `delete` over the
//! one cookie store (`transport::cookies`), as Blink 150's `cookie_store.cc` asks `RestrictedCookieManager` --
//! measured on stock Chrome 150 first (`core/korvus-runtime/tests/fixtures/cookies`). It kept a cookie list of its
//! own: `getAll()` never saw a response's cookie or a `document.cookie` write's path, and every item came back
//! `sameSite: "strict"`, `path: "/"`.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use transport::cookies::{CookieInit, CookieItem, ScriptContext};
use crate::modules::crypto::shared::{ScErr, sc_settle};
use crate::modules::cookie_store::jar::{cookie_emit, cookie_to_obj, realm_script_context, with_harness};
use crate::binding::protos::{get_prop, resolved_promise};
use crate::install::live::method_row;
use crate::install::MemberDef;

#[repr(C)]
#[derive(CppgcBase)]
pub struct CookieStore {
    /// cppgc needs a non-zero-sized base; the store holds no state of its own (its `onchange` is the
    /// one event-handler store's, core/events/event_target.rs).
    _brand: std::cell::Cell<u8>,
}

pub(crate) trait CookieStoreBehavior {
   fn construct() -> CookieStore where Self: Sized { stub!("CookieStore::constructor") }
}

#[webapi(base, own)]
impl CookieStore {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<CookieStore, ::deno_error::JsErrorBox> { dispatch!("CookieStore::constructor", <CookieStore as CookieStoreBehavior>::construct()) }
}

impl CookieStoreBehavior for CookieStore {
    fn construct() -> CookieStore { CookieStore { _brand: std::cell::Cell::new(0) } }
}

/// A `cookieStore` method's first argument, as WebIDL resolves the method's two overloads (Chrome 150):
/// an object, `null` or `undefined` (or no argument) is the options dictionary `dict` -- converted, its
/// members read once each in WebIDL's order -- and anything else the cookie's name, a string. `Err` is the exception
/// to reject with: these methods answer a promise. korvus read `name` alone and named `CookieStoreDeleteOptions` in
/// every refusal.
enum CookieArg<'s> {
    Name(String),
    Dict(v8::Local<'s, v8::Object>),
}

fn cookie_arg<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, method: &str, dict: &str) -> Result<CookieArg<'s>, v8::Local<'s, v8::Value>> {
    let prefix = format!("Failed to execute '{method}' on 'CookieStore': ");
    let a = args.get(0);
    if args.length() >= 1 && !(a.is_null_or_undefined() || a.is_object()) {
        return cookie_string(scope, a, &prefix).map(CookieArg::Name);
    }
    crate::install::ctor_dicts::convert_dict_caught(scope, a, dict, &prefix).map(CookieArg::Dict)
}

/// A USVString argument of a promise-returning method: a Symbol is a TypeError to reject with.
fn cookie_string<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>, prefix: &str) -> Result<String, v8::Local<'s, v8::Value>> {
    if v.is_symbol() {
        // Blink's context applied after the error is made
        return Err(deno_core::error::native_error(scope, "TypeError", &format!("{prefix}Cannot convert a Symbol value to a string")));
    }
    Ok(v.to_rust_string_lossy(scope))
}

/// A string member of a converted options dictionary, when it holds one (`null` and absent: `None`).
fn dict_str(scope: &mut v8::PinScope, d: v8::Local<v8::Object>, key: &str) -> Option<String> {
    get_prop(scope, d, key).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

/// A number member of a converted options dictionary (`null` and absent: `None`).
fn dict_num(scope: &mut v8::PinScope, d: v8::Local<v8::Object>, key: &str) -> Option<f64> {
    get_prop(scope, d, key).filter(|v| v.is_number()).and_then(|v| v.number_value(scope))
}

/// The promise a method answers with when it refuses: a TypeError with Blink's context
/// ("Failed to execute 'set' on 'CookieStore': ...").
fn reject_type<'s>(scope: &mut v8::PinScope<'s, '_>, method: &str, msg: &str) -> v8::Local<'s, v8::Value> {
    let e: Result<v8::Local<v8::Value>, ScErr> = Err(ScErr::Type(format!("Failed to execute '{method}' on 'CookieStore': {msg}")));
    sc_settle(scope, e)
}

fn resolved_undef<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    resolved_promise(scope, u)
}

/// Blink's `StripHttpWhitespace` (`CookieStoreAPIWhitespaceStripping`, on in Chrome 150).
fn strip(s: &str) -> &str {
    s.trim_matches([' ', '\t'])
}

/// What `set()` or `delete()` asked to write: a `CookieInit` as Blink's binding hands it over.
struct WriteArgs {
    name: String,
    value: String,
    domain: Option<String>,
    path: String,
    /// Milliseconds since the epoch; `None` for `null`.
    expires: Option<f64>,
    max_age: Option<i64>,
    same_site: String,
    partitioned: bool,
}

impl WriteArgs {
    /// `CookieStore::Delete`: the cookie written again with a value of `""` (`"deleted"` for a nameless one), expired at
    /// the epoch, `SameSite=Strict`.
    fn deletion(name: &str, domain: Option<String>, path: String, partitioned: bool) -> WriteArgs {
        let name = strip(name).to_string();
        let value = if name.is_empty() { "deleted" } else { "" }.to_string();
        WriteArgs { name, value, domain, path, expires: Some(0.0), max_age: None, same_site: "strict".into(), partitioned }
    }
}

/// What the backend is asked to store (`RestrictedCanonicalCookieParams`).
struct Params {
    name: String,
    value: String,
    domain: String,
    path: String,
    expires: Option<i64>,
    same_site: String,
    partitioned: bool,
}

fn now_micros() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

/// Blink's `ToCookieParams`: the parameters a write asks the backend to store, or the TypeError it is refused with --
/// before the backend sees it.
fn to_cookie_params(ctx: &ScriptContext, a: WriteArgs) -> Result<Params, String> {
    let name = strip(&a.name).to_string();
    let value = strip(&a.value).to_string();
    if name.is_empty() && value.contains('=') {
        return Err("Cookie value cannot contain '=' if the name is empty".into());
    }
    if name.is_empty() && value.is_empty() {
        return Err("Cookie name and value both cannot be empty".into());
    }
    if name.contains('=') {
        return Err("Cookie name cannot contain '='".into());
    }
    let expires = match (a.max_age, a.expires) {
        (Some(_), Some(_)) => return Err("Cookie expires and maxAge cannot both be specified".into()),
        (Some(ma), None) => Some(if ma <= 0 { i64::MIN } else { now_micros().saturating_add(ma.saturating_mul(1_000_000)) }),
        (None, Some(ms)) => Some((ms * 1000.0) as i64),
        (None, None) => None,
    };
    let lower = name.to_ascii_lowercase();
    let http = lower.starts_with("__http-");
    if http || lower.starts_with("__host-http-") {
        return Err(format!("Cookies with \"{}\" prefix cannot be set using the CookieStore API.", if http { "__Http-" } else { "__Host-Http-" }));
    }
    let host_prefixed = lower.starts_with("__host-");
    let domain = match a.domain {
        Some(d) => {
            if host_prefixed {
                return Err("Cookies with \"__Host-\" prefix cannot have a domain".into());
            }
            if d.starts_with('.') {
                return Err("Cookie domain cannot start with \".\"".into());
            }
            let d = format!(".{d}").to_ascii_lowercase();
            if !transport::cookies::domain_settable(ctx, &d) {
                return Err("Cookie domain must domain-match current host".into());
            }
            d
        }
        None => String::new(),
    };
    let mut path = a.path;
    if !path.is_empty() {
        if host_prefixed && path != "/" {
            return Err("Cookies with \"__Host-\" prefix cannot have a non-\"/\" path".into());
        }
        if !path.starts_with('/') {
            return Err("Cookie path must start with \"/\"".into());
        }
        if !path.ends_with('/') {
            path.push('/');
        }
    }
    if !transport::cookies::script_may_set_secure(ctx) {
        return Err("Cannot modify a secure cookie on insecure origin".into());
    }
    Ok(Params { name, value, domain, path, expires, same_site: a.same_site, partitioned: a.partitioned })
}

/// `CookieStore::DoWrite`: validate, ask the backend, tell the realm's change listeners, answer.
fn do_write<'s>(scope: &mut v8::PinScope<'s, '_>, method: &str, a: WriteArgs) -> v8::Local<'s, v8::Value> {
    const MALFORMED: &str = "Cookie was malformed and could not be stored, due to problem(s) while parsing.";
    let Some(ctx) = realm_script_context(scope) else { return reject_type(scope, method, MALFORMED) };
    let p = match to_cookie_params(&ctx, a) {
        Ok(p) => p,
        Err(msg) => return reject_type(scope, method, &msg),
    };
    let init = CookieInit { name: &p.name, value: &p.value, domain: &p.domain, path: &p.path, expires: p.expires, same_site: &p.same_site, partitioned: p.partitioned };
    match with_harness(scope, |h| transport::cookies::script_set(&ctx, &init, h)) {
        Some(changes) => {
            cookie_emit(scope, &changes);
            resolved_undef(scope)
        }
        None => reject_type(scope, method, MALFORMED),
    }
}

/// `CookieStore::DoRead`: the cookies the realm's document may read named `name` (all when `None`), for the
/// document's URL -- an `url` option must be that URL, its fragment aside. `Err`: the TypeError message.
fn do_read(scope: &mut v8::PinScope, name: Option<&str>, url: Option<&str>) -> Result<Vec<CookieItem>, String> {
    let Some(ctx) = realm_script_context(scope) else { return Ok(Vec::new()) };
    if let Some(u) = url {
        let doc = match crate::core::html::browsing_context::realm_cookie_context(scope) {
            Some(c) => c.0,
            None => crate::modules::cookie_store::jar::page_cookie_url(scope),
        };
        let same = url::Url::parse(&doc).ok().and_then(|d| {
            let mut r = d.join(u).ok()?;
            r.set_fragment(None);
            Some(r.as_str() == d.as_str())
        });
        if same != Some(true) {
            return Err("URL must match the document URL".into());
        }
    }
    let name = name.map(strip);
    Ok(with_harness(scope, |h| transport::cookies::script_cookies(&ctx, name, h)))
}

fn cb_cs_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !cs_recv(scope, args.this().into()) { return; }
    if let Some(e) = require_cookie_arg(&args, "set") {
        let r = sc_settle(scope, Err(e));
        rv.set(r);
        return;
    }
    // The two overloads: `set(USVString name, USVString value)` for two arguments, `set(CookieInit options)` for one
    // -- converted as WebIDL converts it: `domain`, `expires`, `maxAge`, `name` (REQUIRED), `partitioned`, `path`,
    // `sameSite`, `value` (REQUIRED), each read once; a refusal rejects.
    let a = if args.length() >= 2 {
        const PREFIX: &str = "Failed to execute 'set' on 'CookieStore': ";
        let name = match cookie_string(scope, args.get(0), PREFIX) { Ok(n) => n, Err(e) => { let p = crate::binding::protos::rejected_promise(scope, e); rv.set(p); return; } };
        let value = match cookie_string(scope, args.get(1), PREFIX) { Ok(v) => v, Err(e) => { let p = crate::binding::protos::rejected_promise(scope, e); rv.set(p); return; } };
        WriteArgs { name, value, domain: None, path: "/".into(), expires: None, max_age: None, same_site: "strict".into(), partitioned: false }
    } else {
        let d = match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), "CookieInit", "Failed to execute 'set' on 'CookieStore': ") {
            Ok(d) => d,
            Err(e) => { let p = crate::binding::protos::rejected_promise(scope, e); rv.set(p); return; }
        };
        WriteArgs {
            name: dict_str(scope, d, "name").unwrap_or_default(),
            value: dict_str(scope, d, "value").unwrap_or_default(),
            domain: dict_str(scope, d, "domain"),
            path: dict_str(scope, d, "path").unwrap_or_else(|| "/".into()),
            expires: dict_num(scope, d, "expires"),
            max_age: dict_num(scope, d, "maxAge").map(|n| n as i64),
            same_site: dict_str(scope, d, "sameSite").unwrap_or_else(|| "strict".into()),
            partitioned: get_prop(scope, d, "partitioned").is_some_and(|v| v.is_true()),
        }
    };
    let r = do_write(scope, "set", a);
    rv.set(r);
}

/// The argument count every `cookieStore` method checks before it touches the store: no argument at all is "1
/// argument required" (Brave 151 recorded `get()`, `set()` and `delete()` rejecting -- they had resolved). What the
/// argument IS is [`cookie_arg`]'s question.
fn require_cookie_arg(args: &v8::FunctionCallbackArguments, method: &str) -> Option<ScErr> {
    if args.length() < 1 {
        return Some(ScErr::Type(format!(
            "Failed to execute '{method}' on 'CookieStore': 1 argument required, but only 0 present."
        )));
    }
    None
}

fn cb_cs_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !cs_recv(scope, args.this().into()) { return; }
    // `get(USVString name)` or `get(optional CookieStoreGetOptions options = {})`: an options dictionary naming
    // neither `name` nor `url` is refused (Chrome 150: "CookieStoreGetOptions must not be empty", `get()` included)
    let (name, url) = match cookie_arg(scope, &args, "get", "CookieStoreGetOptions") {
        Ok(CookieArg::Name(n)) => (Some(n), None),
        Ok(CookieArg::Dict(d)) => {
            let name = dict_str(scope, d, "name");
            let url = dict_str(scope, d, "url");
            if name.is_none() && url.is_none() {
                let r = reject_type(scope, "get", "CookieStoreGetOptions must not be empty");
                rv.set(r);
                return;
            }
            (name, url)
        }
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let r = match do_read(scope, name.as_deref(), url.as_deref()) {
        Ok(list) => {
            let v = match list.first() {
                Some(c) => cookie_to_obj(scope, c),
                None => v8::null(scope).into(),
            };
            resolved_promise(scope, v)
        }
        Err(msg) => reject_type(scope, "get", &msg),
    };
    rv.set(r);
}

fn cb_cs_get_all(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !cs_recv(scope, args.this().into()) { return; }
    // `getAll(USVString name)` or `getAll(optional CookieStoreGetOptions options = {})`
    let (name, url) = match cookie_arg(scope, &args, "getAll", "CookieStoreGetOptions") {
        Ok(CookieArg::Name(n)) => (Some(n), None),
        Ok(CookieArg::Dict(d)) => (dict_str(scope, d, "name"), dict_str(scope, d, "url")),
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let r = match do_read(scope, name.as_deref(), url.as_deref()) {
        Ok(list) => {
            let arr = v8::Array::new(scope, list.len() as i32);
            for (i, c) in list.iter().enumerate() {
                let o = cookie_to_obj(scope, c);
                arr.set_index(scope, i as u32, o);
            }
            resolved_promise(scope, arr.into())
        }
        Err(msg) => reject_type(scope, "getAll", &msg),
    };
    rv.set(r);
}

fn cb_cs_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !cs_recv(scope, args.this().into()) { return; }
    if let Some(e) = require_cookie_arg(&args, "delete") {
        let r = sc_settle(scope, Err(e));
        rv.set(r);
        return;
    }
    // `delete(USVString name)` or `delete(CookieStoreDeleteOptions options)` -- `domain`, `name` (REQUIRED),
    // `partitioned`, `path`, each read once
    let a = match cookie_arg(scope, &args, "delete", "CookieStoreDeleteOptions") {
        Ok(CookieArg::Name(n)) => WriteArgs::deletion(&n, None, "/".into(), false),
        Ok(CookieArg::Dict(d)) => {
            let name = dict_str(scope, d, "name").unwrap_or_default();
            let domain = dict_str(scope, d, "domain");
            let path = dict_str(scope, d, "path").unwrap_or_else(|| "/".into());
            let partitioned = get_prop(scope, d, "partitioned").is_some_and(|v| v.is_true());
            WriteArgs::deletion(&name, domain, path, partitioned)
        }
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    let r = do_write(scope, "delete", a);
    rv.set(r);
}

// ── the table ──────────────────────────────────────────────────────────────
//
// WHAT THIS REPLACES. The `CookieStore` block of `dom/08f-crypto.js`: a hand `Object.setPrototypeOf`,
// six `Object.defineProperty` sites over `op_cs_*` builder round-trips onto the cppgc CLASS
// TEMPLATE's prototype, a throwing JS wrapper that shadowed the class while SHARING that prototype,
// and `globalThis.cookieStore = new NativeCookieStore()`. The five members are rows now and the
// singleton is minted in `install::live::install_window_crypto_xhr`.
//
// WINDOW ONLY. `worker.surface.json` has no `CookieStore` and no `cookieStore`, and the bundle is the
// denominator. `CookieChangeEvent` stays `host.js`'s `Event` subclass (W4.b's).

/// The receiver as a `CookieStore`, or the WebIDL `TypeError` a foreign one gets. Every body reads
/// the one cookie store, so before this an `Object.create(CookieStore.prototype)` could drive it.
fn cs_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<CookieStore>(scope, this).is_some() {
        return true;
    }
    crate::binding::protos::throw_type_error(scope, "Illegal invocation");
    false
}

/// Mint the realm's one `CookieStore` on the TABLE's prototype (`install::live`'s `cookieStore`).
pub fn make_cookie_store<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let obj = crate::binding::class::mint(scope, <CookieStore as CookieStoreBehavior>::construct());
    crate::install::live::set_table_proto(scope, obj, "CookieStore");
    obj
}

/// The body every method row of [`COOKIESTORE`] names; `onchange` takes the shared handler kit.
pub fn install_cookie_store(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "delete" => method_row(scope, proto, member, cb_cs_delete),
        "get" => method_row(scope, proto, member, cb_cs_get),
        "getAll" => method_row(scope, proto, member, cb_cs_get_all),
        "set" => method_row(scope, proto, member, cb_cs_set),
        other => debug_assert!(false, "install_cookie_store: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `CookieStore` block of `install/generated_registry.rs`, in the capture's
// order. `[interface, window]`: `new CookieStore()` is "Illegal constructor" (the bundle's oracle
// records the TypeError) and the worker capture does not list the interface at all.
//
// `required` is the registry's (1 for delete/set, 0 for get/getAll), so the arity guard the registry
// pass used to wrap these two in is now the row's -- same synchronous TypeError for a bare call. The
// zero-argument cases the guard does not cover (`get()`, `getAll()`, and a dictionary with no `name`)
// stay `require_cookie_arg`'s, which REJECTS rather than throws, as the oracle records.
webapi::interface! {
    "CookieStore" : "EventTarget" [interface, window, secure] {
        ctor    "CookieStore"                        len 0 = stub,
        method  "delete" : Promise len 1 required 1        = install_cookie_store,
        method  "get"    : Promise len 0                   = install_cookie_store,
        method  "getAll" : Promise len 0                   = install_cookie_store,
        method  "set"    : Promise len 1 required 1        = install_cookie_store,
        handler "onchange"                                 = event_handler
    }
}
