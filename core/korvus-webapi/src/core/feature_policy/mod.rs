//! `core/feature_policy/` — the document's permissions policy, and the `FeaturePolicy` object that
//! reports it (L22 finding E9).
//!
//! # The defect
//!
//! `document.featurePolicy` answered `null`. It was a `stub` row on the `Document` table, and a
//! `stub` row whose IDL type is `Object` answers `null` by the D2 rule — shape-correct and empty,
//! never a lie. Every real browser on this box answers a `FeaturePolicy` object there, and one very
//! widely deployed script reads it: `gtag`'s attribution-eligibility test is
//! `document.featurePolicy.allowedFeatures().includes("attribution-reporting")`, so with `null` the
//! call throws inside a `try`, the test is false, and **every GA4 hit loses its `are=1`
//! parameter**. L22 measured that on 28 of the 504 replay rows — the single most repeated named
//! parameter difference in the resources bucket.
//!
//! # What was measured, and on what
//!
//! Stock `/usr/bin/google-chrome` 150.0.7871.46, `/usr/bin/brave-browser` 151.1.93.138,
//! `/usr/bin/microsoft-edge` 151.0.4129.93 and `/usr/bin/opera` 135.0.5973.133, each
//! `--headless=new` with the parity oracle's own flag list, against a loopback fixture that posted
//! its answers back to the server that served it (2026-09-18).
//!
//! **Chrome 152 renamed the interface `PermissionsPolicy`** and keeps `FeaturePolicy` as a legacy window alias
//! for the same object, so its name and `Symbol.toStringTag` are `"PermissionsPolicy"`; the rest of this note
//! (the 2026-09-18 measurements) holds for the renamed object.
//!
//! **The interface object is identical in all four.** `FeaturePolicy` is a non-constructible
//! interface (`new` and a plain call both throw), `length` 0, prototype members in the order
//! `allowedFeatures`, `allowsFeature`, `features`, `getAllowlistForFeature`, `constructor`, lengths
//! 0/1/0/1, `Symbol.toStringTag` `"FeaturePolicy"`, and a prototype-receiver call is
//! `TypeError: Illegal invocation`. `document.featurePolicy` is an `enumerable`, `configurable`
//! getter-only accessor on `Document.prototype` that returns the SAME object every read.
//! `document.permissionsPolicy` is `undefined` in all four — it is not added here, and
//! `core/html/html_iframe_element.rs`'s header records the same absence for the element.
//!
//! **The default allowlists are identical in all four.** For every feature the four browsers share,
//! `getAllowlistForFeature(f)` on a plain top-level document answers either `["*"]` or
//! `[the document's origin]`, and which of the two it is never differs between the browsers. That
//! is what makes [`REGISTRY`] below a family-structural table rather than one browser's identity:
//! its two columns are the same fact in Chrome 150, Brave 151, Edge 151 and Opera 135.
//!
//! **The order is one order.** Each browser's `features()` is a SUBSEQUENCE of Chrome's (verified by
//! filtering each list to the shared names and comparing element by element); Brave additionally
//! interleaves three of its own (`cardano`, `ethereum`, `solana`).
//!
//! # The one thing that DOES differ, and is therefore not settled here
//!
//! MEMBERSHIP. `features()` has 81 entries in Chrome 150, 68 in Brave 151 (16 of Chrome's absent,
//! three of its own present), 80 in Edge 151 (no `language-model`) and 80 in Opera 135 (no
//! `on-device-speech-recognition`). Which features a build ships is a property of the BUILD, so the
//! honest source for it is the persona bundle — and no bundle carries it, because no capture ever
//! recorded it. Adding it means a new bundle artifact, which is `matches_capture = false` (a
//! different session from `[capture]`), which needs a `known_incoherence`, which moves
//! `plugins/korvus-persona/src/bundle.rs`'s `every_foreign_artifact_is_accounted_for` ratchet — a
//! ratchet whose own comment says it may only go DOWN. A unit does not move a ratchet. So the set
//! below is Chrome 152's, stated as measured, and the four-bundle delta is reported to the
//! orchestrator as a ruling rather than guessed at here. Under the `chrome-152-*` bundle it is exact:
//! Chrome 152.0.7977.82's `features()` is Chrome 150's 81 in the same order without
//! `attribution-reporting` (the Attribution Reporting API's feature, gone in 152 -- so `gtag`'s test above is
//! false in Chrome 152 too), 80, measured 2026-10-16.
//!
//! # The header grammar, as Chrome 150 actually parses it
//!
//! Every row below was measured on the same fixture, one launch per row, by sending the header and
//! reading `getAllowlistForFeature` back (2026-09-18):
//!
//! ```text
//! Permissions-Policy: geolocation                       geolocation -> []      a name with no "=" is ()
//! Permissions-Policy: camera=self                       camera -> [self]       a bare token
//! Permissions-Policy: camera="https://example.com"      camera -> [that]       a bare string
//! Permissions-Policy: camera=*                          camera -> ["*"]
//! Permissions-Policy: camera=(none)                     camera -> []           an unknown token drops
//! Permissions-Policy: camera=(self *)                   camera -> ["*"]        "*" anywhere wins
//! Permissions-Policy: camera=(*)                        camera -> ["*"]
//! Permissions-Policy: camera=("https://example.com/")   camera -> [https://example.com]  origin-serialised
//! Permissions-Policy: geolocation=(), geolocation=*     geolocation -> ["*"]   the LAST wins
//! two Permissions-Policy headers, "()" then "*"         geolocation -> ["*"]   the LAST wins
//! Permissions-Policy: korvus-nope=(), geolocation=()    geolocation -> []      an unknown feature is skipped
//! Permissions-Policy: GEOLOCATION=()                    geolocation -> [self]  names are case-SENSITIVE
//! Permissions-Policy: this is not a policy              no effect
//! Permissions-Policy: (empty)                           no effect
//! Feature-Policy: geolocation 'none'                    geolocation -> []      the legacy header counts too
//! ```
//!
//! # What this does NOT do, said out loud
//!
//! Nothing here ENFORCES a policy. `camera=()` removes `camera` from `allowedFeatures()` and that is
//! all it does; `navigator.mediaDevices` is unchanged, because this engine has no camera to gate.
//! The object reports the policy the response declared, which is exactly the question every caller
//! measured on the corpus asks it, and claiming enforcement that does not exist would be the lie
//! this file was written to remove rather than move.

