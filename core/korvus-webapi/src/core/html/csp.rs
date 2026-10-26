//! `core/html/csp.rs` -- the document's Content-Security-Policy, as far as the BROWSER-INITIATED
//! ICON downloads need it.
//!
//! THE DEFECT. P10c gave the engine the favicon rule of `core/html/link_icons.rs`: after `load`, the
//! candidate list is read, and a document that declares no icon contributes `/favicon.ico`. Measured
//! over the 504-row replay corpus (oracle 2026-09-17 vs korvus 2026-09-18), korvus asked for an icon
//! on thirteen rows where the oracle asked for none. Two of them were regressions at L0.94 --
//! `man.openbsd.org` and `matter-js.github.io/matter-js/demo/` -- and both documents are delivered
//! with a restrictive CSP:
//!
//! ```text
//! man.openbsd.org   Content-Security-Policy: default-src 'none'; style-src 'self' 'unsafe-inline'
//! matter-js (a 404) Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline';
//!                                            img-src data:; connect-src 'self'
//! ```
//!
//! An icon download is a fetch, and CSP's `img-src` governs it: Chrome makes NO request at all. The
//! brief's other hypothesis -- that Chrome skips the default icon for an error document, matter-js
//! being a 404 -- is DISPROVED below (`doc 404, no CSP` still asks).
//!
//! # Measured on stock Chrome 150.0.7871.46, headless, over a loopback server's ACCESS LOG
//!
//! One fresh profile and one server per row; the fixture's document declares no icon unless the row
//! says so, and `/favicon.ico` in the log means Chrome asked for the icon.
//!
//! ```text
//! no CSP                                              /favicon.ico
//! doc 404, no CSP                                     /favicon.ico   -- a 404 document is not the rule
//! default-src 'none'                                  nothing
//! default-src 'none' + <link rel=icon>                nothing        -- the DECLARED icon too
//! img-src 'none'                                      nothing
//! img-src 'none' + <link rel=icon>                    nothing
//! img-src                (empty source list)          nothing
//! IMG-SRC 'NONE'                                      nothing        -- names and keywords fold case
//! img-src 'none' 'self'                               /favicon.ico   -- 'none' alone in a list blocks
//! img-src 'self'  ·  default-src 'self'  ·  img-src * /favicon.ico
//! default-src *  ·  script-src 'none'  ·  upgrade-insecure-requests  /favicon.ico
//! img-src http:   (on an http document)               /favicon.ico   -- scheme-source
//! img-src https:  (on an http document)               nothing
//! img-src data:   (the default icon is http)          nothing
//! img-src 127.0.0.1:*                                 /favicon.ico   -- host-source, any port
//! img-src 127.0.0.1                                   nothing        -- no port means the DEFAULT port
//! img-src *.nope.test                                 nothing
//! img-src 'self', a CROSS-ORIGIN <link rel=icon>      nothing        -- and no /favicon.ico fallback
//! "img-src *, img-src 'none'"                         nothing        -- a comma is a second policy
//! img-src 'none'; img-src *                           nothing        -- a repeated directive: first wins
//! <meta http-equiv=CSP> in <head>                     nothing        -- a meta counts
//! <meta http-equiv=CSP> in <body>                     /favicon.ico   -- only <head>'s children count
//! Content-Security-Policy-Report-Only: default-src 'none'   /favicon.ico
//! no CSP header at all · an empty one · "foo bar"     /favicon.ico
//! default-src 'self'; img-src 'none', with a manifest  the manifest, but neither icon
//! default-src 'self'; manifest-src 'none'              the favicon, but NOT the manifest
//! ```
//!
//! # Scope, stated rather than implied
//!
//! This file began as ONE question -- may the icon loader fetch this URL (`link_icons.rs`). It now also
//! answers `script-src` for script elements (below), and holds each document's CSP LIST --
//! the page's, and every frame's, inherited where no response delivered one -- for Trusted Types at the
//! DOM sinks and for turning strings into code ([`CspList`]). A page's own `<img>`
//! and `<link rel=manifest>` are still fetched whatever the policy says (the last row above is a real,
//! measured gap: `manifest-src` is a different directive on a different loader). Nothing here reports a
//! violation, because nothing in the corpus reads violation reports and a `report-uri` request Chrome
//! makes and this engine does not would be a new diff rather than a closed one.
//!
//! # Rulings
//!
//! * A header value is split on `,` into separate policies, per CSP3's "the header is a
//!   comma-separated list of policies", and on `;` into directives. A comma inside a `report-uri`
//!   value would therefore be read as a policy boundary and produce a policy with a nonsense
//!   directive name -- which allows, since a policy with no `img-src` and no `default-src` allows.
//!   Cost if wrong: none for the icon question.
//! * `'self'` also matches the https form of an http document's origin (CSP3's upgrade allowance).
//!   Cost if wrong: one icon fetched on an http page whose icon is https and whose policy is `'self'`.
//! * A host-source with no port requires the URL to be on its scheme's DEFAULT port, measured above.
//!   The spec's extra allowance for an http document naming an https URL on 443 is not implemented.

use deno_core::v8;
use deno_core::OpState;
use url::Url;

use crate::binding::HostDom;

/// The policies a document was DELIVERED with, one entry per policy: the enforced ones, and the
/// `Content-Security-Policy-Report-Only` ones, which enforce nothing and still make Trusted Types run
/// a default policy (see [`CspList`]).
#[derive(Default)]
pub(crate) struct DocCsp {
    header: Vec<Policy>,
    report_only: Vec<Policy>,
}

/// One policy: its directives in delivery order, the FIRST occurrence of each name only (measured:
/// `img-src 'none'; img-src *` blocks), each with its value as written (Blink's error messages quote a
/// directive as `<name> <raw value>`), and whether it was delivered report-only.
#[derive(Clone)]
pub(crate) struct Policy {
    directives: Vec<(String, Vec<String>)>,
    raw: Vec<String>,
    report_only: bool,
}

