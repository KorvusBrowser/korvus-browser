//! `core/html/isolation_policy.rs` -- COOP / COEP: the policy a document was DELIVERED with.
//!
//! A browsing context's cross-origin isolation is not a property of its markup. It is carried on the
//! navigation RESPONSE, in `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy`, and until
//! this algorithm existed nothing in the engine read either one: the top document's headers were
//! dropped by the caller (only `Content-Type` survived, to pick a parser) and a child navigation's
//! headers were fetched, handed to JS on the response object, and never looked at.
//!
//! ONE PARSER, TWO CALLERS. The top document's headers arrive from the runtime at every navigation
//! ([`set_document_policy`], called per navigation from Rust -- core/korvus-runtime/src/navigation.rs)
//! in the same `[name, value]` pair shape a subresource response carries, precisely so that the page's
//! own policy and a popup's (`browsing_context::apply_coop`) are computed by the code below rather
//! than by two implementations that can drift. The RESULT is a kernel service
//! ([`spec::IsolationPolicyHandle`], invariant I9: the runtime owns the navigation and provides the
//! service from the response; `crossOriginIsolated` and the popup check read it and never re-parse a
//! header).
//!
//! WHAT IS NOT HERE, stated rather than approximated. COEP is parsed and reported, but nothing is
//! BLOCKED by it: a `require-corp` document still embeds a no-CORP subresource, and a nested
//! navigation that COEP should refuse still completes. COOP is enforced for the one navigation this
//! engine actually performs -- an auxiliary context's own load -- and the browsing-context-group swap
//! a real browser performs when the TOP-LEVEL context navigates has no implementation here, because
//! Korvus has no top-level navigation to hang it on.
//!
//! THE `SharedArrayBuffer` GATE. Chrome exposes `SharedArrayBuffer` only in a cross-origin-isolated
//! document; V8 exposes it unconditionally, which is a tell. [`isolation_gate`] (one call, from
//! `install::live::install_window_bootstrap`, once the shells are finalized) REMOVES the constructor from the global
//! into [`IsolationGate`], and every [`set_document_policy`] puts it back or takes it away again
//! according to the resolved policy -- so a window reporting `crossOriginIsolated === true` with no
//! `SharedArrayBuffer`, a state no browser can be in, cannot arise in either direction.

use deno_core::v8;
use spec::{DeliveredIsolation, IsolationPolicyHandle};

use crate::binding::protos::{get_prop, global_get};

// ---- RFC 8941 sec. 4.2: a structured-field ITEM whose bare item is a Token -----------------------
//
// COOP and COEP are both "an sf-item whose bare item is a token"; every other shape -- a string, a
// byte sequence, a list, a trailing `;` with no parameter after it -- is a PARSE FAILURE, and a parse
// failure is the unsafe-none default. That is not a detail: WPT's `header-parsing-failures` and COEP's
// `header-parsing` are 29 subtests asserting which shapes fail.

const SF_TOKEN_PUNCT: &str = "!#$%&'*+-.^_`|~:/";
fn sf_is_token_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '*'
}
fn sf_is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || SF_TOKEN_PUNCT.contains(c)
}
fn sf_is_key_start(c: char) -> bool {
    c.is_ascii_lowercase() || c == '*'
}
fn sf_is_key_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '*')
}

/// A parameter's bare item. Only its EXTENT matters -- parameters are discarded once the item parses
/// -- so this returns the index just past it, or `None` for a parse failure.
fn sf_skip_bare_item(s: &[char], i: usize) -> Option<usize> {
    let c = *s.get(i)?;
    if c == '-' || c.is_ascii_digit() {
        // integer / decimal
        let mut j = i;
        if s.get(j) == Some(&'-') {
            j += 1;
        }
        let mut digits = 0;
        let mut dot = false;
        while j < s.len() && (s[j].is_ascii_digit() || (s[j] == '.' && !dot)) {
            if s[j] == '.' {
                dot = true;
            } else {
                digits += 1;
            }
            j += 1;
        }
        return if digits > 0 { Some(j) } else { None };
    }
    if c == '"' {
        // string
        let mut j = i + 1;
        while j < s.len() {
            let d = s[j];
            if d == '\\' {
                j += 2;
                continue;
            }
            if d == '"' {
                return Some(j + 1);
            }
            if d < ' ' || d > '~' {
                return None;
            }
            j += 1;
        }
        return None;
    }
    if c == ':' {
        // byte sequence
        return s[i + 1..].iter().position(|&d| d == ':').map(|p| i + 1 + p + 1);
    }
    if c == '?' {
        // boolean
        return match s.get(i + 1) {
            Some('0') | Some('1') => Some(i + 2),
            _ => None,
        };
    }
    if c == '@' {
        // date
        let mut j = i + 1;
        if s.get(j) == Some(&'-') {
            j += 1;
        }
        let mut digits = 0;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
            digits += 1;
        }
        return if digits > 0 { Some(j) } else { None };
    }
    if sf_is_token_start(c) {
        // token
        let mut j = i;
        while j < s.len() && sf_is_token_char(s[j]) {
            j += 1;
        }
        return Some(j);
    }
    None
}

