//! The worker scope's KERNEL SERVICE bodies (invariant I9).
//!
//! `WorkerGlobalScope` and `DedicatedWorkerGlobalScope` are `interface!` tables in `korvus-webapi`
//! (`core/workers/worker_global_scope.rs`). Every row of them whose body is an ALGORITHM SERVICE
//! rather than a Web API fact is built HERE: the five timer members over [`crate::scheduler`],
//! `importScripts` over the transport plus a real V8 compile, `close` over this isolate's run flag,
//! `postMessage` over the worker→main channel, and the two animation-frame methods over the same
//! scheduler. The kernel provides the service; the table owns the shape.
//!
//! This is the shape the `Worker` table has used since W4.c — a FACTORY op that hands back a genuine
//! native function carrying the ROW's own `name` and `length` — generalised to a member set. A realm
//! that does not register the factory (the bare, messaging-only worker seam) gets the D2 stub for
//! each row, which is the honest not-implemented answer and is counted as one.
//!
//! THE FIVE TIMER MEMBERS MOVED OUT to [`crate::scope_timers`], unchanged, because the
//! WINDOW needs the same five and the only thing the two realms disagree about is the holder name
//! WebIDL puts in the error messages. They are still this table's rows; the bodies are simply not
//! written twice. [`op_worker_scope_fn`] below asks for them with `'WorkerGlobalScope'`.
//!
//! WHAT THIS REPLACES, line for line: `worker-host.js`'s `kPump`/`kArm` and the four global
//! assignments that published them, the same file's `postMessage` and `close` arrows (neither of
//! which was `[native code]` — the scope probe measured `close` reporting its own JS source inside
//! the worker), and `worker-finalize.js`'s `op_nativize`-masked `importScripts`,
//! `requestAnimationFrame` and `cancelAnimationFrame`.

use deno_core::{op2, v8, JsRuntime};

use crate::scheduler::Scheduler;
use crate::scope_timers::{require_args, start_pump};
use crate::worker::{apply_transfer, import_scripts_source, parse_import_url, ModuleWorker, WorkerSender};
use crate::HostLocation;

/// `self.close()` was called: the receive loops stop and the isolate drains.
///
/// This USED TO BE `globalThis.__closed`, an own property of the worker global that
/// `Object.getOwnPropertyNames(self)` reports and no real worker has. It is `OpState` now — the flag
/// is the kernel's, not the page's, and page script can neither read nor clear it.
#[derive(Default)]
pub struct WorkerClosed(std::cell::Cell<bool>);

impl WorkerClosed {
    /// Whether `close()` has been called. `worker_recv.rs`'s three loops read the flag straight off
    /// `OpState` -- they are Rust now, and an op call to ask their own kernel would be the long way
    /// round -- while page script still reaches it only through the `close` ROW that sets it.
    pub(crate) fn get(&self) -> bool {
        self.0.get()
    }
}

// `op_worker_scope_closed` IS DELETED. It existed for one reader -- `worker-host.js`'s
// three receive loops, which its own doc named -- and those loops are `worker_recv.rs` now and read
// `WorkerClosed` off `OpState` directly. An op with no JS caller is a name in a table nothing can
// reach: `Deno` is stripped from the worker global before any script runs, so it was not even
// page-visible surface; it was just the last piece of the JS loop's plumbing.

