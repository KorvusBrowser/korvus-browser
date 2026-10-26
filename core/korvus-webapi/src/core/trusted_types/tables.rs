//! `core/trusted_types/tables.rs` — the five `interface!` tables behind `window.trustedTypes`, and
//! their bodies.
//!
//! `TrustedTypePolicyFactory` 10 rows, `TrustedTypePolicy` 4, `TrustedHTML` / `TrustedScript` /
//! `TrustedScriptURL` 2 each. All four captures record exactly these, in exactly this order, with
//! these lengths and `ctorIndex` 10 / 4 / 2 / 2 / 2 — which the create places from the capture
//! (`Mount::ctor_at`), not this file.
//!
//! # WHAT THE DEVICE ANSWERS — Chrome 150.0.7871.46 and Brave 151.1.93.138, headed over CDP under
//! Xvfb, 2026-09-19/20, at two origins. Identical.
//!
//! ```text
//! trustedTypes                own accessor, `get trustedTypes` only, e=1 c=1; one object per realm
//! emptyHTML / emptyScript     STABLE per realm (`tt.emptyHTML === tt.emptyHTML`), String() is "",
//!                             [object TrustedHTML] / [object TrustedScript], isHTML/isScript true
//! defaultPolicy               null, until `createPolicy('default', …)` — then that policy, ===
//! createPolicy(n, o)          a TrustedTypePolicy whose `name` is n. A DUPLICATE name is fine on a
//!                             page with no CSP (measured: a second `createPolicy('x')` succeeds)
//! policy.createHTML(s, …)     calls o.createHTML with ALL the arguments (3 in, 3 seen) and `this`
//!                             undefined (a sloppy callback therefore sees the global). Its return
//!                             is String()-ified, and null and undefined both become "" — NOT
//!                             "null"/"undefined", which is the reading an IDL would not predict.
//!                             A callback that throws lets the throw through unchanged.
//! a policy with no such member
//!                             TypeError "Failed to execute 'createHTML' on 'TrustedTypePolicy':
//!                             Policy <name>'s TrustedTypePolicyOptions did not specify a
//!                             'createHTML' member."
//! the Trusted* values         no own properties; chain TrustedHTML -> Object; `new TrustedHTML()`
//!                             is Illegal constructor; toString() and toJSON() both give the string,
//!                             so `'p' + h`, `String(h)` and `JSON.stringify({h})` all work
//! isHTML(x)                   true only for a TrustedHTML — a TrustedScript, a string and `{}` are
//!                             all false; no argument is a TypeError
//! every prototype member      TypeError: Illegal invocation on a foreign receiver, the BARE
//!                             sentence (none of these rows returns a promise)
//! ```
//!
//! # Exposure
//!
//! **Unconditional**, measured (FP5.7/FP5.9): `trustedTypes` is an own property of `window` on
//! `http://web-platform.test:<p>/` — `isSecureContext` false — on all four browsers, and none of the
//! five interface objects is gated either. All five are absent from
//! `tools/sync/data/secure_context.json`, and that file's silence IS the reading.
//!
//! # Realms
//!
//! The five interfaces are `[Exposed=(Window,Worker)]` and every `worker.surface.json` in the tree
//! lists all five with member lists byte-identical to the window's — so they are installed in BOTH
//! realms (invariant I8), from these same tables. **The `trustedTypes` SINGLETON is window-only**:
//! no worker capture lists it as an own global, because in a worker it is a `WorkerGlobalScope`
//! prototype attribute rather than an own property of the global object, and that row is not this
//! lane's.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, require_args, str_val, throw_type_error};
use crate::install::installer::{self, Flavor};
use crate::install::MemberDef;
use crate::install::kits::branded;

// ── the private slots ───────────────────────────────────────────────────────────────────────────

fn slot<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).expect("a slot name is a string");
    v8::Private::for_api(scope, Some(n))
}

/// Which of the three Trusted types a value is, or `None` for anything this module did not mint.
/// The brand is a PRIVATE SLOT rather than the prototype chain, so `Object.create(TrustedHTML.prototype)`
/// is not an HTML — measured: `tt.isHTML({})` and `tt.isHTML('x')` are both false, and so is a
/// TrustedScript.
fn kind_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    let k = slot(scope, "korvus.trustedTypes.kind");
    let got = obj.get_private(scope, k)?;
    if got.is_undefined() {
        return None;
    }
    Some(got.to_rust_string_lossy(scope))
}

/// The string a Trusted value wraps.
fn value_of(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Option<String> {
    let k = slot(scope, "korvus.trustedTypes.value");
    let got = obj.get_private(scope, k)?;
    if got.is_undefined() {
        return None;
    }
    Some(got.to_rust_string_lossy(scope))
}

/// A sink's `(TrustedX or DOMString)` argument as Blink's bindings convert it: a value of the union's Trusted type
/// is its own string, read from its slot -- nothing on it runs; anything else is WebIDL's ToString, a Trusted value
/// of ANOTHER type included (its `toString` runs, as in Chrome). `kind` is `"TrustedHTML"`, `"TrustedScript"` or
/// `"TrustedScriptURL"`; `""` is `setAttribute`'s `TrustedType`, any of the three. `None`: ToString threw.
///
/// Chrome 150, measured (the `tt-sinks` probe): none of the probe's 30 sinks calls a page's
/// `toString` or `@@toPrimitive` on a value of its own Trusted type; this engine called both on every one -- the
/// an interactive-widget challenge challenge frame's `new Worker(policy.createScriptURL(blobURL))` among them.
pub fn sink_string<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<'_, v8::Value>, kind: &str) -> Option<v8::Local<'s, v8::String>> {
    if kind_of(scope, value).is_some_and(|k| kind.is_empty() || k == kind) {
        let obj = v8::Local::<v8::Object>::try_from(value).ok()?;
        let vk = slot(scope, "korvus.trustedTypes.value");
        if let Some(s) = obj.get_private(scope, vk).and_then(|v| v8::Local::<v8::String>::try_from(v).ok()) {
            return Some(s);
        }
    }
    value.to_string(scope)
}

/// Node `id` if `this` is an HTML `<script>` element -- the one element whose `textContent` and `innerText`
/// (and `text`) are Trusted Types script sinks (Blink `HTMLScriptElement`; a `<div>`'s are not, measured).
pub fn html_script_id(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    let id = crate::binding::protos::unwrap_id(scope, this.into())?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.try_borrow::<crate::binding::HostDom>()?.0;
    (dom.is_html_ns(id) && dom.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("script"))).then_some(id)
}

