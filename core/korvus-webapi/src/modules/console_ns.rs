//! `modules/console_ns.rs` — the `console` namespace.
//!
//! Was an object literal of JS closures in `core/korvus-runtime/src/js/shared-globals.js`. The SINK is
//! unchanged: every printing method routes to the native `op_kprint`, directly rather than through
//! `globalThis.print`, so a worker can delete its `print` global (a real `WorkerGlobalScope` has none)
//! without breaking console. Same sink in both realms.
//!
//! MEASURED on `google-chrome --headless=new` 150.0.7871.46 with
//! `tests/fixtures/base64-console-probe.js`. THREE tells the literal carried:
//!
//! * `Object.prototype.toString.call(console)` was `"[object Object]"`; Chrome's is
//!   `"[object console"` + `"]"` — lower-case `console`, which is the WebIDL NAMESPACE's name and not
//!   an interface's.
//! * `console.log` was a JS arrow, so `Function.prototype.toString` printed its SOURCE. Chrome's is
//!   `[native code]`, `name` "log", `length` 0. This file's is all three.
//! * `console` was an ENUMERABLE own property of the global; Chrome's is non-enumerable (writable and
//!   configurable, and `Object.getOwnPropertyNames(globalThis)` still lists it, which is what the
//!   surface board reads).
//!
//! The member list is Chrome's twenty-five, in Chrome's own-key order, and each member is
//! `{writable, enumerable, configurable}` — all measured by the same probe.
//!
//! WHAT A CALL DOES WITH ITS ARGUMENTS (the live a challenge provider challenge, 2026-10-07). V8's console
//! builtins run the console spec's Formatter (`builtins-console.cc`): `%d` `%i` `%f` convert their argument
//! through `parseInt`/`parseFloat`, `%s` through `String` (and the result is scanned for more specifiers),
//! `%c` `%o` `%O` `%_` leave theirs alone. Then -- because Blink registers every context with V8's inspector,
//! DevTools open or not -- the inspector builds the message's text (`inspector/v8-console-message.cc`,
//! `V8ValueStringBuilder`): a plain object becomes `Object.prototype.toString` of it (its `Symbol.toStringTag`
//! read), an Error / RegExp / function / Date its own `toString`, an array its elements, a Proxy
//! "[object Proxy]" untouched. `count`, `time` and the other labelled calls `ToString` their label, and
//! `context(name)` its name (`inspector/v8-console.cc`). korvus ran `String(v)` over every argument of every
//! printing call and nothing else: a plain object's own `toString` and `Symbol.toPrimitive`, and a Proxy's
//! traps, ran where Chrome's never do, and a label's `toString` never did -- and a bot-challenge frame
//! logs objects whose `toString`s and getters record being called. Both halves are reproduced here,
//! conversion for conversion, exceptions included: a Formatter or label conversion's throw leaves the console
//! call; a message conversion's is swallowed (Chrome 150, measured with no DevTools client attached:
//! core/korvus-runtime/tests/fixtures/console-calls/, `tests/console_calls_live.rs`).

use deno_core::v8;

use crate::binding::protos::{build_fn, set_prop, str_val, template_fn};
use crate::core::xmlhttprequest::xml_http_request::deno_op;

/// Chrome's `Object.getOwnPropertyNames(console)`, IN ITS OWN ORDER (measured unsorted), with each
/// member's body and `length`. `createTask` and `memory` are the last two and are defined after this list,
/// because neither is a plain method.
const MEMBERS: &[(&str, Body, i32)] = &[
    ("debug", Body::Debug, 0),
    ("error", Body::Error, 0),
    ("info", Body::Info, 0),
    ("log", Body::Log, 0),
    ("warn", Body::Warn, 0),
    ("dir", Body::Plain, 0),
    ("dirxml", Body::Plain, 0),
    ("table", Body::Plain, 0),
    ("trace", Body::Trace, 0),
    ("group", Body::Group, 0),
    ("groupCollapsed", Body::GroupCollapsed, 0),
    ("groupEnd", Body::Silent, 0),
    ("clear", Body::Silent, 0),
    ("count", Body::Count, 0),
    ("countReset", Body::CountReset, 0),
    ("assert", Body::Assert, 0),
    ("profile", Body::Label, 0),
    ("profileEnd", Body::Label, 0),
    ("time", Body::Time, 0),
    ("timeLog", Body::TimeLog, 0),
    ("timeEnd", Body::TimeEnd, 0),
    ("timeStamp", Body::Label, 0),
    // `length` 1: the one console member whose is not 0 (measured).
    ("context", Body::Context, 1),
];

