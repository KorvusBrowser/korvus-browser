//! A FRAME's modules: `import()` from a frame's script loads, links and evaluates the module graph
//! IN THE FRAME'S REALM -- its globals are the frame's `document`, `location`, `window` and its own variables.
//!
//! deno_core's module map belongs to the page's realm, and a frame realm shares its slots (see `new_realm`), so an
//! `import()` from a frame went through deno_core's loader and evaluated the module in the PAGE: SvelteKit's start
//! compares `document.URL` with `location.href` and reloads when they differ, and on bloomberg.com (2026-09-30)
//! the page's document against the frame's location navigated the TOP. Measured on Chrome 150
//! (`tools/realweb/seed/frameimport`): the module sees the frame (`window !== top`, the frame's own `var`s, not the
//! page's) and `import.meta.url` is its own URL.
//!
//! Two public deno_core hooks carry it, so deno_core itself is unchanged:
//! * a frame script is compiled with host-defined options of kind `VM_DYNAMIC_IMPORT_CALLBACK` and a key
//!   `register_vm_dynamic_import_callback` handed out for (frame, referrer URL); deno_core then calls that
//!   function with the specifier, and the promise it returns is the `import()`'s;
//! * `register_external_module_import_meta_cb` answers `import.meta` for a module deno_core's map does not hold.
//!
//! The graph is loaded through the engine's own loader (the destination `script`, mode `cors`), each module
//! compiled in the frame's realm with the same host-defined options (so ITS `import()` stays in the frame), linked
//! against this frame's map, and evaluated. A frame's map is dropped when the frame gets a new document.

use std::collections::{HashMap, HashSet};

use deno_core::v8;

use super::{enter_realm, leave_realm, realm_of};
use crate::binding::protos::{build_fn_data, get_prop, str_val};

/// Every frame's modules, by (frame, URL).
#[derive(Default)]
pub(crate) struct FrameModules {
    entries: HashMap<(u32, String), Entry>,
    /// A module's identity hash -> its frame, the URL its imports resolve against, and its `import.meta.url`
    /// (the two differ only for an inline module script: the document's base, and the document's URL).
    by_hash: HashMap<i32, Vec<(v8::Global<v8::Module>, u32, String, String)>>,
    loads: HashMap<u32, Load>,
    next_load: u32,
    /// The `import()` callback key per (frame, referrer).
    keys: HashMap<(u32, String), u32>,
    /// The `import()` promises whose module is evaluating, by id (a resolver cannot ride in function data).
    evaluating: HashMap<u32, v8::Global<v8::PromiseResolver>>,
    /// Loads waiting on a module still being fetched.
    waiters: HashMap<(u32, String), Vec<u32>>,
    meta_registered: bool,
}

enum Entry {
    Fetching,
    Ready { module: v8::Global<v8::Module>, requests: Vec<String> },
    Failed(v8::Global<v8::Value>),
}

struct Load {
    ctx: u32,
    root: String,
    visited: HashSet<String>,
    pending: u32,
    resolver: v8::Global<v8::PromiseResolver>,
    done: bool,
}

fn with<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut FrameModules) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<FrameModules>().is_none() {
        st.put(FrameModules::default());
    }
    f(st.borrow_mut::<FrameModules>())
}

/// Frame `ctx` has a new document: its modules are the old document's.
pub(crate) fn forget(scope: &mut v8::PinScope, ctx: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let Some(m) = st.try_borrow_mut::<FrameModules>() else { return };
    m.entries.retain(|(c, _), _| *c != ctx);
    m.waiters.retain(|(c, _), _| *c != ctx);
    for list in m.by_hash.values_mut() {
        list.retain(|(_, c, _, _)| *c != ctx);
    }
    m.by_hash.retain(|_, l| !l.is_empty());
    m.keys.retain(|(c, _), _| *c != ctx);
}