/// A script element's text sink (`HTMLScriptElement text` / `textContent` / `innerText`): `value` -- `None`
/// for a null the binding made "" -- through [`compliant_text`] with Blink's sample and sentence.
pub fn script_text_sink(scope: &mut v8::PinScope, id: u32, value: Option<v8::Local<v8::Value>>, prop: &str) -> Option<String> {
    let (sample, context) = (format!("HTMLScriptElement {prop}"), format!("Failed to set the '{prop}' property on 'HTMLScriptElement'"));
    let sink = Sink { kind: "TrustedScript", sample: &sample, context: &context };
    match value {
        Some(v) => compliant_text(scope, v, &sink, SinkAt::Node(id)),
        None => check_text(scope, String::new(), &sink, SinkAt::Node(id)),
    }
}

/// A `(TrustedType or DOMString)` argument (`setAttribute`'s value): a Trusted value of any of the three
/// types is its own string, nothing on it called, and says which type it is; anything else is ToString'd.
/// `None`: ToString threw.
pub fn trusted_or_string(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Option<(String, Option<String>)> {
    if let Some(kind) = kind_of(scope, value) {
        if let Some(text) = v8::Local::<v8::Object>::try_from(value).ok().and_then(|o| value_of(scope, o)) {
            return Some((text, Some(kind)));
        }
    }
    Some((value.to_string(scope)?.to_rust_string_lossy(scope), None))
}

/// Whether `value` is a Trusted value of type `kind` -- which of a sink's overloads it picks (`document.write`).
pub fn is_trusted(scope: &mut v8::PinScope, value: v8::Local<v8::Value>, kind: &str) -> bool {
    kind_of(scope, value).is_some_and(|k| k == kind)
}

/// [`sink_string`] as Rust text (a lone surrogate replaced, as a USVString is).
pub fn sink_text(scope: &mut v8::PinScope, value: v8::Local<v8::Value>, kind: &str) -> Option<String> {
    sink_string(scope, value, kind).map(|s| s.to_rust_string_lossy(scope))
}

// ── eval(TrustedScript) ─────────────────────────────────────────────────────────────
//
// Chrome makes a TrustedScript CODE-LIKE and hands V8 a callback that turns one into its script, so
// `eval(policy.createScript("1+1"))` is 2 (measured, `tools/realweb/seed/tt`); korvus's plain object came
// back from `eval` unevaluated, and knockoutjs.com compiles every `data-bind` that way. The two V8 calls
// are in `shim/codegen.cc` (rusty_v8 150 binds neither).

type ModifyFn = extern "C" fn(*const v8::Context, *const v8::Value, bool, *mut bool) -> *const v8::String;

extern "C" {
    fn korvus_set_codegen_callback(isolate: *mut std::ffi::c_void, modify: ModifyFn);
    fn korvus_template_set_code_like(templ: *const v8::ObjectTemplate);
}

/// Register the code-generation callback on the isolate, and route the realm being set up through it.
/// Once, at engine (or worker) start.
pub fn install_codegen_hook(scope: &mut v8::PinScope) {
    route_codegen_through_policy(scope);
    let isolate: &mut v8::Isolate = scope;
    // SAFETY: `UnsafeRawIsolatePtr` is `repr(transparent)` over the C++ `v8::Isolate*`, which is what
    // the shim's `SetModifyCodeGenerationFromStringsCallback` is a member of.
    unsafe {
        let raw = isolate.as_raw_isolate_ptr();
        let ptr: *mut std::ffi::c_void = std::mem::transmute(raw);
        korvus_set_codegen_callback(ptr, korvus_codegen_modify);
    }
}

/// The realm being set up asks the code-generation callback for every string `eval` / `Function` (its
/// CSP and Trusted Types decide, [`korvus_codegen_modify`]); a frame realm is set up the same way when
/// it is made (`browsing_context::new_realm`).
pub fn route_codegen_through_policy(scope: &mut v8::PinScope) {
    let ctx = scope.get_current_context();
    ctx.set_allow_generation_from_strings(false);
}

/// V8's question from inside `eval` / `new Function`, asked for EVERY string (every korvus context has
/// code generation from strings off) and every code-like object -- Blink's code-generation check, in its
/// order (`CodeGenerationCheckCallbackInMainThread`):
///
/// 1. anything but a string or a `TrustedScript` is left to V8 (`eval` returns it);
/// 2. Trusted Types required AND `'trusted-types-eval'` listed: allowed as it is, nothing more asked;
/// 3. a `TrustedScript` is its script; a string, where the realm's CSP says `require-trusted-types-for
///    'script'`, goes through the default policy's `createScript(input, "TrustedScript", sink)` --
///    refused with no default policy, when it throws or answers null/undefined, or when its answer is not
///    the input itself (CSP3 "can compile strings" 1.2.8);
/// 4. then CSP's `'unsafe-eval'`: refused where a `script-src` (or `default-src`) omits it.
///
/// Chrome 150, measured: under `require-trusted-types-for 'script'` with a default policy, `eval("1+1")`
/// calls `createScript` with ("1+1", "TrustedScript", "eval") and `new Function("return 5")` with
/// ("(function anonymous(\n) {\nreturn 5\n})", "TrustedScript", "Function"); korvus never asked
/// (korvus-analysis U-1, a bot-challenge frame). A refusal throws V8's own EvalError text, not
/// Chrome's CSP sentence (rusty_v8 binds no `SetErrorMessageForCodeGenerationFromStrings`).
extern "C" fn korvus_codegen_modify(context: *const v8::Context, source: *const v8::Value, _is_code_like: bool, allowed: *mut bool) -> *const v8::String {
    if context.is_null() || source.is_null() || allowed.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a rusty_v8 `Local` is `repr(transparent)` over the same handle pointer V8 passed the shim.
    let context: v8::Local<v8::Context> = unsafe { std::mem::transmute(context) };
    let source: v8::Local<v8::Value> = unsafe { std::mem::transmute(source) };
    v8::callback_scope!(unsafe scope, context);
    // a DevTools client switched the page's policy off (`Page.setBypassCSP`): every string may become code
    if spec::devtools::bypass_csp() {
        return std::ptr::null();
    }
    // the policies of the realm whose `eval` / `Function` this is (Blink `ExecutionContext::From(context)`),
    // not the realm that is running: `frame.contentWindow.eval(s)` answers to the FRAME's policies
    let list = {
        let id = crate::core::html::browsing_context::ctx_of_context(scope, context);
        crate::core::html::browsing_context::ctx_csp(scope, id)
    };
    // A refusal is V8's EvalError with the message the realm's policies give (Blink sets each context's
    // `EvalDisabledErrorMessage`): "... because 'unsafe-eval' is not an allowed source of script: <directive>"
    // or "... violates this document's Trusted Type assignment requirements." -- measured on Chrome 150.
    let refuse = |scope: &mut v8::PinScope| {
        if let Some(m) = list.eval_disabled_message().and_then(|m| v8::String::new(scope, &m)) {
            crate::binding::v8_legacy::set_error_message_for_code_generation_from_strings(context, m);
        }
        // SAFETY: the shim's own stack flag, valid for this call.
        unsafe { *allowed = false };
        std::ptr::null()
    };
    let text = if let Ok(s) = v8::Local::<v8::String>::try_from(source) {
        s
    } else if let Ok(obj) = v8::Local::<v8::Object>::try_from(source) {
        let kk = slot(scope, "korvus.trustedTypes.kind");
        if obj.get_private(scope, kk).map(|v| v.to_rust_string_lossy(scope)).as_deref() != Some("TrustedScript") {
            return std::ptr::null();
        }
        let vk = slot(scope, "korvus.trustedTypes.value");
        match obj.get_private(scope, vk).and_then(|v| v8::Local::<v8::String>::try_from(v).ok()) {
            Some(t) => t,
            None => return refuse(scope),
        }
    } else {
        return std::ptr::null();
    };
    let policy = list.eval_policy();
    if policy.trusted_types_for_script && policy.trusted_types_eval {
        return std::ptr::null();
    }
    // a report-only `require-trusted-types-for` asks the default policy too, and lets a refusal through
    let tt = list.trusted_types();
    if tt.required && source.is_string() {
        let sink = eval_sink_name(&text.to_rust_string_lossy(scope));
        let same = match default_policy_create_script(scope, context, text, sink) {
            DefaultPolicyAnswer::Script(out) => out.strict_equals(text.into()),
            _ => false,
        };
        if !same && tt.enforced {
            return refuse(scope);
        }
    }
    // each policy without 'unsafe-eval' reports a violation, a report-only one too
    let violations = list.eval_violations(&text.to_rust_string_lossy(scope));
    crate::core::html::csp::report_violations(scope, None, violations, true);
    if !policy.eval_allowed {
        return refuse(scope);
    }
    // SAFETY: the handle lives in the HandleScope V8 opened around this callback.
    unsafe { std::mem::transmute::<v8::Local<v8::String>, *const v8::String>(text) }
}

/// The sink a code string names to the default policy, as Blink reads it off the string V8 hands over
/// (`GetSamplePrefix`): "Function" for the source V8 builds for a `Function` (or async / generator)
/// constructor, "eval" for everything else.
fn eval_sink_name(text: &str) -> &'static str {
    const FUNCTION: [&str; 4] = ["(function anonymous", "(async function anonymous", "(function* anonymous", "(async function* anonymous"];
    if FUNCTION.iter().any(|p| text.starts_with(p)) { "Function" } else { "eval" }
}