/// The bare token of an sf-item, or `None` for anything else (a parse failure).
pub fn sf_item_token(raw: &str) -> Option<String> {
    // HTTP strips leading and trailing OWS -- SP and HTAB, and nothing else -- from a field value
    // before anything parses it. A vertical tab, form feed or CR therefore STAYS in the string and
    // makes the parse fail, which is exactly what `header-parsing-failures` asserts for each of the
    // three, next to the tab variants it asserts succeed.
    let trimmed = raw.trim_matches(|c| c == ' ' || c == '\t');
    let s: Vec<char> = trimmed.chars().collect();
    let mut i = 0;
    while i < s.len() && s[i] == ' ' {
        i += 1; // sec. 4.2 step 1
    }
    if i >= s.len() || !sf_is_token_start(s[i]) {
        return None;
    }
    let start = i;
    while i < s.len() && sf_is_token_char(s[i]) {
        i += 1;
    }
    let token: String = s[start..i].iter().collect();
    while i < s.len() && s[i] == ';' {
        // sec. 4.2.3, parameters
        i += 1;
        while i < s.len() && s[i] == ' ' {
            i += 1;
        }
        if i >= s.len() || !sf_is_key_start(s[i]) {
            return None;
        }
        while i < s.len() && sf_is_key_char(s[i]) {
            i += 1;
        }
        if s.get(i) == Some(&'=') {
            i = sf_skip_bare_item(&s, i + 1)?;
        }
    }
    while i < s.len() && s[i] == ' ' {
        i += 1; // sec. 4.2 step 4
    }
    if i == s.len() {
        Some(token)
    } else {
        None
    }
}

/// Every value of one field, combined the way HTTP combines a repeated field: joined with ", ". So
/// `Cross-Origin-Opener-Policy: same-origin` sent TWICE becomes `same-origin, same-origin`, which is
/// a list and not an item, fails to parse, and leaves the document unsafe-none. That is the whole of
/// `header-parsing-repeated`'s assertion, and joining is why it comes out right without a special
/// case for it.
pub fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    let mut out: Option<String> = None;
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case(name) {
            continue;
        }
        out = Some(match out {
            None => v.clone(),
            Some(o) => format!("{o}, {v}"),
        });
    }
    out
}

const COOP_VALUES: [&str; 4] = ["unsafe-none", "same-origin", "same-origin-allow-popups", "noopener-allow-popups"];
const COEP_VALUES: [&str; 3] = ["unsafe-none", "require-corp", "credentialless"];

/// A recognised token, or the unsafe-none default. An UNRECOGNISED token is not an error either --
/// `same-site` and `same-origin unsafe-allow-outgoing` are obsolete COOP values that a current
/// browser reads as unsafe-none, which several WPT files test by name.
fn enum_value(raw: Option<&str>, allowed: &[&str]) -> String {
    match raw.and_then(sf_item_token) {
        Some(t) if allowed.contains(&t.as_str()) => t,
        _ => "unsafe-none".to_string(),
    }
}

/// The policy a document delivered with `headers` at `url` HAS, given whether `url` is potentially
/// trustworthy.
///
/// COOP and COEP are ignored outside a secure context. `no-https.html` is a whole WPT file asserting
/// that: a `Cross-Origin-Opener-Policy: same-origin` served over plain http severs no opener and
/// leaves `crossOriginIsolated` false. Gating it here, once, means nothing downstream has to remember.
pub fn policy_for(headers: &[(String, String)], url: &str, secure: bool) -> DeliveredIsolation {
    let origin = url::Url::parse(url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|_| "null".to_string());
    if !secure {
        return DeliveredIsolation::unsafe_none(origin, false);
    }
    DeliveredIsolation {
        coop: enum_value(header_value(headers, "cross-origin-opener-policy").as_deref(), &COOP_VALUES),
        coop_report_only: enum_value(
            header_value(headers, "cross-origin-opener-policy-report-only").as_deref(),
            &COOP_VALUES,
        ),
        coep: enum_value(header_value(headers, "cross-origin-embedder-policy").as_deref(), &COEP_VALUES),
        coep_report_only: enum_value(
            header_value(headers, "cross-origin-embedder-policy-report-only").as_deref(),
            &COEP_VALUES,
        ),
        origin,
        secure: true,
    }
}

