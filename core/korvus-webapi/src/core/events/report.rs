//! `core/events/report.rs` -- HTML's "report an exception", in Rust.
//!
//! One path for every uncaught error the engine sees: a throwing listener or event handler
//! (`event_target.rs`), a throwing timer callback, a top-level page-script throw, a failed module, a
//! custom-element reaction, a mutation-observer callback, a page's own `reportError(e)`. It
//! constructs `ErrorEvent("error", {message, filename, lineno, colno, error, cancelable})` -- the four
//! site fields read off V8's message, see [`ErrorSite`] -- with the realm's constructor, dispatches it
//! at the running realm's global through the same
//! `et_dispatch` a page's `dispatchEvent` runs, and only when nothing cancelled it records the error
//! in [`UncaughtErrors`] -- the tally `Engine::uncaught_error_count` (CDP `Page.js_error_count`) reads.
//! That is the standard's order: the log gets an error only if the page did not handle it -- and the
//! page's own `console` never gets it at all (see [`report_exception_at`]).
//!
//! Re-entrancy: dispatching runs page listeners, and a listener that throws routes its own error back
//! here. A nested error while one is being reported is RECORDED, not dispatched -- the alternative is
//! unbounded recursion, which is how this first showed up (two WPT shards dying of stack exhaustion).
//!
//! In a worker realm an unhandled error is also forwarded to the parent's `Worker` object
//! (`op_worker_self_error`, reached through the stashed ops table exactly as XHR reaches `op_fetch`),
//! which fires `worker.onerror` on the page -- the same routing the worker's own uncaught script
//! errors take.
//!
//! Exposed to JS as ONE native function of length 1: `window.reportError` on the global and the
//! `KB.__recordUncaught` name the runtime's page drivers keep calling are both `build_fn` over the
//! same Rust callback, [`cb_report_exception`] — mounted by `core/html/window.rs` in the page realm
//! and `core/workers/worker_global_scope.rs` in a worker's. Never a JS body.
//!
//! # The other half: unhandled promise REJECTIONS
//!
//! HTML's "notify about rejected promises" is the same shape one level down, and it was the last
//! piece of error reporting still written in JavaScript — a closure inside the kernel's `BOOTSTRAP`
//! string, handed to `Deno.core.setUnhandledPromiseRejectionHandler`. [`install_rejection_handler`]
//! installs a native function in its place, and [`UnhandledRejections`] is the store
//! `Engine::unhandled_rejections` reads; it used to read the page-visible JS array
//! `KB.__korvusRejections`, which is now gone the way `KB.__korvusUncaught` went in W5.0b.
//!
//! Returning `true` from the handler is what keeps a page ALIVE: deno_core's default surfaces an
//! unhandled rejection as a fatal event-loop error, which would kill a real page load. A browser
//! fires `unhandledrejection` and KEEPS RUNNING — a sensor's fetch whose connection closes must not
//! take the document with it.

use deno_core::{v8, OpState};

use crate::binding::protos::{build_fn, get_prop, global_get, set_bool, set_prop, str_val, throw_type_error};
use crate::core::events::event_target::et_dispatch;

/// The uncaught-error tally of one realm: `String(err.stack || err)` per error nothing handled.
#[derive(Default)]
pub struct UncaughtErrors {
    errors: Vec<String>,
    reporting: bool,
}

/// How many uncaught errors this realm has recorded since the last reset (CDP `Page.js_error_count`).
pub fn uncaught_error_count(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) -> usize {
    let st = op_state.borrow();
    st.try_borrow::<UncaughtErrors>().map(|u| u.errors.len()).unwrap_or(0)
}

/// The recorded uncaught errors themselves (`String(err.stack || err)` each), in order -- what the
/// corpus runner and the `korvus` binary's probes print. They used to read the JS array
/// `KB.__korvusUncaught` by eval; the array is gone and this is the one store.
pub fn uncaught_errors(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) -> Vec<String> {
    let st = op_state.borrow();
    st.try_borrow::<UncaughtErrors>().map(|u| u.errors.clone()).unwrap_or_default()
}