/// What the realm's default policy answered for a script sink (Trusted Types "process value with a
/// default policy", Blink `TrustedTypesCheckForScript`).
enum DefaultPolicyAnswer<'s> {
    /// no default policy
    NoPolicy,
    /// a default policy whose options have no `createScript`
    NoCreateScript,
    /// `createScript` threw this
    Threw(v8::Local<'s, v8::Value>),
    /// `createScript` answered null or undefined
    Failed,
    Script(v8::Local<'s, v8::String>),
}

/// The realm's `TrustedTypePolicyFactory` as the engine holds it -- read from its slot, never through
/// the page-visible `trustedTypes` getter (Blink reads its own; a getter call here showed as a
/// `Window.get trustedTypes` Chrome never makes). None yet = nothing has read `trustedTypes`, so no
/// policy can exist.
fn realm_factory<'s>(scope: &mut v8::PinScope<'s, '_>, context: v8::Local<v8::Context>) -> Option<v8::Local<'s, v8::Object>> {
    let global = context.global(scope);
    let v = crate::core::html::window_globals::window_slot_get(scope, "trustedTypes")
        .or_else(|| crate::core::css::css_style_declaration::priv_get(scope, global, "korvus_worker_trusted_types"))?;
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// The realm's default policy run over `input` for the script sink `sink`: its
/// `createScript(input, "TrustedScript", sink)`.
fn default_policy_create_script<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    context: v8::Local<v8::Context>,
    input: v8::Local<v8::String>,
    sink: &str,
) -> DefaultPolicyAnswer<'s> {
    default_policy_create(scope, context, "TrustedScript", input, sink)
}

/// The realm's default policy run over `input` for a sink of Trusted type `kind`: its `createHTML`,
/// `createScript` or `createScriptURL`, called with `(input, kind, sink)` (Blink `GetDefaultCallbackArgs`:
/// the type, then the sample prefix) and `undefined` for `this`; its answer read as Blink's `DOMString?`.
fn default_policy_create<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    context: v8::Local<v8::Context>,
    kind: &str,
    input: v8::Local<v8::String>,
    sink: &str,
) -> DefaultPolicyAnswer<'s> {
    let (member, label) = match kind {
        "TrustedHTML" => ("createHTML", "CreateHTMLCallback"),
        "TrustedScript" => ("createScript", "CreateScriptCallback"),
        _ => ("createScriptURL", "CreateURLCallback"),
    };
    let Some(factory) = realm_factory(scope, context) else { return DefaultPolicyAnswer::NoPolicy };
    let k = slot(scope, DEFAULT_POLICY);
    let Some(policy) = factory.get_private(scope, k).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return DefaultPolicyAnswer::NoPolicy };
    let k = slot(scope, cb_slot_name(member));
    let Some(cb) = policy.get_private(scope, k).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return DefaultPolicyAnswer::NoCreateScript };
    let argv = [input.into(), str_val(scope, kind), str_val(scope, sink)];
    let recv: v8::Local<v8::Value> = v8::undefined(scope).into();
    v8::tc_scope!(let tc, scope);
    let out = call_policy_callback(tc, label, cb, recv, &argv);
    if tc.has_caught() {
        let e = tc.exception().unwrap_or_else(|| v8::undefined(tc).into());
        tc.reset();
        return DefaultPolicyAnswer::Threw(e);
    }
    match out {
        Some(v) if !v.is_null_or_undefined() => match v.to_string(tc) {
            Some(t) => DefaultPolicyAnswer::Script(t),
            None => {
                let e = tc.exception().unwrap_or_else(|| v8::undefined(tc).into());
                tc.reset();
                DefaultPolicyAnswer::Threw(e)
            }
        },
        _ => DefaultPolicyAnswer::Failed,
    }
}