/// `importScripts(...urls)` — SYNCHRONOUS, per spec: each URL is fetched through the transport
/// (resolved against THIS worker's own script URL, never the page's) and run to completion, in order,
/// in the worker's global scope, before the call returns, so a later call or a top-level statement can
/// depend on what an earlier one defined.
///
/// The script is COMPILED AND RUN, not `eval`'d: `worker-finalize.js` used `(0, eval)(code)` because a
/// JS body had nothing else to reach for; a V8 `Script` run in the current context is the same
/// semantics without the indirection — and without a call site of `eval` inside the engine.
///
/// Its `(USVString or TrustedScriptURL)...` arguments are ALL converted first, as the binding converts them -- a
/// TrustedScriptURL read as itself, nothing on it called -- so a `toString` that throws stops the call before any
/// script runs (this fetched and ran each URL before converting the next).
///
/// THE ALGORITHM IS BLINK'S `WorkerGlobalScope::ImportScriptsInternal` (worker_global_scope.cc:292-399), each step
/// measured on stock Chrome 150 in a blob, a URL-loaded and a shared worker (`tests/fixtures/worker-script-loading`):
///
/// 1. a MODULE worker has no `importScripts`: `TypeError: ... Module scripts don't support importScripts().`, before
///    anything else -- korvus loaded and ran the script;
/// 2. no URL: nothing;
/// 3. EVERY URL is parsed before any is fetched: `importScripts(a, 'http://[')` throws `SyntaxError` ("The URL
///    'http://[' is invalid.") and `a` never runs -- korvus ran `a`, then threw a `NetworkError` of its own words;
/// 4. then each in turn is fetched ([`import_scripts_source`]) and run: a failed fetch is `NetworkError` ("The script
///    at '<url>' failed to load.") -- the scripts before it have run, the ones after it do not; a script's own throw
///    reaches the caller as itself, where it was thrown; its SYNTAX error is the V8 `SyntaxError` with the operation's
///    context on its message, as V8's exception propagation callback has Blink put it (the compile runs in the
///    callback's own frame; v8_initializer.cc:399, v8_binding_for_core.cc:878); and a script another origin delivered
///    has its errors muted -- either becomes the `NetworkError` (v8_script_runner.cc:719-760).
///
/// The script is compiled under the URL it was asked for, its fragment cut (`ClassicScript::StripFragmentIdentifier
/// (complete_url)`, worker_global_scope.cc:374-377), not the response's after a redirect.
fn cb_import_scripts(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    const CONTEXT: &str = "Failed to execute 'importScripts' on 'WorkerGlobalScope': ";
    let mut urls = Vec::with_capacity(args.length().max(0) as usize);
    for i in 0..args.length() {
        let Some(url) = webapi::sink_text(scope, args.get(i), "TrustedScriptURL") else { return };
        urls.push(url);
    }
    let (base, module) = {
        let op_state = JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        (st.try_borrow::<HostLocation>().map(|l| l.get()).unwrap_or_default(), st.try_borrow::<ModuleWorker>().is_some())
    };
    if module {
        let Some(m) = v8::String::new(scope, &format!("{CONTEXT}Module scripts don't support importScripts().")) else { return };
        let ex = v8::Exception::type_error(scope, m);
        scope.throw_exception(ex);
        return;
    }
    let mut parsed = Vec::with_capacity(urls.len());
    for url in &urls {
        match parse_import_url(&base, url) {
            Some(u) => parsed.push(u),
            None => {
                webapi::binding::protos::throw_dom(scope, &format!("{CONTEXT}The URL '{url}' is invalid."), "SyntaxError");
                return;
            }
        }
    }
    for url in parsed {
        let failed = format!("{CONTEXT}The script at '{}' failed to load.", elided(url.as_str()));
        let fetched = {
            let op_state = JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            import_scripts_source(&mut st, &url)
        };
        let Some(script) = fetched else {
            webapi::binding::protos::throw_dom(scope, &failed, "NetworkError");
            return;
        };
        let mut name = url.clone();
        name.set_fragment(None);
        let Some(src) = v8::String::new(scope, &script.source) else { return };
        // Compiled under its own URL: stack frames read `at http://.../x.js:40:41`
        // where they read `at <anonymous>:40:41`, the classic tell of evaluated code.
        let Some(name) = v8::String::new(scope, name.as_str()) else { return };
        let origin = v8::ScriptOrigin::new(scope, name.into(), 0, 0, false, 0, None, false, false, false, None);
        let outcome = {
            v8::tc_scope!(let tc, scope);
            let compiled = v8::Script::compile(tc, src, Some(&origin));
            let ran = compiled.is_some_and(|s| s.run(tc).is_some());
            if ran {
                Imported::Ran
            } else if !tc.has_caught() || !tc.can_continue() {
                // terminating (`Worker.terminate()`): nothing more runs, and the termination goes on unwinding
                tc.rethrow();
                Imported::Stopped
            } else if script.muted {
                tc.reset();
                Imported::Muted
            } else {
                if compiled.is_none() {
                    if let Some(ex) = tc.exception().and_then(|e| v8::Local::<v8::Object>::try_from(e).ok()) {
                        add_context(tc, ex, CONTEXT);
                    }
                }
                // ReThrow keeps the throw's own site -- the imported script's line, which a worker's `onerror`
                // reports (v8_script_runner.cc:719-731)
                tc.rethrow();
                Imported::Stopped
            }
        };
        match outcome {
            Imported::Ran => {}
            Imported::Stopped => return,
            Imported::Muted => {
                webapi::binding::protos::throw_dom(scope, &failed, "NetworkError");
                return;
            }
        }
    }
}