impl Policy {
    /// The source list that governs a fetch of `directive`, falling back to `default-src`. `None` =
    /// this policy says nothing about it, which allows.
    fn source_list(&self, directive: &str) -> Option<&[String]> {
        let find = |name: &str| self.directives.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_slice());
        find(directive).or_else(|| find("default-src"))
    }

    /// The `sandbox` directive's tokens, when this policy has one (CSP3 "sandbox"). A `<meta>` or report-only
    /// policy's sandbox is ignored, so the caller asks the DELIVERED, ENFORCED policies only
    /// (`browsing_context::ctx_delivered_enforced`).
    pub(crate) fn sandbox(&self) -> Option<&[String]> {
        self.find("sandbox")
    }

    /// Directive `name`'s source list, when the policy has it.
    fn find(&self, name: &str) -> Option<&[String]> {
        self.directives.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_slice())
    }

    /// Directive `name` as Blink's messages quote it: the name, a space, its value as written.
    /// The policy as a header carries it: its directives, each with its value as written.
    fn text(&self) -> String {
        self.directives
            .iter()
            .enumerate()
            .map(|(i, (n, _))| match self.raw.get(i).map(|r| r.trim()).filter(|r| !r.is_empty()) {
                Some(r) => format!("{n} {r}"),
                None => n.clone(),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn raw_directive(&self, name: &str) -> String {
        let raw = self.directives.iter().position(|(n, _)| n == name).and_then(|i| self.raw.get(i)).map(String::as_str).unwrap_or("");
        format!("{name} {raw}")
    }
}

/// `Content-Security-Policy: <policy>, <policy>` -> the policies it carries.
fn parse_header(value: &str) -> Vec<Policy> {
    value.split(',').filter_map(|p| parse_policy(p, false)).collect()
}

/// One policy string. `None` for a policy that declares nothing.
fn parse_policy(text: &str, report_only: bool) -> Option<Policy> {
    let mut directives: Vec<(String, Vec<String>)> = Vec::new();
    let mut raw = Vec::new();
    for part in text.split(';') {
        let part = part.trim_matches(|c: char| c.is_ascii_whitespace());
        let mut tokens = part.split_ascii_whitespace();
        let Some(name) = tokens.next() else { continue };
        let name = name.to_ascii_lowercase();
        if directives.iter().any(|(n, _)| *n == name) {
            continue;
        }
        raw.push(part[part.find(|c: char| c.is_ascii_whitespace()).unwrap_or(part.len())..].trim_start_matches(|c: char| c.is_ascii_whitespace()).to_string());
        directives.push((name, tokens.map(str::to_string).collect()));
    }
    (!directives.is_empty()).then_some(Policy { directives, raw, report_only })
}

/// The policies of a response's `Content-Security-Policy` headers, then its `-Report-Only` ones.
fn header_policies(headers: &[(String, String)]) -> (Vec<Policy>, Vec<Policy>) {
    let (mut enforce, mut report) = (Vec::new(), Vec::new());
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("content-security-policy") {
            enforce.extend(parse_header(value));
        } else if name.eq_ignore_ascii_case("content-security-policy-report-only") {
            report.extend(value.split(',').filter_map(|p| parse_policy(p, true)));
        }
    }
    (enforce, report)
}

/// A new document: the policies its response delivered. Called once per navigation, so a navigation
/// whose response carried none clears the last one's.
pub(crate) fn set_document_csp(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    let (header, report_only) = header_policies(headers);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    // HTML: an element connected to a document whose CSP list holds a HEADER-delivered policy has its nonce hidden
    // -- the parser's elements now, every later one as it is inserted.
    let hide = !header.is_empty();
    if let Some(h) = st.try_borrow_mut::<HostDom>() {
        let root = h.0.root();
        h.0.set_nonce_hiding(root, hide);
    }
    st.put(DocCsp { header, report_only });
}

// ── a document's CSP list, for Trusted Types and string compilation ──────────
//
// a bot-challenge frame decides two of its verdict's values on how its NESTED documents enforce
// the policies they hold, and korvus answered both as a document with no policy at all (measured on stock
// Chrome 150.0.7871.46, `tests/fixtures/trusted-types-enforced`):
//
// * an `about:blank` frame it creates inherits the challenge document's CSP -- HTML's policy container is
//   cloned into a document that no response delivered (`about:blank`, `about:srcdoc`, `blob:`, `data:`) --
//   so its `require-trusted-types-for 'script'` holds there, and that frame's `div.innerHTML = '<b>t</b>'`
//   and `script.src = 'data:,0'` go through the frame's default policy (`createHTML` / `createScriptURL`,
//   with "Element innerHTML" / "HTMLScriptElement src"). korvus asked no policy at any DOM sink.
// * a `srcdoc` frame's own `<meta http-equiv=Content-Security-Policy content="script-src 'nonce-…'">` joins
//   the policies it inherited, and that one has no `'unsafe-eval'`: `frame.contentWindow.eval('this')` asks
//   the frame's default policy (Trusted Types first) and then throws an EvalError. korvus read only the
//   PAGE document's metas, and asked the policy of the realm that was running instead of the one whose
//   `eval` it was.
//
// So every document now has a list: the page's from its response and `<head>` metas; a frame's from its
// response, or inherited from its parent when no response delivered it, plus its own `<head>` metas.

/// A document's policies, enforced and report-only, in the order they were delivered.
#[derive(Clone, Default)]
pub(crate) struct CspList(Vec<Policy>);

/// What a document's policies say about Trusted Types at a sink: `required` when any policy, enforced or
/// report-only, says `require-trusted-types-for 'script'` (Blink `RequireTrustedTypes`: the default policy
/// is asked), `enforced` when an enforced one does (a refusal throws; report-only lets the value through).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrustedTypesRequirement {
    pub(crate) required: bool,
    pub(crate) enforced: bool,
}

impl CspList {
    /// The policies a response's headers deliver.
    pub(crate) fn from_headers(headers: &[(String, String)]) -> CspList {
        let (mut enforce, report) = header_policies(headers);
        enforce.extend(report);
        CspList(enforce)
    }

    /// The `<head>` metas of the document whose root is `root`, appended (a meta policy is enforced).
    pub(crate) fn add_metas(&mut self, dom: &korvus_dom::Dom, root: u32) {
        self.0.extend(head_metas(dom, root, None));
    }

    fn enforced(&self) -> impl Iterator<Item = &Policy> {
        self.0.iter().filter(|p| !p.report_only)
    }

    /// The enforced policies, copied.
    pub(crate) fn enforced_policies(&self) -> Vec<Policy> {
        self.enforced().cloned().collect()
    }

    /// Every policy as the header that would deliver it (`content-security-policy`, or `-report-only`), one policy
    /// a header -- what a worker's realm, on its own thread, is given of its creator's list.
    pub(crate) fn as_headers(&self) -> Vec<(String, String)> {
        self.0
            .iter()
            .map(|p| {
                let name = if p.report_only { "content-security-policy-report-only" } else { "content-security-policy" };
                (name.to_string(), p.text())
            })
            .collect()
    }

    pub(crate) fn trusted_types(&self) -> TrustedTypesRequirement {
        policies_trusted_types(self.0.iter())
    }

    /// What the enforced policies say about turning a string into code.
    pub(crate) fn eval_policy(&self) -> EvalPolicy {
        eval_policy_of_iter(self.enforced())
    }

    /// The EvalError's message when a string may not become code (Blink `EvalDisabledErrorMessage`, which
    /// the context carries): the first enforced policy whose `script-src` (or `default-src`) leaves out
    /// `'unsafe-eval'` names that directive; else the first that requires Trusted Types says so.
    pub(crate) fn eval_disabled_message(&self) -> Option<String> {
        for p in self.enforced() {
            let operative = ["script-src", "default-src"].into_iter().find(|n| p.find(n).is_some());
            if let Some(name) = operative {
                if !p.find(name).is_some_and(|v| has_keyword(v, "'unsafe-eval'")) {
                    return Some(format!(
                        "Evaluating a string as JavaScript violates the following Content Security Policy directive because 'unsafe-eval' is not an allowed source of script: {}\".\n",
                        p.raw_directive(name)
                    ));
                }
            }
            if p.find("require-trusted-types-for").is_some_and(|v| has_keyword(v, "'script'")) {
                return Some("Evaluating a string as JavaScript violates this document's Trusted Type assignment requirements.".to_string());
            }
        }
        None
    }

    /// May `createPolicy(name)` make a policy (Blink `AllowTrustedTypePolicy`, every policy's
    /// `trusted-types`)? `Err(true)`: refused as a duplicate name, `Err(false)`: refused as a name the
    /// policies do not list. A report-only policy refuses nothing.
    pub(crate) fn allows_policy_name(&self, name: &str, is_duplicate: bool) -> Result<(), bool> {
        let mut out = Ok(());
        for p in self.enforced() {
            let Some(list) = p.find("trusted-types") else { continue };
            let none = list.len() == 1 && list[0] == "'none'";
            let any = !none && list.iter().any(|t| t == "*");
            let dups = !none && list.iter().any(|t| t.eq_ignore_ascii_case("'allow-duplicates'"));
            let named = !none && list.iter().any(|t| t == name && is_policy_name(t));
            let verdict = if is_duplicate && !dups {
                Err(true)
            } else if is_duplicate && name == "default" {
                Err(true)
            } else if !is_policy_name(name) || !(any || named) {
                Err(false)
            } else {
                Ok(())
            };
            if out.is_ok() {
                out = verdict;
            }
        }
        out
    }
}

impl std::ops::BitOrAssign for TrustedTypesRequirement {
    fn bitor_assign(&mut self, o: Self) {
        self.required |= o.required;
        self.enforced |= o.enforced;
    }
}

/// The Trusted Types requirement of policies, read in place (every DOM sink asks, so nothing is cloned).
fn policies_trusted_types<'a>(policies: impl Iterator<Item = &'a Policy>) -> TrustedTypesRequirement {
    let mut r = TrustedTypesRequirement::default();
    for p in policies {
        if p.find("require-trusted-types-for").is_some_and(|v| has_keyword(v, "'script'")) {
            r.required = true;
            r.enforced |= !p.report_only;
        }
    }
    r
}