use deno_core::v8;

pub(crate) mod feature_policy;

/// The default allowlist a policy-controlled feature has when nothing declares one.
///
/// Two values because the four browsers only ever answer two things: `["*"]` or `[the document's
/// own origin]`. There is no third shape in the 80 rows below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Default {
    /// `*` — every origin, so a cross-origin frame inherits it.
    All,
    /// `self` — the document's own origin only.
    SelfOrigin,
}

/// Every policy-controlled feature this UA reports, in the order it reports them, with the default
/// allowlist each one has.
///
/// ORDER IS PART OF THE ANSWER: `features()` returns this sequence and `allowedFeatures()` returns
/// the subsequence of it the policy permits, so a page that compares two lists index by index sees
/// what a browser shows. See the module header for what the order and the defaults were measured
/// against, and for why the MEMBERSHIP of this table is a ruling that is still open.
pub(crate) const REGISTRY: &[(&str, Default)] = &[
    ("geolocation", Default::SelfOrigin),
    ("ch-ua-full-version-list", Default::SelfOrigin),
    ("cross-origin-isolated", Default::SelfOrigin),
    ("screen-wake-lock", Default::SelfOrigin),
    ("on-device-speech-recognition", Default::SelfOrigin),
    ("translator", Default::SelfOrigin),
    ("publickey-credentials-get", Default::SelfOrigin),
    ("shared-storage-select-url", Default::All),
    ("ch-ua-arch", Default::SelfOrigin),
    ("compute-pressure", Default::SelfOrigin),
    ("ch-prefers-reduced-transparency", Default::SelfOrigin),
    ("deferred-fetch", Default::SelfOrigin),
    ("usb", Default::SelfOrigin),
    ("ch-save-data", Default::All),
    ("publickey-credentials-create", Default::SelfOrigin),
    ("shared-storage", Default::All),
    ("deferred-fetch-minimal", Default::All),
    ("run-ad-auction", Default::All),
    ("ch-downlink", Default::SelfOrigin),
    ("ch-ua-form-factors", Default::SelfOrigin),
    ("otp-credentials", Default::SelfOrigin),
    ("payment", Default::SelfOrigin),
    ("ch-ua", Default::All),
    ("ch-ua-model", Default::SelfOrigin),
    ("ch-ect", Default::SelfOrigin),
    ("autoplay", Default::SelfOrigin),
    ("camera", Default::SelfOrigin),
    ("language-detector", Default::SelfOrigin),
    ("private-state-token-issuance", Default::All),
    ("digital-credentials-get", Default::SelfOrigin),
    ("accelerometer", Default::SelfOrigin),
    ("ch-ua-platform-version", Default::SelfOrigin),
    ("idle-detection", Default::SelfOrigin),
    ("language-model", Default::SelfOrigin),
    ("private-aggregation", Default::All),
    ("interest-cohort", Default::All),
    ("ch-viewport-height", Default::SelfOrigin),
    ("captured-surface-control", Default::SelfOrigin),
    ("local-fonts", Default::SelfOrigin),
    ("ch-ua-platform", Default::All),
    ("midi", Default::SelfOrigin),
    ("ch-ua-full-version", Default::SelfOrigin),
    ("xr-spatial-tracking", Default::SelfOrigin),
    ("clipboard-read", Default::SelfOrigin),
    ("gamepad", Default::All),
    ("display-capture", Default::SelfOrigin),
    ("keyboard-map", Default::SelfOrigin),
    ("join-ad-interest-group", Default::All),
    ("aria-notify", Default::All),
    ("local-network", Default::SelfOrigin),
    ("ch-ua-high-entropy-values", Default::All),
    ("ch-width", Default::SelfOrigin),
    ("ch-prefers-reduced-motion", Default::SelfOrigin),
    ("browsing-topics", Default::All),
    ("encrypted-media", Default::SelfOrigin),
    ("local-network-access", Default::SelfOrigin),
    ("gyroscope", Default::SelfOrigin),
    ("serial", Default::SelfOrigin),
    ("ch-rtt", Default::SelfOrigin),
    ("ch-ua-mobile", Default::All),
    ("window-management", Default::SelfOrigin),
    ("unload", Default::All),
    ("ch-dpr", Default::SelfOrigin),
    ("ch-prefers-color-scheme", Default::SelfOrigin),
    ("ch-ua-wow64", Default::SelfOrigin),
    ("fullscreen", Default::SelfOrigin),
    ("identity-credentials-get", Default::SelfOrigin),
    ("private-state-token-redemption", Default::All),
    ("hid", Default::SelfOrigin),
    ("summarizer", Default::SelfOrigin),
    ("ch-ua-bitness", Default::SelfOrigin),
    ("storage-access", Default::All),
    ("sync-xhr", Default::All),
    ("ch-device-memory", Default::SelfOrigin),
    ("ch-viewport-width", Default::SelfOrigin),
    ("picture-in-picture", Default::All),
    ("loopback-network", Default::SelfOrigin),
    ("magnetometer", Default::SelfOrigin),
    ("clipboard-write", Default::SelfOrigin),
    ("microphone", Default::SelfOrigin),
];