/// The methods of a `console.context()` object, in V8's order (`dirXml` with V8's capital X), each a fresh
/// function of `length` 1 per call (measured).
const CONTEXT_MEMBERS: &[(&str, Body)] = &[
    ("dir", Body::Plain),
    ("dirXml", Body::Plain),
    ("table", Body::Plain),
    ("groupEnd", Body::Silent),
    ("clear", Body::Silent),
    ("count", Body::Count),
    ("countReset", Body::CountReset),
    ("profile", Body::Label),
    ("profileEnd", Body::Label),
    ("debug", Body::Debug),
    ("error", Body::Error),
    ("info", Body::Info),
    ("log", Body::Log),
    ("warn", Body::Warn),
    ("trace", Body::Trace),
    ("group", Body::Group),
    ("groupCollapsed", Body::GroupCollapsed),
    ("assert", Body::Assert),
    ("time", Body::Time),
    ("timeLog", Body::TimeLog),
    ("timeEnd", Body::TimeEnd),
    ("timeStamp", Body::Label),
];

/// What a member does with its arguments -- V8's builtin, then its inspector's `V8Console` method.
#[derive(Clone, Copy)]
enum Body {
    /// `debug` `error` `info` `log` `warn`: the Formatter, then the message, printed; no message for none. One body
    /// each, so a DevTools client is told which method the page called.
    Log,
    Debug,
    Info,
    Error,
    Warn,
    /// `trace`: as `Log`, "console.trace" for none.
    Trace,
    /// `group`: as `Log`, "console.group" for none.
    Group,
    /// `groupCollapsed`: as `Log`, "console.groupCollapsed" for none.
    GroupCollapsed,
    /// `dir` `dirxml` `table`: the message (no Formatter), printed.
    Plain,
    /// `groupEnd` `clear`: the message, which nothing shows.
    Silent,
    /// `assert`: nothing for a truthy first argument; else the Formatter from the second, and the message.
    Assert,
    Count,
    CountReset,
    Time,
    TimeLog,
    TimeEnd,
    /// `profile` `profileEnd` `timeStamp`: the label, and nothing this engine records.
    Label,
    Context,
}

/// A member's function: from its template for `console`'s own members (one per isolate, as the other
/// namespace bodies), a fresh one for a `console.context()` object's.
fn member<'s>(scope: &mut v8::PinScope<'s, '_>, body: Body, name: &str, length: i32, template: bool) -> v8::Local<'s, v8::Function> {
    macro_rules! make {
        ($cb:expr) => {
            if template { template_fn(scope, $cb, name, length, None) } else { build_fn(scope, $cb, name, length) }
        };
    }
    match body {
        Body::Log => make!(cb_log),
        Body::Debug => make!(cb_debug),
        Body::Info => make!(cb_info),
        Body::Error => make!(cb_error),
        Body::Warn => make!(cb_warn),
        Body::Trace => make!(cb_trace),
        Body::Group => make!(cb_group),
        Body::GroupCollapsed => make!(cb_group_collapsed),
        Body::Plain => make!(cb_plain),
        Body::Silent => make!(cb_silent),
        Body::Assert => make!(cb_assert),
        Body::Count => make!(cb_count),
        Body::CountReset => make!(cb_count_reset),
        Body::Time => make!(cb_time),
        Body::TimeLog => make!(cb_time_log),
        Body::TimeEnd => make!(cb_time_end),
        Body::Label => make!(cb_label),
        Body::Context => make!(cb_context),
    }
}