/// The host-defined options a script of frame `ctx` at `referrer` is compiled with: its `import()` comes here.
pub(crate) fn host_options<'s>(scope: &mut v8::PinScope<'s, '_>, ctx: u32, referrer: &str) -> Option<v8::Local<'s, v8::Data>> {
    let known = with(scope, |m| m.keys.get(&(ctx, referrer.to_string())).copied());
    let key = match known {
        Some(k) => k,
        None => {
            let data = v8::Array::new(scope, 2);
            let c = v8::Integer::new_from_unsigned(scope, ctx);
            data.set_index(scope, 0, c.into());
            let r = str_val(scope, referrer);
            data.set_index(scope, 1, r.into());
            let f = build_fn_data(scope, cb_import, "", 2, data.into());
            let k = deno_core::register_vm_dynamic_import_callback(scope, f);
            with(scope, |m| m.keys.insert((ctx, referrer.to_string()), k));
            k
        }
    };
    Some(deno_core::create_host_defined_options_with_kind_and_key(
        scope,
        deno_core::host_defined_options_kind::VM_DYNAMIC_IMPORT_CALLBACK,
        key,
    ))
}

fn data_pair(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<(u32, String)> {
    let d = crate::binding::protos::body_data(scope, args);
    let arr = v8::Local::<v8::Array>::try_from(d).ok()?;
    let c = arr.get_index(scope, 0)?.uint32_value(scope)?;
    let s = arr.get_index(scope, 1)?.to_rust_string_lossy(scope);
    Some((c, s))
}

fn type_error<'s>(scope: &mut v8::PinScope<'s, '_>, msg: &str) -> v8::Local<'s, v8::Value> {
    let m = v8::String::new(scope, msg).unwrap();
    v8::Exception::type_error(scope, m)
}

fn js_str<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap()
}

/// HTML's "resolve a module specifier" for what a frame can import: a URL, or a path relative to `base`.
fn resolve(spec: &str, base: &str) -> Result<String, String> {
    if let Ok(u) = url::Url::parse(spec) {
        return Ok(u.to_string());
    }
    if spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../") {
        if let Some(u) = url::Url::parse(base).ok().and_then(|b| b.join(spec).ok()) {
            return Ok(u.to_string());
        }
    }
    Err(format!("Failed to resolve module specifier \"{spec}\". Relative references must start with either \"/\", \"./\", or \"../\"."))
}

/// `import(specifier)` from a script of the frame and referrer this function was made for.
fn cb_import(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((ctx, referrer)) = data_pair(scope, &args) else { return };
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let promise = resolver.get_promise(scope);
    rv.set(promise.into());
    let spec = args.get(0).to_rust_string_lossy(scope);
    let url = match resolve(&spec, &referrer) {
        Ok(u) => u,
        Err(_) => {
            // `import()`'s own wording (Chrome 150); a static import's is `resolve`'s.
            let e = type_error(scope, &format!("Failed to resolve module specifier '{spec}'"));
            resolver.reject(scope, e);
            return;
        }
    };
    let id = with(scope, |m| {
        m.next_load += 1;
        m.next_load
    });
    let g = v8::Global::new(scope, resolver);
    with(scope, |m| m.loads.insert(id, Load { ctx, root: url.clone(), visited: HashSet::new(), pending: 0, resolver: g, done: false }));
    visit(scope, id, &url);
    settle_if_loaded(scope, id);
}

/// A frame's `<script type=module>`: load, link and evaluate it in the frame's realm, then settle
/// `promise` -- fulfilled with the namespace, or rejected with a fetch failure (`Failed to fetch dynamically
/// imported module: <url>`), a parse error or an evaluation error. `inline` is `(source, document URL, base)`.
pub(crate) fn run_script<'s>(scope: &mut v8::PinScope<'s, '_>, ctx: u32, src: Option<&str>, inline: Option<(&str, &str, &str)>) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let id = with(scope, |m| {
        m.next_load += 1;
        m.next_load
    });
    let g = v8::Global::new(scope, resolver);
    match (src, inline) {
        (Some(url), _) => {
            with(scope, |m| m.loads.insert(id, Load { ctx, root: url.to_string(), visited: HashSet::new(), pending: 0, resolver: g, done: false }));
            visit(scope, id, url);
            settle_if_loaded(scope, id);
        }
        (None, Some((code, doc_url, base))) => {
            // A key of its own: an inline module is not in the map under any URL another module can import.
            let key = format!("{doc_url}#korvus-inline-module-{id}");
            with(scope, |m| {
                m.loads.insert(id, Load { ctx, root: key.clone(), visited: HashSet::from([key.clone()]), pending: 1, resolver: g, done: false });
                m.waiters.insert((ctx, key.clone()), vec![id]);
            });
            let compiled = compile_as(scope, ctx, doc_url, base, code);
            fetched(scope, ctx, &key, compiled);
        }
        _ => return None,
    }
    Some(promise)
}