/// HTML: "matching COOP values". Step 3's ORIGIN test is the part worth naming -- two
/// same-origin-allow-popups documents on DIFFERENT origins do not match, which is why the
/// cross-origin variants of the WPT files expect a severed opener where the same-origin ones expect a
/// preserved one.
pub fn coop_matches(a: &str, a_origin: &str, b: &str, b_origin: &str) -> bool {
    if a == "unsafe-none" && b == "unsafe-none" {
        return true;
    }
    if a == "unsafe-none" || b == "unsafe-none" {
        return false;
    }
    a == b && a_origin == b_origin && a_origin != "null"
}

/// HTML: "check if COOP values require a browsing context group switch". `is_initial_about_blank` is
/// true exactly when the context being navigated has never left its initial empty document -- the
/// state a freshly created popup is in, and the reason `same-origin-allow-popups` is named that: it
/// lets the popups IT opens keep the link even though its own COOP is not unsafe-none.
pub fn coop_requires_switch(is_initial_about_blank: bool, active: &DeliveredIsolation, response: &DeliveredIsolation) -> bool {
    if coop_matches(&active.coop, &active.origin, &response.coop, &response.origin) {
        return false;
    }
    if is_initial_about_blank && active.coop == "same-origin-allow-popups" && response.coop == "unsafe-none" {
        return false;
    }
    true
}

// ---- the gate and the service ------------------------------------------------------------------

/// The `SharedArrayBuffer` constructor, held off the global while the document is not cross-origin
/// isolated.
#[derive(Default)]
pub struct IsolationGate {
    sab: Option<v8::Global<v8::Value>>,
}

fn kernel_context(scope: &mut v8::PinScope) -> Option<spec::kernel::Context> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<spec::kernel::Context>().cloned()
}

/// `SharedArrayBuffer` is present exactly when the document is cross-origin isolated: put the held
/// constructor back, or take it away again.
pub(crate) fn apply_gate(scope: &mut v8::PinScope, isolated: bool) {
    let gated: Option<v8::Global<v8::Value>> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<IsolationGate>().and_then(|g| g.sab.clone())
    };
    let Some(g) = gated else { return };
    let global = scope.get_current_context().global(scope);
    let Some(k) = v8::String::new(scope, "SharedArrayBuffer") else { return };
    let has = global.has_own_property(scope, k.into()).unwrap_or(false);
    if isolated && !has {
        let v = v8::Local::new(scope, &g);
        // `{ writable: true, enumerable: false, configurable: true }` -- the descriptor V8 gave it.
        global.define_own_property(scope, k.into(), v, v8::PropertyAttribute::DONT_ENUM);
    } else if !isolated && has {
        global.delete(scope, k.into());
    }
}

/// The bootstrap's one call (was `generated_shells.js`'s, through `isolation_gate`): take the
/// constructor off the (non-isolated) window and keep it for [`apply_gate`]. Idempotent -- a second
/// call finds nothing on the global.
pub(crate) fn isolation_gate(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    let Some(k) = v8::String::new(scope, "SharedArrayBuffer") else { return };
    let Some(v) = global.get(scope, k.into()) else { return };
    if v.is_undefined() {
        return;
    }
    let g = v8::Global::new(scope, v);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        // A realm without the gate yet (a worker's bootstrap runs this before its install) gets one,
        // so the constructor is KEPT for an isolated realm and not simply lost.
        if st.try_borrow::<IsolationGate>().is_none() {
            st.put(IsolationGate::default());
        }
        st.borrow_mut::<IsolationGate>().sab = Some(g);
    }
    global.delete(scope, k.into());
}

