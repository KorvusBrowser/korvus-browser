//! The page's DevTools log: what a CDP client with `Runtime` enabled is told the page logged -- each `console.*`
//! call with its arguments (`Runtime.consoleAPICalled`) and each error nothing handled (`Runtime.exceptionThrown`) --
//! and the bindings it added (`Runtime.addBinding`) with the calls the page made to them (`Runtime.bindingCalled`).
//!
//! OFF UNTIL A CLIENT ASKS. [`set_capture`] is the `Runtime` domain's switch; while it is off nothing is kept and no
//! value is held, so a page with no DevTools client keeps no reference it would not otherwise keep (invariant I9/J2).
//! The entries keep the arguments as live values -- a client is handed each as a `RemoteObject` it can inspect, as
//! Chrome's inspector hands them -- and are drained by the runtime (`Engine::take_devtools_log`), on the page's
//! thread, after every command and every slice the page runs.
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use deno_core::v8;

/// One `console.*` call.
pub struct ConsoleCall {
    /// CDP's `Runtime.consoleAPICalled.type`: `log`, `debug`, `info`, `error`, `warning`, `dir`, `table`, `trace`,
    /// `startGroup`, `startGroupCollapsed`, `assert`, `count`, `timeEnd`.
    pub kind: &'static str,
    pub args: Vec<v8::Global<v8::Value>>,
    /// The isolated world it was made in (`None`: the page's own, or one of its frames').
    pub world: Option<usize>,
    /// Milliseconds since the epoch, as `Runtime.Timestamp`.
    pub timestamp: f64,
}

/// One error nothing handled, as the page's `error` event reported it.
pub struct PageError {
    /// The event's message (`Uncaught Error: boom`).
    pub text: String,
    pub url: String,
    pub line: u32,
    pub column: u32,
    /// The thrown value (`None` for a worker's error reported one level up).
    pub value: Option<v8::Global<v8::Value>>,
    pub class_name: Option<String>,
    /// The Error's own `stack`, which is what Chrome's `description` is.
    pub description: Option<String>,
    pub is_error: bool,
    pub timestamp: f64,
}

pub enum Entry {
    Console(ConsoleCall),
    Error(PageError),
}

static CAPTURE: AtomicBool = AtomicBool::new(false);