/// Walk `url` into load `id`: a module already compiled has its imports walked, one being fetched is waited
/// for, and a new one is fetched.
fn visit(scope: &mut v8::PinScope, id: u32, url: &str) {
    let Some(ctx) = with(scope, |m| {
        let l = m.loads.get_mut(&id)?;
        if l.done || !l.visited.insert(url.to_string()) {
            return None;
        }
        Some(l.ctx)
    }) else {
        return;
    };
    enum Next {
        Walk(Vec<String>),
        Wait,
        Fetch,
        Fail(v8::Global<v8::Value>),
    }
    let next = with(scope, |m| match m.entries.get(&(ctx, url.to_string())) {
        Some(Entry::Ready { requests, .. }) => Next::Walk(requests.clone()),
        Some(Entry::Fetching) => Next::Wait,
        Some(Entry::Failed(e)) => Next::Fail(e.clone()),
        None => {
            m.entries.insert((ctx, url.to_string()), Entry::Fetching);
            Next::Fetch
        }
    });
    match next {
        Next::Walk(reqs) => {
            for r in reqs {
                visit(scope, id, &r);
            }
        }
        Next::Wait => {
            with(scope, |m| {
                m.waiters.entry((ctx, url.to_string())).or_default().push(id);
                if let Some(l) = m.loads.get_mut(&id) {
                    l.pending += 1;
                }
            });
        }
        Next::Fetch => {
            with(scope, |m| {
                m.waiters.entry((ctx, url.to_string())).or_default().push(id);
                if let Some(l) = m.loads.get_mut(&id) {
                    l.pending += 1;
                }
            });
            fetch(scope, ctx, url);
        }
        Next::Fail(e) => {
            let e = v8::Local::new(scope, &e);
            fail(scope, id, e);
        }
    }
}

fn fetch(scope: &mut v8::PinScope, ctx: u32, url: &str) {
    let data = v8::Array::new(scope, 2);
    let c = v8::Integer::new_from_unsigned(scope, ctx);
    data.set_index(scope, 0, c.into());
    let u = str_val(scope, url);
    data.set_index(scope, 1, u.into());
    let ok = build_fn_data(scope, cb_fetched, "", 1, data.into());
    let bad = build_fn_data(scope, cb_fetch_failed, "", 1, data.into());
    match crate::core::html::subresource::start_fetch(scope, url, "script", "cors") {
        Some(p) => {
            // the fetch's end is a task, not a promise job (`then_task`)
            let _ = crate::then_task(scope, p, ok, Some(bad));
        }
        None => {
            let e = type_error(scope, "load");
            fetched(scope, ctx, url, Err(e));
        }
    }
}

fn cb_fetched(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((ctx, url)) = data_pair(scope, &args) else { return };
    let resp = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let status = resp.and_then(|r| get_prop(scope, r, "status")).and_then(|s| s.int32_value(scope)).unwrap_or(0);
    // A module script must be served as JavaScript (HTML's strict MIME check for modules): the page's own rule
    // (`script_response_ok`), over every header line -- this read the first `Content-Type` line cut at `;`, let a
    // response with none through and took any `*javascript` (A/B C5).
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(h) = resp.and_then(|r| get_prop(scope, r, "headers")).and_then(|h| v8::Local::<v8::Array>::try_from(h).ok()) {
        for i in 0..h.length() {
            let Some(pair) = h.get_index(scope, i).and_then(|p| v8::Local::<v8::Array>::try_from(p).ok()) else { continue };
            let k = pair.get_index(scope, 0).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let v = pair.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            headers.push((k, v));
        }
    }
    let js = crate::core::html::html_script_element::script_response_ok(crate::core::html::html_script_element::ScriptFlavor::Module, 200, &headers);
    if !(200..300).contains(&status) || !js {
        let e = type_error(scope, "load");
        fetched(scope, ctx, &url, Err(e));
        return;
    }
    let body = resp.and_then(|r| get_prop(scope, r, "body")).map(|b| b.to_rust_string_lossy(scope)).unwrap_or_default();
    let compiled = compile(scope, ctx, &url, &body);
    fetched(scope, ctx, &url, compiled);
}