// ── the Formatter (V8's `builtins-console.cc`) ──────────────────────────────────────────────────────────

fn args_of<'s>(args: &v8::FunctionCallbackArguments<'s>) -> Vec<v8::Local<'s, v8::Value>> {
    (0..args.length()).map(|i| args.get(i)).collect()
}

fn utf16(scope: &mut v8::PinScope, s: v8::Local<v8::String>) -> Vec<u16> {
    s.to_rust_string_lossy(scope).encode_utf16().collect()
}

/// The console spec's Formatter as V8 runs it, over `args` from `at` (the format string's position: 0, or 1
/// for `assert`), replacing each converted argument with its conversion. `%d` `%i` `%f`: `parseInt` /
/// `parseFloat` of it (its `ToString`; a Symbol is NaN), `%s`: `String` of it, scanned in turn for
/// specifiers, `%c` `%o` `%O` `%_`: skipped, `%%`: no argument. `false`: a conversion threw, and its
/// exception is left for the console call to throw.
fn format<'s>(scope: &mut v8::PinScope<'s, '_>, args: &mut [v8::Local<'s, v8::Value>], at: usize) -> bool {
    if args.len() < at + 2 || !args[at].is_string() {
        return true;
    }
    let Ok(first) = v8::Local::<v8::String>::try_from(args[at]) else { return true };
    let mut states: Vec<(Vec<u16>, usize)> = vec![(utf16(scope, first), 0)];
    let mut index = at + 1;
    while !states.is_empty() && index < args.len() {
        let top = states.len() - 1;
        let (found, len) = {
            let (s, off) = &states[top];
            (s.iter().skip(*off).position(|&c| c == u16::from(b'%')).map(|p| p + off), s.len())
        };
        let Some(off) = found.filter(|&o| o + 1 != len) else {
            states.pop();
            continue;
        };
        let spec = states[top].0[off + 1];
        let current = args[index];
        let converted: v8::Local<v8::Value> = match char::from_u32(u32::from(spec)).unwrap_or('\0') {
            c @ ('d' | 'i' | 'f') => {
                let n = if current.is_symbol() {
                    f64::NAN
                } else {
                    let Some(text) = current.to_string(scope) else { return false };
                    let text = text.to_rust_string_lossy(scope);
                    if c == 'f' { parse_float(&text) } else { parse_int(&text) }
                };
                v8::Number::new(scope, n).into()
            }
            's' => {
                let text = if current.is_symbol() {
                    symbol_descriptive_string(scope, current)
                } else {
                    match current.to_string(scope) {
                        Some(t) => t,
                        None => return false,
                    }
                };
                // The result is scanned for specifiers of its own, BEFORE the rest of this string.
                states[top].1 = off + 2;
                args[index] = text.into();
                index += 1;
                let inner = utf16(scope, text);
                states.push((inner, 0));
                continue;
            }
            'c' | 'o' | 'O' | '_' => {
                index += 1;
                states[top].1 = off + 2;
                continue;
            }
            '%' => {
                states[top].1 = off + 2;
                continue;
            }
            _ => {
                states[top].1 = off + 1;
                continue;
            }
        };
        args[index] = converted;
        index += 1;
        states[top].1 = off + 2;
    }
    true
}

/// `String(symbol)`: "Symbol(" + description + ")", no page code.
fn symbol_descriptive_string<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::String> {
    let desc = v8::Local::<v8::Symbol>::try_from(v).ok().map(|s| s.description(scope)).filter(|d| d.is_string()).map(|d| d.to_rust_string_lossy(scope)).unwrap_or_default();
    str_val_string(scope, &format!("Symbol({desc})"))
}

fn str_val_string<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap_or_else(|| v8::String::empty(scope))
}

