//! `Cache` and `CacheStorage` — the two `interface!` tables of the Cache API, and the `CacheStore`
//! they read.
//!
//! WHAT WAS HERE. This file used to be eight `#[op2]`s and nothing else; the BODIES were twelve
//! `op_nativize`-masked JS closures in `core/korvus-runtime/src/js/worker-finalize.js`, defined onto
//! the two registry SHELLS' prototypes (a second writer for every one of the twelve rows), plus a
//! `caches` accessor defined onto `WorkerGlobalScope.prototype` (a second writer for that row too),
//! plus an `Object.create(Cache.prototype)` instance carrying a `__name` own property no browser's
//! `Cache` has. Every one of those is gone: the rows are tables, the bodies are Rust, the name is a
//! v8 private, and `caches` is the `WorkerGlobalScope` row reading this realm's published singleton.
//!
//! SCOPE OF THE STORE, unchanged: an in-process, per-isolate map of cache name -> url -> body TEXT.
//! It is the MVP the deleted JS implemented (string bodies; full `Request`/`Response` + header
//! fidelity is a follow-up), ported faithfully rather than extended — `put` reads the `Response`'s own
//! body slot instead of awaiting `res.text()`, which is the same value without the promise hop.

use deno_core::v8;

use crate::binding::protos::{new_global, resolved_promise, str_val};
use crate::install::MemberDef;

/// One origin's caches: cache name -> url -> response. In a page of a browser endpoint's context the store is the
/// CONTEXT's, one per origin, in the router's context store ([`with_store`]), so the context's other pages see it and
/// it outlives the page's navigations; anywhere else it is the realm's, in its `OpState`, as it always was.
///
/// THE EIGHT `op_cache_*` OPS ARE GONE with the JS that called them. They existed only so that
/// `worker-finalize.js`'s twelve JS closures could reach this map; the rows are Rust now and reach it
/// directly through `OpState`, so eight ops left the registry with them.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CacheStore {
    map: std::collections::HashMap<String, std::collections::HashMap<String, CachedResponse>>,
}

/// One stored response: what `match` rebuilds, including its TYPE.
///
/// It was the body TEXT alone, and `match` answered `new Response(text)` -- so a stored opaque
/// response came back as `type "default"`, status 200, a readable body. Measured on Chrome 150:
/// `cache.put(u, await fetch(crossOrigin, {mode: 'no-cors'}))` resolves, and `cache.match(u)` is
/// `{type: "opaque", status: 0, body: null}`; `put` of an opaque-redirect resolves too.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedResponse {
    ty: String,
    status: f64,
    status_text: String,
    url: String,
    redirected: bool,
    headers: Vec<(String, String)>,
    /// `None` for a null body (opaque, opaque-redirect).
    body: Option<Vec<u8>>,
}

// ── the store, reached from a table row ─────────────────────────────────────────────────────────

/// Which cache a `Cache` instance is — a v8 PRIVATE, not the `__name` own property the deleted JS
/// defined on every instance. Chrome's `Object.getOwnPropertyNames(await caches.open("v1"))` is `[]`.
const NAME_SLOT: &str = "korvus.cache.name";

