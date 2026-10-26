//! A DEDICATED WORKER'S DEVTOOLS CHANNEL: `Runtime.evaluate`, `callFunctionOn`, `getProperties` and `releaseObject` on
//! a worker target's session, answered in the worker's own isolate, on its own thread.
//!
//! THE DEFECT. A worker is a target of its own (`Target.attachedToTarget`, type `worker`), announced to a client that
//! auto-attaches, and its realm is announced too -- but nothing carried a client's script into the worker's thread,
//! so `Runtime.evaluate` there was refused ("not supported by this engine yet") and Playwright's `worker.evaluate`
//! never returned. Chrome's worker inspector runs the command on the worker's thread, in its realm, and hands back
//! objects the client may call functions on (Playwright's utility script lives there).
//!
//! Here each dedicated worker that is a DevTools target takes [`Command`]s on a channel its thread serves between
//! two turns of its event loop ([`handle`]); [`call`] is the page thread's side. Objects a command hands out stay in
//! the worker ([`Objects`]), named `<tag>.<context>.<serial>` as every `objectId` this engine issues; an awaited
//! promise is driven on the worker's own loop.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use deno_core::{v8, JsRuntime, PollEventLoopOptions};
use serde_json::{json, Value};

/// One command on a worker's session, and where its answer goes.
pub(crate) struct Command {
    method: String,
    params: Value,
    /// The execution context id the client knows the worker's realm by (in the `objectId`s handed out).
    ctx: i64,
    reply: tokio::sync::oneshot::Sender<Result<Value, String>>,
}

fn channels() -> &'static Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Command>>> {
    static C: OnceLock<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Command>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The worker that is DevTools target `target` takes commands from here on.
pub(crate) fn register(target: &str) -> tokio::sync::mpsc::UnboundedReceiver<Command> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    channels().lock().unwrap_or_else(|e| e.into_inner()).insert(target.to_string(), tx);
    rx
}

/// The worker is gone: its target takes no more commands.
pub(crate) fn unregister(target: &str) {
    channels().lock().unwrap_or_else(|e| e.into_inner()).remove(target);
}

fn loaded_set() -> &'static Mutex<std::collections::HashSet<String>> {
    static L: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Worker target `target` has run its top-level script (Chrome's `Inspector.workerScriptLoaded` moment).
pub(crate) fn note_script_loaded(target: &str) {
    loaded_set().lock().unwrap_or_else(|e| e.into_inner()).insert(target.to_string());
}

/// Has worker target `target` run its top-level script?
pub fn script_loaded(target: &str) -> bool {
    loaded_set().lock().unwrap_or_else(|e| e.into_inner()).contains(target)
}

/// Unregisters a worker's target, and its console, when the worker's thread is done with it.
pub(crate) struct Guard(pub(crate) Option<String>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            unregister(t);
            loaded_set().lock().unwrap_or_else(|e| e.into_inner()).remove(t);
            webapi::devtools_log::set_worker_target(None);
        }
    }
}

/// A `Runtime.*` command on worker target `target`'s session, `ctx` the worker realm's context id: the result Chrome
/// answers, or the message of the `-32000` it refuses with.
pub async fn call(target: &str, method: &str, params: Value, ctx: i64) -> Result<Value, String> {
    let tx = channels().lock().unwrap_or_else(|e| e.into_inner()).get(target).cloned();
    let Some(tx) = tx else { return Err("Cannot find context with specified id".to_string()) };
    let (reply, answer) = tokio::sync::oneshot::channel();
    tx.send(Command { method: method.to_string(), params, ctx, reply }).map_err(|_| "Cannot find context with specified id".to_string())?;
    match tokio::time::timeout(std::time::Duration::from_secs(30), answer).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err("Execution context was destroyed.".to_string()),
        Err(_) => Err("the worker did not answer".to_string()),
    }
}

/// The objects a worker's session holds: what its `objectId`s name.
pub(crate) struct Objects {
    next: u32,
    map: HashMap<u32, v8::Global<v8::Value>>,
    /// The first part of every `objectId`: the worker isolate's, a random number as Chrome's is.
    tag: i64,
}