/// ECMAScript's StrWhiteSpaceChar.
fn js_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// `parseInt(text, 10)` of an already-converted string.
fn parse_int(text: &str) -> f64 {
    let t = text.trim_start_matches(js_space);
    let (neg, rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return f64::NAN;
    }
    let v = digits.parse::<f64>().unwrap_or(f64::NAN);
    if neg { -v } else { v }
}

/// `parseFloat(text)` of an already-converted string: the longest StrDecimalLiteral prefix.
fn parse_float(text: &str) -> f64 {
    let t = text.trim_start_matches(js_space);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if t[i..].starts_with("Infinity") {
        return if b.first() == Some(&b'-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    if i < b.len() && b[i] == b'.' {
        let mut k = i + 1;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if int_digits > 0 || k > i + 1 {
            i = k;
        }
    }
    if i == int_start || (int_digits == 0 && i == int_start + 1) {
        return f64::NAN;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut k = i + 1;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        let digits = k;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > digits {
            i = k;
        }
    }
    t[..i].parse::<f64>().unwrap_or(f64::NAN)
}

// ── the message (V8's inspector, `V8ValueStringBuilder`) ────────────────────────────────────────────────

extern "C" {
    fn korvus_object_proto_to_string(context: *const v8::Context, object: *const v8::Object) -> *const v8::String;
    fn korvus_boolean_object_value(value: *const v8::Value) -> bool;
    fn korvus_number_object_value(value: *const v8::Value) -> f64;
    fn korvus_string_object_value(value: *const v8::Value) -> *const v8::String;
    fn korvus_symbol_object_value(value: *const v8::Value) -> *const v8::Symbol;
    fn korvus_bigint_object_value(value: *const v8::Value) -> *const v8::BigInt;
}

/// A handle the shim made in the current scope, or `None` for its null.
///
/// SAFETY: `p` is null or a handle slot of the current HandleScope (a rusty_v8 `Local` is `repr(transparent)`
/// over that pointer, as the codegen shim relies on too).
unsafe fn local<'s, T>(p: *const T) -> Option<v8::Local<'s, T>> {
    if p.is_null() { None } else { Some(unsafe { std::mem::transmute::<*const T, v8::Local<'s, T>>(p) }) }
}

fn raw<T>(l: v8::Local<T>) -> *const T {
    // SAFETY: as [`local`], the other way.
    unsafe { std::mem::transmute::<v8::Local<T>, *const T>(l) }
}

/// A primitive wrapper's value (no page code runs), anything else as it is.
fn unwrapped<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Value> {
    let p = raw(v);
    // SAFETY: each shim call is V8's own `ValueOf` on a value of the kind just checked.
    unsafe {
        if v.is_big_int_object() {
            local(korvus_bigint_object_value(p)).map(Into::into).unwrap_or(v)
        } else if v.is_boolean_object() {
            v8::Boolean::new(scope, korvus_boolean_object_value(p)).into()
        } else if v.is_number_object() {
            v8::Number::new(scope, korvus_number_object_value(p)).into()
        } else if v.is_string_object() {
            local(korvus_string_object_value(p)).map(Into::into).unwrap_or(v)
        } else if v.is_symbol_object() {
            local(korvus_symbol_object_value(p)).map(Into::into).unwrap_or(v)
        } else {
            v
        }
    }
}

/// V8's `maxArrayItemsLimit` and `maxStackDepthLimit`.
const ARRAY_ITEMS: u32 = 10_000;
const ARRAY_DEPTH: usize = 32;

/// One argument's text under construction.
struct Text<'s> {
    out: String,
    items_left: u32,
    arrays: Vec<v8::Local<'s, v8::Array>>,
    /// The builder's `TryCatch` has caught: every later append fails, and the text is thrown away.
    caught: bool,
}

impl Text<'_> {
    fn push(&mut self, s: &str) -> bool {
        if self.caught {
            return false;
        }
        self.out.push_str(s);
        true
    }
}

/// One argument's message text, as V8's inspector builds it: what it converts is the page-visible part, and
/// a throw anywhere is swallowed, leaving the text empty.
fn arg_text<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> String {
    v8::tc_scope!(let tc, scope);
    let mut t = Text { out: String::new(), items_left: ARRAY_ITEMS, arrays: Vec::new(), caught: false };
    let ok = append(tc, &mut t, v, false);
    tc.reset();
    if ok && !t.caught { t.out } else { String::new() }
}

fn append<'s>(scope: &mut v8::PinScope<'s, '_>, t: &mut Text<'s>, v: v8::Local<'s, v8::Value>, in_array: bool) -> bool {
    // An array's null and undefined elements are skipped.
    if in_array && v.is_null_or_undefined() {
        return true;
    }
    let v = unwrapped(scope, v);
    if v.is_string() {
        let s = v.to_rust_string_lossy(scope);
        return t.push(&s);
    }
    if v.is_big_int() {
        let Some(s) = v.to_string(scope) else {
            t.caught = true;
            return false;
        };
        let s = s.to_rust_string_lossy(scope);
        return t.push(&s) && t.push("n");
    }
    if let Ok(sym) = v8::Local::<v8::Symbol>::try_from(v) {
        t.out.push_str("Symbol(");
        let d = sym.description(scope);
        let ok = if d.is_undefined() { true } else { append(scope, t, d, false) };
        t.out.push(')');
        return ok;
    }
    if let Ok(a) = v8::Local::<v8::Array>::try_from(v) {
        return append_array(scope, t, a);
    }
    if v.is_proxy() {
        t.out.push_str("[object Proxy]");
        return true;
    }
    if v.is_object() && !v.is_date() && !v.is_function() && !v.is_native_error() && !v.is_reg_exp() {
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            let ctx = scope.get_current_context();
            // SAFETY: V8's own `ObjectProtoToString`; a returned handle is in this scope.
            match unsafe { local(korvus_object_proto_to_string(raw(ctx), raw(o))) } {
                Some(s) => {
                    let s = s.to_rust_string_lossy(scope);
                    return t.push(&s);
                }
                // its tag's getter threw: V8 goes on to `ToString`, whose result is then refused
                None => t.caught = true,
            }
        }
    }
    match v.to_string(scope) {
        Some(s) => {
            let s = s.to_rust_string_lossy(scope);
            t.push(&s)
        }
        None => {
            t.caught = true;
            false
        }
    }
}