/// A fresh tally for the next navigation.
pub fn reset_uncaught_errors(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) {
    let mut st = op_state.borrow_mut();
    if let Some(u) = st.try_borrow_mut::<UncaughtErrors>() {
        u.errors.clear();
        u.reporting = false;
    }
}

fn record(scope: &mut v8::PinScope, text: String) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(u) = st.try_borrow_mut::<UncaughtErrors>() { u.errors.push(text); }
}

/// What the tally keeps: V8's own text for the error (its `Uncaught ` dropped) and where V8 locates it, read
/// without running page code. It was `String((e && e.stack) || e)`: a `[[Get]]` of `stack` for every uncaught error
/// and unhandled rejection, which ran a page's `Error.prepareStackTrace` and replaced the error's call sites with the
/// formatted text -- so the ErrorEvent's site, read after it, lost the error's construction site for the reporter's
/// (`reportError(e)` reported the `reportError` line). Chrome without DevTools reads neither.
fn error_text(scope: &mut v8::PinScope, err: v8::Local<v8::Value>) -> String {
    let msg = v8::Exception::create_message(scope, err);
    let text = msg.get(scope).to_rust_string_lossy(scope);
    let text = text.strip_prefix("Uncaught ").unwrap_or(&text).to_string();
    let site = ErrorSite::from_message(scope, msg);
    if site.lineno > 0 { format!("{text}\n    at {}:{}:{}", site.filename, site.lineno, site.colno) } else { text }
}
/// `String((e && e.message) || e)` -- the ErrorEvent's `message`.
fn error_message(scope: &mut v8::PinScope, err: v8::Local<v8::Value>) -> String {
    if let Ok(o) = v8::Local::<v8::Object>::try_from(err) {
        if let Some(m) = get_prop(scope, o, "message") {
            if m.boolean_value(scope) { return m.to_rust_string_lossy(scope); }
        }
    }
    err.to_rust_string_lossy(scope)
}
fn location_href(scope: &mut v8::PinScope) -> String {
    global_get(scope, "location")
        .and_then(|l| v8::Local::<v8::Object>::try_from(l).ok())
        .and_then(|l| get_prop(scope, l, "href"))
        .filter(|h| h.is_string())
        .map(|h| h.to_rust_string_lossy(scope))
        .unwrap_or_default()
}

/// WHERE an uncaught exception happened, the four fields a browser's `ErrorEvent` carries: V8's own
/// message text (`"Uncaught TypeError: …"`, `"Uncaught str"` for a thrown string), the script's
/// resource name, and the 1-based line and column. Chrome 150, measured over loopback (P10b):
///
/// ```text
/// <script>undefined.foo</script>        message "Uncaught TypeError: Cannot read properties of undefined (reading 'foo')"
/// throw "str"                           message "Uncaught str", error "str"
/// <script>var x = ;</script>            message "Uncaught SyntaxError: Unexpected token ';'"
/// reportError(new Error("rep"))         message "Uncaught Error: rep", line/col of the `new Error`
/// thrower.js line 3 `null.bar`          filename ".../thrower.js", lineno 3, colno 6
/// ```
///
/// It is exactly what `v8::Message` answers, which is why it is read off one: the TryCatch's message
/// when the thrower hands one over (a classic script's own throw, where a thrown PRIMITIVE has no
/// stack to recover a location from), else `v8::Exception::create_message(err)`, which for an Error
/// object recovers the location V8 captured when it was constructed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorSite {
    pub message: String,
    pub filename: String,
    pub lineno: u32,
    pub colno: u32,
}

impl ErrorSite {
    /// Read the four fields off a V8 message.
    pub fn from_message(scope: &mut v8::PinScope, msg: v8::Local<v8::Message>) -> ErrorSite {
        let message = msg.get(scope).to_rust_string_lossy(scope);
        let filename = msg
            .get_script_resource_name(scope)
            .filter(|v| v.is_string())
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_default();
        let (lineno, colno) = match msg.get_line_number(scope) {
            Some(l) if l > 0 => (l as u32, msg.get_start_column() as u32 + 1),
            _ => (0, 0),
        };
        ErrorSite { message, filename, lineno, colno }
    }