/// "Is url potentially trustworthy" (Secure Contexts sec. 3.2) -- the ONE definition, the page
/// window's (`isSecureContext`), so `isSecureContext` and "was this document's COOP honoured" cannot
/// disagree.
///
/// A direct Rust call: the definition is `core/html/window.rs`'s Window row body,
/// and this used to reach it through the `KB.__potentiallyTrustworthy` bridge slot that
/// `dom/09f-window.js` published. Same single definition, one less indirection and one less bridge
/// name -- and it no longer silently answers `false` in a realm where the slot is missing.
pub(crate) fn potentially_trustworthy(_scope: &mut v8::PinScope, url: &str) -> bool {
    crate::core::html::window::potentially_trustworthy(url)
}

/// The policy a response delivered with `headers` at `url` has -- the popup navigation's half of
/// "one parser, two callers".
pub(crate) fn resolve_for(scope: &mut v8::PinScope, headers: &[(String, String)], url: &str) -> DeliveredIsolation {
    let secure = potentially_trustworthy(scope, url);
    policy_for(headers, url, secure)
}

fn location_href(scope: &mut v8::PinScope) -> String {
    global_get(scope, "location")
        .and_then(|l| v8::Local::<v8::Object>::try_from(l).ok())
        .and_then(|l| get_prop(scope, l, "href"))
        .filter(|h| h.is_string())
        .map(|h| h.to_rust_string_lossy(scope))
        .unwrap_or_default()
}

/// The TOP document's policy: resolve `headers` at the current location, provide the service on the
/// kernel Context, and apply the `SharedArrayBuffer` gate. The runtime calls this at the start of every
/// navigation (an empty list for a caller that had no response to read, which is the unsafe-none a
/// synthesised document really has).
pub(crate) fn set_document_policy(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    let here = location_href(scope);
    let policy = resolve_for(scope, headers, &here);
    // a remote frame's agent cluster is the top's, its parent says (Chrome 150: a cross-origin frame of an isolated
    // page without `allow="cross-origin-isolated"` reads false, has no `SharedArrayBuffer` and a 100 µs clock;
    // korvus's child engine decided by the frame's own COOP and COEP)
    let frame = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<crate::core::html::remote_frame::FrameEmbedding>().map(|e| e.cluster_isolated)
    };
    // the document's capability: its agent cluster's isolation and the `cross-origin-isolated` feature enabled in
    // it -- by its container (a remote frame's, as its parent decided) and by its own `Permissions-Policy`
    // (Chrome 150: an isolated page sending `cross-origin-isolated=()` reads false, no `SharedArrayBuffer`, a
    // 100 µs clock); the runtime sets the document's permissions policy first
    let isolated = frame.unwrap_or_else(|| policy.is_cross_origin_isolated())
        && crate::core::feature_policy::enabled_in(scope, "cross-origin-isolated", None);
    if let Some(ctx) = kernel_context(scope) {
        ctx.provide(IsolationPolicyHandle(std::sync::Arc::new(policy)));
    }
    apply_gate(scope, isolated);
    // ...and the clock's clamp: Chrome's isolated realms read a 5 µs clock.
    crate::core::timing::performance::set_clock_isolated(scope, isolated);
}

/// The top document's policy as the service holds it. Lazy initialisation covers the isolate that has
/// booted but not yet navigated.
pub(crate) fn doc_policy(scope: &mut v8::PinScope) -> DeliveredIsolation {
    if let Some(h) = kernel_context(scope).and_then(|c| c.get::<IsolationPolicyHandle>()) {
        return h.0.delivered();
    }
    set_document_policy(scope, &[]);
    kernel_context(scope)
        .and_then(|c| c.get::<IsolationPolicyHandle>())
        .map(|h| h.0.delivered())
        .unwrap_or_default()
}

/// Is the agent cluster of this engine's documents cross-origin isolated? The top document's policy -- or, in a
/// remote frame's child engine, what the parent said of the top's.
pub(crate) fn cluster_isolated(scope: &mut v8::PinScope) -> bool {
    let frame = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<crate::core::html::remote_frame::FrameEmbedding>().map(|e| e.cluster_isolated)
    };
    frame.unwrap_or_else(|| doc_policy(scope).is_cross_origin_isolated())
}

/// The cross-origin isolated capability of context `ctx`'s document (`None`: this engine's own document): its agent
/// cluster is isolated AND the `cross-origin-isolated` feature is enabled in it (HTML; Blink's
/// `LocalDOMWindow::CrossOriginIsolatedCapability`). It decides `crossOriginIsolated`, the `SharedArrayBuffer`
/// global and the clock's 5 µs grain.
pub(crate) fn capability(scope: &mut v8::PinScope, ctx: Option<u32>) -> bool {
    cluster_isolated(scope) && crate::core::feature_policy::enabled_in(scope, "cross-origin-isolated", ctx)
}