fn append_array<'s>(scope: &mut v8::PinScope<'s, '_>, t: &mut Text<'s>, a: v8::Local<'s, v8::Array>) -> bool {
    if t.arrays.iter().any(|seen| seen.strict_equals(a.into())) {
        return true;
    }
    let len = a.length();
    if len > t.items_left || t.arrays.len() > ARRAY_DEPTH {
        return false;
    }
    t.items_left -= len;
    t.arrays.push(a);
    let mut result = true;
    for i in 0..len {
        if i > 0 {
            t.out.push(',');
        }
        // A getter that throws: the element is skipped, and the next append fails.
        let Some(v) = a.get_index(scope, i) else {
            t.caught = true;
            continue;
        };
        if !append(scope, t, v, true) {
            result = false;
            break;
        }
    }
    t.arrays.pop();
    result
}

/// The message of `args`: each argument's text, space-separated.
fn message<'s>(scope: &mut v8::PinScope<'s, '_>, args: &[v8::Local<'s, v8::Value>]) -> String {
    let parts: Vec<String> = args.iter().map(|v| arg_text(scope, *v)).collect();
    parts.join(" ")
}

/// The host sink.
fn print(scope: &mut v8::PinScope, line: &str) {
    let line = str_val(scope, line);
    let Some(f) = deno_op(scope, "op_kprint") else { return };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    f.call(scope, undef, &[line]);
}

// ── the members ─────────────────────────────────────────────────────────────────────────────────────────