// ── Trusted Types at the DOM sinks ────────────────────────────────────────────
//
// Under `require-trusted-types-for 'script'` a STRING reaching an injection sink goes through the
// document's default policy, and a refusal throws -- every sink Blink annotates, with the sample it hands
// the policy and the sentence it throws, measured on stock Chrome 150.0.7871.46 over 33 sinks in five
// policy shapes, three deliveries and a nested `about:blank` (`tests/fixtures/trusted-types-enforced`).
// korvus took a Trusted value as itself at every sink and never asked a policy about a string, so the
// an interactive-widget challenge challenge frame's nested-frame test (`innerHTML`, `eval`, `script.src` under the frame's
// default policy) answered 2 where Chrome answers 7.
//
// Whose policy: the NODE's document's (Blink `Element::GetExecutionContext()`), not the caller's -- an
// element moved into another document answers to that one, and a frame's `innerHTML` setter called on a
// page element asks the page. A realm-level sink (`new Worker`, `DOMParser`, `Document.parseHTMLUnsafe`)
// asks the realm it belongs to.

/// Where a sink's value lands: a node (its document's policies and default policy apply) or a realm.
#[derive(Clone, Copy)]
pub enum SinkAt<'s> {
    Node(u32),
    Realm(v8::Local<'s, v8::Context>),
}

/// One sink: the Trusted type it takes, the sample prefix Blink hands the default policy
/// (`"Element innerHTML"`), and the binding's sentence a refusal starts with
/// (`"Failed to set the 'innerHTML' property on 'Element'"`).
pub struct Sink<'a> {
    pub kind: &'static str,
    pub sample: &'a str,
    pub context: &'a str,
}

/// A `(TrustedX or DOMString)` sink argument as the text the sink may use: a value of the sink's own
/// Trusted type is its string, nothing on it called and nothing asked; anything else is WebIDL's ToString
/// (a Trusted value of another type included -- its `toString` runs), then [`check_text`]. `None`: an
/// exception is pending (a ToString threw, the policy threw, or the sink refused).
pub fn compliant_text(scope: &mut v8::PinScope, value: v8::Local<v8::Value>, sink: &Sink, at: SinkAt) -> Option<String> {
    if kind_of(scope, value).as_deref() == Some(sink.kind) {
        let obj = v8::Local::<v8::Object>::try_from(value).ok()?;
        return value_of(scope, obj);
    }
    let text = value.to_string(scope)?.to_rust_string_lossy(scope);
    check_text(scope, text, sink, at)
}

/// Trusted Types' "get Trusted Type compliant string" for a string at `sink` (Blink
/// `TrustedTypesCheckForHTML` / `ForScript` / `ForScriptURL`): with no `require-trusted-types-for 'script'`
/// in the document's policies, the string itself; else the default policy's answer, which is the text
/// from then on, changed or not. A missing default policy, a missing `create*`, or a null answer refuses:
/// under an enforced policy a TypeError (the binding's sentence, then Blink's), under a report-only one
/// the string goes through. What the policy throws propagates as thrown.
pub fn check_text(scope: &mut v8::PinScope, text: String, sink: &Sink, at: SinkAt) -> Option<String> {
    let ctx = match at {
        SinkAt::Node(n) => crate::core::html::browsing_context::node_ctx(scope, n),
        SinkAt::Realm(c) => crate::core::html::browsing_context::ctx_of_context(scope, c),
    };
    let req = crate::core::html::browsing_context::ctx_trusted_types(scope, ctx);
    if !req.required {
        return Some(text);
    }
    let Some(realm) = crate::core::html::browsing_context::ctx_realm(scope, ctx) else { return Some(text) };
    // The policy is the realm's and runs there; a refusal is thrown in the caller's realm, as Blink's
    // binding throws it.
    let outcome = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let input = v8::String::new(scope, &text)?;
        match default_policy_create(scope, realm, sink.kind, input, sink.sample) {
            DefaultPolicyAnswer::Script(t) => Outcome::Text(t.to_rust_string_lossy(scope)),
            DefaultPolicyAnswer::Threw(e) => Outcome::Threw(v8::Global::new(scope, e)),
            DefaultPolicyAnswer::NoPolicy => Outcome::Refused(Refusal::NoPolicy),
            DefaultPolicyAnswer::NoCreateScript => Outcome::Refused(Refusal::NoCreate),
            DefaultPolicyAnswer::Failed => Outcome::Refused(Refusal::Failed),
        }
    };
    match outcome {
        Outcome::Text(t) => Some(t),
        Outcome::Threw(e) => {
            let e = v8::Local::new(scope, &e);
            scope.throw_exception(e);
            None
        }
        Outcome::Refused(why) => refuse_sink(scope, text, sink, req, why),
    }
}

/// [`check_text`]'s policy step, carried out of the policy's realm.
enum Outcome {
    Text(String),
    Threw(v8::Global<v8::Value>),
    Refused(Refusal),
}

/// Why a sink refused a string (Blink `TrustedTypeViolationKind`).
#[derive(Clone, Copy)]
enum Refusal {
    NoPolicy,
    NoCreate,
    Failed,
}

/// The refusal: a TypeError under an enforced policy, the string itself under a report-only one.
fn refuse_sink(scope: &mut v8::PinScope, text: String, sink: &Sink, req: crate::core::html::csp::TrustedTypesRequirement, why: Refusal) -> Option<String> {
    if !req.enforced {
        return Some(text);
    }
    let k = sink.kind;
    let why = match why {
        Refusal::NoPolicy => format!("This document requires '{k}' assignment."),
        Refusal::NoCreate => format!("This document requires '{k}' assignment and no 'default' policy for '{k}' has been defined."),
        Refusal::Failed => format!("This document requires '{k}' assignment and the 'default' policy failed to execute."),
    };
    crate::binding::protos::throw_type_error(scope, &format!("{}: {why}", sink.context));
    None
}