/// The Trusted Types requirement of the `<head>` metas of the document at `root`: only a meta whose content
/// names `require-trusted-types-for` is parsed.
pub(crate) fn metas_trusted_types(dom: &korvus_dom::Dom, root: u32) -> TrustedTypesRequirement {
    let is = |e: u32, t: &str| dom.tag_name(e).is_some_and(|n| n.eq_ignore_ascii_case(t));
    let Some(html) = dom.child_elements(root).into_iter().find(|e| is(*e, "html")) else { return Default::default() };
    let Some(head) = dom.child_elements(html).into_iter().find(|e| is(*e, "head")) else { return Default::default() };
    let mut r = TrustedTypesRequirement::default();
    for meta in dom.child_elements(head) {
        if !is(meta, "meta") || !dom.get_attribute(meta, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("content-security-policy")) {
            continue;
        }
        if let Some(content) = dom.get_attribute(meta, "content").filter(|c| c.to_ascii_lowercase().contains("require-trusted-types-for")) {
            r |= policies_trusted_types(parse_header(content).iter());
        }
    }
    r
}

/// The page document's Trusted Types requirement: its response's policies and its `<head>` metas.
pub(crate) fn page_trusted_types(st: &OpState) -> TrustedTypesRequirement {
    let mut r = st.try_borrow::<DocCsp>().map(|d| policies_trusted_types(d.header.iter().chain(d.report_only.iter()))).unwrap_or_default();
    if let Some(h) = st.try_borrow::<HostDom>() {
        r |= metas_trusted_types(&h.0, h.0.root());
    }
    r
}

/// `tt-policy-name`: ASCII letters, digits and `-#=_/@.%`.
fn is_policy_name(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-#=_/@.%".contains(&b))
}

/// The page document's list: its response's policies and its `<head>` metas.
pub(crate) fn page_csp(st: &OpState) -> CspList {
    let mut list = CspList::default();
    if let Some(d) = st.try_borrow::<DocCsp>() {
        list.0.extend(d.header.iter().cloned());
        list.0.extend(d.report_only.iter().cloned());
    }
    list.0.extend(meta_policies(st));
    list
}

/// `<meta http-equiv="Content-Security-Policy" content="...">` -- the CHILDREN OF `<head>` only
/// (measured: a meta in `<body>` is ignored).
fn meta_policies(st: &OpState) -> Vec<Policy> {
    let mut out = Vec::new();
    let Some(host) = st.try_borrow::<HostDom>() else { return out };
    let dom = &host.0;
    let html = dom.child_elements(dom.root()).into_iter().find(|e| dom.tag_name(*e).is_some_and(|t| t.eq_ignore_ascii_case("html")));
    let Some(head) = html.and_then(|h| dom.child_elements(h).into_iter().find(|e| dom.tag_name(*e).is_some_and(|t| t.eq_ignore_ascii_case("head")))) else {
        return out;
    };
    for meta in dom.child_elements(head) {
        if !dom.tag_name(meta).is_some_and(|t| t.eq_ignore_ascii_case("meta")) {
            continue;
        }
        if !dom.get_attribute(meta, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("content-security-policy")) {
            continue;
        }
        if let Some(content) = dom.get_attribute(meta, "content") {
            out.extend(parse_header(content));
        }
    }
    out
}

/// May the icon loader fetch `url`? Every enforced policy -- the response's and the head's metas --
/// must allow it, under `img-src` falling back to `default-src`.
pub(crate) fn allows_image(scope: &mut v8::PinScope, url: &str) -> bool {
    if spec::devtools::bypass_csp() {
        return true;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Ok(target) = Url::parse(url) else { return true };
    let doc_url = st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default();
    let doc = Url::parse(&doc_url).ok();
    let metas = meta_policies(&st);
    let empty: Vec<Policy> = Vec::new();
    let delivered = st.try_borrow::<DocCsp>().map_or(&empty, |c| &c.header);
    delivered.iter().chain(metas.iter()).all(|p| policy_allows(p, doc.as_ref(), &target))
}

// ── script-src ─────────────────────────────────────────────────────────────────────
//
// A script ELEMENT is governed by `script-src-elem`, then `script-src`, then `default-src`. Measured on
// stock Chrome 150 over `tools/realweb/seed/csp` (three frames, one meta policy each):
//
// ```text
// 'self' 'unsafe-inline' 'sha256-A'   inline A runs; plain inline B, a script-inserted inline and a
//                                     nonce'd one NOT in the list do not; 'self' external runs, a
//                                     cross-origin one does not, and an inserted one fires `error`
// 'nonce-n1' 'strict-dynamic' ...     nonce'd inline and external run; a parser-inserted script
//                                     without the nonce does not (inline or 'self'); a SCRIPT-INSERTED
//                                     script runs, inline or cross-origin (strict-dynamic's trust)
// 'unsafe-inline' alone               every inline runs, script-inserted too; an external does not
// ```
//
// So a nonce or a hash in the list voids 'unsafe-inline' (CSP3), and 'strict-dynamic' voids the URL
// allowlist and trusts whatever a running script inserts. about.gitea.com is the corpus row: its meta
// lists 'unsafe-inline' AND hashes, so Chrome never runs the Hotjar snippet it inserts with
// `innerText`, and korvus ran it and fetched Hotjar.
//
// SCOPE: the element's own document's head metas, and the document's delivered policies -- the page's response's,
// or a frame document's own response's or the ones it inherited (`about:srcdoc`, `about:blank`, `blob:`, `data:`
// hold their creator's: HTML's policy container). A frame's scripts ran under its metas only (Chrome 150, measured
// 2026-10-11: a srcdoc frame of a `script-src 'nonce-abc'` page runs only its nonce'd script, a frame whose response
// says `script-src 'none'` runs nothing; korvus ran all of them). The image check above keeps the page's only.

/// The policies governing a script element, and the URL its `'self'` means.
fn script_policies(scope: &mut v8::PinScope, el: u32, parser_inserted: bool) -> (Vec<Policy>, Option<Url>) {
    let home = crate::core::html::subresource::home_of(scope, el);
    let frame_delivered = match home {
        crate::core::html::subresource::Home::Frame(ctx) => crate::core::html::browsing_context::ctx_delivered_enforced(scope, ctx),
        _ => Vec::new(),
    };
    let doc_url = match home {
        crate::core::html::subresource::Home::Frame(ctx) => crate::core::html::browsing_context::frame_context_url(scope, ctx),
        _ => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default()
        }
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let mut out = Vec::new();
    if let Some(host) = st.try_borrow::<HostDom>() {
        let root = host.0.shadow_including_root(el);
        out.extend(head_metas(&host.0, root, parser_inserted.then_some(el)));
    }
    if home == crate::core::html::subresource::Home::Page {
        if let Some(c) = st.try_borrow::<DocCsp>() {
            out.extend(c.header.iter().cloned());
        }
    }
    out.extend(frame_delivered);
    (out, Url::parse(&doc_url).ok())
}

/// `<meta http-equiv=Content-Security-Policy>` among the `<head>` children of the document at `root`.
///
/// `before`: a PARSER-INSERTED script, which only the metas the parser had already reached govern. A
/// meta policy is enforced from the moment it is parsed, and a document's `<script>`s above it ran
/// before it existed -- every WPT CSP test that loads `testharness.js` ahead of its policy meta relies
/// on that (13 floor files went dark when the whole head applied to every script).
fn head_metas(dom: &korvus_dom::Dom, root: u32, before: Option<u32>) -> Vec<Policy> {
    let mut out = Vec::new();
    let is = |e: u32, t: &str| dom.tag_name(e).is_some_and(|n| n.eq_ignore_ascii_case(t));
    let Some(html) = dom.child_elements(root).into_iter().find(|e| is(*e, "html")) else { return out };
    let Some(head) = dom.child_elements(html).into_iter().find(|e| is(*e, "head")) else { return out };
    for meta in dom.child_elements(head) {
        if before.is_some_and(|el| !precedes(dom, meta, el)) {
            continue;
        }
        if is(meta, "meta")
            && dom.get_attribute(meta, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("content-security-policy"))
        {
            if let Some(content) = dom.get_attribute(meta, "content") {
                out.extend(parse_header(content));
            }
        }
    }
    out
}

/// Whether `a` comes before `b` in tree order (`a` an ancestor of `b` counts as before).
fn precedes(dom: &korvus_dom::Dom, a: u32, b: u32) -> bool {
    let chain = |mut n: u32| {
        let mut v = vec![n];
        while let Some(p) = dom.node(n).parent {
            v.push(p);
            n = p;
        }
        v.reverse();
        v
    };
    let (ca, cb) = (chain(a), chain(b));
    let mut i = 0;
    while i < ca.len() && i < cb.len() && ca[i] == cb[i] {
        i += 1;
    }
    match (ca.get(i), cb.get(i)) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(&x), Some(&y)) => {
            let kids = dom.children(ca[i - 1]);
            kids.iter().position(|&k| k == x) < kids.iter().position(|&k| k == y)
        }
    }
}