fn cb_fetch_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((ctx, url)) = data_pair(scope, &args) else { return };
    let e = type_error(scope, "load");
    fetched(scope, ctx, &url, Err(e));
}

/// Compile `source` as the module at `url` in frame `ctx`'s realm, with the frame's `import()`. `Ok` is the
/// module and the absolute URLs it imports; `Err` the exception to reject with.
fn compile<'s>(scope: &mut v8::PinScope<'s, '_>, ctx: u32, url: &str, source: &str) -> Result<(v8::Local<'s, v8::Module>, Vec<String>), v8::Local<'s, v8::Value>> {
    compile_as(scope, ctx, url, url, source)
}

/// [`compile`] for a module whose imports resolve against `base` and whose name (its `import.meta.url`, its stack
/// frames) is `name` -- an inline module script: the document's base URL and the document's URL.
fn compile_as<'s>(scope: &mut v8::PinScope<'s, '_>, ctx: u32, name: &str, base: &str, source: &str) -> Result<(v8::Local<'s, v8::Module>, Vec<String>), v8::Local<'s, v8::Value>> {
    let url = name;
    ensure_meta(scope);
    let Some(realm) = realm_of(scope, ctx) else { return Err(type_error(scope, "load")) };
    let prev = enter_realm(scope, ctx);
    let out = {
        let scope = &mut v8::ContextScope::new(scope, realm);
        let opts = host_options(scope, ctx, base);
        let name = str_val(scope, url);
        let origin = v8::ScriptOrigin::new(scope, name.into(), 0, 0, false, 0, None, false, false, true, opts);
        let code = js_str(scope, source);
        v8::tc_scope!(let tc, scope);
        let mut src = v8::script_compiler::Source::new(code, Some(&origin));
        match v8::script_compiler::compile_module(tc, &mut src) {
            Some(module) => {
                let reqs = module.get_module_requests();
                let mut out = Vec::new();
                let mut bad = None;
                for i in 0..reqs.length() {
                    let Some(item) = reqs.get(tc, i) else { continue };
                    let Ok(req) = v8::Local::<v8::ModuleRequest>::try_from(item) else { continue };
                    let spec = req.get_specifier().to_rust_string_lossy(tc);
                    match resolve(&spec, base) {
                        Ok(u) => out.push(u),
                        Err(msg) => bad = Some(msg),
                    }
                }
                match bad {
                    Some(msg) => {
                        let m = v8::String::new(tc, &msg).unwrap();
                        Err(v8::Local::new(tc, v8::Exception::type_error(tc, m)).cast())
                    }
                    None => {
                        let g = v8::Global::new(tc, module);
                        let h = module.get_identity_hash().get();
                        let op_state = deno_core::JsRuntime::op_state_from(tc);
                        let mut st = op_state.borrow_mut();
                        if st.try_borrow::<FrameModules>().is_none() {
                            st.put(FrameModules::default());
                        }
                        st.borrow_mut::<FrameModules>().by_hash.entry(h).or_default().push((g, ctx, base.to_string(), url.to_string()));
                        Ok((v8::Local::new(tc, module).cast(), out))
                    }
                }
            }
            None => {
                let e = tc.exception().unwrap_or_else(|| v8::undefined(tc).into());
                Err(v8::Local::new(tc, e))
            }
        }
    };
    leave_realm(scope, prev);
    out
}