    /// The site of `err` when nobody captured one: `v8::Exception::create_message`'s, which V8 locates by the
    /// error's own call sites -- the top frame of the stack captured when the Error was constructed, within
    /// `Error.stackTraceLimit` -- and, for an error with none, where the REPORTER is running. Chrome 152 (measured,
    /// no DevTools): `reportError(new Error("rep"))` is sited at the `new Error`, and at the `reportError` call
    /// when `Error.stackTraceLimit` was 0.
    fn of_exception(scope: &mut v8::PinScope, err: v8::Local<v8::Value>) -> ErrorSite {
        let msg = v8::Exception::create_message(scope, err);
        ErrorSite::from_message(scope, msg)
    }
}

/// The `message` an engine-fired `ErrorEvent` carries: the site's (V8's "Uncaught …" text), else the
/// error's own.
fn event_message(scope: &mut v8::PinScope, err: v8::Local<v8::Value>, site: &ErrorSite) -> String {
    if site.message.is_empty() { error_message(scope, err) } else { site.message.clone() }
}

/// The `filename` an engine-fired `ErrorEvent` carries. A name that is not a URL is an engine-internal
/// script name (a bare test isolate's script runs as "worker") and says nothing about where the page's
/// code lives: the realm's own URL stands in, as it does for no name at all. Since `caught_site` reads a
/// stackless throw's site off its TryCatch message, a worker's `throw 42` from a listener reported
/// "worker" -- while a classic worker's script still ran under that name -- where Chrome reports the
/// worker script's URL (WPT workers/semantics/reporting-errors/001, 002).
fn event_filename(scope: &mut v8::PinScope, site: &ErrorSite) -> String {
    if site.filename.is_empty() || !site.filename.contains(':') { location_href(scope) } else { site.filename.clone() }
}

/// `new ErrorEvent("error", {message, filename, lineno, colno, error, cancelable: true})` from the
/// realm's constructor; a realm whose ErrorEvent does not take the init (the worker's generated shell
/// builds a typed object and runs no Event constructor) gets a plain cancelable `Event("error")` --
/// an `error` event that says it is an `error` event matters more than the extra fields. `message` and
/// `href` are [`event_message`] and [`event_filename`], which the reporter also tells a parent `Worker`.
fn make_error_event<'s>(scope: &mut v8::PinScope<'s, '_>, err: v8::Local<'s, v8::Value>, site: &ErrorSite, msg: &str, href: &str) -> Option<v8::Local<'s, v8::Object>> {
    let ty = str_val(scope, "error");
    if let Some(ctor) = crate::binding::protos::iface_object(scope, "ErrorEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        let init = crate::binding::protos::init_dict(scope);
        let m = str_val(scope, msg);
        set_prop(scope, init, "message", m);
        let f = str_val(scope, href);
        set_prop(scope, init, "filename", f);
        let line: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, site.lineno).into();
        set_prop(scope, init, "lineno", line);
        let col: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, site.colno).into();
        set_prop(scope, init, "colno", col);
        set_prop(scope, init, "error", err);
        set_bool(scope, init, "cancelable", true);
        if let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) {
            let is_error = get_prop(scope, ev, "type").map(|t| t.is_string() && t.to_rust_string_lossy(scope) == "error").unwrap_or(false);
            if is_error {
                // An event the ENGINE fires is TRUSTED. It is built through the page-visible
                // constructor -- deliberately, so a page cannot tell one from the other -- and that
                // constructor leaves `isTrusted` false, which is right for a page and wrong here.
                crate::core::html::html_dialog_element::mark_trusted(scope, ev);
                return Some(ev);
            }
        }
    }
    let ctor = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "cancelable", true);
    ctor.new_instance(scope, &[ty, init.into()])
}

/// A worker realm forwards an unhandled error to its parent `Worker` object (`worker.onerror`), with the
/// message and site its own `ErrorEvent` carried. The op exists only in a worker isolate; the window has
/// nothing to forward to.
fn forward_to_parent(scope: &mut v8::PinScope, message: &str, filename: &str, lineno: u32, colno: u32) {
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_worker_self_error") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let m = str_val(scope, message);
    let fv = str_val(scope, filename);
    let l: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, lineno).into();
    let c: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, colno).into();
    v8::tc_scope!(let tc, scope);
    f.call(tc, undef, &[m, fv, l, c]);
    tc.reset();
}