impl Policy {
    fn script_list(&self) -> Option<&[String]> {
        let find = |name: &str| self.directives.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_slice());
        find("script-src-elem").or_else(|| find("script-src")).or_else(|| find("default-src"))
    }
}

/// What a realm's policies say about turning a STRING into code (`eval`, `new Function`): the two
/// questions Blink's code-generation check asks (`CodeGenerationCheckCallbackInMainThread`), in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EvalPolicy {
    /// `require-trusted-types-for 'script'`: a string must pass the default policy's `createScript`
    /// unchanged first, and is refused without one.
    pub(crate) trusted_types_for_script: bool,
    /// Some policy's `script-src` (falling back to `default-src`) lists `'trusted-types-eval'`: with
    /// Trusted Types required, `eval` of a string is allowed outright -- no default policy, no
    /// `'unsafe-eval'` check (Blink `ContentSecurityPolicy::AllowTrustedTypesEval`).
    pub(crate) trusted_types_eval: bool,
    /// Every policy's `script-src` (falling back to `default-src`) lists `'unsafe-eval'`, or says nothing.
    pub(crate) eval_allowed: bool,
}

impl Default for EvalPolicy {
    fn default() -> Self {
        EvalPolicy { trusted_types_for_script: false, trusted_types_eval: false, eval_allowed: true }
    }
}

fn eval_policy_of_iter<'a>(policies: impl Iterator<Item = &'a Policy>) -> EvalPolicy {
    let mut p = EvalPolicy::default();
    for policy in policies {
        let find = |name: &str| policy.directives.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_slice());
        if find("require-trusted-types-for").is_some_and(|v| has_keyword(v, "'script'")) {
            p.trusted_types_for_script = true;
        }
        if let Some(list) = find("script-src").or_else(|| find("default-src")) {
            if has_keyword(list, "'trusted-types-eval'") {
                p.trusted_types_eval = true;
            }
            if !has_keyword(list, "'unsafe-eval'") {
                p.eval_allowed = false;
            }
        }
    }
    p
}

fn has_keyword(list: &[String], kw: &str) -> bool {
    list.iter().any(|s| s.eq_ignore_ascii_case(kw))
}

fn nonce_matches(list: &[String], nonce: &str) -> bool {
    !nonce.is_empty()
        && list.iter().any(|s| {
            // `get`, not indexing: a policy is page text, and a multi-byte character there must not
            // split a slice and panic.
            s.len() > 8
                && s.starts_with('\'')
                && s.ends_with('\'')
                && s.get(1..7).is_some_and(|k| k.eq_ignore_ascii_case("nonce-"))
                && s.get(7..s.len() - 1) == Some(nonce)
        })
}

fn is_nonce_or_hash(src: &str) -> bool {
    let l = src.to_ascii_lowercase();
    l.starts_with("'nonce-") || l.starts_with("'sha256-") || l.starts_with("'sha384-") || l.starts_with("'sha512-")
}

/// Does a hash-source in `list` match `text`? The digest is compared in base64, a base64url spelling
/// read as base64 first.
fn hash_matches(list: &[String], text: &str) -> bool {
    use base64::Engine;
    use sha2::Digest;
    let b64 = |d: &[u8]| base64::engine::general_purpose::STANDARD.encode(d);
    let mut digests: [Option<String>; 3] = [None, None, None];
    list.iter().any(|s| {
        let l = s.to_ascii_lowercase();
        let (i, alg_len) = if l.starts_with("'sha256-") { (0, 8) } else if l.starts_with("'sha384-") { (1, 8) } else if l.starts_with("'sha512-") { (2, 8) } else { return false };
        if !s.ends_with('\'') || s.len() <= alg_len + 1 {
            return false;
        }
        // base64url is accepted and read as base64 (WPT scripthash-base64url-converts-to-base64).
        let want: String = s[alg_len..s.len() - 1].chars().map(|c| match c { '-' => '+', '_' => '/', c => c }).collect();
        let got = digests[i].get_or_insert_with(|| match i {
            0 => b64(&sha2::Sha256::digest(text.as_bytes())),
            1 => b64(&sha2::Sha384::digest(text.as_bytes())),
            _ => b64(&sha2::Sha512::digest(text.as_bytes())),
        });
        &want == got
    })
}

/// May the inline script `el` with source `text` run? `parser_inserted` is HTML's flag: false for a
/// script a running script inserted.
pub(crate) fn allows_inline_script(scope: &mut v8::PinScope, el: u32, text: &str, parser_inserted: bool) -> bool {
    // a DevTools client switched the page's policy off (`Page.setBypassCSP`)
    if spec::devtools::bypass_csp() {
        return true;
    }
    let (policies, _) = script_policies(scope, el, parser_inserted);
    if policies.is_empty() {
        return true;
    }
    let nonce = element_nonce(scope, el);
    let refused: Vec<&Policy> = policies
        .iter()
        .filter(|p| {
            let Some(list) = p.script_list() else { return false };
            if nonce_matches(list, &nonce) || hash_matches(list, text) {
                return false;
            }
            if has_keyword(list, "'strict-dynamic'") {
                return parser_inserted;
            }
            !(has_keyword(list, "'unsafe-inline'") && !list.iter().any(|s| is_nonce_or_hash(s)))
        })
        .collect();
    // each refusing policy reports: `inline`
    let violations: Vec<Violation> = refused
        .iter()
        .map(|p| Violation { directive: "script-src-elem", resource: "inline".into(), policy: p.text(), report_only: p.report_only, sample: sample_of(p.script_list(), text) })
        .collect();
    let allowed = violations.is_empty();
    report_script_violations(scope, el, violations);
    allowed
}

/// A violation's `sample`: the first 40 characters of the trimmed source, when the directive asks for samples with
/// `'report-sample'` (Blink `directive.source_list->report_sample`), else nothing.
fn sample_of(list: Option<&[String]>, text: &str) -> String {
    if list.is_some_and(|l| has_keyword(l, "'report-sample'")) {
        text.trim().chars().take(40).collect()
    } else {
        String::new()
    }
}

impl CspList {
    /// The violations turning `text` into code makes (`eval`, `new Function`): one for each policy whose
    /// `script-src` (else `default-src`) lacks `'unsafe-eval'` (Blink `CSPDirectiveListAllowEval`).
    pub(crate) fn eval_violations(&self, text: &str) -> Vec<Violation> {
        self.0
            .iter()
            .filter_map(|p| {
                let list = p.find("script-src").or_else(|| p.find("default-src"))?;
                (!has_keyword(list, "'unsafe-eval'")).then(|| Violation {
                    directive: "script-src",
                    resource: "eval".into(),
                    policy: p.text(),
                    report_only: p.report_only,
                    sample: sample_of(Some(list), text),
                })
            })
            .collect()
    }
}