/// How one imported script's run ended.
enum Imported {
    Ran,
    /// a throw is pending (or the worker is terminating): the call ends there
    Stopped,
    /// another origin's script threw: the caller gets a `NetworkError` instead
    Muted,
}

/// Blink's `ApplyContextToException` for a plain error (v8_binding_for_core.cc:886-898): `message` becomes
/// `<context><message>`, an own data property.
fn add_context(scope: &mut v8::PinScope, ex: v8::Local<v8::Object>, context: &str) {
    let Some(key) = v8::String::new(scope, "message") else { return };
    let old = ex.get(scope, key.into()).and_then(|m| m.to_string(scope)).map(|m| m.to_rust_string_lossy(scope)).unwrap_or_default();
    let Some(new) = v8::String::new(scope, &format!("{context}{old}")) else { return };
    let _ = ex.create_data_property(scope, key.into(), new.into());
}

/// `KURL::ElidedString` (kurl.cc:223-231): a URL longer than 1024 characters is shown as its first 511, `...`, and
/// its last 510. (A parsed URL is ASCII, so characters are its UTF-16 units.)
pub(crate) fn elided(url: &str) -> String {
    let chars: Vec<char> = url.chars().collect();
    if chars.len() <= 1024 {
        return url.to_string();
    }
    let head: String = chars[..511].iter().collect();
    let tail: String = chars[chars.len() - 510..].iter().collect();
    format!("{head}...{tail}")
}

/// `self.close()` — ends this worker: the receive loops break and the isolate drains.
fn cb_close(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let op_state = JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<WorkerClosed>().is_none() {
        st.put(WorkerClosed::default());
    }
    st.borrow::<WorkerClosed>().0.set(true);
}

/// `self.postMessage(message, transfer)` in a dedicated worker: structured-clone the value to bytes,
/// ship them to the main thread, then detach any transferred ArrayBuffers.
fn cb_post_message<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "DedicatedWorkerGlobalScope", "postMessage") {
        return;
    }
    let value = args.get(0);
    let transfer = args.get(1);
    // A platform object no structured clone may copy is a DataCloneError, and transferred
    // ports ride a port frame.
    if !webapi::refuse_platform_objects(scope, value, "DedicatedWorkerGlobalScope", "postMessage") {
        return;
    }
    let bytes = crate::worker::worker_out_frame(scope, value, transfer);
    {
        let op_state = JsRuntime::op_state_from(scope);
        let state = op_state.borrow();
        if let Some(tx) = state.try_borrow::<WorkerSender>() {
            let _ = tx.0.send(bytes);
            crate::worker::wake_page();
        }
    }
    apply_transfer(scope, transfer);
}