fn cache_name(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<String> {
    let n = v8::String::new(scope, NAME_SLOT)?;
    let k = v8::Private::for_api(scope, Some(n));
    this.get_private(scope, k).filter(|v| !v.is_undefined()).map(|v| v.to_rust_string_lossy(scope))
}

/// The context's caches, one store per origin. In a page of a browser endpoint's context each store is a slot of
/// the router's context store ([`transport::context_store`]), read current and written back on every use; in a
/// process no router holds, the process is the context.
fn stores() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, transport::context_store::Synced<CacheStore>>> {
    static STORES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, transport::context_store::Synced<CacheStore>>>> =
        std::sync::OnceLock::new();
    STORES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The calling realm's origin (a document's or a worker's), the store's key.
fn store_origin(scope: &mut v8::PinScope) -> String {
    crate::binding::protos::realm_self_get(scope, "origin")
        .filter(|o| o.is_string())
        .map(|o| o.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "null".to_string())
}

fn with_origin_store<R>(origin: String, f: impl FnOnce(&mut CacheStore) -> R) -> R {
    let key = format!("caches\u{0}{origin}");
    stores().entry(origin).or_insert_with(|| transport::context_store::Synced::new(key)).update(f)
}

fn with_store<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut CacheStore) -> R) -> R {
    if transport::context_store::is_remote() {
        let origin = store_origin(scope);
        return with_origin_store(origin, f);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<CacheStore>().is_none() {
        st.put(CacheStore::default());
    }
    f(st.borrow_mut::<CacheStore>())
}

/// A `RequestInfo` argument as the URL string it keys on: a string is itself, a `Request` (or anything
/// with a `url`) is its `url`. The deleted JS's `urlOf`, unchanged.
fn url_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> String {
    if v.is_string() {
        return v.to_rust_string_lossy(scope);
    }
    if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
        if let Some(u) = crate::binding::protos::get_prop(scope, o, "url").filter(|u| !u.is_undefined()) {
            return u.to_rust_string_lossy(scope);
        }
    }
    v.to_rust_string_lossy(scope)
}

/// The body text a `Response` is carrying. The JS awaited `res.text()`; this is the same string
/// without the promise hop, read from the Response's body STREAM (the `fetchBody`
/// string slot this used to read: a body is a stream of bytes now, `modules/fetch/body.rs`). Reading it
/// consumes it, as `cache.put` does in the platform.
fn response_body(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> CachedResponse {
    response_with_body(scope, v, None)
}

/// A `fetch()` Response whose body is still ARRIVING (the Response comes at the headers).
/// Its bytes cannot be read synchronously; `put` reads them first, as the platform's does.
fn body_still_arriving<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    use crate::core::events::event::evt_slot_read;
    let o = v8::Local::<v8::Object>::try_from(v).ok().filter(|_| crate::install::webidl::has_brand(scope, v, "Response"))?;
    let stream = evt_slot_read(scope, o, "body").and_then(|b| v8::Local::<v8::Object>::try_from(b).ok())?;
    crate::core::css::css_style_declaration::priv_get(scope, stream, crate::modules::fetch::body::BODY_ID).filter(|i| i.is_number()).map(|_| o)
}

/// Store `response` under `url` in cache `name` -- now, or once a body still arriving has arrived. The
/// promise `put` answers.
fn store<'s>(scope: &mut v8::PinScope<'s, '_>, name: String, url: String, response: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let response = v8::Local::new(scope, response);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(o) = body_still_arriving(scope, response) else {
        let body = response_body(scope, response);
        with_store(scope, |s| {
            s.map.entry(name).or_default().insert(url, body);
        });
        return resolved_promise(scope, undef);
    };
    let read = crate::modules::fetch::body::consume(scope, o, "Response", crate::modules::fetch::body::Kind::ArrayBuffer);
    let n = str_val(scope, &name);
    let u = str_val(scope, &url);
    let data: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &[n, u, response]).into();
    let then = crate::binding::protos::build_fn_data(scope, cb_store_read, "", 1, data);
    crate::binding::protos::promise_then(scope, read, then, None).unwrap_or_else(|| resolved_promise(scope, undef))
}

/// [`store`]'s continuation: data is `[name, url, response]`, argument 0 the body's ArrayBuffer.
fn cb_store_read(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let d = v8::Local::new(scope, d);
    let name = d.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let url = d.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let Some(response) = d.get_index(scope, 2) else { return };
    let bytes = v8::Local::<v8::ArrayBuffer>::try_from(args.get(0))
        .ok()
        .map(|b| {
            let len = b.byte_length();
            let store = b.get_backing_store();
            let mut out = vec![0u8; len];
            if let Some(ptr) = store.data() {
                unsafe { std::ptr::copy_nonoverlapping(ptr.as_ptr() as *const u8, out.as_mut_ptr(), len) };
            }
            out
        })
        .unwrap_or_default();
    let body = response_with_body(scope, response, Some(bytes));
    with_store(scope, |s| {
        s.map.entry(name).or_default().insert(url, body);
    });
}

