//! Fetch's response tainting and the CORS protocol, for the requests a PAGE makes.
//!
//! THE DEFECT. Every page `fetch()`/XHR resolved as a `basic` response with the real status, headers
//! and body, whatever its mode and whatever the server allowed. So a `no-cors` beacon was readable
//! (gtag's fallback hit fires because of it -- L17 E13, 9 `resources` rows), a CORS request the server
//! refused succeeded, no preflight was ever sent, and `redirect: 'manual'` followed the redirect.
//!
//! MEASURED on stock `google-chrome` 150.0.7871.46, page `http://127.0.0.1:A`, cross-origin
//! `http://127.0.0.2:B`, server log for the wire (every row is a live loopback probe):
//!
//! ```text
//! fetch(B/t, {mode:'no-cors'})                    opaque, status 0, headers [], body null, url "", text() ""
//! fetch(B/t)  no ACAO                             TypeError: Failed to fetch (the request WAS sent)
//! fetch(B/t)  ACAO * | ACAO <origin>              cors; headers = the safelisted ones (+ ACEH; ACEH * = all)
//! credentials:'include' + ACAO *                  Failed to fetch; ACAO <origin> + ACAC true -> cors
//! mode:'same-origin' to B                         Failed to fetch, NOTHING sent
//! redirect:'manual' (same-origin 302)             opaqueredirect, status 0, url = the request url, body null
//! redirect:'manual' + no-cors to B                Failed to fetch, nothing sent
//! redirect:'error'                                Failed to fetch after the 302 arrived
//! A -> 302 -> B  (cors)                           cors, redirected; Origin added on the B hop
//! B -> 302 -> A  (cors, ACAO * on the redirect)   cors; A hop carries `Origin: null`
//! B -> 302 (no ACAO) -> B?acao=*                  Failed to fetch; the second hop is never sent
//! A -> 302 -> B  (no-cors)                        opaque
//! {headers:{'x-foo':1}} to B                      OPTIONS first: Access-Control-Request-Method GET,
//!                                                 -Headers "x-foo" (sorted, lowercase, comma, no space)
//! PUT to B                                        OPTIONS (ACRM PUT, no ACRH); fails unless ACAM lists it
//! a second identical preflighted request < 5 s    no second OPTIONS (the preflight cache, default 5 s)
//! ACAH *                                          covers every header, `authorization` included
//! ACAH * with credentials:'include'               Failed to fetch
//! preflight answered 404 / 302                    Failed to fetch, the actual request never sent
//! POST text/plain;charset=utf-8, accept x/y       no preflight (safelisted)
//! accept: 200 x 'a'                               preflight (a safelisted value is at most 128 bytes)
//! method 'patch'                                  ACRM "patch": only DELETE GET HEAD OPTIONS POST PUT
//!                                                 are normalized, and a method list match is exact
//! ```
//!
//! What is NOT here: whether cookies are attached (`credentials`) is still the client jar's decision
//! for every request, and a module script or `<img crossorigin>` is not CORS-checked (they do not go
//! through a page's request path).

use crate::{FetchResponse, Mode};

/// Fetch's `redirect` mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RedirectMode {
    #[default]
    Follow,
    Error,
    Manual,
}

/// Fetch's `credentials` mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialsMode {
    Omit,
    #[default]
    SameOrigin,
    Include,
}

/// A response's type, as `Response.type` names it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseType {
    #[default]
    Basic,
    Cors,
    Opaque,
    #[serde(rename = "opaqueredirect")]
    OpaqueRedirect,
}

impl ResponseType {
    pub fn token(self) -> &'static str {
        match self {
            ResponseType::Basic => "basic",
            ResponseType::Cors => "cors",
            ResponseType::Opaque => "opaque",
            ResponseType::OpaqueRedirect => "opaqueredirect",
        }
    }
}