/// "Report an exception" (HTML 8.1.3.5.2) for `err` at this realm's global. Public because the
/// kernel's timer task source (`korvus-runtime::scheduler`) reports a throwing timer callback through
/// this one path -- the file header already named that caller; it just could not reach it.
pub fn report_exception(scope: &mut v8::PinScope, err: v8::Local<v8::Value>) {
    report_exception_at(scope, err, None);
}

/// The site a CAUGHT throw reports at, read off its `TryCatch` message (`msg`) before the catch is
/// reset: where the `throw` ran -- for an Error too, not where it was constructed (stock Chrome 150, no
/// DevTools attached, 2026-10-10: `var e = new Error(); \n throw e;` reports the throw's line and column in
/// a classic script, a timer, a string timer, a listener and a microtask alike; with an inspector attached
/// V8 captures stacks for uncaught exceptions and Blink reports the construction instead, which is what an
/// earlier CDP-driven measurement here saw).
///
/// Its message is V8's as `MessageHandler::ReportMessage` hands it to Blink's listener: a thrown OBJECT that
/// is not an Error is first converted with the full `ToString` -- page code runs, `{}` reads `[object
/// Object]` (V8's own formatter would print `#<Object>`), a throwing conversion reads `exception`.
pub fn caught_site(scope: &mut v8::PinScope, err: v8::Local<v8::Value>, msg: Option<v8::Local<v8::Message>>) -> Option<ErrorSite> {
    let mut site = ErrorSite::from_message(scope, msg?);
    if err.is_object() && !err.is_native_error() {
        let text = {
            v8::tc_scope!(let tc, scope);
            let t = err.to_string(tc).map(|s| s.to_rust_string_lossy(tc));
            tc.reset();
            t
        };
        site.message = format!("Uncaught {}", text.as_deref().unwrap_or("exception"));
    }
    Some(site)
}

/// [`report_exception`] with the site the thrower already has -- a classic script's `TryCatch`
/// message, which is the only place a thrown primitive's line survives.
///
/// # Where it goes, and where it does NOT
///
/// An `ErrorEvent` at the global of the realm whose script is running -- a nested browsing context's
/// window while one of its scripts or callbacks is on the stack, the page's global otherwise -- and,
/// when nothing cancelled it, the engine's uncaught tally, which is this engine's DevTools log
/// (`korvus fetch --errors`, `Engine::uncaught_errors`). Chrome 150, measured: the same two places,
/// `Runtime.exceptionThrown` being its DevTools log, and a `preventDefault()` removes the error from
/// the log as well.
///
/// NEVER the page's `console.error`. The runtime used to call it for every uncaught error, and
/// `console` is page-owned: a page that wraps it (every error-monitoring SDK does) was told about an
/// error Chrome never reports through it, and the parity collector counted that call as a
/// `console_errors` the oracle did not have -- on 112 `threw` rows of L14's sweep.
pub fn report_exception_at(scope: &mut v8::PinScope, err: v8::Local<v8::Value>, site: Option<ErrorSite>) {
    report_at(scope, err, site, None);
}

/// HTML's runtime-script-error reporting ONE LEVEL UP: an error a worker this realm created left
/// unhandled, and that nothing cancelled at its `Worker` object, is reported again here -- an
/// `ErrorEvent` at this realm's global with the worker's message and site and a null `error` -- and,
/// unhandled here too, goes on to this realm's own parent. Chrome 150, measured: a worker's uncaught
/// `Error('w1')` with no listener on its `Worker` reaches `window.onerror("Uncaught Error: w1", <the
/// worker's URL>, 1, 26, null)`; an inner worker's reaches the outer worker's `self.onerror`, then the
/// page's `Worker`, then `window.onerror`. The page's tally gets it too: Chrome logs it on the page.
pub fn report_worker_error(scope: &mut v8::PinScope, message: &str, filename: &str, lineno: u32, colno: u32) {
    let site = ErrorSite { message: message.to_string(), filename: filename.to_string(), lineno, colno };
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    report_at(scope, null, Some(site), Some(message.to_string()));
}