/// `debug` `error` `info` `log` `warn` `trace` `group` `groupCollapsed`: the Formatter, then the message --
/// none for a call without arguments, unless the method has a default one.
fn log_like<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, default: Option<&str>, kind: &'static str) {
    // A DevTools client hears the call with its arguments as they were passed, before any formatting (V8's inspector
    // reports `consoleAPICalled` from the raw arguments).
    crate::devtools_log::console(scope, kind, &args_of(args));
    let mut v = args_of(args);
    if !format(scope, &mut v, 0) {
        return;
    }
    if v.is_empty() {
        if let Some(d) = default {
            print(scope, d);
        }
        return;
    }
    let text = message(scope, &v);
    print(scope, &text);
}

fn cb_log<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, None, "log");
}

fn cb_debug<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, None, "debug");
}

fn cb_info<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, None, "info");
}

fn cb_error<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, None, "error");
}

fn cb_warn<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, None, "warning");
}

fn cb_trace<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, Some("console.trace"), "trace");
}

fn cb_group<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, Some("console.group"), "startGroup");
}

fn cb_group_collapsed<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    log_like(scope, &args, Some("console.groupCollapsed"), "startGroupCollapsed");
}

/// `dir` `dirxml` `table`: the message of the arguments, no Formatter.
fn cb_plain<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let v = args_of(&args);
    crate::devtools_log::console(scope, "dir", &v);
    if v.is_empty() {
        return;
    }
    let text = message(scope, &v);
    print(scope, &text);
}

/// `groupEnd` `clear`: their arguments' message is built, and nothing shows it.
fn cb_silent<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let v = args_of(&args);
    let _ = message(scope, &v);
}

/// `assert(condition, ...data)`: nothing at all for a truthy condition (V8 returns before formatting);
/// otherwise the Formatter from the second argument and the message of the data, or "console.assert".
fn cb_assert<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if args.get(0).boolean_value(scope) {
        return;
    }
    let mut v = args_of(&args);
    if !format(scope, &mut v, 1) {
        return;
    }
    if v.len() >= 2 {
        let _ = message(scope, &v[1..]);
    }
}

/// V8's `firstArgToString`: a string as it is, none or `undefined` "default", anything else `ToString`'d --
/// whose throw leaves the call (`None`).
fn label(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<String> {
    if args.length() == 0 {
        return Some("default".to_string());
    }
    let a = args.get(0);
    if a.is_string() {
        return Some(a.to_rust_string_lossy(scope));
    }
    if a.is_undefined() {
        return Some("default".to_string());
    }
    a.to_string(scope).map(|s| s.to_rust_string_lossy(scope))
}

/// The realm's count and timer tables, on its global: V8 keeps them per context.
fn table<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Map>> {
    let global = scope.get_current_context().global(scope);
    if let Some(m) = crate::core::css::css_style_declaration::priv_get(scope, global, name).and_then(|v| v8::Local::<v8::Map>::try_from(v).ok()) {
        return Some(m);
    }
    let m = v8::Map::new(scope);
    crate::core::dom::element::priv_set(scope, global, name, m.into());
    Some(m)
}

const COUNTS: &str = "korvus_console_counts";
const TIMERS: &str = "korvus_console_timers";

fn now_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

fn cb_count<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(l) = label(scope, &args) else { return };
    let Some(m) = table(scope, COUNTS) else { return };
    let k = str_val(scope, &l);
    let n = m.get(scope, k).and_then(|v| v.integer_value(scope)).unwrap_or(0) + 1;
    let nv: v8::Local<v8::Value> = v8::Number::new(scope, n as f64).into();
    m.set(scope, k, nv);
    print(scope, &format!("{l}: {n}"));
}

fn cb_count_reset<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(l) = label(scope, &args) else { return };
    let Some(m) = table(scope, COUNTS) else { return };
    let k = str_val(scope, &l);
    if m.has(scope, k) == Some(true) {
        let zero: v8::Local<v8::Value> = v8::Number::new(scope, 0.0).into();
        m.set(scope, k, zero);
    } else {
        print(scope, &format!("Count for '{l}' does not exist"));
    }
}