/// The default allowlist of `name`, or `None` when this UA has no such feature.
pub(crate) fn default_of(name: &str) -> Option<Default> {
    REGISTRY.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// One feature's resolved allowlist: the strings `getAllowlistForFeature` hands back.
///
/// `Any` serialises as the one-element list `["*"]`, which is what every browser answers for a
/// feature whose allowlist is `*` — the string is the allowlist, not a wildcard the caller expands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Allowlist {
    Any,
    Origins(Vec<String>),
}

impl Allowlist {
    fn from_default(d: Default, self_origin: &str) -> Allowlist {
        match d {
            Default::All => Allowlist::Any,
            Default::SelfOrigin => Allowlist::Origins(vec![self_origin.to_string()]),
        }
    }

    /// Does this allowlist admit `origin`? `Any` admits everything, including an opaque origin.
    pub(crate) fn admits(&self, origin: &str) -> bool {
        match self {
            Allowlist::Any => true,
            Allowlist::Origins(v) => v.iter().any(|o| o == origin),
        }
    }

    pub(crate) fn serialise(&self) -> Vec<String> {
        match self {
            Allowlist::Any => vec!["*".to_string()],
            Allowlist::Origins(v) => v.clone(),
        }
    }
}

/// The declarations a document's response headers made, in the order they were parsed, with the
/// LAST declaration of a feature winning (measured: both a repeated key inside one header and a
/// second header line behave that way).
///
/// Lives in `OpState` and is replaced whole on every navigation, exactly like
/// `core/html/csp.rs::DocCsp` — a document's policy is a property of the RESPONSE, so it arrives
/// with the response or not at all.
#[derive(Default)]
pub(crate) struct DocPermissionsPolicy {
    decls: Vec<(String, Allowlist)>,
}