thread_local! {
    static LOG: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// How many entries wait at most: a page that logs in a loop while nobody drains it does not grow without bound.
const CAP: usize = 10_000;

/// The `Runtime` domain on or off. Turning it off drops what was waiting.
pub fn set_capture(on: bool) {
    CAPTURE.store(on, Ordering::Relaxed);
    if !on {
        LOG.with(|l| l.borrow_mut().clear());
    }
}

pub fn capturing() -> bool {
    CAPTURE.load(Ordering::Relaxed)
}

/// Everything logged since the last call, in order.
pub fn take() -> Vec<Entry> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// Drop what was logged and not drained: its values are handles into the realm being torn down
/// ([`crate::release_realm_handles`]).
pub(crate) fn forget() {
    LOG.with(|l| l.borrow_mut().clear());
}

fn push(e: Entry) {
    LOG.with(|l| {
        let mut l = l.borrow_mut();
        if l.len() < CAP {
            l.push(e);
        }
    });
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// A `console.*` call of `kind` with `args`, in the realm that is running.
pub(crate) fn console(scope: &mut v8::PinScope, kind: &'static str, args: &[v8::Local<v8::Value>]) {
    if !capturing() {
        return;
    }
    // a dedicated worker's console goes to its own target's session, described as it crosses ([`worker_console`])
    if WORKER_TARGET.with(|t| t.borrow().is_some()) {
        worker_console(scope, kind, args);
        return;
    }
    let ctx = scope.get_current_context();
    let world = crate::core::html::isolated_world::devtools_realm_of(scope, ctx);
    let args = args.iter().map(|a| v8::Global::new(scope, *a)).collect();
    push(Entry::Console(ConsoleCall { kind, args, world, timestamp: now_ms() }));
}

/// An error the page's `error` event reported and nothing cancelled.
pub(crate) fn error(scope: &mut v8::PinScope, err: v8::Local<v8::Value>, text: &str, url: &str, line: u32, column: u32) {
    if !capturing() {
        return;
    }
    let is_error = err.is_native_error();
    let (value, class_name, description) = if err.is_null_or_undefined() {
        (None, None, None)
    } else {
        let (class, desc) = match v8::Local::<v8::Object>::try_from(err) {
            Ok(obj) => {
                let class = obj.get_constructor_name().to_rust_string_lossy(scope);
                let desc = if is_error {
                    v8::String::new(scope, "stack").and_then(|k| obj.get(scope, k.into())).filter(|s| s.is_string()).map(|s| s.to_rust_string_lossy(scope))
                } else {
                    None
                };
                (Some(class), desc)
            }
            Err(_) => (None, None),
        };
        (Some(v8::Global::new(scope, err)), class, desc)
    };
    push(Entry::Error(PageError {
        text: text.to_string(),
        url: url.to_string(),
        line,
        column,
        value,
        class_name,
        description,
        is_error,
        timestamp: now_ms(),
    }));
}

// ── the bindings a client added ──────────────────────────────────────────────────────────────────────

static BINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CALLS: Mutex<Vec<(String, String, Option<usize>)>> = Mutex::new(Vec::new());

/// `Runtime.addBinding`: a global function `name` in every document of the page from now on.
pub fn add_binding(name: &str) {
    let mut b = BINDINGS.lock().unwrap_or_else(|e| e.into_inner());
    if !name.is_empty() && !b.iter().any(|n| n == name) {
        b.push(name.to_string());
    }
}

/// `Runtime.removeBinding`: later documents no longer get it (the current one keeps the function, as Chrome's does).
pub fn remove_binding(name: &str) {
    BINDINGS.lock().unwrap_or_else(|e| e.into_inner()).retain(|n| n != name);
}

pub fn binding_names() -> Vec<String> {
    BINDINGS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

static NEW_DOC_SCRIPTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// `Page.addScriptToEvaluateOnNewDocument`'s scripts, `(world name, source)` (`""` the page's own world): what every
/// frame document runs before its own scripts (`browsing_context`'s DevTools start), as the page's do.
pub fn set_new_document_scripts(list: Vec<(String, String)>) {
    *NEW_DOC_SCRIPTS.lock().unwrap_or_else(|e| e.into_inner()) = list;
}

pub fn new_document_scripts() -> Vec<(String, String)> {
    NEW_DOC_SCRIPTS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The calls the page made to the bindings since the last call: `(name, payload, world)`.
pub fn take_binding_calls() -> Vec<(String, String, Option<usize>)> {
    std::mem::take(&mut *CALLS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// A binding's body: its name rides as the function's data, its one argument is the payload (a string, as Chrome's
/// `binding(payload)` takes it).
fn cb_binding(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = args.data().to_rust_string_lossy(scope);
    let payload = if args.length() > 0 && !args.get(0).is_null_or_undefined() { args.get(0).to_rust_string_lossy(scope) } else { String::new() };
    let ctx = scope.get_current_context();
    let world = crate::core::html::isolated_world::devtools_realm_of(scope, ctx);
    let mut calls = CALLS.lock().unwrap_or_else(|e| e.into_inner());
    if calls.len() < CAP {
        calls.push((name, payload, world));
    }
}

/// Install `names` as globals of the realm that is current. Nothing to install, nothing added.
pub fn install_bindings(scope: &mut v8::PinScope, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let g = scope.get_current_context().global(scope);
    for name in names {
        let data = crate::binding::protos::str_val(scope, name);
        let f = crate::binding::protos::build_fn_data(scope, cb_binding, name, 1, data);
        crate::binding::protos::set_prop(scope, g, name, f.into());
    }
}

/// Every binding a client added, on the realm that is current -- where a new document's injected scripts run, before
/// them, so a script a client added can call one.
pub fn install_bindings_current(scope: &mut v8::PinScope) {
    let names = binding_names();
    install_bindings(scope, &names);
}

// ── downloads and file choosers a client takes over ─────────────────────────────────────────────────

// ── Debugger domain (D106): the classic scripts this page compiled ───────────────────────────────
//
// CDP's `Debugger` domain serves the Sources side of DevTools against korvus's own engine: which scripts parsed and
// their text. Every classic script -- inline, external `<script src>` and a string timer -- compiles through ONE
// point (`core/html/browsing_context.rs::classic_script`), which records it here when a client has the Debugger
// domain on. OFF until then ([`set_debugger_capture`], the domain's switch), so a run no one is debugging records
// nothing and holds no source. Live pause/step/resume is the V8 inspector's and is NOT recorded here.
static DEBUGGER_CAPTURE: AtomicBool = AtomicBool::new(false);
static NEXT_SCRIPT_ID: AtomicU32 = AtomicU32::new(1);
static SCRIPTS: Mutex<Vec<ParsedScript>> = Mutex::new(Vec::new());
static SCRIPTS_NEW: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// One classic script the page compiled: its id, the URL it reads as, where its first line sits, and its text.
#[derive(Clone, Debug)]
pub struct ParsedScript {
    pub id: u32,
    pub url: String,
    pub start_line: i32,
    pub source: String,
}

/// The `Debugger` domain's switch. On while a client has `Debugger.enable`d.
pub fn set_debugger_capture(on: bool) {
    DEBUGGER_CAPTURE.store(on, Ordering::Relaxed);
}

/// Whether a client is debugging -- the gate the compile point checks before it records a thing.
pub fn debugger_capturing() -> bool {
    DEBUGGER_CAPTURE.load(Ordering::Relaxed)
}

/// Record a classic script the page just compiled; returns its id. Called only while [`debugger_capturing`]. An
/// empty source is not a script.
pub fn record_script(url: &str, source: &str, start_line: i32) -> Option<u32> {
    if source.is_empty() {
        return None;
    }
    let id = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
    SCRIPTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(ParsedScript { id, url: url.to_string(), start_line, source: source.to_string() });
    SCRIPTS_NEW.lock().unwrap_or_else(|e| e.into_inner()).push(id);
    Some(id)
}

/// The scripts compiled since the last drain -- what the CDP loop sends as `Debugger.scriptParsed`.
pub fn take_parsed_scripts() -> Vec<ParsedScript> {
    let new_ids = std::mem::take(&mut *SCRIPTS_NEW.lock().unwrap_or_else(|e| e.into_inner()));
    if new_ids.is_empty() {
        return Vec::new();
    }
    let scripts = SCRIPTS.lock().unwrap_or_else(|e| e.into_inner());
    new_ids.iter().filter_map(|id| scripts.iter().find(|s| s.id == *id).cloned()).collect()
}

/// The text of script `id`, for `Debugger.getScriptSource`.
pub fn script_source(id: u32) -> Option<String> {
    SCRIPTS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|s| s.id == id).map(|s| s.source.clone())
}

static DOWNLOAD_ASKS: Mutex<Vec<(String, Option<String>)>> = Mutex::new(Vec::new());
static INTERCEPT_CHOOSER: AtomicBool = AtomicBool::new(false);
static CHOOSERS: Mutex<Vec<(u32, bool)>> = Mutex::new(Vec::new());

/// A link the page followed as a download (`<a download>`): its URL and the name it proposes. Kept only while a
/// DevTools client set a download behaviour (`Browser.setDownloadBehavior`), which then performs it.
pub(crate) fn download_requested(url: String, name: Option<String>) {
    if spec::devtools::download_behavior().is_none() {
        return;
    }
    let mut d = DOWNLOAD_ASKS.lock().unwrap_or_else(|e| e.into_inner());
    if d.len() < CAP {
        d.push((url, name));
    }
}

pub fn take_downloads() -> Vec<(String, Option<String>)> {
    std::mem::take(&mut *DOWNLOAD_ASKS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// `Page.setInterceptFileChooserDialog`: a file chooser the page opens is the client's to answer
/// (`Page.fileChooserOpened`), not a plugin's.
pub fn set_intercept_file_chooser(on: bool) {
    INTERCEPT_CHOOSER.store(on, Ordering::Relaxed);
    if !on {
        CHOOSERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

pub fn intercepting_file_chooser() -> bool {
    INTERCEPT_CHOOSER.load(Ordering::Relaxed)
}

/// An `<input type=file>` (arena node `node`) opened its chooser while a client intercepts them.
pub(crate) fn file_chooser_opened(node: u32, multiple: bool) {
    let mut c = CHOOSERS.lock().unwrap_or_else(|e| e.into_inner());
    if c.len() < CAP {
        c.push((node, multiple));
    }
}

/// The choosers opened since the last call: `(arena node, multiple)`.
pub fn take_file_choosers() -> Vec<(u32, bool)> {
    std::mem::take(&mut *CHOOSERS.lock().unwrap_or_else(|e| e.into_inner()))
}

// ── touch emulation ──────────────────────────────────────────────────────────────────────────────────

/// GlobalEventHandlers' four `ontouch*` handlers, which Blink exposes only with `TouchEventFeatureDetection` -- on a
/// device with touch, and under DevTools' touch emulation (`Emulation.setTouchEmulationEnabled`), whatever the
/// device. A desktop persona's realm has none; while a client emulates touch, the realm that is current gets them on
/// the window and on `Document`, `HTMLElement` and `SVGElement`'s prototypes, so `'ontouchstart' in window` is true
/// and an inline `ontouchstart="..."` runs, as in Chrome's emulated page. Nothing happens without emulation, or when
/// the realm has them already (a touch device's own surface).
pub fn install_touch_handlers(scope: &mut v8::PinScope) {
    if spec::devtools::touch_points().is_none() {
        return;
    }
    let global = scope.get_current_context().global(scope);
    let Some(key) = v8::String::new(scope, "ontouchstart") else { return };
    let mut holders: Vec<v8::Local<v8::Object>> = vec![global];
    for iface in ["Document", "HTMLElement", "SVGElement"] {
        let proto = crate::binding::protos::iface_object(scope, iface)
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
            .and_then(|c| crate::binding::protos::get_prop(scope, c, "prototype"))
            .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
        if let Some(p) = proto {
            holders.push(p);
        }
    }
    for holder in holders {
        if holder.has(scope, key.into()).unwrap_or(false) && holder.has_own_property(scope, key.into()).unwrap_or(false) {
            continue;
        }
        for ty in ["touchcancel", "touchend", "touchmove", "touchstart"] {
            let g = crate::core::events::event_target::make_et_on_getter(scope, ty);
            let s = crate::core::events::event_target::make_et_on_setter(scope, ty);
            let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), s.into());
            d.set_enumerable(true);
            d.set_configurable(true);
            if let Some(k) = v8::String::new(scope, &format!("on{ty}")) {
                holder.define_property(scope, k.into(), &d);
            }
        }
    }
}

// ── a worker's console ───────────────────────────────────────────────────────────────────────────────

thread_local! {
    /// The DevTools target of the worker this thread runs (its `Target.attachedToTarget` id), when it is one.
    static WORKER_TARGET: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// What each worker logged and the client has not been told yet: `Runtime.consoleAPICalled` params, by target.
fn worker_log() -> &'static Mutex<std::collections::HashMap<String, Vec<serde_json::Value>>> {
    static L: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Vec<serde_json::Value>>>> = std::sync::OnceLock::new();
    L.get_or_init(Default::default)
}

/// This thread runs the worker that is DevTools target `target` (`None`: it ends).
pub fn set_worker_target(target: Option<String>) {
    if target.is_none() {
        if let Some(t) = WORKER_TARGET.with(|w| w.borrow().clone()) {
            worker_log().lock().unwrap_or_else(|e| e.into_inner()).remove(&t);
        }
    }
    WORKER_TARGET.with(|w| *w.borrow_mut() = target);
}

/// What worker `target` logged since the last call, as `Runtime.consoleAPICalled` params (no context id yet).
pub fn take_worker_console(target: &str) -> Vec<serde_json::Value> {
    worker_log().lock().unwrap_or_else(|e| e.into_inner()).get_mut(target).map(std::mem::take).unwrap_or_default()
}

/// A worker's `console.*` call: its arguments described as Chrome's `RemoteObject`s by value -- they cannot cross to
/// the page's thread as live values -- kept for the worker's session.
fn worker_console(scope: &mut v8::PinScope, kind: &'static str, args: &[v8::Local<v8::Value>]) {
    let Some(target) = WORKER_TARGET.with(|t| t.borrow().clone()) else { return };
    let args: Vec<serde_json::Value> = args.iter().map(|a| remote_by_value(scope, *a)).collect();
    let entry = serde_json::json!({ "type": kind, "args": args, "timestamp": now_ms(), "stackTrace": { "callFrames": [] } });
    let mut log = worker_log().lock().unwrap_or_else(|e| e.into_inner());
    let list = log.entry(target).or_default();
    if list.len() < 1000 {
        list.push(entry);
    }
}

/// `v` as a `Runtime.RemoteObject` carried by value: a primitive as itself, an object with its class and a description.
pub fn remote_by_value(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> serde_json::Value {
    use serde_json::json;
    if v.is_undefined() {
        return json!({ "type": "undefined" });
    }
    if v.is_null() {
        return json!({ "type": "object", "subtype": "null", "value": null });
    }
    if v.is_boolean() {
        return json!({ "type": "boolean", "value": v.is_true() });
    }
    if v.is_number() {
        let n = v.number_value(scope).unwrap_or(0.0);
        if n.is_finite() && !(n == 0.0 && n.is_sign_negative()) {
            return json!({ "type": "number", "value": n, "description": v.to_rust_string_lossy(scope) });
        }
        let u = if n.is_nan() { "NaN" } else if n == 0.0 { "-0" } else if n > 0.0 { "Infinity" } else { "-Infinity" };
        return json!({ "type": "number", "unserializableValue": u, "description": u });
    }
    if v.is_string() {
        return json!({ "type": "string", "value": v.to_rust_string_lossy(scope) });
    }
    if v.is_big_int() {
        let d = format!("{}n", v.to_rust_string_lossy(scope));
        return json!({ "type": "bigint", "unserializableValue": d, "description": d });
    }
    if v.is_symbol() {
        let d = v.to_detail_string(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default();
        return json!({ "type": "symbol", "description": d });
    }
    if v.is_function() {
        return json!({ "type": "function", "className": "Function", "description": v.to_rust_string_lossy(scope) });
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return json!({ "type": "object" }) };
    let class = o.get_constructor_name().to_rust_string_lossy(scope);
    let subtype = if v.is_array() {
        Some("array")
    } else if v.is_native_error() {
        Some("error")
    } else if v.is_promise() {
        Some("promise")
    } else if v.is_date() {
        Some("date")
    } else if v.is_reg_exp() {
        Some("regexp")
    } else if v.is_map() {
        Some("map")
    } else if v.is_set() {
        Some("set")
    } else {
        None
    };
    let description = match subtype {
        Some("array") => format!("Array({})", v8::Local::<v8::Array>::try_from(v).map(|a| a.length()).unwrap_or(0)),
        Some("error") => v8::String::new(scope, "stack").and_then(|k| o.get(scope, k.into())).filter(|s| s.is_string()).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_else(|| class.clone()),
        _ => class.clone(),
    };
    let mut r = json!({ "type": "object", "className": class, "description": description });
    if let Some(st) = subtype {
        r["subtype"] = json!(st);
    }
    r
}

#[cfg(test)]
mod script_registry_tests {
    use super::*;

    #[test]
    fn records_drains_once_and_reads_source_back() {
        // Serial within this test: the registry is process-global. Capture must be on to record.
        set_debugger_capture(true);
        let _ = take_parsed_scripts(); // clear anything a prior line left
        let id = record_script("https://x/a.js", "function boot(){return 1}", 0).expect("a non-empty script records");
        assert_eq!(record_script("https://x/empty.js", "", 0), None, "an empty source is not a script");
        let drained = take_parsed_scripts();
        assert!(drained.iter().any(|s| s.id == id && s.url == "https://x/a.js"), "the new script drains once: {drained:?}");
        assert!(take_parsed_scripts().iter().all(|s| s.id != id), "it does not drain a second time");
        assert_eq!(script_source(id).as_deref(), Some("function boot(){return 1}"), "its source reads back by id");
        assert_eq!(script_source(u32::MAX), None, "an unknown id has no source");
        set_debugger_capture(false);
    }
}