/// [`report_exception_at`], with the tally's text given (`None`: the error's own).
fn report_at(scope: &mut v8::PinScope, err: v8::Local<v8::Value>, site: Option<ErrorSite>, tally: Option<String>) {
    let text = match tally {
        Some(t) => t,
        None => error_text(scope, err),
    };
    let nested = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match st.try_borrow_mut::<UncaughtErrors>() {
            Some(u) if u.reporting => true,
            Some(u) => { u.reporting = true; false }
            None => false,
        }
    };
    if nested {
        // Already reporting one error: this one is recorded, never dispatched (no recursion).
        record(scope, text);
        return;
    }
    let err_g = v8::Global::new(scope, err);
    let err = v8::Local::new(scope, &err_g);
    let site = match site {
        Some(s) => s,
        None => ErrorSite::of_exception(scope, err),
    };
    // A site of its own (`reportError(o)`, an error nobody threw) keeps V8's message for a non-Error object, the bare
    // "Uncaught " -- a DOMException's too: Chrome 152 (measured) reports `reportError(new DOMException("m",
    // "SyntaxError"))` as "Uncaught ", and a thrown one is "Uncaught SyntaxError: m" by its throw's message
    // ([`caught_site`]). This called every such object's `toString`, which Chrome never does here.
    let message = event_message(scope, err, &site);
    let href = event_filename(scope, &site);
    // An uncaught page error, to any observer plugin (plugin ABI 3, F4): read-only, after the nested-report guard
    // so each error is reported once. Costs nothing when no plugin observes.
    if plugin_host::has_observer() {
        let payload = serde_json::json!({ "message": message, "filename": href, "lineno": site.lineno, "colno": site.colno });
        plugin_host::event("exception", &payload.to_string());
    }
    let not_cancelled = match make_error_event(scope, err, &site, &message, &href) {
        Some(ev) => {
            let target: v8::Local<v8::Value> = match crate::core::html::browsing_context::running_frame_window(scope) {
                Some(w) => w.into(),
                None => { let ctx = scope.get_current_context(); ctx.global(scope).into() }
            };
            et_dispatch(scope, target, ev)
        }
        None => true,
    };
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(u) = st.try_borrow_mut::<UncaughtErrors>() { u.reporting = false; }
    }
    if not_cancelled {
        record(scope, text);
        // and a DevTools client's log (`Runtime.exceptionThrown`), with the event's message and site -- what Chrome's
        // inspector reports for an error the page left unhandled
        crate::devtools_log::error(scope, err, &message, &href, site.lineno, site.colno);
        // The parent's `Worker` is told the message and site this realm's `ErrorEvent` carried: Chrome 150,
        // measured, `["Uncaught Error: ctop", <the worker's URL>, 1, 124]` on both sides. It was told
        // `"Uncaught " + String(error)` and no site, and its `ErrorEvent` read "" / 0 / 0.
        forward_to_parent(scope, &message, &href, site.lineno, site.colno);
    }
}

/// Is an error being reported -- its `error` event being dispatched -- right now? Blink reports an uncaught
/// exception inside the call that threw it (a verbose TryCatch, before that call's MicrotasksScope ends), so the
/// `error` listeners' microtasks wait for the report to finish; [`crate::core::html::task_queue::
/// clean_up_after_running_callback`] asks this so no checkpoint runs inside the report (one ran
/// there, the page's next step ran inside the report, and its own error was swallowed as a nested one).
pub(crate) fn is_reporting(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<UncaughtErrors>().is_some_and(|u| u.reporting)
}

/// `reportError(e)` -- WebIDL `undefined reportError(any e)`: one argument required.
pub(crate) fn cb_report_exception(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.length() < 1 {
        throw_type_error(scope, "Failed to execute 'reportError' on 'Window': 1 argument required, but only 0 present.");
        return;
    }
    let e = args.get(0);
    report_exception(scope, e);
}
// The `op_report_exception` builder that used to hand this callback to JS by name IS DELETED:
// it had no caller. Both mount sites -- `window.rs` for `reportError` and `__recordUncaught`,
// `worker_global_scope.rs` for the worker's -- name `cb_report_exception` directly.