/// An inline script's violations, reported with its element as their target (Chrome 150, measured).
fn report_script_violations(scope: &mut v8::PinScope, el: u32, violations: Vec<Violation>) {
    if violations.is_empty() {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let element = crate::binding::protos::wrap_cached(scope, &op_state, el);
    report_violations(scope, Some(element), violations, true);
}

/// May the external script `el` fetch and run `url`?
pub(crate) fn allows_script_url(scope: &mut v8::PinScope, el: u32, url: &str, parser_inserted: bool) -> bool {
    if spec::devtools::bypass_csp() {
        return true;
    }
    let violations = script_url_violations(scope, el, url, parser_inserted);
    let allowed = violations.is_empty();
    // a refused URL fires at the document (see `element_request_blocked`)
    report_violations(scope, None, violations, true);
    allowed
}

/// [`allows_script_url`] for a parser script, asked without reporting: the page's fetch pass does not
/// request a script its document's policies refuse -- Chrome 150 never asks for one -- and the parser's own check
/// reports it when it reaches the element.
pub(crate) fn parser_script_url_refused_quietly(scope: &mut v8::PinScope, el: u32, url: &str) -> bool {
    if spec::devtools::bypass_csp() {
        return false;
    }
    !script_url_violations(scope, el, url, true).is_empty()
}

fn script_url_violations(scope: &mut v8::PinScope, el: u32, url: &str, parser_inserted: bool) -> Vec<Violation> {
    let (policies, doc) = script_policies(scope, el, parser_inserted);
    if policies.is_empty() {
        return Vec::new();
    }
    let Ok(target) = Url::parse(url) else { return Vec::new() };
    let nonce = element_nonce(scope, el);
    let integrity = element_attr(scope, el, "integrity");
    policies
        .iter()
        .filter(|p| {
            let Some(list) = p.script_list() else { return false };
            if nonce_matches(list, &nonce) || integrity_listed(list, &integrity) {
                return false;
            }
            if has_keyword(list, "'strict-dynamic'") {
                return parser_inserted;
            }
            !list.iter().any(|src| source_matches(src, doc.as_ref(), &target))
        })
        .map(|p| Violation { directive: "script-src-elem", resource: url.to_string(), policy: p.text(), report_only: p.report_only, sample: String::new() })
        .collect()
}

/// The element's nonce: its `[[CryptographicNonce]]` slot (`korvus_dom::Dom::nonce`), which keeps the value once a
/// header-delivered policy has hidden the content attribute.
fn element_nonce(scope: &mut v8::PinScope, el: u32) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<HostDom>().map(|h| h.0.nonce(el).to_string()).unwrap_or_default()
}

fn element_attr(scope: &mut v8::PinScope, el: u32, name: &str) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(el, name).map(str::to_string)).unwrap_or_default()
}

/// CSP3: an external script whose `integrity` metadata is all listed as hash-sources is allowed (WPT
/// script-src-strict_dynamic_hashes). Each token is `<alg>-<digest>`, options after a `?` ignored.
fn integrity_listed(list: &[String], integrity: &str) -> bool {
    let mut tokens = integrity.split_ascii_whitespace().map(|t| t.split('?').next().unwrap_or(t)).peekable();
    if tokens.peek().is_none() {
        return false;
    }
    tokens.all(|t| {
        let Some((alg, digest)) = t.split_once('-') else { return false };
        if !matches!(alg.to_ascii_lowercase().as_str(), "sha256" | "sha384" | "sha512") {
            return false;
        }
        list.iter().any(|src| {
            src.strip_prefix('\'')
                .and_then(|x| x.strip_suffix('\''))
                .and_then(|x| x.split_once('-'))
                .is_some_and(|(a, d)| a.eq_ignore_ascii_case(alg) && d == digest)
        })
    })
}

fn policy_allows(policy: &Policy, doc: Option<&Url>, url: &Url) -> bool {
    let Some(list) = policy.source_list("img-src") else { return true };
    if list.is_empty() {
        return false;
    }
    if list.len() == 1 && list[0].eq_ignore_ascii_case("'none'") {
        return false;
    }
    list.iter().any(|src| source_matches(src, doc, url))
}

/// CSP3 sec. 6.6.2.6, narrowed to what an icon URL can be.
fn source_matches(src: &str, doc: Option<&Url>, url: &Url) -> bool {
    if src == "*" {
        // `*` is every NETWORK scheme; it deliberately does not cover `data:`/`blob:`.
        return !matches!(url.scheme(), "data" | "blob" | "filesystem");
    }
    if src.starts_with('\'') {
        // 'self' is the only keyword that can match a URL; 'none', 'unsafe-inline', a nonce and a
        // hash never do.
        return src.eq_ignore_ascii_case("'self'") && doc.is_some_and(|d| self_matches(d, url));
    }
    if let Some(scheme) = src.strip_suffix(':') {
        if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return scheme_matches(&scheme.to_ascii_lowercase(), url.scheme());
        }
    }
    host_source_matches(src, doc, url)
}

/// A scheme-source matches its own scheme, and `http`/`ws` also match their secure forms.
fn scheme_matches(expr: &str, actual: &str) -> bool {
    expr == actual || (expr == "http" && actual == "https") || (expr == "ws" && actual == "wss")
}

/// `'self'`: the document's origin, plus CSP3's upgrade allowance (an http document's own host over
/// https).
fn self_matches(doc: &Url, url: &Url) -> bool {
    if doc.host_str() != url.host_str() || doc.host_str().is_none() {
        return false;
    }
    if doc.scheme() == url.scheme() {
        return doc.port_or_known_default() == url.port_or_known_default();
    }
    scheme_matches(doc.scheme(), url.scheme())
}

/// `[scheme "://"] host [":" port] [path]`, with `*.` on the host and `*` for the port.
fn host_source_matches(src: &str, doc: Option<&Url>, url: &Url) -> bool {
    let (scheme_part, rest) = match src.split_once("://") {
        Some((s, r)) => (Some(s.to_ascii_lowercase()), r),
        None => (None, src),
    };
    let expected_scheme = scheme_part.or_else(|| doc.map(|d| d.scheme().to_ascii_lowercase()));
    if !expected_scheme.is_some_and(|s| scheme_matches(&s, url.scheme())) {
        return false;
    }
    // An IPv6 host is bracketed, and its colons are not the port separator.
    let after_host = if let Some(end) = rest.strip_prefix('[').and_then(|_| rest.find(']')) { end + 1 } else { 0 };
    let sep = rest[after_host..].find([':', '/']).map(|i| i + after_host);
    let (host_part, tail) = match sep {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if host_part.is_empty() {
        return false;
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else { return false };
    let host_part = host_part.to_ascii_lowercase();
    let host_ok = if host_part == "*" {
        true
    } else if let Some(suffix) = host_part.strip_prefix("*.") {
        host.len() > suffix.len() + 1 && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
    } else {
        host == host_part
    };
    if !host_ok {
        return false;
    }
    let (port_part, path_part) = match tail.strip_prefix(':') {
        Some(t) => {
            let i = t.find('/').unwrap_or(t.len());
            (Some(&t[..i]), &t[i..])
        }
        None => (None, tail),
    };
    let port_ok = match port_part {
        None => url.port().is_none(),
        Some("*") => true,
        Some(p) => p.parse::<u16>().ok() == url.port_or_known_default(),
    };
    if !port_ok {
        return false;
    }
    path_matches(path_part, url.path())
}

/// CSP3 sec. 6.6.2.8, on the raw paths: a source path ending in `/` is a prefix, anything else is an
/// exact match, and no path at all matches everything.
fn path_matches(expr: &str, path: &str) -> bool {
    if expr.is_empty() || expr == "/" {
        return true;
    }
    if let Some(prefix) = expr.strip_suffix('/') {
        return path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/');
    }
    path == expr
}

// ── every fetch directive, and the violation a refused request reports (a bot-detection script D1) ─────────
//
// korvus asked a document's policies two questions -- may an image load (`img-src`), may a script element run
// (`script-src`) -- and reported nothing. Every other request went out whatever its directive said, and no
// `securitypolicyviolation` event ever fired. Stock Chrome 150, measured on a bot-detection script's block page
// (policies from `<head>` metas): `img-src 'self'` refuses a cross-origin
// image and `connect-src 'self'` a `fetch()` ("TypeError: Failed to fetch"), and each fires a
// `SecurityPolicyViolationEvent` at the document that bubbles to the window. A request is now checked against the
// directive CSP3 names for its destination, and each policy it violates reports (Blink `ContentSecurityPolicy::
// ReportViolation`): the event is a task on the networking task source, fired at the element that made the request
// while it is connected to the policy's document, else at that document. `tests/csp_violations_live.rs` pins each
// destination against Chrome.

/// CSP3 "get the effective directive for request", by the request's destination. `None` for a request no fetch
/// directive governs (a navigation, a report).
pub(crate) fn effective_directive(destination: &str) -> Option<&'static str> {
    Some(match destination {
        // `fetch()`, XHR, a beacon, an EventSource, a WebSocket
        "" => "connect-src",
        // CSP3: a request whose initiator is "prefetch" or "prerender" (`<link rel=prefetch>`) answers to `default-src`
        // alone (WPT content-security-policy/resource-hints)
        "prefetch" | "prerender" => "default-src",
        "manifest" => "manifest-src",
        "object" | "embed" => "object-src",
        "frame" | "iframe" => "frame-src",
        "audio" | "track" | "video" => "media-src",
        "font" => "font-src",
        "image" => "img-src",
        "style" => "style-src-elem",
        "script" | "xslt" | "audioworklet" | "paintworklet" => "script-src-elem",
        "serviceworker" | "sharedworker" | "worker" => "worker-src",
        _ => return None,
    })
}

/// CSP3 "get the effective directive's fallback list": the directives that govern `effective`, most specific first.
fn fallback_list(effective: &str) -> &'static [&'static str] {
    match effective {
        "script-src-elem" => &["script-src-elem", "script-src", "default-src"],
        "style-src-elem" => &["style-src-elem", "style-src", "default-src"],
        "worker-src" => &["worker-src", "child-src", "script-src", "default-src"],
        "frame-src" => &["frame-src", "child-src", "default-src"],
        "connect-src" => &["connect-src", "default-src"],
        "manifest-src" => &["manifest-src", "default-src"],
        "object-src" => &["object-src", "default-src"],
        "media-src" => &["media-src", "default-src"],
        "font-src" => &["font-src", "default-src"],
        "img-src" => &["img-src", "default-src"],
        "default-src" => &["default-src"],
        _ => &[],
    }
}