impl DocPermissionsPolicy {
    fn declared(&self, feature: &str) -> Option<&Allowlist> {
        self.decls.iter().rev().find(|(n, _)| n == feature).map(|(_, a)| a)
    }
}

/// An origin token as a permissions policy spells it, serialised the way the browser reports it
/// back: `"https://example.com/"` is reported as `https://example.com`.
///
/// Anything that is not an absolute URL with a tuple origin is dropped, which is the measured
/// behaviour of an unrecognised token (`camera=(none)` answers `[]`).
pub(crate) fn origin_token(tok: &str) -> Option<String> {
    let u = url::Url::parse(tok).ok()?;
    let o = u.origin();
    o.is_tuple().then(|| o.ascii_serialization())
}

/// One structured-header inner list, bare token or bare string, as a permissions-policy allowlist.
///
/// `self` is the document's own origin; `src` is the frame's source origin (only meaningful in a
/// container policy, where the caller passes it); `*` anywhere in the list makes the whole list
/// `Any`. Every other token must parse as an origin or it is dropped.
fn parse_allowlist(raw: &str, self_origin: &str, src_origin: Option<&str>) -> Allowlist {
    let body = raw.trim();
    let inner = match (body.strip_prefix('('), body.strip_suffix(')')) {
        (Some(_), Some(_)) => &body[1..body.len() - 1],
        _ => body,
    };
    let mut origins: Vec<String> = Vec::new();
    for tok in inner.split_whitespace() {
        let tok = tok.trim_matches(|c| c == ',' || c == ';');
        if tok.is_empty() {
            continue;
        }
        if tok == "*" {
            return Allowlist::Any;
        }
        // The legacy `Feature-Policy` header quotes its keywords (`'self'`, `'src'`, `'none'`);
        // `Permissions-Policy` spells the same two bare. Both are accepted here because both
        // headers are, and the keyword set is the same one.
        let bare = tok.trim_matches('\'');
        let resolved = match bare {
            "self" => Some(self_origin.to_string()),
            "src" => src_origin.map(|s| s.to_string()),
            "none" => None,
            _ => origin_token(tok.trim_matches('"')),
        };
        if let Some(o) = resolved {
            if !origins.contains(&o) {
                origins.push(o);
            }
        }
    }
    Allowlist::Origins(origins)
}