/// [`response_body`], with the body's bytes already read when `known` is given.
fn response_with_body(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, known: Option<Vec<u8>>) -> CachedResponse {
    use crate::core::events::event::evt_slot_read;
    if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
        if crate::install::webidl::has_brand(scope, v, "Response") {
            let body = match (known, evt_slot_read(scope, o, "body").filter(|b| !b.is_null())) {
                (Some(bytes), _) => Some(bytes),
                (None, Some(b)) => Some(crate::modules::fetch::body::buffered_bytes(scope, b).unwrap_or_default()),
                (None, None) => None,
            };
            let text = |scope: &mut v8::PinScope, k: &str| evt_slot_read(scope, o, k).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
            let headers = evt_slot_read(scope, o, "headers")
                .and_then(|h| crate::binding::protos::call_intrinsic(scope, "Array.from", &[h]))
                .and_then(|arr| v8::Local::<v8::Array>::try_from(arr).ok())
                .map(|arr| {
                    (0..arr.length())
                        .filter_map(|i| arr.get_index(scope, i))
                        .filter_map(|p| v8::Local::<v8::Array>::try_from(p).ok())
                        .map(|p| {
                            let k = p.get_index(scope, 0).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                            let v = p.get_index(scope, 1).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
                            (k, v)
                        })
                        .collect()
                })
                .unwrap_or_default();
            return CachedResponse {
                ty: text(scope, "type"),
                status: evt_slot_read(scope, o, "status").and_then(|x| x.number_value(scope)).unwrap_or(200.0),
                status_text: text(scope, "statusText"),
                url: text(scope, "url"),
                redirected: evt_slot_read(scope, o, "redirected").map(|x| x.boolean_value(scope)).unwrap_or(false),
                headers,
                body,
            };
        }
    }
    let text = if v.is_null_or_undefined() { String::new() } else { v.to_rust_string_lossy(scope) };
    CachedResponse { ty: "default".into(), status: 200.0, body: Some(text.into_bytes()), ..Default::default() }
}

/// The `Response` a stored entry is matched as: the stored type, status, url, headers and body.
fn rebuild<'s>(scope: &mut v8::PinScope<'s, '_>, c: &CachedResponse) -> v8::Local<'s, v8::Value> {
    use crate::binding::protos::set_prop;
    let rec = v8::Object::new(scope);
    let st: v8::Local<v8::Value> = v8::Number::new(scope, c.status).into();
    set_prop(scope, rec, "status", st);
    let stx = str_val(scope, &c.status_text);
    set_prop(scope, rec, "statusText", stx);
    let u = str_val(scope, &c.url);
    set_prop(scope, rec, "url", u);
    let t = str_val(scope, &c.ty);
    set_prop(scope, rec, "type", t);
    let r: v8::Local<v8::Value> = v8::Boolean::new(scope, c.redirected).into();
    set_prop(scope, rec, "redirected", r);
    let arr = v8::Array::new(scope, c.headers.len() as i32);
    for (i, (k, v)) in c.headers.iter().enumerate() {
        let pair = v8::Array::new(scope, 2);
        let kv = str_val(scope, k);
        let vv = str_val(scope, v);
        pair.set_index(scope, 0, kv);
        pair.set_index(scope, 1, vv);
        arr.set_index(scope, i as u32, pair.into());
    }
    set_prop(scope, rec, "headers", arr.into());
    let b: v8::Local<v8::Value> = match &c.body {
        Some(bytes) => crate::modules::fetch::body::u8_array(scope, bytes.clone()),
        None => v8::null(scope).into(),
    };
    set_prop(scope, rec, "body", b);
    match crate::modules::fetch::response::response_from_network(scope, rec.into()) {
        Some(o) => o.into(),
        None => v8::undefined(scope).into(),
    }
}

/// Mint one `Cache` for `name`, on the TABLE's prototype.
fn make_cache<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Value> {
    let obj = crate::install::live::mint_table(scope, "Cache");
    if let Some(n) = v8::String::new(scope, NAME_SLOT) {
        let k = v8::Private::for_api(scope, Some(n));
        let nv = str_val(scope, name);
        obj.set_private(scope, k, nv);
    }
    obj.into()
}

// ── Cache ───────────────────────────────────────────────────────────────────────────────────────