fn cb_time<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(l) = label(scope, &args) else { return };
    let Some(m) = table(scope, TIMERS) else { return };
    let k = str_val(scope, &l);
    if m.has(scope, k) == Some(true) {
        print(scope, &format!("Timer '{l}' already exists"));
        return;
    }
    let t: v8::Local<v8::Value> = v8::Number::new(scope, now_ms()).into();
    m.set(scope, k, t);
}

/// `timeLog(label, ...data)`: "label: N ms" and the data's message.
fn cb_time_log<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(l) = label(scope, &args) else { return };
    let Some(m) = table(scope, TIMERS) else { return };
    let k = str_val(scope, &l);
    let Some(start) = m.get(scope, k).filter(|v| v.is_number()).and_then(|v| v.number_value(scope)) else {
        print(scope, &format!("Timer '{l}' does not exist"));
        return;
    };
    let v = args_of(&args);
    let rest = if v.len() > 1 { message(scope, &v[1..]) } else { String::new() };
    let head = format!("{l}: {} ms", now_ms() - start);
    print(scope, &if rest.is_empty() { head } else { format!("{head} {rest}") });
}

fn cb_time_end<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(l) = label(scope, &args) else { return };
    let Some(m) = table(scope, TIMERS) else { return };
    let k = str_val(scope, &l);
    let Some(start) = m.get(scope, k).filter(|v| v.is_number()).and_then(|v| v.number_value(scope)) else {
        print(scope, &format!("Timer '{l}' does not exist"));
        return;
    };
    m.delete(scope, k);
    print(scope, &format!("{l}: {} ms", now_ms() - start));
}

/// `profile` `profileEnd` `timeStamp`: the label is converted (that is what the page can see), and this
/// engine has no profiler or timeline to hand it to.
fn cb_label<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let _ = label(scope, &args);
}

/// `console.context(name)`: V8's -- the name `ToString`'d when given (its throw leaves the call), then a
/// fresh object over a fresh empty prototype, its own enumerable members fresh functions of `length` 1
/// (measured: two calls share neither the prototype nor a function).
fn cb_context<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if args.length() > 0 && args.get(0).to_string(scope).is_none() {
        return;
    }
    let proto = v8::Object::new(scope);
    let ctx = v8::Object::new(scope);
    ctx.set_prototype(scope, proto.into());
    for (name, body) in CONTEXT_MEMBERS {
        let f = member(scope, *body, name, 1, false);
        set_prop(scope, ctx, name, f.into());
    }
    rv.set(ctx.into());
}

/// `console.createTask(name)` -- V8's, as Chrome 152.0.7977.82 answers it (measured): the
/// name must be a non-empty string (anything else is `Error: First argument must be a non-empty string.`),
/// and the task is an instance of a function template's: its prototype is that function's own (one per realm,
/// shared by every task of the realm), holding only the anonymous `constructor` of length 0, over
/// `Object.prototype` (Chrome 150's task was a plain object, `Object.prototype` its prototype). Its own `run`
/// is ONE function per realm -- `a.run === b.run` -- made from its template, constructible (it carries a
/// `prototype`, as V8's does). It used to accept any name and give every task a `run` of its own (length 1)
/// that checked nothing.
fn cb_console_create_task<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let name = args.get(0);
    if !name.is_string() || name.to_rust_string_lossy(scope).is_empty() {
        throw_error(scope, "First argument must be a non-empty string.");
        return;
    }
    // the instance template of an anonymous function template (V8's task is one): `new` of the realm's function
    let ctor = crate::binding::protos::template_ctor(scope, cb_console_task_ctor, "", 0, None);
    let Some(task) = ctor.new_instance(scope, &[]) else { return };
    let run = crate::binding::protos::template_ctor(scope, cb_task_run, "run", 0, None);
    set_prop(scope, task, "run", run.into());
    let mark: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    crate::core::dom::element::priv_set(scope, task, TASK_MARK, mark);
    rv.set(task.into());
}

/// The task's anonymous constructor, the `constructor` of its prototype: a construction answers its new object,
/// a call its receiver (measured: `C()` is the window, `new C()` an object), as an API function with no
/// callback of its own does.
fn cb_console_task_ctor(_scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(args.this().into());
}