impl Policy {
    /// The source list of the first directive in `effective`'s fallback list that this policy has.
    fn governing(&self, effective: &str) -> Option<&[String]> {
        fallback_list(effective).iter().find_map(|d| self.find(d))
    }

    /// Whether one of the fetch directives Blink tries for a resource hint allows `target` (a directive the policy
    /// does not have, with no `default-src` behind it, allows everything).
    fn allows_resource_hint(&self, doc: Option<&Url>, target: &Url) -> bool {
        const HINT: &[&[&str]] = &[
            &["child-src", "default-src"],
            &["connect-src", "default-src"],
            &["font-src", "default-src"],
            &["frame-src", "child-src", "default-src"],
            &["img-src", "default-src"],
            &["manifest-src", "default-src"],
            &["media-src", "default-src"],
            &["object-src", "default-src"],
            &["script-src", "default-src"],
            &["script-src-elem", "script-src", "default-src"],
            &["style-src", "default-src"],
            &["style-src-elem", "style-src", "default-src"],
            &["worker-src", "child-src", "script-src", "default-src"],
        ];
        HINT.iter().any(|fallback| match fallback.iter().find_map(|d| self.find(d)) {
            None => true,
            Some(list) => {
                (fallback[0] == "worker-src" && has_keyword(list, "'strict-dynamic'")) || list.iter().any(|src| source_matches(src, doc, target))
            }
        })
    }
}

/// One violation (CSP3 sec. 2.4): what the event and a report carry of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    /// `effectiveDirective`, and `violatedDirective` (Blink sets both to the effective directive's name).
    pub(crate) directive: &'static str,
    /// The resource: the URL a request was refused for, or `inline` / `eval`.
    pub(crate) resource: String,
    /// `originalPolicy`: the policy as it was delivered.
    pub(crate) policy: String,
    pub(crate) report_only: bool,
    /// `sample`: up to 40 characters of the refused source text, `""` for a URL.
    pub(crate) sample: String,
}

impl CspList {
    /// CSP3 "Should request be blocked by Content Security Policy?" for a request to `url` whose destination is
    /// `destination`, made by the document at `doc`: one violation for each policy whose governing directive does
    /// not allow the URL, and `true` when an enforced one is among them.
    pub(crate) fn check_request(&self, doc: Option<&Url>, url: &str, destination: &str) -> (bool, Vec<Violation>) {
        self.check_request_nonced(doc, url, destination, "")
    }

    /// [`check_request`] for a request that carries its element's nonce (a `<link>`'s, Blink's request
    /// `ContentSecurityPolicyNonce`): a list holding that nonce allows it, as CSP3 "does nonce match source list" does
    /// for a style or script request. `<link rel=stylesheet nonce>` under `style-src 'nonce-…'` loads (WPT
    /// style-src-stylesheet-nonce-allowed).
    pub(crate) fn check_request_nonced(&self, doc: Option<&Url>, url: &str, destination: &str, nonce: &str) -> (bool, Vec<Violation>) {
        let Some(effective) = effective_directive(destination) else { return (false, Vec::new()) };
        let nonced = !nonce.is_empty() && matches!(destination, "style" | "script");
        let Ok(target) = Url::parse(url) else { return (false, Vec::new()) };
        // `about:blank` / `about:srcdoc` are never fetched: no directive governs them
        if target.scheme() == "about" {
            return (false, Vec::new());
        }
        let mut blocked = false;
        let mut out = Vec::new();
        for p in &self.0 {
            // CSP3 "Does resource hint request violate policy?" (Blink `AllowResourceHintRequestForPolicy`): an enforced
            // policy allows a prefetch that any of its fetch directives would allow; only `default-src` is reported
            if effective == "default-src" && !p.report_only && p.allows_resource_hint(doc, &target) {
                continue;
            }
            let Some(list) = p.governing(effective) else { continue };
            if list.iter().any(|src| source_matches(src, doc, &target)) || (nonced && nonce_matches(list, nonce)) {
                continue;
            }
            // CSP3 "script directives pre-request check": under 'strict-dynamic' a script-like request no parser made
            // (a worker, a module a script imports) is allowed whatever the list says -- every request this function
            // checks is one a script or the engine made (WPT script-src-strict_dynamic_worker*)
            let script_like = matches!(destination, "script" | "worker" | "sharedworker" | "serviceworker" | "audioworklet" | "paintworklet" | "xslt");
            if script_like && has_keyword(list, "'strict-dynamic'") {
                continue;
            }
            blocked |= !p.report_only;
            out.push(Violation { directive: effective, resource: url.to_string(), policy: p.text(), report_only: p.report_only, sample: String::new() });
        }
        (blocked, out)
    }
}

/// CSP3 "strip URL for use in reports" (Blink `StripURLForUseInReport`): a URL whose scheme is not http(s)/ws(s)
/// reports as its scheme alone; a cross-origin `frame-src` / `object-src` one as its origin; any other without
/// its fragment and credentials.
pub(crate) fn strip_url_for_report(doc: Option<&Url>, url: &str, effective: &str) -> String {
    let Ok(mut u) = Url::parse(url) else { return String::new() };
    if !matches!(u.scheme(), "http" | "https" | "ws" | "wss") {
        return u.scheme().to_string();
    }
    let same_origin = doc.is_some_and(|d| d.origin() == u.origin());
    if !same_origin && matches!(effective, "frame-src" | "object-src" | "fenced-frame-src") {
        return u.origin().ascii_serialization();
    }
    u.set_fragment(None);
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.to_string()
}

/// The URL `url` names, resolved against the running realm's document base.
fn resolve_for_check(scope: &mut v8::PinScope, url: &str) -> Option<String> {
    if let Ok(u) = Url::parse(url) {
        return Some(u.to_string());
    }
    let base = crate::core::dom::base_url(scope);
    Url::parse(&base).ok()?.join(url).ok().map(|u| u.to_string())
}

/// A request the RUNNING realm's document makes -- `fetch()`, XHR, a beacon, an EventSource, a WebSocket, a
/// worker: its violations are reported, and `true` when an enforced policy blocks it.
pub(crate) fn realm_request_blocked(scope: &mut v8::PinScope, url: &str, destination: &str) -> bool {
    if spec::devtools::bypass_csp() {
        return false;
    }
    let ctx = crate::core::html::browsing_context::realm_ctx(scope);
    let list = crate::core::html::browsing_context::ctx_csp(scope, ctx);
    if list.0.is_empty() {
        return false;
    }
    let Some(url) = resolve_for_check(scope, url) else { return false };
    let doc = realm_document_url(scope, ctx);
    let (blocked, violations) = list.check_request(doc.as_ref(), &url, destination);
    report_violations(scope, None, violations, true);
    blocked
}