/// A string-or-`TrustedScript` argument of the script sink `member` on `iface` (`Window setTimeout`,
/// `WorkerGlobalScope setInterval`) as the script it may run, Blink `TrustedTypesCheckForScript`: a
/// `TrustedScript` is its script; anything else is stringified, and where the realm's CSP says
/// `require-trusted-types-for 'script'` goes through the default policy, whose answer -- CHANGED or not
/// -- is the script ([`compliant_text`]). `None` = an exception is pending: the TypeError Chrome throws
/// (wording measured on Chrome 150: "Failed to execute 'setTimeout' on 'Window': This document requires
/// 'TrustedScript' assignment."), or what the policy threw. The realm is the timer's own -- the one whose
/// `setTimeout` it is.
pub fn compliant_script<'s>(scope: &mut v8::PinScope<'s, '_>, value: v8::Local<v8::Value>, iface: &str, member: &str) -> Option<v8::Local<'s, v8::String>> {
    if kind_of(scope, value).as_deref() == Some("TrustedScript") {
        let obj = v8::Local::<v8::Object>::try_from(value).ok()?;
        let vk = slot(scope, "korvus.trustedTypes.value");
        return obj.get_private(scope, vk).and_then(|v| v8::Local::<v8::String>::try_from(v).ok());
    }
    let text = value.to_string(scope)?;
    let context = scope.get_current_context();
    let ctx = crate::core::html::browsing_context::ctx_of_context(scope, context);
    // the script exactly as given (lone surrogates and all) unless a policy has a say
    if !crate::core::html::browsing_context::ctx_trusted_types(scope, ctx).required {
        return Some(text);
    }
    let sample = format!("{iface} {member}");
    let ctx_msg = format!("Failed to execute '{member}' on '{iface}'");
    let text = text.to_rust_string_lossy(scope);
    let text = check_text(scope, text, &Sink { kind: "TrustedScript", sample: &sample, context: &ctx_msg }, SinkAt::Realm(context))?;
    v8::String::new(scope, &text)
}

/// Whether the running realm's CSP lets a string become code (`'unsafe-eval'`), the question a string
/// timer asks once its script is known (Blink `DOMTimer` `IsAllowed`: refused, the timer is never made
/// and `setTimeout` answers 0). `'trusted-types-eval'` does not count here (Chrome 150, measured).
pub fn string_compilation_allowed(scope: &mut v8::PinScope) -> bool {
    crate::core::html::browsing_context::running_eval_policy(scope).eval_allowed
}

/// A policy's option callback, called as Blink calls an IDL callback (recorded as one in an analysis
/// build: `CreateHTMLCallback`, `CreateScriptCallback`, `CreateURLCallback` -- the names Chrome's tracer records).
fn call_policy_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    label: &'static str,
    cb: v8::Local<v8::Function>,
    recv: v8::Local<v8::Value>,
    argv: &[v8::Local<v8::Value>],
) -> Option<v8::Local<'s, v8::Value>> {
    #[cfg(feature = "analysis")]
    {
        crate::analysis_hook::call_traced(scope, label, cb, recv, argv)
    }
    #[cfg(not(feature = "analysis"))]
    {
        let _ = label;
        cb.call(scope, recv, argv)
    }
}

/// `TrustedScript`'s instance template, marked CODE-LIKE (V8's `ObjectTemplate::SetCodeLike`), so `eval`
/// and `new Function` take a `TrustedScript` as its source text. Applied once per isolate by the builder,
/// before anything instantiates the template (`install::instance_shapes`); a
/// TrustedScript was minted from a cached code-like `ObjectTemplate` of its own until then.
pub(crate) fn configure_code_like(it: v8::Local<v8::ObjectTemplate>) {
    // SAFETY: a rusty_v8 `Local<ObjectTemplate>` is the handle pointer the shim turns back into one.
    unsafe { korvus_template_set_code_like(std::mem::transmute::<v8::Local<v8::ObjectTemplate>, *const v8::ObjectTemplate>(it)) };
}

/// Mint one `TrustedHTML` / `TrustedScript` / `TrustedScriptURL` around `value`.
fn mint_trusted<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, value: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, iface)?;
    // from the interface's template: `TrustedScript`'s is code-like ([`configure_code_like`])
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    let k = slot(scope, "korvus.trustedTypes.kind");
    let kv = str_val(scope, iface);
    o.set_private(scope, k, kv);
    let k = slot(scope, "korvus.trustedTypes.value");
    let vv = str_val(scope, value);
    o.set_private(scope, k, vv);
    Some(o)
}

// ── the brand every row checks ──────────────────────────────────────────────────────────────────


fn illegal(scope: &mut v8::PinScope) {
    throw_type_error(scope, "Illegal invocation");
}

/// A row whose body carries the interface prototype as its `data`.
fn row_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    name: &str,
    length: i32,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) -> v8::Local<'s, v8::Function> {
    crate::install::kits::proto_data_fn(scope, proto, cb, name, length)
}

// ── TrustedHTML / TrustedScript / TrustedScriptURL ──────────────────────────────────────────────

/// `toString()` and `toJSON()` — both the wrapped string, which is what makes `'p' + h`,
/// `String(h)` and `JSON.stringify({h})` all produce it.
fn cb_trusted_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    let s = value_of(scope, args.this()).unwrap_or_default();
    let v = str_val(scope, &s);
    rv.set(v);
}

/// Mount one row of the three value interfaces.
pub fn install_trusted_value(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "toJSON" | "toString" => {
            let f = row_fn(scope, proto, member.name, member.length as i32, cb_trusted_value);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_trusted_value: unexpected member {other}"),
    }
}

// ── TrustedTypePolicy ───────────────────────────────────────────────────────────────────────────

const POLICY_NAME: &str = "korvus.trustedTypes.policyName";

/// The slot a policy keeps one of its three callbacks in.
fn cb_slot_name(kind: &str) -> &'static str {
    match kind {
        "createHTML" => "korvus.trustedTypes.createHTML",
        "createScript" => "korvus.trustedTypes.createScript",
        _ => "korvus.trustedTypes.createScriptURL",
    }
}