/// A module of frame `ctx` has been fetched (and compiled, or failed): every load waiting on it goes on.
fn fetched(scope: &mut v8::PinScope, ctx: u32, url: &str, result: Result<(v8::Local<v8::Module>, Vec<String>), v8::Local<v8::Value>>) {
    let key = (ctx, url.to_string());
    let (waiting, requests) = match result {
        Ok((module, requests)) => {
            let g = v8::Global::new(scope, module);
            let w = with(scope, |m| {
                m.entries.insert(key.clone(), Entry::Ready { module: g, requests: requests.clone() });
                m.waiters.remove(&key).unwrap_or_default()
            });
            (w, Ok(requests))
        }
        Err(e) => {
            let g = v8::Global::new(scope, e);
            let w = with(scope, |m| {
                m.entries.insert(key.clone(), Entry::Failed(g));
                m.waiters.remove(&key).unwrap_or_default()
            });
            (w, Err(e))
        }
    };
    for id in waiting {
        with(scope, |m| {
            if let Some(l) = m.loads.get_mut(&id) {
                l.pending = l.pending.saturating_sub(1);
            }
        });
        match &requests {
            Ok(reqs) => {
                for r in reqs {
                    visit(scope, id, r);
                }
                settle_if_loaded(scope, id);
            }
            Err(e) => fail(scope, id, *e),
        }
    }
}

/// Reject load `id`: a fetch failure as Chrome words it (naming the module `import()` asked for), anything else
/// (a syntax error, an unresolvable specifier) as itself.
fn fail(scope: &mut v8::PinScope, id: u32, e: v8::Local<v8::Value>) {
    let Some((resolver, root)) = with(scope, |m| {
        let l = m.loads.get_mut(&id)?;
        if l.done {
            return None;
        }
        l.done = true;
        Some((l.resolver.clone(), l.root.clone()))
    }) else {
        return;
    };
    with(scope, |m| m.loads.remove(&id));
    let resolver = v8::Local::new(scope, &resolver);
    let is_load = e.is_native_error() && e.to_rust_string_lossy(scope) == "TypeError: load";
    let e = if is_load { type_error(scope, &format!("Failed to fetch dynamically imported module: {root}")) } else { e };
    resolver.reject(scope, e);
}

/// When nothing of load `id` is still being fetched: link and evaluate its root, and settle the `import()`.
fn settle_if_loaded(scope: &mut v8::PinScope, id: u32) {
    let Some((ctx, root, resolver)) = with(scope, |m| {
        let l = m.loads.get(&id)?;
        (!l.done && l.pending == 0).then(|| (l.ctx, l.root.clone(), l.resolver.clone()))
    }) else {
        return;
    };
    with(scope, |m| {
        if let Some(l) = m.loads.get_mut(&id) {
            l.done = true;
        }
        m.loads.remove(&id);
    });
    let module = with(scope, |m| match m.entries.get(&(ctx, root.clone())) {
        Some(Entry::Ready { module, .. }) => Some(module.clone()),
        _ => None,
    });
    let resolver = v8::Local::new(scope, &resolver);
    let Some(module) = module else {
        let e = type_error(scope, &format!("Failed to fetch dynamically imported module: {root}"));
        resolver.reject(scope, e);
        return;
    };
    let Some(realm) = realm_of(scope, ctx) else { return };
    let prev = enter_realm(scope, ctx);
    {
        let scope = &mut v8::ContextScope::new(scope, realm);
        let module = v8::Local::new(scope, &module);
        v8::tc_scope!(let tc, scope);
        if module.get_status() == v8::ModuleStatus::Uninstantiated && module.instantiate_module(tc, resolve_cb).is_none() {
            let e = tc.exception().unwrap_or_else(|| v8::undefined(tc).into());
            resolver.reject(tc, e);
        } else if module.get_status() == v8::ModuleStatus::Errored {
            let e = module.get_exception();
            resolver.reject(tc, e);
        } else {
            match module.evaluate(tc) {
                Some(done) => {
                    // The namespace once evaluation (top-level `await` included) has finished.
                    let ns = module.get_module_namespace();
                    let rg = v8::Global::new(tc, resolver);
                    let eid = {
                        let op_state = deno_core::JsRuntime::op_state_from(tc);
                        let mut st = op_state.borrow_mut();
                        let m = st.borrow_mut::<FrameModules>();
                        m.next_load += 1;
                        m.evaluating.insert(m.next_load, rg);
                        m.next_load
                    };
                    let data = v8::Array::new(tc, 2);
                    let ev = v8::Integer::new_from_unsigned(tc, eid);
                    data.set_index(tc, 0, ev.into());
                    data.set_index(tc, 1, ns);
                    let ok = build_fn_data(tc, cb_evaluated, "", 1, data.into());
                    let bad = build_fn_data(tc, cb_eval_failed, "", 1, data.into());
                    match v8::Local::<v8::Promise>::try_from(done) {
                        Ok(p) => {
                            let _ = p.then2(tc, ok, bad);
                        }
                        Err(_) => {
                            resolver.resolve(tc, ns);
                        }
                    }
                }
                None => {
                    let e = tc.exception().unwrap_or_else(|| v8::undefined(tc).into());
                    resolver.reject(tc, e);
                }
            }
        }
    }
    leave_realm(scope, prev);
}