/// `window.crossOriginIsolated`: the RUNNING realm's document's [`capability`]. A frame its container did not grant
/// the feature (`allow`; its default allowlist is `self`) reads false in Chrome 150; it read its page's true here.
pub(crate) fn cross_origin_isolated(scope: &mut v8::PinScope) -> bool {
    cluster_isolated(scope) && crate::core::feature_policy::enabled_in_current_realm(scope, "cross-origin-isolated")
}

/// A frame document without the [`capability`] has no `SharedArrayBuffer`: its realm keeps the constructor while its
/// agent cluster is isolated (`browsing_context`'s realm build), and this takes it away when the document's container
/// or its own policy left the feature disabled (Chrome 150, 2026-10-11: `allow="cross-origin-isolated 'none'"`, or
/// the frame's own `Permissions-Policy: cross-origin-isolated=()`, reads `typeof SharedArrayBuffer` "undefined").
pub(crate) fn gate_frame_realm(scope: &mut v8::PinScope, id: u32, realm: v8::Local<v8::Context>) {
    if capability(scope, Some(id)) {
        return;
    }
    let scope = &mut v8::ContextScope::new(scope, realm);
    let global = realm.global(scope);
    let Some(k) = v8::String::new(scope, "SharedArrayBuffer") else { return };
    if global.has_own_property(scope, k.into()).unwrap_or(false) {
        global.delete(scope, k.into());
    }
}

/// A JS `[[name, value], ...]` header list (the response object's `headers`, or the runtime's
/// per-navigation array) as Rust pairs. A missing name or value reads as "", as `String(undefined)`
/// did not: the shape the JS parser accepted.
pub(crate) fn headers_from_js(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(arr) = v8::Local::<v8::Array>::try_from(v) else { return out };
    for i in 0..arr.length() {
        let Some(h) = arr.get_index(scope, i) else { continue };
        if h.is_null_or_undefined() {
            continue;
        }
        let Ok(pair) = v8::Local::<v8::Object>::try_from(h) else { continue };
        let k = pair.get_index(scope, 0).filter(|x| !x.is_undefined()).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
        let val = pair.get_index(scope, 1).filter(|x| !x.is_undefined()).map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
        out.push((k, val));
    }
    out
}

// `op_set_doc_policy` IS GONE. It took the isolation-relevant response headers as a JS
// `[[name, value], ...]` array, which is how the per-navigation reset used to reach this file when
// that reset was an eval. It is Rust now: `korvus-runtime/src/navigation.rs` calls
// `webapi::page::set_document_policy` -> [`set_document_policy`] with Rust pairs, so the op had no
// caller and the JS-array decoding it did is not on that path. [`headers_from_js`] itself STAYS --
// `core/html/browsing_context.rs` still reads a response object's `headers` through it.

/// A WORKER realm's cross-origin isolation, decided by its creator when it was spawned.
#[derive(Default, Clone, Copy)]
pub struct WorkerIsolation(pub bool);

/// A worker's install takes `SharedArrayBuffer` off its global and HOLDS it (the gate the window uses), so
/// [`apply_worker_isolation`] can put it back for an isolated worker.
pub(crate) fn hold_worker_sab(scope: &mut v8::PinScope) {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<IsolationGate>().is_none() {
            st.put(IsolationGate::default());
        }
    }
    isolation_gate(scope);
    // Either order: if the creator's isolation was recorded before this install ran, apply it now.
    let isolated = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<WorkerIsolation>().map(|w| w.0).unwrap_or(false)
    };
    if isolated {
        apply_gate(scope, true);
    }
}

/// The runtime's call once a worker realm is built: record whether it is cross-origin isolated and apply the
/// `SharedArrayBuffer` gate to match.
pub fn apply_worker_isolation(scope: &mut v8::PinScope, isolated: bool) {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().put(WorkerIsolation(isolated));
    }
    apply_gate(scope, isolated);
    crate::core::timing::performance::set_clock_isolated(scope, isolated);
}

/// `WorkerGlobalScope.crossOriginIsolated`: what the worker's creator decided (a worker has no document policy).
pub(crate) fn cb_worker_cross_origin_isolated(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let b = op_state.borrow().try_borrow::<WorkerIsolation>().map(|w| w.0).unwrap_or(false);
    rv.set_bool(b);
}