/// [`element_request_blocked`] asked without reporting: a frame document's preload scanner refuses a URL
/// quietly, and the element's own load reports it.
pub(crate) fn element_request_refused_quietly(scope: &mut v8::PinScope, el: u32, url: &str, destination: &str) -> bool {
    if spec::devtools::bypass_csp() {
        return false;
    }
    let ctx = match crate::core::html::subresource::home_of(scope, el) {
        crate::core::html::subresource::Home::Frame(c) => Some(c),
        crate::core::html::subresource::Home::Page => None,
        crate::core::html::subresource::Home::Nowhere => crate::core::html::browsing_context::realm_ctx(scope),
    };
    let list = crate::core::html::browsing_context::ctx_csp(scope, ctx);
    if list.0.is_empty() {
        return false;
    }
    let doc = realm_document_url(scope, ctx);
    let nonce = element_nonce(scope, el);
    list.check_request_nonced(doc.as_ref(), url, destination, &nonce).0
}

/// An element's own load (`<img>`, `<link>`, `<audio>`, ...): its document's policies decide. The violation fires at
/// the DOCUMENT, not the element -- Blink checks a request where it is fetched, with no element (Chrome 150, measured:
/// a refused stylesheet `<link>` in `<head>` and a refused `<script src>` fire at `#document`; only an inline
/// script's violation fires at its element).
pub(crate) fn element_request_blocked(scope: &mut v8::PinScope, el: u32, url: &str, destination: &str) -> bool {
    let ctx = match crate::core::html::subresource::home_of(scope, el) {
        crate::core::html::subresource::Home::Frame(c) => Some(c),
        crate::core::html::subresource::Home::Page => None,
        // a detached element loads under its node document's policies: the running realm's
        crate::core::html::subresource::Home::Nowhere => crate::core::html::browsing_context::realm_ctx(scope),
    };
    let list = crate::core::html::browsing_context::ctx_csp(scope, ctx);
    if list.0.is_empty() {
        return false;
    }
    let doc = realm_document_url(scope, ctx);
    let nonce = element_nonce(scope, el);
    let (blocked, violations) = list.check_request_nonced(doc.as_ref(), url, destination, &nonce);
    // An image, a media resource and a frame load OUTSIDE the script that asked for them -- an image in its
    // microtask, media in the media element's task, a frame's navigation in the browser -- so their violations
    // carry no source location (Chrome 150, measured: `line 0` and `sourceFile ""` for those, the script's
    // position for a stylesheet, a script or a font, whose checks run in the call).
    let located = !matches!(destination, "image" | "audio" | "video" | "track" | "iframe" | "frame" | "object" | "embed");
    report_violations(scope, None, violations, located);
    blocked
}

/// Whether the PAGE document's policies allow a request for `url` as `destination`, asked without reporting a
/// violation: the speculative scan of written input ([`crate::core::html::subresource::speculate_written`])
/// leaves a refused URL alone, and the element's own load reports it once the element is inserted.
pub(crate) fn page_request_allowed_quietly(scope: &mut v8::PinScope, url: &str, destination: &str) -> bool {
    let list = crate::core::html::browsing_context::ctx_csp(scope, None);
    if list.0.is_empty() {
        return true;
    }
    let doc = realm_document_url(scope, None);
    !list.check_request(doc.as_ref(), url, destination).0
}

/// The URL `'self'` and a report mean for the document in context `ctx` (the page's when `None`).
fn realm_document_url(scope: &mut v8::PinScope, ctx: Option<u32>) -> Option<Url> {
    let s = match ctx {
        Some(c) => crate::core::html::browsing_context::frame_context_url(scope, c),
        None => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default()
        }
    };
    Url::parse(&s).ok()
}

/// CSP3 "report a violation" for each of `violations`, gathered now (the running script's location is the
/// violation's) and fired as a task (Blink `ExecutionContextCSPDelegate::DispatchViolationEvent` posts it on the
/// networking task runner). `element`: the inline script whose violation it is; the event's target while it is
/// connected. `located`: the check ran inside the script that made the request, whose position the violation carries.
pub(crate) fn report_violations(scope: &mut v8::PinScope, element: Option<v8::Local<v8::Object>>, violations: Vec<Violation>, located: bool) {
    use crate::binding::protos::{build_fn_data, get_prop, init_dict, set_prop, str_val};
    if violations.is_empty() {
        return;
    }
    // the realm of the policies' document: the element's own, else the running one
    let ctx = match element.and_then(|e| e.get_creation_context(scope)) {
        Some(c) => c,
        None => scope.get_current_context(),
    };
    let scope = &mut v8::ContextScope::new(scope, ctx);
    let global = ctx.global(scope);
    let Some(doc) = get_prop(scope, global, "document").and_then(|d| v8::Local::<v8::Object>::try_from(d).ok()) else { return };
    let doc_url = crate::binding::protos::pristine_get(scope, doc.into(), "Document.URL").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let doc_parsed = Url::parse(&doc_url).ok();
    let referrer = crate::binding::protos::pristine_get(scope, doc.into(), "Document.referrer").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let status = crate::core::timing::performance::document_status(scope);
    // "If the user agent is currently executing script": the top frame's location, its query cleared
    // (frame 0 only when there IS one: `GetFrame(0)` of an empty trace reads past its end -- a SIGSEGV in
    // `StackFrame::GetLocation` when a policy refused something with no script running)
    let site = v8::StackTrace::current_stack_trace(scope, 1)
        .filter(|st| located && st.get_frame_count() > 0)
        .and_then(|st| st.get_frame(scope, 0))
        .and_then(|f| {
        let line = f.get_line_number();
        let name = f.get_script_name_or_source_url(scope).map(|n| n.to_rust_string_lossy(scope)).unwrap_or_default();
        (line > 0).then_some((name, line, f.get_column()))
    });
    for v in violations {
        let init = init_dict(scope);
        let source_file = site.as_ref().map(|(n, _, _)| {
            let mut u = Url::parse(n).ok();
            if let Some(u) = u.as_mut() {
                u.set_query(None);
            }
            u.map(|u| strip_url_for_report(doc_parsed.as_ref(), u.as_str(), v.directive)).unwrap_or_default()
        });
        let blocked = match v.resource.as_str() {
            "inline" | "eval" | "wasm-eval" => v.resource.clone(),
            url => strip_url_for_report(doc_parsed.as_ref(), url, v.directive),
        };
        let strings = [
            ("documentURI", strip_url_for_report(doc_parsed.as_ref(), &doc_url, "default-src")),
            ("referrer", referrer.clone()),
            ("blockedURI", blocked),
            ("violatedDirective", v.directive.to_string()),
            ("effectiveDirective", v.directive.to_string()),
            ("originalPolicy", v.policy.clone()),
            ("sourceFile", source_file.unwrap_or_default()),
            ("sample", v.sample.clone()),
            ("disposition", if v.report_only { "report" } else { "enforce" }.to_string()),
        ];
        for (k, s) in strings {
            let s = str_val(scope, &s);
            set_prop(scope, init, k, s);
        }
        let (line, col) = site.as_ref().map_or((0, 0), |(_, l, c)| (*l, *c));
        for (k, n) in [("statusCode", status as f64), ("lineNumber", line as f64), ("columnNumber", col as f64)] {
            let n = v8::Number::new(scope, n);
            set_prop(scope, init, k, n.into());
        }
        let t = v8::Boolean::new(scope, true);
        set_prop(scope, init, "bubbles", t.into());
        set_prop(scope, init, "composed", t.into());
        let target: v8::Local<v8::Value> = match element {
            Some(e) => e.into(),
            None => v8::null(scope).into(),
        };
        let data = v8::Array::new_with_elements(scope, &[target, doc.into(), init.into()]);
        let fire = build_fn_data(scope, cb_fire_violation, "", 0, data.into());
        crate::core::intersection_observer::rendering::queue_task(scope, fire, 0.0);
    }
}