/// A cache method's `CacheQueryOptions` / `MultiCacheQueryOptions`, converted as WebIDL converts it after the request
/// (Chrome 150): `ignoreMethod`, `ignoreSearch`, `ignoreVary` (and `cacheName`), each read
/// once -- korvus read none of them. `Err` is the exception the method's promise rejects with.
fn query_options<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>, dict: &str, at: &str) -> Result<v8::Local<'s, v8::Object>, v8::Local<'s, v8::Value>> {
    crate::install::ctor_dicts::convert_dict_caught(scope, v, dict, at)
}

fn cb_cache_match(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = cache_name(scope, args.this()) else { return };
    let url = url_of(scope, args.get(0));
    if let Err(e) = query_options(scope, args.get(1), "CacheQueryOptions", "Failed to execute 'match' on 'Cache': ") {
        let p = crate::binding::protos::rejected_promise(scope, e);
        rv.set(p);
        return;
    }
    let hit = with_store(scope, |s| s.map.get(&name).and_then(|c| c.get(&url)).cloned());
    let v: v8::Local<v8::Value> = match hit {
        Some(c) => rebuild(scope, &c),
        None => v8::undefined(scope).into(),
    };
    let p = resolved_promise(scope, v);
    rv.set(p);
}

fn cb_cache_put(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = cache_name(scope, args.this()) else { return };
    let url = url_of(scope, args.get(0));
    let p = store(scope, name, url, args.get(1));
    rv.set(p);
}

fn cb_cache_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = cache_name(scope, args.this()) else { return };
    let url = url_of(scope, args.get(0));
    if let Err(e) = query_options(scope, args.get(1), "CacheQueryOptions", "Failed to execute 'delete' on 'Cache': ") {
        let p = crate::binding::protos::rejected_promise(scope, e);
        rv.set(p);
        return;
    }
    let gone = with_store(scope, |s| s.map.get_mut(&name).map(|c| c.remove(&url).is_some()).unwrap_or(false));
    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, gone).into();
    let p = resolved_promise(scope, b);
    rv.set(p);
}

fn cb_cache_keys(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(name) = cache_name(scope, args.this()) else { return };
    let urls = with_store(scope, |s| s.map.get(&name).map(|c| c.keys().cloned().collect::<Vec<_>>()).unwrap_or_default());
    let arr = v8::Array::new(scope, urls.len() as i32);
    for (i, u) in urls.iter().enumerate() {
        let uv = str_val(scope, u);
        let req = new_global(scope, "Request", &[uv]).map(|o| -> v8::Local<v8::Value> { o.into() }).unwrap_or_else(|| str_val(scope, u));
        arr.set_index(scope, i as u32, req);
    }
    let av: v8::Local<v8::Value> = arr.into();
    let p = resolved_promise(scope, av);
    rv.set(p);
}

/// `cache.add(request)` = `fetch(request).then((res) => cache.put(request, res))`, which is what the
/// spec says and what the JS did. The fetch and the `put` are both this realm's own.
fn cb_cache_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let req = args.get(0);
    let gv: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    let fetched = crate::binding::protos::call_method(scope, gv, "fetch", &[req]);
    let Some(p) = fetched.and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        let r = resolved_promise(scope, undef);
        rv.set(r);
        return;
    };
    let pair = v8::Array::new(scope, 2);
    pair.set_index(scope, 0, this.into());
    pair.set_index(scope, 1, req);
    let handler = crate::binding::protos::build_fn_data(scope, cb_cache_add_put, "", 1, pair.into());
    match p.then(scope, handler) {
        Some(chained) => rv.set(chained.into()),
        None => rv.set(p.into()),
    }
}

/// The `.then` of [`cb_cache_add`]: data is `[cache, request]`, argument 0 is the `Response`.
fn cb_cache_add_put(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(cache) = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(req) = d.get_index(scope, 1) else { return };
    let Some(name) = cache_name(scope, cache) else { return };
    let url = url_of(scope, req);
    // `add` settles when the response is STORED, its body read to the end.
    let p = store(scope, name, url, args.get(0));
    rv.set(p);
}