/// `requestAnimationFrame(cb)` — a worker has no vsync, so this is the realm's own timer at ~16 ms,
/// exactly as the JS it replaces did. The id IS the timer id, which is what makes
/// `cancelAnimationFrame` the same clear and removes the `Map` + counter the JS kept beside it.
fn cb_raf(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "DedicatedWorkerGlobalScope", "requestAnimationFrame") {
        return;
    }
    let Ok(cb) = v8::Local::<v8::Function>::try_from(args.get(0)) else { return };
    let d: v8::Local<v8::Value> = cb.into();
    let wrapper = v8::Function::builder(webapi::binding::refs::f(cb_raf_fire))
        .data(d)
        .length(0)
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .build(scope)
        .unwrap();
    let cbg = v8::Global::new(scope, wrapper);
    let (id, claim) = {
        let op_state = JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let Some(sched) = st.try_borrow_mut::<Scheduler>() else { return };
        let id = sched.set_timer(cbg, Vec::new(), None, 16.0, false, false);
        (id, sched.claim_pump())
    };
    if claim {
        start_pump(scope);
    }
    rv.set_uint32(id);
}

/// The frame callback: call the page's with this realm's high-resolution time, read from the realm's
/// own `performance` (the worker's timeline, not the page's).
fn cb_raf_fire(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(cb) = v8::Local::<v8::Function>::try_from(webapi::binding::protos::body_data(scope, &args)) else { return };
    let now = now_ms(scope);
    let t: v8::Local<v8::Value> = v8::Number::new(scope, now).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    v8::tc_scope!(let tc, scope);
    cb.call(tc, undef, &[t]);
    tc.reset();
}

/// `performance.now()` in this realm, or 0 where there is no timeline.
fn now_ms(scope: &mut v8::PinScope) -> f64 {
    let global = scope.get_current_context().global(scope);
    let Some(pk) = v8::String::new(scope, "performance") else { return 0.0 };
    let Some(perf) = global.get(scope, pk.into()).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return 0.0 };
    let Some(nk) = v8::String::new(scope, "now") else { return 0.0 };
    let Some(now) = perf.get(scope, nk.into()).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return 0.0 };
    let recv: v8::Local<v8::Value> = perf.into();
    v8::tc_scope!(let tc, scope);
    let r = now.call(tc, recv, &[]).and_then(|v| v.number_value(tc)).unwrap_or(0.0);
    tc.reset();
    r
}

fn cb_caf(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // WebIDL's `cancelAnimationFrame(unsigned long handle)`: Chrome 150.0.7871.46 counts the argument
    // ("1 argument required, but only 0 present", measured), as this realm's
    // `requestAnimationFrame` always did.
    if !require_args(scope, &args, 1, "DedicatedWorkerGlobalScope", "cancelAnimationFrame") {
        return;
    }
    // a frame handle is the scheduler's own id, not a per-realm timer handle
    crate::scope_timers::clear_scheduler_timer(scope, &args);
}

/// THE FACTORY the two scope tables call for every row whose body is a kernel service. Answers a
/// genuine native function with the ROW's `name` and `length`, or `undefined` for a name this kernel
/// does not serve — at which point the table mounts its D2 stub for that row.
#[op2]
pub fn op_worker_scope_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    #[string] name: &str,
    #[smi] length: i32,
) -> v8::Local<'s, v8::Value> {
    // The five timer members are [`crate::scope_timers`]'s, asked for with THIS realm's holder name —
    // the one thing a window and a worker disagree about.
    if let Some(f) = crate::scope_timers::timer_row(scope, name, length, "WorkerGlobalScope") {
        return f.into();
    }
    // webapi's kernel builder, not `worker.rs`'s copy: it RECORDS what it builds while a template capture
    // runs, so the worker scope interfaces take the template step like every other.
    let f = match name {
        "importScripts" => webapi::binding::protos::build_kernel_fn(scope, cb_import_scripts, name, length),
        "close" => webapi::binding::protos::build_kernel_fn(scope, cb_close, name, length),
        "postMessage" => webapi::binding::protos::build_kernel_fn(scope, cb_post_message, name, length),
        "requestAnimationFrame" => webapi::binding::protos::build_kernel_fn(scope, cb_raf, name, length),
        "cancelAnimationFrame" => webapi::binding::protos::build_kernel_fn(scope, cb_caf, name, length),
        _ => return v8::undefined(scope).into(),
    };
    f.into()
}