pub(crate) fn cb_cross_origin_isolated(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let b = cross_origin_isolated(scope);
    rv.set_bool(b);
}
// `op_cross_origin_isolated_get` IS GONE TOO. Its doc said "dom/09f-window.js mounts it; the Window
// table takes it in W5.1" -- the table took it, the JS file is deleted, and the op outlived both:
// `core/html/window.rs` and `core/workers/worker_global_scope.rs` each build the accessor from
// [`cb_cross_origin_isolated`] directly, one per realm. This file declares no ops.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_parses_and_every_other_shape_fails() {
        assert_eq!(sf_item_token("same-origin").as_deref(), Some("same-origin"));
        assert_eq!(sf_item_token("  same-origin\t").as_deref(), Some("same-origin"));
        assert_eq!(sf_item_token("same-origin; report-to=\"x\"").as_deref(), Some("same-origin"));
        assert_eq!(sf_item_token("same-origin;report-to=x; a=1.5; b=?1; c=:YWJj:; d=@1234; e").as_deref(), Some("same-origin"));
        assert_eq!(sf_item_token("\"same-origin\""), None, "a string is not a token");
        assert_eq!(sf_item_token("same-origin, same-origin"), None, "a list is not an item");
        assert_eq!(sf_item_token("same-origin;"), None, "a trailing `;` with no parameter");
        assert_eq!(sf_item_token("same-origin; Report=1"), None, "a parameter key is lowercase");
        assert_eq!(sf_item_token("\x0bsame-origin"), None, "VT is not OWS");
        assert_eq!(sf_item_token("same-origin\r"), None, "CR is not OWS");
        assert_eq!(sf_item_token(""), None);
        assert_eq!(sf_item_token("1same"), None);
    }

    #[test]
    fn a_repeated_field_joins_into_a_list_and_fails() {
        let h = vec![
            ("Cross-Origin-Opener-Policy".to_string(), "same-origin".to_string()),
            ("cross-origin-opener-policy".to_string(), "same-origin".to_string()),
        ];
        assert_eq!(header_value(&h, "cross-origin-opener-policy").as_deref(), Some("same-origin, same-origin"));
        assert_eq!(policy_for(&h, "https://a.test/", true).coop, "unsafe-none");
    }

    #[test]
    fn the_policy_is_secure_context_gated_and_unknown_tokens_are_unsafe_none() {
        let h = vec![
            ("cross-origin-opener-policy".to_string(), "same-origin".to_string()),
            ("cross-origin-embedder-policy".to_string(), "require-corp".to_string()),
        ];
        let secure = policy_for(&h, "https://a.test:8443/x", true);
        assert_eq!((secure.coop.as_str(), secure.coep.as_str(), secure.origin.as_str(), secure.secure), ("same-origin", "require-corp", "https://a.test:8443", true));
        assert!(secure.is_cross_origin_isolated());
        let insecure = policy_for(&h, "http://a.test/x", false);
        assert_eq!((insecure.coop.as_str(), insecure.coep.as_str(), insecure.secure), ("unsafe-none", "unsafe-none", false));
        assert!(!insecure.is_cross_origin_isolated());
        let obsolete = vec![("cross-origin-opener-policy".to_string(), "same-site".to_string())];
        assert_eq!(policy_for(&obsolete, "https://a.test/", true).coop, "unsafe-none");
        assert_eq!(policy_for(&[], "about:blank", true).origin, "null");
    }

    #[test]
    fn the_group_switch_rule() {
        let mk = |coop: &str, origin: &str| DeliveredIsolation { coop: coop.to_string(), origin: origin.to_string(), secure: true, ..Default::default() };
        let a = mk("unsafe-none", "https://a.test");
        assert!(!coop_requires_switch(true, &a, &mk("unsafe-none", "https://b.test")));
        assert!(coop_requires_switch(true, &a, &mk("same-origin", "https://a.test")));
        let sa = mk("same-origin", "https://a.test");
        assert!(!coop_requires_switch(false, &sa, &mk("same-origin", "https://a.test")));
        assert!(coop_requires_switch(false, &sa, &mk("same-origin", "https://b.test")), "the origin test");
        let allow = mk("same-origin-allow-popups", "https://a.test");
        assert!(!coop_requires_switch(true, &allow, &mk("unsafe-none", "https://b.test")), "a popup's first navigation keeps the link");
        assert!(coop_requires_switch(false, &allow, &mk("unsafe-none", "https://b.test")));
    }
}