/// `cache.addAll(requests)` = `Promise.all(requests.map((r) => cache.add(r)))`, the JS's own shape.
fn cb_cache_add_all(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let mut promises: Vec<v8::Local<v8::Value>> = Vec::new();
    if let Ok(list) = v8::Local::<v8::Array>::try_from(args.get(0)) {
        for i in 0..list.length() {
            let Some(item) = list.get_index(scope, i) else { continue };
            if let Some(p) = crate::binding::protos::call_method(scope, this, "add", &[item]) {
                promises.push(p);
            }
        }
    }
    let arr = v8::Array::new(scope, promises.len() as i32);
    for (i, p) in promises.iter().enumerate() {
        arr.set_index(scope, i as u32, *p);
    }
    let all = crate::binding::protos::call_intrinsic(scope, "Promise.all", &[arr.into()]);
    match all {
        Some(p) => rv.set(p),
        None => {
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            let p = resolved_promise(scope, undef);
            rv.set(p);
        }
    }
}

/// `matchAll` is not implemented: the store keys one body per URL, so the multi-response semantics
/// this member exists for have nothing to answer from. Its ROW says `= stub`, so it never reaches
/// here — a member the table declares as a body and the installer then stubs is a live/declared
/// disagreement `tests/layers_live.rs` fails on, and rightly.
pub fn install_cache(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    // `method_row` AND NOT `build_fn` + `define_value`, which is what this was: the same two steps
    // with the WebIDL argument count left out, so every `required` count in the table below was
    // decorative. `cache.put()` answered `undefined` and `caches.open()` a resolved `Cache` where
    // Chrome 150.0.7871.46 answers a promise rejected with "1 argument required, but only 0
    // present." (measured 2026-09-23, awaiting the promise). A guard is not something a table's
    // install arm should be able to forget.
    use crate::install::live::method_row;
    match member.name {
        "match" => method_row(scope, proto, member, cb_cache_match),
        "put" => method_row(scope, proto, member, cb_cache_put),
        "delete" => method_row(scope, proto, member, cb_cache_delete),
        "keys" => method_row(scope, proto, member, cb_cache_keys),
        "add" => method_row(scope, proto, member, cb_cache_add),
        "addAll" => method_row(scope, proto, member, cb_cache_add_all),
        _ => crate::install::installer::put_table_stub(scope, proto, member, true),
    }
}

// ── CacheStorage ────────────────────────────────────────────────────────────────────────────────

// ── a storage bucket's CacheStorage ──────────────────────────────────────────
//
// `StorageBucket.caches` is a `CacheStorage` of its own over the BUCKET's caches (Blink `StorageBucket::caches`):
// Chrome 150 lists a cache a bucket opened in `bucket.caches.keys()` and not in `caches.keys()`. The store keeps one
// map by cache name; a bucket's caches are under a key that names the bucket ([`cache_key`]).

/// The private slot naming the bucket a `CacheStorage` serves; absent on the realm's own `caches`.
const BUCKET_SLOT: &str = "korvus.cache.bucket";

fn bucket_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<String> {
    let n = v8::String::new(scope, BUCKET_SLOT)?;
    let k = v8::Private::for_api(scope, Some(n));
    this.get_private(scope, k).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

/// The store's key for cache `name` of the receiver's bucket (its name itself in the default bucket).
fn cache_key(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> String {
    match bucket_of(scope, this) {
        Some(b) => format!("\u{0}{b}\u{0}{name}"),
        None => name.to_string(),
    }
}

/// The bucket a store key belongs to (`None`: the default one) and the cache's own name.
fn split_cache_key(key: &str) -> (Option<&str>, &str) {
    match key.strip_prefix('\u{0}').and_then(|r| r.split_once('\u{0}')) {
        Some((b, n)) => (Some(b), n),
        None => (None, key),
    }
}

/// A new `CacheStorage` over storage bucket `bucket`'s caches, on this realm's `CacheStorage` prototype.
pub(crate) fn mint_bucket_caches<'s>(scope: &mut v8::PinScope<'s, '_>, bucket: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, "CacheStorage")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let n = v8::String::new(scope, BUCKET_SLOT)?;
    let k = v8::Private::for_api(scope, Some(n));
    let b = str_val(scope, bucket);
    obj.set_private(scope, k, b);
    Some(obj)
}

/// Storage bucket `bucket` of the calling realm's origin deleted: its caches go with it.
pub(crate) fn delete_bucket_caches(scope: &mut v8::PinScope, bucket: &str) {
    with_store(scope, |store| store.map.retain(|k, _| split_cache_key(k).0 != Some(bucket)));
}

fn cb_caches_open(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = args.get(0).to_rust_string_lossy(scope);
    let name = cache_key(scope, args.this(), &name);
    with_store(scope, |s| {
        s.map.entry(name.clone()).or_default();
    });
    let c = make_cache(scope, &name);
    let p = resolved_promise(scope, c);
    rv.set(p);
}

fn cb_caches_has(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = args.get(0).to_rust_string_lossy(scope);
    let name = cache_key(scope, args.this(), &name);
    let has = with_store(scope, |s| s.map.contains_key(&name));
    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, has).into();
    let p = resolved_promise(scope, b);
    rv.set(p);
}