/// The private a task carries: what `run` checks its receiver for.
const TASK_MARK: &str = "korvus_console_task";

/// `task.run(f)`: `f()`, its result passed back. The receiver is checked first -- a foreign one (or a `new`
/// call's fresh object) is `Error: 'run' called with illegal receiver.` -- then the argument, `Error: First
/// argument must be a function.`
fn cb_task_run(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if crate::core::css::css_style_declaration::priv_get(scope, args.this(), TASK_MARK).is_none() {
        throw_error(scope, "'run' called with illegal receiver.");
        return;
    }
    let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        throw_error(scope, "First argument must be a function.");
        return;
    };
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    if let Some(r) = f.call(scope, undef, &[]) {
        rv.set(r);
    }
}

/// An `Error` (not a TypeError: V8's console throws the plain kind).
fn throw_error(scope: &mut v8::PinScope, msg: &str) {
    let m = v8::String::new(scope, msg).unwrap();
    let e = v8::Exception::error(scope, m);
    scope.throw_exception(e);
}

/// Install `globalThis.console`. NON-ENUMERABLE, writable and configurable — Chrome's descriptor.
pub fn install_console<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, window_realm: bool) {
    // Chrome's `console` is NOT directly on `Object.prototype`: `Object.getPrototypeOf(console)` is an
    // object of its own whose `constructor` is `Object` (measured). One empty level, reproduced.
    let console = v8::Object::new(scope);
    let mid = v8::Object::new(scope);
    console.set_prototype(scope, mid.into());
    let define = |scope: &mut v8::PinScope<'s, '_>, name: &str, f: v8::Local<v8::Function>| {
        let Some(k) = v8::String::new(scope, name) else { return };
        let mut pd = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
        pd.set_enumerable(true);
        pd.set_configurable(true);
        console.define_property(scope, k.into(), &pd);
    };
    // Each method from its template: `console` is V8's key, which no realm's global
    // template records, so every realm's install makes its object -- from the isolate's templates.
    for (name, body, length) in MEMBERS {
        let f = member(scope, *body, name, *length, true);
        define(scope, name, f);
    }

    let ct = template_fn(scope, cb_console_create_task, "createTask", 0, Some("createTask"));
    define(scope, "createTask", ct);
    // `memory` is a plain object in Chrome (its fields are behind a flag) and is the LAST own key --
    // and it is WINDOW-ONLY. Measured on google-chrome --headless=new 150 with
    // `core/korvus-runtime/tests/fixtures/worker-scope-probe.js` run in a page and in a real worker:
    // the page's `Object.getOwnPropertyNames(console)` ends `…, context, createTask, memory` and the
    // worker's ends `…, context, createTask`. It is one of the twelve window/worker asymmetries
    // `tests/worker_scope_live.rs` pins; this realm used to have it in both.
    if let Some(k) = v8::String::new(scope, "memory").filter(|_| window_realm) {
        let m = v8::Object::new(scope);
        let mut pd = v8::PropertyDescriptor::new_from_value_writable(m.into(), true);
        pd.set_enumerable(true);
        pd.set_configurable(true);
        console.define_property(scope, k.into(), &pd);
    }
    // `Object.prototype.toString.call(console)` is "[object console]" -- the WebIDL NAMESPACE's name,
    // lower-case, and not an interface's.
    if let Some(tag) = v8::String::new(scope, "console") {
        let key = v8::Symbol::get_to_string_tag(scope);
        let mut pd = v8::PropertyDescriptor::new_from_value_writable(tag.into(), false);
        pd.set_enumerable(false);
        pd.set_configurable(true);
        console.define_property(scope, key.into(), &pd);
    }
    let Some(k) = v8::String::new(scope, "console") else { return };
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(console.into(), true);
    pd.set_enumerable(false);
    pd.set_configurable(true);
    global.define_property(scope, k.into(), &pd);
}