/// Which Trusted type each of the policy's three operations produces.
fn produced_by(member: &str) -> &'static str {
    match member {
        "createHTML" => "TrustedHTML",
        "createScript" => "TrustedScript",
        _ => "TrustedScriptURL",
    }
}

fn cb_policy_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    let k = slot(scope, POLICY_NAME);
    let name = args.this().get_private(scope, k).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let v = str_val(scope, &name);
    rv.set(v);
}

/// `createHTML` / `createScript` / `createScriptURL`, all three from one body: the member's own name
/// says which callback to read and which type to mint, and it rides in `data` beside the prototype.
fn cb_policy_create(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // `this` must be a TrustedTypePolicy, of any realm: its brand (`binding::brand`)
    let on_chain = crate::binding::protos::implements(scope, args.this().into(), "TrustedTypePolicy");
    if !on_chain {
        illegal(scope);
        return;
    }
    if !require_args(scope, &args, 1, "TrustedTypePolicy", &member) {
        return;
    }
    let this = args.this();
    let name = {
        let k = slot(scope, POLICY_NAME);
        this.get_private(scope, k).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
    };
    let k = slot(scope, cb_slot_name(&member));
    let cb = this.get_private(scope, k).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    let Some(cb) = cb else {
        throw_type_error(
            scope,
            &format!(
                "Failed to execute '{member}' on 'TrustedTypePolicy': Policy {name}'s \
                 TrustedTypePolicyOptions did not specify a '{member}' member."
            ),
        );
        return;
    };
    // EVERY argument, and an `undefined` receiver: measured, `createHTML('x', 1, 2)` reaches the
    // callback as three arguments and a sloppy-mode callback's `this` reads as the global.
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let recv: v8::Local<v8::Value> = v8::undefined(scope).into();
    // A callback that throws lets the throw through unchanged (measured: `Error: boom`). Blink's IDL callback
    // types, as an analysis build records them: `createScriptURL`'s is `CreateURLCallback`.
    let label = match member.as_str() {
        "createHTML" => "CreateHTMLCallback",
        "createScript" => "CreateScriptCallback",
        _ => "CreateURLCallback",
    };
    let Some(out) = call_policy_callback(scope, label, cb, recv, &argv) else { return };
    // null and undefined both become "" — not "null"/"undefined". Measured on both browsers.
    let s = if out.is_null_or_undefined() { String::new() } else { out.to_rust_string_lossy(scope) };
    match mint_trusted(scope, produced_by(&member), &s) {
        Some(o) => rv.set(o.into()),
        None => rv.set_undefined(),
    }
}

/// Mount one `TrustedTypePolicy` row.
pub fn install_trusted_policy(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "name" => {
            let g = row_fn(scope, proto, "get name", 0, cb_policy_name);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        "createHTML" | "createScript" | "createScriptURL" => {
            // the data is the member's name (a primitive); the brand check is by interface name
            let n = str_val(scope, member.name);
            let f = build_fn_data(scope, cb_policy_create, member.name, member.length as i32, n);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_trusted_policy: unexpected member {other}"),
    }
}

// ── TrustedTypePolicyFactory ────────────────────────────────────────────────────────────────────

const DEFAULT_POLICY: &str = "korvus.trustedTypes.defaultPolicy";
const EMPTY_PREFIX: &str = "korvus.trustedTypes.empty.";

/// The realm's ONE `emptyHTML` / `emptyScript`, minted on first read and kept on the factory itself
/// so `tt.emptyHTML === tt.emptyHTML` (measured true on both browsers).
fn empty_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    factory: v8::Local<v8::Object>,
    iface: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let k = slot(scope, &format!("{EMPTY_PREFIX}{iface}"));
    if let Some(v) = factory.get_private(scope, k) {
        if !v.is_undefined() {
            return Some(v);
        }
    }
    let o = mint_trusted(scope, iface, "")?;
    factory.set_private(scope, k, o.into());
    Some(o.into())
}

fn cb_empty_html(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    if let Some(v) = empty_value(scope, args.this(), "TrustedHTML") {
        rv.set(v);
    }
}

fn cb_empty_script(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    if let Some(v) = empty_value(scope, args.this(), "TrustedScript") {
        rv.set(v);
    }
}