fn cb_caches_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = args.get(0).to_rust_string_lossy(scope);
    let name = cache_key(scope, args.this(), &name);
    let gone = with_store(scope, |s| s.map.remove(&name).is_some());
    let b: v8::Local<v8::Value> = v8::Boolean::new(scope, gone).into();
    let p = resolved_promise(scope, b);
    rv.set(p);
}

fn cb_caches_keys(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let bucket = bucket_of(scope, args.this());
    let names = with_store(scope, |s| {
        s.map.keys().filter(|k| split_cache_key(k).0 == bucket.as_deref()).map(|k| split_cache_key(k).1.to_string()).collect::<Vec<_>>()
    });
    let arr = v8::Array::new(scope, names.len() as i32);
    for (i, n) in names.iter().enumerate() {
        let nv = str_val(scope, n);
        arr.set_index(scope, i as u32, nv);
    }
    let av: v8::Local<v8::Value> = arr.into();
    let p = resolved_promise(scope, av);
    rv.set(p);
}

fn cb_caches_match(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let url = url_of(scope, args.get(0));
    if let Err(e) = query_options(scope, args.get(1), "MultiCacheQueryOptions", "Failed to execute 'match' on 'CacheStorage': ") {
        let p = crate::binding::protos::rejected_promise(scope, e);
        rv.set(p);
        return;
    }
    let bucket = bucket_of(scope, args.this());
    let hit = with_store(scope, |s| s.map.iter().filter(|(k, _)| split_cache_key(k).0 == bucket.as_deref()).find_map(|(_, c)| c.get(&url).cloned()));
    let v: v8::Local<v8::Value> = match hit {
        Some(c) => rebuild(scope, &c),
        None => v8::undefined(scope).into(),
    };
    let p = resolved_promise(scope, v);
    rv.set(p);
}

pub fn install_cache_storage(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    // See [`install_cache`]: `method_row` is the mount that carries the row's `required` count.
    use crate::install::live::method_row;
    match member.name {
        "open" => method_row(scope, proto, member, cb_caches_open),
        "has" => method_row(scope, proto, member, cb_caches_has),
        "delete" => method_row(scope, proto, member, cb_caches_delete),
        "keys" => method_row(scope, proto, member, cb_caches_keys),
        "match" => method_row(scope, proto, member, cb_caches_match),
        _ => crate::install::installer::put_table_stub(scope, proto, member, true),
    }
}

// Rows VERBATIM from the `Cache` and `CacheStorage` blocks of `install/generated_registry.rs`, in the
// capture's order. `[interface, both]`: both captures list both interfaces with the same member set.
webapi::interface! {
    "Cache" [interface, both, secure] {
        ctor   "Cache"                          len 0            = stub,
        method "add"      : Promise len 1 required 1              = install_cache,
        method "addAll"   : Promise len 1 required 1              = install_cache,
        method "delete"   : Promise len 1 required 1              = install_cache,
        method "keys"     : Promise len 0                         = install_cache,
        method "match"    : Promise len 1 required 1              = install_cache,
        method "matchAll" : Promise len 0                         = stub,
        method "put"      : Promise len 2 required 2              = install_cache
    }
}