// ── unhandled promise rejections ────────────────────────────────────────────────────────────────

/// The unhandled rejections of one realm: `String((reason && reason.stack) || reason)` each.
#[derive(Default)]
pub struct UnhandledRejections(Vec<String>);

/// What `Engine::unhandled_rejections` reads. It used to `eval` the page-visible JS array
/// `KB.__korvusRejections`, which a page could read, write and clear.
pub fn unhandled_rejections(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) -> Vec<String> {
    let st = op_state.borrow();
    st.try_borrow::<UnhandledRejections>().map(|u| u.0.clone()).unwrap_or_default()
}

/// A fresh tally for the next navigation.
pub fn reset_unhandled_rejections(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) {
    let mut st = op_state.borrow_mut();
    if let Some(u) = st.try_borrow_mut::<UnhandledRejections>() {
        u.0.clear();
    }
}

/// `new PromiseRejectionEvent("unhandledrejection", {promise, reason, cancelable: true})` from the
/// realm's own constructor — the SAME one a page reaches, so the event a page receives from the
/// engine is indistinguishable from one it built itself. A realm without the interface gets a plain
/// cancelable `Event`, exactly as [`make_error_event`] degrades.
fn make_rejection_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    promise: v8::Local<'s, v8::Value>,
    reason: v8::Local<'s, v8::Value>,
) -> Option<v8::Local<'s, v8::Object>> {
    let ty = str_val(scope, "unhandledrejection");
    if let Some(ctor) = crate::binding::protos::iface_object(scope, "PromiseRejectionEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        let init = crate::binding::protos::init_dict(scope);
        set_prop(scope, init, "promise", promise);
        set_prop(scope, init, "reason", reason);
        set_bool(scope, init, "cancelable", true);
        if let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) {
            let ok = get_prop(scope, ev, "type").map(|t| t.is_string() && t.to_rust_string_lossy(scope) == "unhandledrejection").unwrap_or(false);
            if ok {
                // Trusted, like every event the engine fires -- measured in headless Chrome 141:
                // an `unhandledrejection` delivered by the event loop has `isTrusted === true`.
                crate::core::html::html_dialog_element::mark_trusted(scope, ev);
                return Some(ev);
            }
        }
    }
    let ctor = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let init = crate::binding::protos::init_dict(scope);
    set_bool(scope, init, "cancelable", true);
    ctor.new_instance(scope, &[ty, init.into()])
}

/// HTML's "notify about rejected promises", for ONE rejection: record it, then fire
/// `unhandledrejection` at the global.
///
/// The event is dispatched through the same `et_dispatch` a page's `dispatchEvent` runs, so
/// `window.onunhandledrejection` — an event-handler IDL attribute on the Window table — is invoked by
/// the dispatch itself. The deleted JS also called the handler BY HAND when `dispatchEvent` was
/// missing, which delivered the same rejection twice the moment the handler became a real accessor.
pub(crate) fn cb_unhandled_rejection<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    unhandled_rejection(scope, args.get(0), args.get(1));
    // "handled": do not abort the event loop. A browser keeps running.
    rv.set_bool(true);
}

/// HTML "notify about rejected promises", at the end of a microtask checkpoint: the promises rejected
/// with no handler since the last one are reported in a task of their own, queued now -- `unhandledrejection` at the
/// global for each that still has none when it runs. Blink posts that task at the end of every checkpoint
/// (`EventLoop::PerformMicrotaskCheckpoint` -> `RejectedPromises::ProcessQueue`, DOM manipulation source); deno_core
/// reported them when its loop came round to them, after every task already queued. Chrome 150, a rejection in a
/// parser script after the parser waited for a script: `unhandledrejection` before `readystatechange: interactive`;
/// korvus's came after `load` (WPT unhandled-promise-rejections/promise-rejection-event-during-parse, which Chrome
/// fails the same way).
pub fn notify_rejected_promises(scope: &mut v8::PinScope) {
    let pending = deno_core::JsRuntime::take_pending_promise_rejections(scope);
    if pending.is_empty() {
        return;
    }
    let spawner = deno_core::JsRuntime::op_state_from(scope).try_borrow().ok().and_then(|st| st.try_borrow::<deno_core::V8TaskSpawner>().cloned());
    let Some(spawner) = spawner else {
        // no event loop to queue on: report them now
        report_rejections(scope, pending);
        return;
    };
    let ctx = v8::Global::new(scope, scope.get_current_context());
    spawner.spawn(move |scope| {
        v8::scope!(let hs, scope);
        let ctx = v8::Local::new(hs, &ctx);
        let scope = &mut v8::ContextScope::new(hs, ctx);
        crate::core::html::task_queue::run_as_task(scope, |scope| report_rejections(scope, pending));
    });
}