/// `defaultPolicy` — `null` until a policy called `default` is created, then that policy, by identity.
fn cb_default_policy(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    let k = slot(scope, DEFAULT_POLICY);
    match args.this().get_private(scope, k).filter(|v| !v.is_undefined()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `createPolicy(name, options?)` — a real policy, which is the whole point of the lane. A duplicate
/// name is not an error here: measured, a second `createPolicy('x')` succeeds on a page with no CSP.
fn cb_create_policy(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    if !require_args(scope, &args, 1, "TrustedTypePolicyFactory", "createPolicy") {
        return;
    }
    let Some(name) = crate::binding::class::idl_string(scope, args.get(0), "Failed to execute 'createPolicy' on 'TrustedTypePolicyFactory': ") else { return };
    // `TrustedTypePolicyOptions`, converted with the name before any step (Chrome 150):
    // `createHTML`, `createParserOptions`, `createScript`, `createScriptURL`, each read once and checked as a callback
    // ("The given value is not a function." for any other value) -- korvus read three of them, after its CSP checks,
    // and took a non-callable member for an absent one
    let Ok(options) = crate::install::ctor_dicts::convert_dict(scope, args.get(1), "TrustedTypePolicyOptions", "Failed to execute 'createPolicy' on 'TrustedTypePolicyFactory': ") else { return };
    // Two checks before a policy exists (Blink `TrustedTypePolicyFactory::createPolicy`): the realm's CSP
    // `trusted-types` names (every enforced policy's; a duplicate needs 'allow-duplicates'), then a second
    // `default` -- refused with no CSP at all. Measured on Chrome 150 (`trusted-types a default`):
    // `createPolicy('b')` is "Policy "b" disallowed.", a second `a` or `default` "Policy with name "a"
    // already exists."; korvus made all three.
    let names = policy_names(scope, args.this());
    let key = str_val(scope, &name);
    let is_duplicate = names.has_own_property(scope, key.try_into().unwrap()).unwrap_or(false);
    let verdict = {
        let realm = args.this().get_creation_context(scope).unwrap_or_else(|| scope.get_current_context());
        let id = crate::core::html::browsing_context::ctx_of_context(scope, realm);
        crate::core::html::browsing_context::ctx_csp(scope, id).allows_policy_name(&name, is_duplicate)
    };
    let verdict = match verdict {
        Ok(()) if name == "default" && is_duplicate => Err(true),
        v => v,
    };
    if let Err(duplicate) = verdict {
        let why = if duplicate { format!("Policy with name \"{name}\" already exists.") } else { format!("Policy \"{name}\" disallowed.") };
        crate::binding::protos::throw_type_error(scope, &format!("Failed to execute 'createPolicy' on 'TrustedTypePolicyFactory': {why}"));
        return;
    }
    let t = v8::Boolean::new(scope, true);
    names.set(scope, key, t.into());
    let options = Some(options);
    let Some(proto) = crate::binding::protos::shell_proto(scope, "TrustedTypePolicy") else { return };
    let policy = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, policy);
    let k = slot(scope, POLICY_NAME);
    let nv = str_val(scope, &name);
    policy.set_private(scope, k, nv);
    // The three `TrustedTypePolicyOptions` members, read off the options object here and kept in the
    // policy's own private slots. A member that is not callable is treated as ABSENT, so calling it
    // gives the device's "did not specify a 'createHTML' member" rather than a different error --
    // which is the one corner of this row nobody measured, and it is named rather than assumed.
    if let Some(o) = options {
        for member in ["createHTML", "createScript", "createScriptURL"] {
            let key = str_val(scope, member);
            let Some(v) = o.get(scope, key) else { continue };
            if !v.is_function() {
                continue;
            }
            let k = slot(scope, cb_slot_name(member));
            policy.set_private(scope, k, v);
        }
    }
    // `createPolicy('default', …)` is what makes `defaultPolicy` non-null, and it is that policy by
    // identity (measured `tt.defaultPolicy === p`).
    if name == "default" {
        let k = slot(scope, DEFAULT_POLICY);
        args.this().set_private(scope, k, policy.into());
    }
    rv.set(policy.into());
}

/// The names a factory has made policies under (Blink's `policy_map_`), as a null-prototype object in a
/// private slot of the factory.
fn policy_names<'s>(scope: &mut v8::PinScope<'s, '_>, factory: v8::Local<v8::Object>) -> v8::Local<'s, v8::Object> {
    let k = slot(scope, "korvus.trustedTypes.policyNames");
    if let Some(o) = factory.get_private(scope, k).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return o;
    }
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    let o = v8::Object::with_prototype_and_properties(scope, null, &[], &[]);
    factory.set_private(scope, k, o.into());
    o
}

/// The optional namespace arguments of `getAttributeType` / `getPropertyType`: `undefined` and `null`
/// both read as absent, anything else as the string.
fn opt_string(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> Option<String> {
    let v = args.get(i);
    if v.is_null_or_undefined() {
        return None;
    }
    Some(v.to_rust_string_lossy(scope))
}

fn cb_get_attribute_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    if !require_args(scope, &args, 2, "TrustedTypePolicyFactory", "getAttributeType") {
        return;
    }
    let tag = args.get(0).to_rust_string_lossy(scope);
    let attr = args.get(1).to_rust_string_lossy(scope);
    let el_ns = opt_string(scope, &args, 2);
    let attr_ns = opt_string(scope, &args, 3);
    match super::attribute_type(&tag, &attr, el_ns.as_deref(), attr_ns.as_deref()) {
        Some(t) => {
            let v = str_val(scope, t);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}

fn cb_get_property_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    if !require_args(scope, &args, 2, "TrustedTypePolicyFactory", "getPropertyType") {
        return;
    }
    let tag = args.get(0).to_rust_string_lossy(scope);
    let prop = args.get(1).to_rust_string_lossy(scope);
    let el_ns = opt_string(scope, &args, 2);
    match super::property_type(&tag, &prop, el_ns.as_deref()) {
        Some(t) => {
            let v = str_val(scope, t);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}

/// A `{k: "Type", …}` object, in the order given.
fn type_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    entries: impl IntoIterator<Item = (String, String)>,
) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    for (k, t) in entries {
        let key = str_val(scope, &k);
        let val = str_val(scope, &t);
        o.set(scope, key, val);
    }
    o
}

/// `getTypeMapping(namespace?)` — a FRESH plain object each call (measured:
/// `getTypeMapping() !== getTypeMapping()`), and `null` for any namespace STRING, the XHTML one
/// included. Built from the measured file, so it is the same table the two lookups above read.
fn cb_get_type_mapping(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        illegal(scope);
        return;
    }
    let ns = opt_string(scope, &args, 0);
    if !super::type_mapping_applies(ns.as_deref()) {
        rv.set_null();
        return;
    }
    let out = v8::Object::new(scope);
    let put = |scope: &mut v8::PinScope, on: v8::Local<v8::Object>, key: &str, val: v8::Local<v8::Value>| {
        let k = str_val(scope, key);
        on.set(scope, k, val);
    };
    let pairs = |v: Option<&serde_json::Value>| -> Vec<(String, String)> {
        v.and_then(|v| v.as_object())
            .map(|o| o.iter().filter_map(|(k, t)| t.as_str().map(|t| (k.clone(), t.to_string()))).collect())
            .unwrap_or_default()
    };
    for (element, spec) in super::type_mapping_elements() {
        let entry = v8::Object::new(scope);
        let attrs = type_object(scope, pairs(spec.get("attributes")));
        put(scope, entry, "attributes", attrs.into());
        let props = type_object(scope, pairs(spec.get("properties")));
        put(scope, entry, "properties", props.into());
        put(scope, out, element, entry.into());
    }
    let star = v8::Object::new(scope);
    let t = super::event_handler_type().to_string();
    let attrs = type_object(scope, super::event_handler_attributes().iter().map(|n| (n.clone(), t.clone())));
    put(scope, star, "attributes", attrs.into());
    let props = type_object(
        scope,
        super::any_element_properties()
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
            .collect::<Vec<_>>(),
    );
    put(scope, star, "properties", props.into());
    put(scope, out, "*", star.into());
    rv.set(out.into());
}