impl Objects {
    pub(crate) fn new() -> Objects {
        let mut b = [0u8; 8];
        let _ = getrandom::getrandom(&mut b);
        Objects { next: 1, map: HashMap::new(), tag: (i64::from_le_bytes(b) >> 1).abs() }
    }

    fn keep(&mut self, scope: &mut v8::PinScope, v: v8::Local<v8::Value>, ctx: i64) -> String {
        let id = self.next;
        self.next += 1;
        self.map.insert(id, v8::Global::new(scope, v));
        format!("{}.{}.{}", self.tag, ctx, id)
    }

    fn get<'s>(&self, scope: &mut v8::PinScope<'s, '_>, object_id: &str) -> Option<v8::Local<'s, v8::Value>> {
        let serial: u32 = object_id.rsplit('.').next()?.parse().ok()?;
        self.map.get(&serial).map(|g| v8::Local::new(scope, g))
    }

    fn release(&mut self, object_id: &str) -> bool {
        object_id.rsplit('.').next().and_then(|s| s.parse::<u32>().ok()).is_some_and(|s| self.map.remove(&s).is_some())
    }
}

/// What a script or a call did: its value, or what it threw with the throw's position (0-based).
type Outcome = Result<v8::Global<v8::Value>, (v8::Global<v8::Value>, u32, u32)>;

/// Serve one command on the worker's thread.
pub(crate) async fn handle(runtime: &mut JsRuntime, objects: &mut Objects, cmd: Command) {
    let p = &cmd.params;
    let flag = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
    let r = match cmd.method.as_str() {
        "Runtime.evaluate" => {
            let src = p.get("expression").and_then(Value::as_str).unwrap_or("").to_string();
            let ran = run_script(runtime, &src);
            finish(runtime, objects, ran, flag("awaitPromise"), flag("returnByValue"), cmd.ctx).await
        }
        "Runtime.callFunctionOn" => match call_function(runtime, objects, p) {
            Ok(ran) => finish(runtime, objects, ran, flag("awaitPromise"), flag("returnByValue"), cmd.ctx).await,
            Err(e) => Err(e),
        },
        "Runtime.getProperties" => properties(runtime, objects, p, cmd.ctx),
        "Runtime.releaseObject" => {
            let oid = p.get("objectId").and_then(Value::as_str).unwrap_or("");
            if objects.release(oid) {
                Ok(json!({}))
            } else {
                Err("Could not find object with given id".to_string())
            }
        }
        "Runtime.releaseObjectGroup" => Ok(json!({})),
        m => Err(format!("'{m}' wasn't found")),
    };
    let _ = cmd.reply.send(r);
}

/// Compile and run `src` in the worker's realm, as `Runtime.evaluate` compiles it: no script name.
fn run_script(runtime: &mut JsRuntime, src: &str) -> Outcome {
    let ctx = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
    let scope: &mut v8::PinScope = scope;
    v8::tc_scope!(let tc, scope);
    let empty = v8::String::new(tc, "").expect("an empty string");
    let origin = v8::ScriptOrigin::new(tc, empty.into(), 0, 0, false, 0, None, false, false, false, None);
    let ran = v8::String::new(tc, src).and_then(|code| v8::Script::compile(tc, code, Some(&origin))).and_then(|s| s.run(tc));
    match ran {
        Some(v) => Ok(v8::Global::new(tc, v)),
        None => {
            let (m, e) = (tc.message(), tc.exception());
            Err(caught(tc, m, e))
        }
    }
}

/// What a `TryCatch` caught -- its message and exception, read off it by the caller -- as the value and the throw's
/// line and column, 0-based.
fn caught<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    message: Option<v8::Local<'s, v8::Message>>,
    exception: Option<v8::Local<'s, v8::Value>>,
) -> (v8::Global<v8::Value>, u32, u32) {
    let (line, column) = match message {
        Some(m) => (m.get_line_number(scope).map(|l| l.saturating_sub(1) as u32).unwrap_or(0), m.get_start_column() as u32),
        None => (0, 0),
    };
    let value = exception.unwrap_or_else(|| v8::undefined(scope).into());
    (v8::Global::new(scope, value), line, column)
}