/// `Permissions-Policy: name=value, name2=value2` — a structured-header dictionary, parsed for the
/// one thing this file reports.
///
/// Split on the commas that are NOT inside a parenthesised inner list, because
/// `camera=(self "https://a"), geolocation=()` has one comma of each kind. A member with no `=` is
/// `()` (measured: `Permissions-Policy: geolocation` empties geolocation's allowlist).
fn parse_permissions_policy(value: &str, self_origin: &str) -> Vec<(String, Allowlist)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut pieces: Vec<&str> = Vec::new();
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                pieces.push(&value[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    pieces.push(&value[start..]);
    for piece in pieces {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let (name, raw) = match piece.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            // A bare member name is a structured-header boolean, which is not an allowlist: Chrome
            // resolves it to the empty list rather than ignoring it.
            None => (piece, "()"),
        };
        // Case-SENSITIVE, measured: `GEOLOCATION=()` changes nothing. A name this UA does not carry
        // is skipped and the rest of the dictionary still applies.
        if default_of(name).is_none() {
            continue;
        }
        out.push((name.to_string(), parse_allowlist(raw, self_origin, None)));
    }
    out
}

/// `Feature-Policy: name 'self' https://a; name2 'none'` — the legacy header, which Chrome 150
/// still honours (measured). Semicolon-separated, the first token of each piece is the feature.
fn parse_feature_policy(value: &str, self_origin: &str) -> Vec<(String, Allowlist)> {
    let mut out = Vec::new();
    for piece in value.split(';') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let Some((name, rest)) = piece.split_once(char::is_whitespace) else { continue };
        if default_of(name).is_none() {
            continue;
        }
        out.push((name.to_string(), parse_allowlist(rest, self_origin, None)));
    }
    out
}

/// The document's own origin, from the URL the response was delivered from — NOT from
/// `globalThis.location`, which a page can overwrite. `about:blank` and anything without a tuple
/// origin answer `"null"`, the serialisation of an opaque origin.
pub(crate) fn document_origin(scope: &mut v8::PinScope) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let url = op_state
        .borrow()
        .try_borrow::<crate::core::dom::document::CookieUrl>()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    origin_token(&url).unwrap_or_else(|| "null".to_string())
}