/// `isHTML` / `isScript` / `isScriptURL` — the member name in `data[1]` says which kind to ask for.
fn cb_is_kind(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // `this` must be a TrustedTypePolicyFactory, of any realm: its brand (`binding::brand`)
    let on_chain = crate::binding::protos::implements(scope, args.this().into(), "TrustedTypePolicyFactory");
    if !on_chain {
        illegal(scope);
        return;
    }
    if !require_args(scope, &args, 1, "TrustedTypePolicyFactory", &member) {
        return;
    }
    let want = match member.as_str() {
        "isHTML" => "TrustedHTML",
        "isScript" => "TrustedScript",
        _ => "TrustedScriptURL",
    };
    let got = kind_of(scope, args.get(0));
    rv.set_bool(got.as_deref() == Some(want));
}

/// Mount one `TrustedTypePolicyFactory` row.
pub fn install_trusted_factory(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "emptyHTML" => {
            let g = row_fn(scope, proto, "get emptyHTML", 0, cb_empty_html);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        "emptyScript" => {
            let g = row_fn(scope, proto, "get emptyScript", 0, cb_empty_script);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        "defaultPolicy" => {
            let g = row_fn(scope, proto, "get defaultPolicy", 0, cb_default_policy);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        "isHTML" | "isScript" | "isScriptURL" => {
            // the data is the member's name (a primitive); the brand check is by interface name
            let n = str_val(scope, member.name);
            let f = build_fn_data(scope, cb_is_kind, member.name, member.length as i32, n);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        other => {
            let f = match other {
                "createPolicy" => row_fn(scope, proto, other, member.length as i32, cb_create_policy),
                "getAttributeType" => row_fn(scope, proto, other, member.length as i32, cb_get_attribute_type),
                "getPropertyType" => row_fn(scope, proto, other, member.length as i32, cb_get_property_type),
                "getTypeMapping" => row_fn(scope, proto, other, member.length as i32, cb_get_type_mapping),
                _ => {
                    debug_assert!(false, "install_trusted_factory: unexpected member {other}");
                    return;
                }
            };
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
    }
}

// ── the installs ────────────────────────────────────────────────────────────────────────────────

/// The five interfaces from their tables. Realm-agnostic: every `worker.surface.json` in the tree
/// lists all five with the window's member lists, and `create_interface` publishes each only if the
/// ACTIVE capture records it (`Publish::IfCaptured`, through `exposure::gate`).
fn install_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = installer::take_ledger(scope);
    for def in [&TRUSTEDTYPEPOLICYFACTORY, &TRUSTEDTYPEPOLICY, &TRUSTEDHTML, &TRUSTEDSCRIPT, &TRUSTEDSCRIPTURL] {
        let built =
            installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] trusted-types install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
}

/// The five interfaces and the `trustedTypes` singleton, in the WINDOW.
pub fn install_window_trusted_types<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::exposure::{gate, Exposure, Publishes};
    use crate::core::html::window_globals::{define_window_singleton, mint_shell, Mint, WindowSetter};

    install_interfaces(scope, global);
    if !gate(scope, Publishes::Global, "trustedTypes", Exposure::Unconditional) {
        return;
    }
    let Some(o) = mint_shell(scope, "TrustedTypePolicyFactory") else { return };
    define_window_singleton(scope, global, "trustedTypes", o.into(), WindowSetter::None, true, Mint::Shell("TrustedTypePolicyFactory"));
}

/// The SAME five tables in the WORKER realm (invariant I8), called from
/// `core/korvus-runtime/src/worker.rs`. No singleton: no `worker.surface.json` lists `trustedTypes`
/// as an own global, because in a worker it is a `WorkerGlobalScope` prototype attribute — and a row
/// this lane does not own. Without this call the worker would lose all five interfaces outright, the
/// way it would have lost `URLSearchParams`, because the tables take them out of the registry in BOTH
/// realms.
pub fn install_worker_trusted_types<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    install_interfaces(scope, global);
}

// ── the five tables ─────────────────────────────────────────────────────────────────────────────
//
// Verbatim from the four bundles' `surface.json` AND `worker.surface.json`, which record the same
// member lists in the same order with the same lengths, all enumerable and configurable. None of the
// five is constructible: `new TrustedHTML()` is `Illegal constructor` on all four, which
// `Model::Interface` plus a `stub` ctor row spells.

webapi::interface! {
    "TrustedTypePolicyFactory" [interface, both] {
    ctor   "TrustedTypePolicyFactory"          len 0                      = stub,
    attr   "emptyHTML"        : Any readonly                              = install_trusted_factory,
    attr   "emptyScript"      : Any readonly                              = install_trusted_factory,
    attr   "defaultPolicy"    : Any readonly nullable                     = install_trusted_factory,
    method "createPolicy"     : Any       len 1 required 1                = install_trusted_factory,
    method "getAttributeType" : DomString len 2 required 2 nullable       = install_trusted_factory,
    method "getPropertyType"  : DomString len 2 required 2 nullable       = install_trusted_factory,
    method "getTypeMapping"   : Any       len 0 required 0                = install_trusted_factory,
    method "isHTML"           : Boolean   len 1 required 1                = install_trusted_factory,
    method "isScript"         : Boolean   len 1 required 1                = install_trusted_factory,
    method "isScriptURL"      : Boolean   len 1 required 1                = install_trusted_factory
    }
}

webapi::interface! {
    "TrustedTypePolicy" [interface, both] {
    ctor   "TrustedTypePolicy"                 len 0                      = stub,
    attr   "name"             : DomString readonly                        = install_trusted_policy,
    method "createHTML"       : Any len 1 required 1                      = install_trusted_policy,
    method "createScript"     : Any len 1 required 1                      = install_trusted_policy,
    method "createScriptURL"  : Any len 1 required 1                      = install_trusted_policy
    }
}

webapi::interface! {
    "TrustedHTML" [interface, both] {
    ctor   "TrustedHTML"                       len 0                      = stub,
    method "toJSON"           : DomString len 0 required 0                = install_trusted_value,
    method "toString"         : Any       len 0 required 0                = install_trusted_value
    }
}

webapi::interface! {
    "TrustedScript" [interface, both] {
    ctor   "TrustedScript"                     len 0                      = stub,
    method "toJSON"           : DomString len 0 required 0                = install_trusted_value,
    method "toString"         : Any       len 0 required 0                = install_trusted_value
    }
}

webapi::interface! {
    "TrustedScriptURL" [interface, both] {
    ctor   "TrustedScriptURL"                  len 0                      = stub,
    method "toJSON"           : UsvString len 0 required 0                = install_trusted_value,
    method "toString"         : Any       len 0 required 0                = install_trusted_value
    }
}