/// `Runtime.callFunctionOn`: the declaration compiled in the worker's realm and called with `this` and the arguments
/// as the client named them. `Err` is Chrome's refusal of an `objectId` it never issued.
fn call_function(runtime: &mut JsRuntime, objects: &Objects, p: &Value) -> Result<Outcome, String> {
    let decl = p.get("functionDeclaration").and_then(Value::as_str).unwrap_or("").to_string();
    let ctx = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
    let scope: &mut v8::PinScope = scope;
    let this: v8::Local<v8::Value> = match p.get("objectId").and_then(Value::as_str) {
        Some(oid) => objects.get(scope, oid).ok_or_else(|| "Could not find object with given id".to_string())?,
        None => v8::undefined(scope).into(),
    };
    let mut argv: Vec<v8::Local<v8::Value>> = Vec::new();
    for a in p.get("arguments").and_then(Value::as_array).cloned().unwrap_or_default() {
        let v: v8::Local<v8::Value> = if let Some(oid) = a.get("objectId").and_then(Value::as_str) {
            objects.get(scope, oid).ok_or_else(|| "Could not find object with given id".to_string())?
        } else if let Some(v) = a.get("value") {
            let text = serde_json::to_string(v).unwrap_or_else(|_| "null".into());
            let s = v8::String::new(scope, &text).ok_or("Invalid argument")?;
            v8::json::parse(scope, s).ok_or("Invalid argument")?
        } else {
            match a.get("unserializableValue").and_then(Value::as_str) {
                None => v8::undefined(scope).into(),
                Some("NaN") => v8::Number::new(scope, f64::NAN).into(),
                Some("Infinity") => v8::Number::new(scope, f64::INFINITY).into(),
                Some("-Infinity") => v8::Number::new(scope, f64::NEG_INFINITY).into(),
                Some("-0") => v8::Number::new(scope, -0.0).into(),
                Some(big) => {
                    let s = v8::String::new(scope, &format!("BigInt('{}')", big.trim_end_matches('n'))).ok_or("Invalid unserializable value")?;
                    v8::Script::compile(scope, s, None).and_then(|s| s.run(scope)).ok_or("Invalid unserializable value")?
                }
            }
        };
        argv.push(v);
    }
    v8::tc_scope!(let tc, scope);
    let src = v8::String::new(tc, &format!("({decl})")).ok_or("bad function source")?;
    let Some(f) = v8::Script::compile(tc, src, None).and_then(|s| s.run(tc)) else {
        let (m, e) = (tc.message(), tc.exception());
        return Ok(Err(caught(tc, m, e)));
    };
    let Ok(f) = v8::Local::<v8::Function>::try_from(f) else { return Err("Given expression does not evaluate to a function".to_string()) };
    Ok(match f.call(tc, this, &argv) {
        Some(v) => Ok(v8::Global::new(tc, v)),
        None => {
            let (m, e) = (tc.message(), tc.exception());
            Err(caught(tc, m, e))
        }
    })
}

/// The answer to an evaluation: its promise awaited on the worker's loop when asked, then the value -- by value, or
/// held -- or the exception as Chrome's `exceptionDetails`.
async fn finish(runtime: &mut JsRuntime, objects: &mut Objects, ran: Outcome, await_promise: bool, by_value: bool, ctx: i64) -> Result<Value, String> {
    let (settled, text) = match ran {
        Ok(v) if await_promise => settle(runtime, v).await,
        other => (other, "Uncaught".to_string()),
    };
    let c = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), c);
    let scope: &mut v8::PinScope = scope;
    Ok(match settled {
        Ok(g) => {
            let v = v8::Local::new(scope, &g);
            json!({ "result": remote(scope, objects, v, by_value, ctx) })
        }
        Err((g, line, column)) => {
            let v = v8::Local::new(scope, &g);
            let exception = remote(scope, objects, v, false, ctx);
            json!({ "result": exception.clone(), "exceptionDetails": {
                "exceptionId": 1, "text": text, "lineNumber": line, "columnNumber": column, "exception": exception,
            } })
        }
    })
}