/// The violation's event: at the element while it is connected to the policy's document, else at the document.
fn cb_fire_violation(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let (Some(target), Some(doc), Some(init)) = (d.get_index(scope, 0), d.get_index(scope, 1), d.get_index(scope, 2)) else { return };
    let Ok(doc_obj) = v8::Local::<v8::Object>::try_from(doc) else { return };
    let Some(ctx) = doc_obj.get_creation_context(scope) else { return };
    let scope = &mut v8::ContextScope::new(scope, ctx);
    let at = if target.is_object()
        && crate::binding::protos::pristine_get(scope, target, "Node.isConnected").is_some_and(|c| c.is_true())
        && crate::binding::protos::pristine_call(scope, target, "Node.getRootNode", &[]).is_some_and(|r| r.strict_equals(doc))
    {
        target
    } else {
        doc
    };
    let ty = crate::binding::protos::str_val(scope, "securitypolicyviolation");
    let Some(ev) = crate::binding::protos::new_global(scope, "SecurityPolicyViolationEvent", &[ty, init]) else { return };
    // fired by the engine: trusted
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let evv: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, at, "dispatchEvent", &[evv]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allows(csp: &str, doc: &str, url: &str) -> bool {
        let doc = Url::parse(doc).unwrap();
        let target = Url::parse(url).unwrap();
        parse_header(csp).iter().all(|p| policy_allows(p, Some(&doc), &target))
    }

    /// Every row of the module doc's measured table that does not need a DOM.
    #[test]
    fn the_measured_img_src_table() {
        let doc = "http://127.0.0.1:8080/page.html";
        let fav = "http://127.0.0.1:8080/favicon.ico";
        assert!(allows("", doc, fav), "no CSP allows");
        assert!(allows("foo bar", doc, fav), "a policy with no img-src and no default-src allows");
        assert!(!allows("default-src 'none'", doc, fav));
        assert!(!allows("img-src 'none'", doc, fav));
        assert!(!allows("default-src 'self'; img-src", doc, fav), "an empty source list blocks");
        assert!(!allows("IMG-SRC 'NONE'", doc, fav), "the directive name and the keyword fold case");
        assert!(allows("img-src 'none' 'self'", doc, fav), "'none' blocks only when it is the whole list");
        assert!(allows("img-src 'self'", doc, fav));
        assert!(allows("default-src 'self'", doc, fav));
        assert!(allows("img-src *", doc, fav));
        assert!(allows("default-src *", doc, fav));
        assert!(allows("script-src 'none'", doc, fav), "another directive's 'none' is not img-src's");
        assert!(allows("upgrade-insecure-requests", doc, fav));
        assert!(allows("default-src 'none'; img-src http:", doc, fav));
        assert!(!allows("default-src 'none'; img-src https:", doc, fav));
        assert!(!allows("default-src 'none'; img-src data:", doc, fav));
        assert!(allows("default-src 'none'; img-src 127.0.0.1:*", doc, fav));
        assert!(!allows("default-src 'none'; img-src 127.0.0.1", doc, fav), "no port means the scheme's default port");
        assert!(!allows("default-src 'none'; img-src *.nope.test", doc, fav));
        assert!(!allows("img-src *, img-src 'none'", doc, fav), "a comma starts a second policy");
        assert!(!allows("img-src 'none'; img-src *", doc, fav), "a repeated directive: the first wins");
    }

    /// `'self'` is the document's origin -- the row that keeps a cross-origin declared icon from
    /// being fetched, and with it the row that proves there is no `/favicon.ico` fallback.
    #[test]
    fn self_is_the_document_s_origin() {
        let doc = "https://example.test/page";
        assert!(allows("img-src 'self'", doc, "https://example.test/i.png"));
        assert!(!allows("img-src 'self'", doc, "https://cdn.example.test/i.png"));
        assert!(!allows("img-src 'self'", doc, "https://example.test:8443/i.png"));
        // CSP3's upgrade allowance: an http document's own host over https.
        assert!(allows("img-src 'self'", "http://example.test/page", "https://example.test/i.png"));
        assert!(!allows("img-src 'self'", "https://example.test/page", "http://example.test/i.png"));
    }

    /// Host-sources: the wildcard label, the wildcard port, and the path.
    #[test]
    fn host_sources() {
        let doc = "https://example.test/page";
        assert!(allows("img-src *.example.test", doc, "https://cdn.example.test/i.png"));
        assert!(!allows("img-src *.example.test", doc, "https://example.test/i.png"), "*.host does not match the bare host");
        assert!(!allows("img-src *.example.test", doc, "https://notexample.test/i.png"));
        assert!(allows("img-src https://cdn.example.test", doc, "https://cdn.example.test/i.png"));
        assert!(allows("img-src http://cdn.example.test", doc, "https://cdn.example.test/i.png"), "an http:// host-source also matches https (the upgrade allowance)");
        assert!(allows("img-src https://cdn.example.test/assets/", doc, "https://cdn.example.test/assets/i.png"));
        assert!(!allows("img-src https://cdn.example.test/assets/", doc, "https://cdn.example.test/other/i.png"));
        assert!(allows("img-src https://cdn.example.test/exact.png", doc, "https://cdn.example.test/exact.png"));
        assert!(!allows("img-src https://cdn.example.test/exact.png", doc, "https://cdn.example.test/exact.png.x"));
        assert!(allows("img-src https://*", doc, "https://anything.test/i.png"));
    }

    /// `*` covers the network schemes and deliberately not `data:`.
    #[test]
    fn the_star_is_not_every_scheme() {
        let doc = "https://example.test/page";
        assert!(allows("img-src *", doc, "https://cdn.test/i.png"));
        assert!(!allows("img-src *", doc, "data:image/png;base64,iVBORw0KGgo="));
        assert!(allows("img-src data:", doc, "data:image/png;base64,iVBORw0KGgo="));
    }

    /// The two rows of the corpus this unit exists for, as their servers really delivered them.
    #[test]
    fn the_two_corpus_rows() {
        assert!(!allows(
            "default-src 'none'; style-src 'self' 'unsafe-inline'",
            "https://man.openbsd.org/",
            "https://man.openbsd.org/favicon.ico",
        ));
        assert!(!allows(
            "default-src 'none'; style-src 'unsafe-inline'; img-src data:; connect-src 'self'",
            "https://matter-js.github.io/matter-js/demo/",
            "https://matter-js.github.io/favicon.ico",
        ));
    }

    fn list(src: &str) -> Vec<String> {
        src.split_ascii_whitespace().map(str::to_string).collect()
    }

    /// A hash of an inline script's text, in base64 and in base64url (`+` as `-`), and case-folded.
    #[test]
    fn a_hash_source_matches_its_script_in_either_alphabet() {
        let text = "\n        hashScriptRan = true;\n    ";
        assert!(hash_matches(&list("'sha256-yU6Q7nD1TCBB9JvY06iIJ8ONLOPU4g8ml5JCDgXkv+M='"), text));
        assert!(hash_matches(&list("'sha256-yU6Q7nD1TCBB9JvY06iIJ8ONLOPU4g8ml5JCDgXkv-M='"), text));
        assert!(hash_matches(&list("'SHA256-yU6Q7nD1TCBB9JvY06iIJ8ONLOPU4g8ml5JCDgXkv+M='"), text));
        assert!(!hash_matches(&list("'sha256-yU6Q7nD1TCBB9JvY06iIJ8ONLOPU4g8ml5JCDgXkv+M='"), "other"));
    }

    #[test]
    fn a_nonce_matches_exactly_and_a_multibyte_policy_does_not_panic() {
        assert!(nonce_matches(&list("'nonce-abc'"), "abc"));
        assert!(!nonce_matches(&list("'nonce-abc'"), "ab"));
        assert!(!nonce_matches(&list("'nonce-abc'"), ""));
        assert!(!nonce_matches(&list("'nonc\u{e9}-abc'"), "abc"));
    }

    #[test]
    fn integrity_is_allowed_only_when_every_hash_is_listed() {
        let l = list("'strict-dynamic' 'sha256-wIc3KtqOuTFEu6t17sIBuOswgkV406VJvhSk79Gw6U0='");
        assert!(integrity_listed(&l, "sha256-wIc3KtqOuTFEu6t17sIBuOswgkV406VJvhSk79Gw6U0="));
        assert!(integrity_listed(&l, "sha256-wIc3KtqOuTFEu6t17sIBuOswgkV406VJvhSk79Gw6U0=?opt"));
        assert!(!integrity_listed(&l, "sha256-wIc3KtqOuTFEu6t17sIBuOswgkV406VJvhSk79Gw6U0= sha384-xyz"));
        assert!(!integrity_listed(&l, ""));
    }
}