/// What a PAGE request carries that an engine request does not: the origin of the environment that
/// made it, and the headers its author set (before the engine added Origin, Referer or the persona's).
/// Its presence is what subjects a request to tainting and CORS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    /// The client's origin, ASCII-serialized (`http://127.0.0.1:8000`).
    pub origin: String,
    pub author_headers: Vec<(String, String)>,
    /// **The loader asked for CORS explicitly**, rather than the request merely having ended up in
    /// cors mode — Blink's `FetchParameters::SetCrossOriginAccessControl`, which is called for a
    /// module script and for a `crossorigin`-attributed subresource, and which sets the `Origin`
    /// header unconditionally instead of leaving it to the tainting rule.
    ///
    /// MEASURED 2026-09-18 on a loopback socket, all four installed Chromium-family browsers
    /// agreeing row for row:
    ///
    /// ```text
    /// request                                   Origin
    /// fetch() same-origin GET (mode cors)       -          <- Fetch's rule: tainting is basic
    /// XHR    same-origin GET                    -
    /// fetch() same-origin POST                  the origin <- Fetch's rule: not GET/HEAD
    /// fetch()/XHR cross-origin, mode cors       the origin
    /// import("<same-origin>.js")                THE ORIGIN <- neither of those two rules
    /// <script crossorigin src="<same-origin>">  THE ORIGIN
    /// <img crossorigin src="<same-origin>">     THE ORIGIN
    /// <script src="<cross-origin>"> (no-cors)   -
    /// new Worker("<same-origin>.js")            -          <- mode same-origin, not cors
    /// ```
    ///
    /// The last three rows of the "the origin" group are a Chromium deviation from Fetch, not a
    /// misreading of it, and they are on the wire on every page with a module graph. It is a flag
    /// rather than a `destination == Script` test because `<img crossorigin>` shows the same
    /// behaviour: what these requests share is the loader's call, not their destination.
    pub cors_preconfigured: bool,
}

/// Fetch's method normalization: the six names matched case-insensitively become uppercase; every
/// other method is left exactly as written.
pub fn normalize_method(m: &str) -> String {
    for n in ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"] {
        if m.eq_ignore_ascii_case(n) {
            return n.to_string();
        }
    }
    m.to_string()
}

/// A CORS-safelisted method.
pub fn is_safelisted_method(m: &str) -> bool {
    matches!(m, "GET" | "HEAD" | "POST")
}

fn has_cors_unsafe_byte(v: &str) -> bool {
    v.bytes().any(|b| (b < 0x20 && b != 0x09) || matches!(b, b'"' | b'(' | b')' | b':' | b'<' | b'>' | b'?' | b'@' | b'[' | b'\\' | b']' | b'{' | b'}' | 0x7f))
}

/// Fetch's "CORS-safelisted request-header" (without `range`, which a page fetch here never sends).
pub fn is_safelisted_request_header(name: &str, value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    match name.to_ascii_lowercase().as_str() {
        "accept" => !has_cors_unsafe_byte(value),
        "accept-language" | "content-language" => {
            value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'*' | b',' | b'-' | b'.' | b';' | b'='))
        }
        "content-type" => {
            if has_cors_unsafe_byte(value) {
                return false;
            }
            // `IsCorsSafelistedLowerCaseContentType` (`cors.cc`): `net::ExtractMimeTypeFromMediaType`, no comma (A/B C5).
            let essence = crate::mime::extract_mime_type_from_media_type(&value.to_ascii_lowercase(), false).unwrap_or_default();
            matches!(essence.as_str(), "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain")
        }
        _ => false,
    }
}

/// The names that make a request need a preflight: lowercase, sorted, unique ("CORS-unsafe request-header
/// names"). A safelisted value set whose total length exceeds 1024 bytes makes all of them unsafe.
pub fn unsafe_header_names(headers: &[(String, String)]) -> Vec<String> {
    let mut unsafe_names = Vec::new();
    let mut safelisted_len = 0usize;
    for (k, v) in headers {
        if is_safelisted_request_header(k, v) {
            safelisted_len += v.len();
        } else {
            unsafe_names.push(k.to_ascii_lowercase());
        }
    }
    if safelisted_len > 1024 {
        for (k, v) in headers {
            if is_safelisted_request_header(k, v) {
                unsafe_names.push(k.to_ascii_lowercase());
            }
        }
    }
    unsafe_names.sort();
    unsafe_names.dedup();
    unsafe_names
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
}