/// Replace the document's declared permissions policy from the response's delivered policy headers.
///
/// Called once per navigation from `korvus-runtime`'s `load_page_inner`, next to
/// `set_document_csp` and `set_document_policy`, over the same already-filtered header slice.
pub fn set_document_permissions_policy(scope: &mut v8::PinScope, headers: &[(String, String)]) {
    let self_origin = document_origin(scope);
    let mut decls = Vec::new();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("permissions-policy") {
            decls.extend(parse_permissions_policy(value, &self_origin));
        } else if name.eq_ignore_ascii_case("feature-policy") {
            decls.extend(parse_feature_policy(value, &self_origin));
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(DocPermissionsPolicy { decls });
}

/// The allowlist `feature` has in this document, or `None` when this UA has no such feature.
///
/// `container` is an iframe's `allow` attribute when the question is being asked of a CONTAINER
/// policy (`iframe.featurePolicy`), and `src` the frame's source origin, which `'src'` resolves to.
pub(crate) fn allowlist_for(
    scope: &mut v8::PinScope,
    feature: &str,
    container: Option<(&str, &str)>,
) -> Option<Allowlist> {
    let d = default_of(feature)?;
    // A FRAME document's own policy: what it inherited, then what it declared.
    if container.is_none() {
        if let Some(id) = crate::core::html::browsing_context::current_realm_ctx(scope) {
            if !enabled_in(scope, feature, Some(id)) {
                return Some(Allowlist::Origins(Vec::new()));
            }
            let origin = context_origin(scope, Some(id));
            return Some(declared_in(scope, Some(id), feature).unwrap_or_else(|| Allowlist::from_default(d, &origin)));
        }
    }
    if container.is_none() {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if crate::core::html::remote_frame::inherited_disabled(&st, feature) {
            return Some(Allowlist::Origins(Vec::new()));
        }
    }
    let self_origin = document_origin(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let declared = {
        let st = op_state.borrow();
        st.try_borrow::<DocPermissionsPolicy>().and_then(|p| p.declared(feature).cloned())
    };
    let base = declared.unwrap_or_else(|| Allowlist::from_default(d, &self_origin));
    let Some((allow_attr, src_origin)) = container else { return Some(base) };
    // A container's `allow` names the features it re-delegates; everything it does not name keeps
    // the document's own answer (measured: with `allow="camera 'src'"` the frame's policy carries
    // all 80 features with their unchanged defaults).
    for (name, list) in parse_feature_policy_attr(allow_attr, &self_origin, src_origin) {
        if name == feature {
            return Some(list);
        }
    }
    Some(base)
}

// ── the policy a FRAME document inherits ──────────────────────────────────────
//
// Permissions Policy sec. 9.7-9.8, measured on stock Chrome 150 headed over `tools/realweb/seed/refperm`
// (page :A served `Permissions-Policy: camera=(), geolocation=(self), sync-xhr=()`):
//
// ```text
//                            allowedFeatures  camera   geolocation  microphone  sync XHR
// page                       79               denied   prompt       prompt      NetworkError
// same-origin frame          79               denied   prompt       prompt      NetworkError
// cross-origin frame         21               denied   denied       denied      NetworkError
// cross-origin, allow="camera; geolocation"   21  denied  denied   denied      NetworkError
// ```
//
// A feature the parent disabled stays disabled; one the parent's DECLARED policy limits to origins the
// frame is not is disabled whatever `allow` says; `allow` delegates the rest; and an undeclared `'self'`
// feature is off in a cross-origin frame. korvus answered every frame with the PAGE's declarations
// (66 of 81 on a bot-challenge frame, where Chrome reports 26).

/// The origin of the document in browsing context `ctx` (`None` = the page's): a frame's URL origin, an
/// `about:` frame's creator's, `"null"` for an opaque one.
fn context_origin(scope: &mut v8::PinScope, ctx: Option<u32>) -> String {
    let Some(id) = ctx else { return document_origin(scope) };
    let url = crate::core::html::browsing_context::frame_context_url(scope, id);
    if url.is_empty() || url.to_ascii_lowercase().starts_with("about:") {
        let parent = crate::core::html::browsing_context::frame_parent(scope, id).flatten();
        return context_origin(scope, parent);
    }
    origin_token(&url).unwrap_or_else(|| "null".to_string())
}

/// The declared policy of the document in `ctx`: its response's policy headers.
fn declared_in(scope: &mut v8::PinScope, ctx: Option<u32>, feature: &str) -> Option<Allowlist> {
    match ctx {
        None => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<DocPermissionsPolicy>().and_then(|p| p.declared(feature).cloned())
        }
        Some(id) => {
            let delivered = crate::core::html::browsing_context::frame_delivered(scope, id)?;
            if delivered.permissions_headers.is_empty() {
                return None;
            }
            let origin = context_origin(scope, Some(id));
            let mut decls = Vec::new();
            for (name, value) in &delivered.permissions_headers {
                if name == "permissions-policy" {
                    decls.extend(parse_permissions_policy(value, &origin));
                } else {
                    decls.extend(parse_feature_policy(value, &origin));
                }
            }
            DocPermissionsPolicy { decls }.declared(feature).cloned()
        }
    }
}

/// Is `feature` enabled in the document of `ctx`, for that document's own origin? `false` for a feature
/// this UA does not have.
pub(crate) fn enabled_in(scope: &mut v8::PinScope, feature: &str, ctx: Option<u32>) -> bool {
    if default_of(feature).is_none() {
        return false;
    }
    let origin = context_origin(scope, ctx);
    if let Some(id) = ctx {
        if !inherited_enabled(scope, feature, id, &origin) {
            return false;
        }
    } else {
        // A remote frame's document: what it inherited was decided by its parent.
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        if crate::core::html::remote_frame::inherited_disabled(&st, feature) {
            return false;
        }
    }
    declared_in(scope, ctx, feature).map_or(true, |l| l.admits(&origin))
}

/// "Define an inherited policy for feature in container at origin" (sec. 9.7), for frame `id`.
pub(crate) fn inherited_enabled(scope: &mut v8::PinScope, feature: &str, id: u32, origin: &str) -> bool {
    // A popup has no container: it is a top-level document.
    let Some(parent) = crate::core::html::browsing_context::frame_parent(scope, id) else { return true };
    if !enabled_in(scope, feature, parent) {
        return false;
    }
    if declared_in(scope, parent, feature).is_some_and(|l| !l.admits(origin)) {
        return false;
    }
    let parent_origin = context_origin(scope, parent);
    if let Some(allow) = crate::core::html::browsing_context::frame_element_attr(scope, id, "allow") {
        for (name, list) in parse_feature_policy_attr(&allow, &parent_origin, origin) {
            if name == feature {
                return list.admits(origin);
            }
        }
    }
    // `allowfullscreen` adds `fullscreen *` to the container policy when `allow` did not name it (HTML,
    // "iframe element" container policy). Chrome 150 / Brave 151, measured 2026-10-05: a cross-origin
    // `<iframe allowfullscreen>` reads `fullscreenEnabled === true`.
    if feature == "fullscreen" && crate::core::html::browsing_context::frame_element_attr(scope, id, "allowfullscreen").is_some() {
        return true;
    }
    match default_of(feature) {
        Some(Default::All) => true,
        Some(Default::SelfOrigin) => origin == parent_origin,
        None => false,
    }
}

/// The origin of frame `id`'s document (an `about:` frame's creator's).
pub(crate) fn frame_origin(scope: &mut v8::PinScope, id: u32) -> String {
    context_origin(scope, Some(id))
}

/// The origin of the CURRENT realm's document -- a frame's own, else the page's.
pub(crate) fn realm_document_origin(scope: &mut v8::PinScope) -> String {
    let ctx = crate::core::html::browsing_context::current_realm_ctx(scope);
    context_origin(scope, ctx)
}

/// The policy question the RUNNING realm's document asks: is `feature` enabled there?
pub(crate) fn enabled_in_current_realm(scope: &mut v8::PinScope, feature: &str) -> bool {
    let ctx = crate::core::html::browsing_context::current_realm_ctx(scope);
    enabled_in(scope, feature, ctx)
}

/// The `allow` attribute's grammar — `Feature-Policy`'s, with `'src'` in scope.
fn parse_feature_policy_attr(value: &str, self_origin: &str, src_origin: &str) -> Vec<(String, Allowlist)> {
    let mut out = Vec::new();
    for piece in value.split(';') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        // `allow="camera"` with no allowlist means `'src'` — the one place the two grammars differ,
        // and the spec's own default for a container policy.
        let (name, rest) = match piece.split_once(char::is_whitespace) {
            Some((n, r)) => (n, r),
            None => (piece, "'src'"),
        };
        if default_of(name).is_none() {
            continue;
        }
        out.push((name.to_string(), parse_allowlist(rest, self_origin, Some(src_origin))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SELF: &str = "http://127.0.0.1:8080";

    #[test]
    fn the_registry_has_no_duplicate_and_every_default_is_one_of_two() {
        let mut seen = std::collections::HashSet::new();
        for (n, _) in REGISTRY {
            assert!(seen.insert(*n), "{n} appears twice in REGISTRY");
            assert!(n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{n} is not a policy name");
        }
        assert_eq!(REGISTRY.len(), 80, "the measured Chrome 152 feature count");
    }

    /// Every row of the header-grammar table in the module header, as Chrome 150 answered it.
    #[test]
    fn the_header_grammar_is_chromes() {
        let cases: &[(&str, &str, Allowlist)] = &[
            ("geolocation", "geolocation", Allowlist::Origins(vec![])),
            ("camera=self", "camera", Allowlist::Origins(vec![SELF.into()])),
            ("camera=\"https://example.com\"", "camera", Allowlist::Origins(vec!["https://example.com".into()])),
            ("camera=*", "camera", Allowlist::Any),
            ("camera=(none)", "camera", Allowlist::Origins(vec![])),
            ("camera=(self *)", "camera", Allowlist::Any),
            ("camera=(*)", "camera", Allowlist::Any),
            ("camera=(\"https://example.com/\")", "camera", Allowlist::Origins(vec!["https://example.com".into()])),
            ("camera=(\"http://example.com\")", "camera", Allowlist::Origins(vec!["http://example.com".into()])),
            ("camera=(self \"https://example.com\")", "camera",
             Allowlist::Origins(vec![SELF.into(), "https://example.com".into()])),
        ];
        for (header, feature, want) in cases {
            let decls = parse_permissions_policy(header, SELF);
            let got = decls.iter().rev().find(|(n, _)| n == feature).map(|(_, a)| a.clone());
            assert_eq!(got.as_ref(), Some(want), "header {header:?}");
        }
    }

    #[test]
    fn the_last_declaration_of_a_feature_wins() {
        let decls = parse_permissions_policy("geolocation=(), geolocation=*", SELF);
        let p = DocPermissionsPolicy { decls };
        assert_eq!(p.declared("geolocation"), Some(&Allowlist::Any));
    }

    #[test]
    fn a_feature_name_is_case_sensitive_and_an_unknown_one_is_skipped() {
        // `GEOLOCATION=()` changed nothing in Chrome 150, and an unknown name does not stop the
        // rest of the dictionary being applied.
        assert!(parse_permissions_policy("GEOLOCATION=()", SELF).is_empty());
        let decls = parse_permissions_policy("korvus-nope=(), geolocation=()", SELF);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].0, "geolocation");
    }

    #[test]
    fn garbage_and_an_empty_header_declare_nothing() {
        assert!(parse_permissions_policy("this is not a policy", SELF).is_empty());
        assert!(parse_permissions_policy("", SELF).is_empty());
    }

    #[test]
    fn the_legacy_feature_policy_header_is_parsed_too() {
        let decls = parse_feature_policy("geolocation 'none'; camera 'self'", SELF);
        assert_eq!(decls[0], ("geolocation".into(), Allowlist::Origins(vec![])));
        assert_eq!(decls[1], ("camera".into(), Allowlist::Origins(vec![SELF.into()])));
    }

    #[test]
    fn a_container_allow_resolves_src_and_defaults_to_it() {
        let src = "https://frame.example";
        let got = parse_feature_policy_attr("camera 'src'; geolocation 'none'; usb", SELF, src);
        assert_eq!(got[0], ("camera".into(), Allowlist::Origins(vec![src.into()])));
        assert_eq!(got[1], ("geolocation".into(), Allowlist::Origins(vec![])));
        // A bare feature in `allow` is `'src'`.
        assert_eq!(got[2], ("usb".into(), Allowlist::Origins(vec![src.into()])));
    }

    #[test]
    fn an_allowlist_admits_by_origin_and_any_admits_everything() {
        let l = Allowlist::Origins(vec![SELF.into()]);
        assert!(l.admits(SELF));
        assert!(!l.admits("https://example.com"));
        assert!(Allowlist::Any.admits("https://anything.test"));
        assert_eq!(Allowlist::Any.serialise(), vec!["*".to_string()]);
    }
}