/// A promise driven to settlement on the worker's own loop (bounded as an awaited evaluation is); anything else as it
/// is. A rejection comes back as what it rejected with, with Chrome's `Uncaught (in promise)` text.
async fn settle(runtime: &mut JsRuntime, g: v8::Global<v8::Value>) -> (Outcome, String) {
    let is_promise = {
        let c = runtime.main_context();
        v8::scope_with_context!(scope, runtime.v8_isolate(), c);
        v8::Local::new(scope, &g).is_promise()
    };
    if !is_promise {
        return (Ok(g), "Uncaught".to_string());
    }
    let promise = g.clone();
    let resolve = runtime.resolve(g);
    let waited = tokio::time::timeout(std::time::Duration::from_secs(15), runtime.with_event_loop_promise(resolve, PollEventLoopOptions::default())).await;
    if let Ok(Ok(v)) = waited {
        return (Ok(v), "Uncaught".to_string());
    }
    let c = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), c);
    let scope: &mut v8::PinScope = scope;
    let local = v8::Local::new(scope, &promise);
    let reason = match v8::Local::<v8::Promise>::try_from(local) {
        Ok(p) if p.state() == v8::PromiseState::Rejected => {
            p.mark_as_handled();
            p.result(scope)
        }
        _ => {
            let m = v8::String::new(scope, "awaitPromise timed out").expect("a message");
            v8::Exception::error(scope, m)
        }
    };
    let text = if reason.is_native_error() {
        let first = reason.to_rust_string_lossy(scope);
        format!("Uncaught (in promise) {first}")
    } else {
        "Uncaught (in promise)".to_string()
    };
    (Err((v8::Global::new(scope, reason), 0, 0)), text)
}

/// `v` as a `Runtime.RemoteObject`: by value when asked (or a primitive), else held under an `objectId` with its type,
/// class and description.
fn remote(scope: &mut v8::PinScope, objects: &mut Objects, v: v8::Local<v8::Value>, by_value: bool, ctx: i64) -> Value {
    let described = webapi::devtools_log::remote_by_value(scope, v);
    if !v.is_object() {
        return described;
    }
    if by_value {
        let value = v8::json::stringify(scope, v).map(|s| s.to_rust_string_lossy(scope)).and_then(|s| serde_json::from_str::<Value>(&s).ok());
        return match value {
            Some(value) => json!({ "type": "object", "value": value }),
            None => json!({ "type": if v.is_function() { "function" } else { "object" }, "value": {} }),
        };
    }
    let mut r = described;
    r["objectId"] = json!(objects.keep(scope, v, ctx));
    r
}

/// `Runtime.getProperties` of a held object: its own properties, each value held or by value, with their attributes.
fn properties(runtime: &mut JsRuntime, objects: &mut Objects, p: &Value, ctx: i64) -> Result<Value, String> {
    let oid = p.get("objectId").and_then(Value::as_str).unwrap_or("").to_string();
    let c = runtime.main_context();
    v8::scope_with_context!(scope, runtime.v8_isolate(), c);
    let scope: &mut v8::PinScope = scope;
    let v = objects.get(scope, &oid).ok_or_else(|| "Could not find object with given id".to_string())?;
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return Ok(json!({ "result": [] })) };
    let args = v8::GetPropertyNamesArgs {
        mode: v8::KeyCollectionMode::OwnOnly,
        property_filter: v8::PropertyFilter::ALL_PROPERTIES,
        index_filter: v8::IndexFilter::IncludeIndices,
        key_conversion: v8::KeyConversionMode::ConvertToString,
    };
    let mut out = Vec::new();
    if let Some(names) = o.get_own_property_names(scope, args) {
        for i in 0..names.length() {
            let Some(key) = names.get_index(scope, i) else { continue };
            let name = key.to_rust_string_lossy(scope);
            let Some(value) = o.get(scope, key) else { continue };
            let value = remote(scope, objects, value, false, ctx);
            out.push(json!({ "name": name, "value": value, "writable": true, "configurable": true, "enumerable": true, "isOwn": true }));
        }
    }
    Ok(json!({ "result": out }))
}