/// Fetch's "CORS check" of a response against the request's serialized `origin`.
pub fn cors_check(headers: &[(String, String)], origin: &str, credentials: CredentialsMode) -> bool {
    let acao = header(headers, "access-control-allow-origin");
    // Exactly one value (a comma-joined pair is not one value either).
    if acao.len() != 1 || acao[0].contains(',') {
        return false;
    }
    let v = acao[0].trim();
    if credentials != CredentialsMode::Include && v == "*" {
        return true;
    }
    if v != origin {
        return false;
    }
    if credentials != CredentialsMode::Include {
        return true;
    }
    header(headers, "access-control-allow-credentials").first().map(|c| c.trim() == "true").unwrap_or(false)
}

fn token_list(headers: &[(String, String)], name: &str) -> Vec<String> {
    header(headers, name)
        .iter()
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// A successful preflight's grant, as the preflight cache keeps it.
#[derive(Debug, Clone)]
pub struct PreflightGrant {
    methods: Vec<String>,
    headers: Vec<String>,
    credentials: bool,
    expires: std::time::Instant,
}

impl PreflightGrant {
    fn allows(&self, method: &str, unsafe_headers: &[String]) -> bool {
        let wildcard = !self.credentials;
        let method_ok = is_safelisted_method(method)
            || self.methods.iter().any(|m| m == method)
            || (wildcard && self.methods.iter().any(|m| m == "*"));
        method_ok
            && unsafe_headers.iter().all(|h| self.headers.iter().any(|g| g.eq_ignore_ascii_case(h)) || (wildcard && self.headers.iter().any(|g| g == "*")))
    }
}

/// Check a preflight response for `method`/`unsafe_headers`: `Some(grant)` when it allows them.
pub fn preflight_grant(status: u16, headers: &[(String, String)], origin: &str, credentials: CredentialsMode, method: &str, unsafe_headers: &[String]) -> Option<PreflightGrant> {
    if !(200..300).contains(&status) || !cors_check(headers, origin, credentials) {
        return None;
    }
    // `Access-Control-Max-Age`: 5 s when absent (Chromium's default), capped at 2 h (Chromium's cap).
    let max_age = header(headers, "access-control-max-age").first().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(5).min(7200);
    let grant = PreflightGrant {
        methods: token_list(headers, "access-control-allow-methods"),
        headers: token_list(headers, "access-control-allow-headers"),
        credentials: credentials == CredentialsMode::Include,
        expires: std::time::Instant::now() + std::time::Duration::from_secs(max_age),
    };
    grant.allows(method, unsafe_headers).then_some(grant)
}

type CacheKey = (String, String, bool);

fn preflight_cache() -> &'static std::sync::Mutex<Vec<(CacheKey, PreflightGrant)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<(CacheKey, PreflightGrant)>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Is a live cached preflight for (`origin`, `url`, credentials) enough for this request?
pub fn cached_preflight_allows(origin: &str, url: &str, credentials: CredentialsMode, method: &str, unsafe_headers: &[String]) -> bool {
    let now = std::time::Instant::now();
    let include = credentials == CredentialsMode::Include;
    let Ok(mut c) = preflight_cache().lock() else { return false };
    c.retain(|(_, g)| g.expires > now);
    c.iter().any(|((o, u, cr), g)| o == origin && u == url && *cr == include && g.allows(method, unsafe_headers))
}

/// Remember a preflight grant.
pub fn cache_preflight(origin: &str, url: &str, grant: PreflightGrant) {
    if let Ok(mut c) = preflight_cache().lock() {
        c.push(((origin.to_string(), url.to_string(), grant.credentials), grant));
    }
}

/// Does a request need a preflight?
pub fn needs_preflight(method: &str, author_headers: &[(String, String)]) -> bool {
    !is_safelisted_method(method) || !unsafe_header_names(author_headers).is_empty()
}

/// The CORS-safelisted response-header names.
const SAFELISTED_RESPONSE_HEADERS: &[&str] =
    &["cache-control", "content-language", "content-length", "content-type", "expires", "last-modified", "pragma"];

/// The response a PAGE is shown: `(status, status_text, url, headers, body)` of `resp` filtered for its
/// type. `basic` drops `set-cookie`/`set-cookie2`; `cors` keeps the safelisted names and the exposed
/// ones; `opaque` shows nothing; `opaqueredirect` shows only its url. `body` is `None` for a null body.
pub fn filtered_for_page(resp: &FetchResponse) -> (u16, String, String, Vec<(String, String)>, Option<Vec<u8>>) {
    let forbidden = |k: &str| k.eq_ignore_ascii_case("set-cookie") || k.eq_ignore_ascii_case("set-cookie2");
    match resp.response_type {
        ResponseType::Basic => {
            let h = resp.headers.iter().filter(|(k, _)| !forbidden(k)).cloned().collect();
            (resp.status, resp.status_text.clone(), resp.url.clone(), h, Some(resp.bytes.clone()))
        }
        ResponseType::Cors => {
            let exposed = token_list(&resp.headers, "access-control-expose-headers");
            let all = resp.credentials != CredentialsMode::Include && exposed.iter().any(|e| e == "*");
            let h = resp
                .headers
                .iter()
                .filter(|(k, _)| !forbidden(k))
                .filter(|(k, _)| all || SAFELISTED_RESPONSE_HEADERS.iter().any(|s| k.eq_ignore_ascii_case(s)) || exposed.iter().any(|e| k.eq_ignore_ascii_case(e)))
                .cloned()
                .collect();
            (resp.status, resp.status_text.clone(), resp.url.clone(), h, Some(resp.bytes.clone()))
        }
        ResponseType::Opaque => (0, String::new(), String::new(), Vec::new(), None),
        ResponseType::OpaqueRedirect => (0, String::new(), resp.url.clone(), Vec::new(), None),
    }
}

/// The tainting a request to `url` takes on in `mode`, from `origin`. `Err` for a cross-origin URL a
/// `same-origin` request may not reach.
pub fn taint(current: ResponseType, origin: &str, url: &url::Url, mode: Mode) -> Result<ResponseType, &'static str> {
    if current != ResponseType::Basic || url.origin().ascii_serialization() == origin || url.scheme() == "data" {
        return Ok(current);
    }
    match mode {
        Mode::SameOrigin => Err("mode same-origin: a cross-origin url"),
        Mode::NoCors => Ok(ResponseType::Opaque),
        _ => Ok(ResponseType::Cors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const O: &str = "http://127.0.0.1:8000";

    #[test]
    fn the_cors_check_is_chromes_table() {
        use CredentialsMode::*;
        assert!(!cors_check(&h(&[]), O, SameOrigin), "no ACAO");
        assert!(cors_check(&h(&[("Access-Control-Allow-Origin", "*")]), O, SameOrigin));
        assert!(cors_check(&h(&[("access-control-allow-origin", O)]), O, SameOrigin));
        assert!(!cors_check(&h(&[("access-control-allow-origin", "http://x.test")]), O, SameOrigin));
        assert!(!cors_check(&h(&[("access-control-allow-origin", "*")]), O, Include), "* with credentials");
        assert!(!cors_check(&h(&[("access-control-allow-origin", O)]), O, Include), "no ACAC");
        assert!(cors_check(&h(&[("access-control-allow-origin", O), ("access-control-allow-credentials", "true")]), O, Include));
        assert!(!cors_check(&h(&[("access-control-allow-origin", O), ("access-control-allow-origin", O)]), O, SameOrigin), "two values");
        assert!(cors_check(&h(&[("access-control-allow-origin", "null")]), "null", SameOrigin), "a tainted origin");
    }

    #[test]
    fn preflight_needs_and_grants_are_chromes_table() {
        use CredentialsMode::*;
        assert!(!needs_preflight("POST", &h(&[("content-type", "text/plain;charset=utf-8"), ("accept", "x/y"), ("accept-language", "ko"), ("content-language", "en")])));
        assert!(needs_preflight("GET", &h(&[("accept", &"a".repeat(200))])), "a safelisted value is at most 128 bytes");
        assert!(needs_preflight("PUT", &[]));
        assert!(needs_preflight("POST", &h(&[("content-type", "application/json")])));
        assert_eq!(unsafe_header_names(&h(&[("X-Z", "1"), ("x-a", "2"), ("accept", "a")])), ["x-a", "x-z"]);
        let unsafe_xq = vec!["x-q".to_string()];
        assert!(preflight_grant(200, &h(&[("access-control-allow-origin", "*"), ("access-control-allow-headers", "x-q")]), O, SameOrigin, "GET", &unsafe_xq).is_some());
        assert!(preflight_grant(200, &h(&[("access-control-allow-origin", "*")]), O, SameOrigin, "GET", &unsafe_xq).is_none(), "the header is not allowed");
        assert!(preflight_grant(404, &h(&[("access-control-allow-origin", "*"), ("access-control-allow-headers", "x-q")]), O, SameOrigin, "GET", &unsafe_xq).is_none(), "a 404 preflight");
        assert!(preflight_grant(200, &h(&[("access-control-allow-origin", "*"), ("access-control-allow-headers", "*")]), O, SameOrigin, "GET", &["authorization".to_string()]).is_some(), "* covers authorization (measured)");
        assert!(preflight_grant(200, &h(&[("access-control-allow-origin", O), ("access-control-allow-credentials", "true"), ("access-control-allow-headers", "*")]), O, Include, "GET", &unsafe_xq).is_none(), "* is not a wildcard with credentials");
        assert!(preflight_grant(200, &h(&[("access-control-allow-origin", "*"), ("access-control-allow-methods", "PUT")]), O, SameOrigin, "PUT", &[]).is_some());
        assert!(preflight_grant(200, &h(&[("access-control-allow-origin", "*"), ("access-control-allow-methods", "PATCH")]), O, SameOrigin, "patch", &[]).is_none(), "an unnormalized method matches exactly");
        assert_eq!(normalize_method("delete"), "DELETE");
        assert_eq!(normalize_method("patch"), "patch");
    }

    #[test]
    fn a_page_sees_the_response_its_type_allows() {
        let base = FetchResponse::new(
            200,
            "OK".into(),
            "http://b/t".into(),
            h(&[("content-type", "text/plain"), ("x-a", "1"), ("x-b", "2"), ("set-cookie", "s=1"), ("access-control-expose-headers", "x-a")]),
            b"hello".to_vec(),
        );
        let names = |r: &FetchResponse| filtered_for_page(r).3.into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        assert_eq!(names(&base), ["content-type", "x-a", "x-b", "access-control-expose-headers"], "basic drops set-cookie");
        let cors = FetchResponse { response_type: ResponseType::Cors, ..base.clone() };
        assert_eq!(names(&cors), ["content-type", "x-a"]);
        let opaque = FetchResponse { response_type: ResponseType::Opaque, ..base.clone() };
        assert_eq!(filtered_for_page(&opaque), (0, String::new(), String::new(), vec![], None));
        let redir = FetchResponse { response_type: ResponseType::OpaqueRedirect, ..base };
        assert_eq!(filtered_for_page(&redir).2, "http://b/t");
    }

    #[test]
    fn tainting_follows_the_mode() {
        let b = url::Url::parse("http://127.0.0.2:9/t").unwrap();
        let a = url::Url::parse("http://127.0.0.1:8000/t").unwrap();
        assert_eq!(taint(ResponseType::Basic, O, &a, Mode::Cors), Ok(ResponseType::Basic));
        assert_eq!(taint(ResponseType::Basic, O, &b, Mode::Cors), Ok(ResponseType::Cors));
        assert_eq!(taint(ResponseType::Basic, O, &b, Mode::NoCors), Ok(ResponseType::Opaque));
        assert!(taint(ResponseType::Basic, O, &b, Mode::SameOrigin).is_err());
        assert_eq!(taint(ResponseType::Cors, O, &a, Mode::Cors), Ok(ResponseType::Cors), "tainting never comes back");
    }
}