webapi::interface! {
    "CacheStorage" [interface, both, secure] {
        ctor   "CacheStorage"                   len 0            = stub,
        method "delete" : Promise len 1 required 1                = install_cache_storage,
        method "has"    : Promise len 1 required 1                = install_cache_storage,
        method "keys"   : Promise len 0                           = install_cache_storage,
        method "match"  : Promise len 1 required 1                = install_cache_storage,
        method "open"   : Promise len 1 required 1                = install_cache_storage
    }
}

/// Build both interfaces from their tables and mint the realm's one `caches` singleton from
/// `CacheStorage`'s template: in the WORKER it is published for the `WorkerGlobalScope` row, in the
/// WINDOW it goes in the slot of the accessor [`create_window_caches`] defined during the registry walk.
pub fn install_cache_api<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, window_realm: bool) {
    use crate::install::installer::{self, Flavor};
    let mut ledger = installer::take_ledger(scope);
    installer::create_interface(scope, global, &CACHE, "Cache", None, None, Flavor::Table, &mut ledger);
    let cs = installer::create_interface(scope, global, &CACHESTORAGE, "CacheStorage", None, None, Flavor::Table, &mut ledger);
    installer::put_ledger(scope, ledger);
    if window_realm {
        // The accessor exists already; this is where its interface does, so the object the walk left in
        // its slot -- which nothing has read: the page's scripts run after the install -- is replaced by
        // one made from the interface's template.
        if window_caches(scope, global).is_some() {
            let obj = crate::binding::protos::mint_on(scope, cs.proto.into());
            crate::core::html::window_globals::window_slot_set(scope, "caches", obj.into());
        }
        return;
    }
    let obj = crate::binding::protos::mint_on(scope, cs.proto.into());
    crate::core::workers::worker_global_scope::publish_scope_singleton(scope, "caches", obj.into());
}

/// The WINDOW's `caches` own global, created by the registry walk at the capture's position of the
/// name (`install::live::ORDERED_HAND`).
///
/// THE WINDOW HAD NO `caches` AT ALL once: every capture in
/// the tree lists it among the window's globals and Chrome 150 answers `typeof caches === "object"`
/// on a secure loopback page, and a page that opens a cache before its first render
/// (`caches.open(...)`) died on a ReferenceError. `Window` has no `caches` row in `core/html/window.rs`,
/// so there is no second writer.
///
/// THE SHAPE IS CHROME'S, measured: `Object.getOwnPropertyDescriptor(globalThis, "caches")` is an
/// ACCESSOR -- a getter, no setter, enumerable and configurable -- handing back one `CacheStorage`
/// with no own property. (This realm's `crypto` and `indexedDB` are own DATA properties where Chrome
/// has accessors too; that is theirs to close, not copied here.) The object is minted now and kept in
/// a private slot on the global, and [`install_cache_api`] replaces it with an instance of `CacheStorage`,
/// because the walk reaches `caches` long before the abort-style block builds that interface.
pub fn create_window_caches<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    _ledger: &mut crate::install::installer::InstallLedger,
) {
    // `[SecureContext]`, MEASURED: `window.caches` is gone on
    // `http://web-platform.test:<p>/` on Chrome 150, Brave 151, Edge 151 and Opera 135, along with the
    // `CacheStorage` and `Cache` interface objects the registry publishes beside it. Through the one
    // gate, so the condition is an argument here exactly as it is on a registry row.
    if !crate::core::html::exposure::gate(
        scope,
        crate::core::html::exposure::Publishes::Global,
        "caches",
        crate::core::html::exposure::Exposure::SecureContext,
    ) {
        return;
    }
    let obj = v8::Object::new(scope);
    crate::core::html::window_globals::define_window_singleton(scope, global, "caches", obj.into(), crate::core::html::window_globals::WindowSetter::None, true, crate::core::html::window_globals::Mint::Plain);
}

fn window_caches<'s>(scope: &mut v8::PinScope<'s, '_>, _global: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    crate::core::html::window_globals::window_slot_get(scope, "caches").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}