/// The notification task: each promise that still has no handler is reported (Blink `RejectedPromises::ProcessQueueNow`).
fn report_rejections(scope: &mut v8::PinScope, pending: Vec<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>) {
    for (promise, reason) in pending {
        let promise = v8::Local::new(scope, &promise);
        if promise.has_handler() {
            continue;
        }
        let reason = v8::Local::new(scope, &reason);
        unhandled_rejection(scope, promise.into(), reason);
    }
}

fn unhandled_rejection<'s>(scope: &mut v8::PinScope<'s, '_>, promise: v8::Local<'s, v8::Value>, reason: v8::Local<'s, v8::Value>) {
    // A MODULE SCRIPT THAT THREW is not a rejected promise the page made: it is an uncaught
    // exception, and Chrome reports it as one (see [`begin_module_evaluation`]).
    if take_module_rethrow(scope, promise) {
        v8::tc_scope!(let tc, scope);
        report_exception(tc, reason);
        tc.reset();
        return;
    }
    let text = error_text(scope, reason);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(u) = st.try_borrow_mut::<UnhandledRejections>() {
            u.0.push(text);
        }
    }
    // A listener that throws routes its own error to `report_exception`, which is re-entrancy-safe;
    // a throw escaping HERE would reach deno_core as a fatal event-loop error, which is the thing
    // this handler exists to prevent, so it is caught.
    v8::tc_scope!(let tc, scope);
    if let Some(ev) = make_rejection_event(tc, promise, reason) {
        let global: v8::Local<v8::Value> = { let ctx = tc.get_current_context(); ctx.global(tc).into() };
        et_dispatch(tc, global, ev);
    }
    tc.reset();
}

// ── a module script's own throw ────────────────────────────────────────────────────────────────────

/// The promises through which deno_core re-raises a module script's evaluation failure.
///
/// # Why this exists
///
/// HTML's "run a module script" REPORTS THE EXCEPTION when the module's evaluation promise rejects:
/// an `ErrorEvent` at the global, `message` `"Uncaught Error: mod"`, the module's URL and line --
/// Chrome 150, measured for a top-level `throw` in an inline `<script type=module>`, and no
/// `unhandledrejection` at all. deno_core's `ModuleMap::mod_evaluate` instead attaches
/// `then2(on_fulfilled, on_rejected)` to that promise, and its `on_rejected` RETHROWS the reason, so
/// the promise `then2` returned rejects with nobody listening -- and the module's throw reached this
/// realm as an unhandled rejection: `unhandledrejection` fired, `window.onerror` never did, and the
/// engine's tally filed it under rejections.
///
/// The derived promise is the one thing that identifies that path exactly, and V8 hands it over at
/// its creation: a PromiseHook's `init` sees every new promise, and `then2`'s result is the LAST
/// promise `mod_evaluate` creates before it returns (the module body, which runs inside the same
/// call, creates its own first). So the hook is armed only across that one synchronous call
/// ([`begin_module_evaluation`] / [`end_module_evaluation`]), the last promise it saw is kept, and a
/// rejection of exactly that object is reported as the exception it is. No message matching, no
/// timing window: identity. (A page's own `Promise.reject()` inside the module body is created
/// earlier in the same call and stays a rejection -- `tests/error_reporting_live.rs` pins both.)
#[derive(Default)]
pub struct ModuleRethrows {
    last: Option<v8::Global<v8::Promise>>,
    pending: std::collections::VecDeque<v8::Global<v8::Promise>>,
}