fn resolver_and_ns<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments) -> Option<(v8::Local<'s, v8::PromiseResolver>, v8::Local<'s, v8::Value>)> {
    let d = crate::binding::protos::body_data(scope, args);
    let arr = v8::Local::<v8::Array>::try_from(d).ok()?;
    let eid = arr.get_index(scope, 0)?.uint32_value(scope)?;
    let ns = arr.get_index(scope, 1)?;
    let g = with(scope, |m| m.evaluating.remove(&eid))?;
    Some((v8::Local::new(scope, &g), ns))
}

fn cb_evaluated(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some((r, ns)) = resolver_and_ns(scope, &args) {
        r.resolve(scope, ns);
    }
}

fn cb_eval_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some((r, _)) = resolver_and_ns(scope, &args) {
        r.reject(scope, args.get(0));
    }
}

/// V8's link step: `specifier` as `referrer` imports it, from the referrer's frame map.
fn resolve_cb<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let spec = specifier.to_rust_string_lossy(scope);
    let (ctx, base) = module_url(scope, referrer)?;
    let url = resolve(&spec, &base).ok()?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let g = {
        let st = op_state.borrow();
        match st.try_borrow::<FrameModules>()?.entries.get(&(ctx, url))? {
            Entry::Ready { module, .. } => module.clone(),
            _ => return None,
        }
    };
    Some(v8::Local::new(scope, &g))
}

/// The frame and URL of a module this file compiled.
fn module_url(scope: &mut v8::PinScope, module: v8::Local<v8::Module>) -> Option<(u32, String)> {
    module_names(scope, module).map(|(c, base, _)| (c, base))
}

/// The frame, the base and the `import.meta.url` of a module this file compiled.
fn module_names(scope: &mut v8::PinScope, module: v8::Local<v8::Module>) -> Option<(u32, String, String)> {
    let h = module.get_identity_hash().get();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let list: Vec<(v8::Global<v8::Module>, u32, String, String)> = {
        let st = op_state.borrow();
        st.try_borrow::<FrameModules>()?.by_hash.get(&h)?.clone()
    };
    list.into_iter().find(|(g, _, _, _)| v8::Local::new(scope, g) == module).map(|(_, c, b, m)| (c, b, m))
}

fn ensure_meta(scope: &mut v8::PinScope) {
    if with(scope, |m| std::mem::replace(&mut m.meta_registered, true)) {
        return;
    }
    deno_core::register_external_module_import_meta_cb(scope, import_meta);
}

/// `import.meta` of a frame module: `url`, and `resolve()` against it.
fn import_meta<'s, 'i, 'm, 'o>(scope: &mut v8::PinScope<'s, 'i>, module: v8::Local<'m, v8::Module>, meta: v8::Local<'o, v8::Object>) {
    let module = v8::Local::new(scope, module);
    let Some((_, _, url)) = module_names(scope, module) else { return };
    let meta = v8::Local::new(scope, meta);
    let k = js_str(scope, "url");
    let v = str_val(scope, &url);
    meta.create_data_property(scope, k.into(), v);
    let d = str_val(scope, &url);
    let f = build_fn_data(scope, cb_meta_resolve, "resolve", 1, d.into());
    let rk = js_str(scope, "resolve");
    meta.create_data_property(scope, rk.into(), f.into());
}

fn cb_meta_resolve(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let base = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let spec = args.get(0).to_rust_string_lossy(scope);
    match resolve(&spec, &base) {
        Ok(u) => {
            let v = str_val(scope, &u);
            rv.set(v.into());
        }
        Err(msg) => {
            let e = type_error(scope, &msg);
            scope.throw_exception(e);
        }
    }
}