/// How many modules' re-raise promises are remembered. A module that never throws leaves one that
/// never rejects, so the list is bounded rather than grown for the life of the page.
const MODULE_RETHROWS_MAX: usize = 256;

fn with_module_rethrows<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut ModuleRethrows) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if !st.has::<ModuleRethrows>() {
        st.put(ModuleRethrows::default());
    }
    f(st.borrow_mut::<ModuleRethrows>())
}

fn cb_module_promise_init(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // `init(promise, parent)`. The parent is NOT a usable filter: V8 passes `undefined` for the
    // promise a C++ `Promise::Then` creates (measured: both inits of a throwing module's
    // `mod_evaluate` arrive without one), so the LAST promise created in the window is the rule.
    let Ok(p) = v8::Local::<v8::Promise>::try_from(args.get(0)) else { return };
    let g = v8::Global::new(scope, p);
    with_module_rethrows(scope, |m| m.last = Some(g));
}

/// Arm the PromiseHook for one `mod_evaluate` call. See [`ModuleRethrows`].
pub fn begin_module_evaluation(scope: &mut v8::PinScope) {
    with_module_rethrows(scope, |m| m.last = None);
    let init = build_fn(scope, cb_module_promise_init, "", 2);
    scope.set_promise_hooks(Some(init), None, None, None);
}

/// Disarm the hook, and react to the promise `mod_evaluate` re-raises a failure through: a module script that threw
/// is reported when its evaluation rejects -- in the task that ran it, before anything after it (HTML "run a module
/// script"; Chrome 150: the `error` precedes the document's `load`, WPT custom-element-exception). Through deno's
/// unhandled-rejection pass it came a turn of the loop later, behind the load task.
pub fn end_module_evaluation(scope: &mut v8::PinScope) {
    scope.set_promise_hooks(None, None, None, None);
    let p = with_module_rethrows(scope, |m| {
        let p = m.last.take();
        if let Some(p) = &p {
            if m.pending.len() >= MODULE_RETHROWS_MAX {
                m.pending.pop_front();
            }
            m.pending.push_back(p.clone());
        }
        p
    });
    if let Some(g) = p {
        let promise = v8::Local::new(scope, &g);
        let on_reject = build_fn(scope, cb_module_rejected, "", 1);
        let _ = promise.catch(scope, on_reject);
    }
}

/// [`end_module_evaluation`]'s reaction: the module's exception, reported as an uncaught one (it is no longer an
/// unhandled rejection, so deno's pass does not see it again).
fn cb_module_rejected(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let reason = args.get(0);
    v8::tc_scope!(let tc, scope);
    report_exception(tc, reason);
    tc.reset();
}

/// Is `promise` a module's re-raise promise? Consumes the entry when it is.
fn take_module_rethrow(scope: &mut v8::PinScope, promise: v8::Local<v8::Value>) -> bool {
    if !promise.is_promise() {
        return false;
    }
    let pending: Vec<v8::Global<v8::Promise>> = with_module_rethrows(scope, |m| m.pending.iter().cloned().collect());
    let hit = pending.iter().position(|g| {
        let l = v8::Local::new(scope, g);
        l.strict_equals(promise)
    });
    match hit {
        Some(i) => {
            with_module_rethrows(scope, |m| {
                m.pending.remove(i);
            });
            true
        }
        None => false,
    }
}

/// Point `Deno.core.setUnhandledPromiseRejectionHandler` at [`cb_unhandled_rejection`].
///
/// Called from the kernel's pre-bootstrap block, which is the last moment `Deno` is still on the
/// global (`hide_deno` removes it right after the bootstrap script). The handler itself only ever
/// runs at event-loop time, so installing it early costs nothing.
pub fn install_rejection_handler(scope: &mut v8::PinScope) {
    let Some(deno) = global_get(scope, "Deno").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(core) = get_prop(scope, deno, "core").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let Some(setter) = get_prop(scope, core, "setUnhandledPromiseRejectionHandler")
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return;
    };
    let handler = build_fn(scope, cb_unhandled_rejection, "", 2);
    let recv: v8::Local<v8::Value> = core.into();
    v8::tc_scope!(let tc, scope);
    setter.call(tc, recv, &[handler.into()]);
    tc.reset();
}
